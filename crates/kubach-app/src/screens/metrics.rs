//! "Metrics" block at the top of the details: CPU, memory, network and
//! filesystem tabs with a chart each.
//!
//! With a Prometheus (ADR 0009) the charts cover the last 15m / 1h / 6h /
//! 24h / 7d; ranges with nothing more to show than a shorter one (a young
//! pod, short retention) are disabled, found by one instant query with the
//! chart refresh while some range is disabled (#376). Without Prometheus, every metrics-server poll adds a CPU
//! and memory sample, so those charts grow while the panel stays open
//! (5m / 15m / All, the windows open as history collects); network and
//! filesystem need Prometheus. Requests and limits (allocatable for nodes) are dashed
//! lines; pods with several containers and workloads can be split per
//! container or pod.
//!
//! The block is an entity of its own: new samples re-render it, not the
//! whole details panel.

use futures::StreamExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::metrics::DEFAULT_POLL_INTERVAL;
use kubach_core::metrics::live::{LiveHistory, LiveRange, LiveSample};
use kubach_core::metrics::prometheus::{
    Chart, Metric, PanelCharts, Prometheus, REFRESH_INTERVAL, Range, Target, coverage,
};
use kubach_core::metrics::shared::{MetricsServer, PollResult, PollScope};
use kubach_core::runtime;
use kubach_i18n::{Message, t};
use kubach_ui::{ChartLine, LineChart, Panel, SegmentTab, SegmentTabs, Tone, format_age, unix_now};

use super::prometheus_link::{self, PrometheusRegistry};

/// The name of a metric (tab, chart title).
pub fn metric_title(metric: Metric) -> String {
    match metric {
        Metric::Cpu => "CPU".to_owned(),
        Metric::Memory => t!("metrics-memory"),
        Metric::Network => t!("metrics-network"),
        Metric::Filesystem => t!("metrics-filesystem"),
    }
}

/// The name of a line of a chart that is not split (see
/// [`Metric::lines`]); split lines are named after containers or pods.
pub fn line_name(name: &str) -> String {
    match name {
        "Working set" => t!("metrics-line-working-set"),
        "Receive" => t!("metrics-line-receive"),
        "Transmit" => t!("metrics-line-transmit"),
        "Read" => t!("metrics-line-read"),
        "Write" => t!("metrics-line-write"),
        name => name.to_owned(),
    }
}

/// A message of [`kubach_core`] with argument `name` set to `value`, such
/// as the subject of a range that is not offered.
pub fn with_arg(message: &Message, name: &'static str, value: &'static str) -> Message {
    let mut message = message.clone();
    message.args.retain(|(arg, _)| arg != name);
    message.arg(name, value)
}

/// A request and a limit (or allocatable), in the metric's unit (cores,
/// bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reference {
    pub request: Option<f64>,
    pub limit: Option<f64>,
}

/// What the block charts, as the details know it.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricsSubject {
    pub target: Target,
    /// Pods sampled from metrics-server (the pod itself, or a workload's).
    pub pods: Vec<String>,
    /// Containers of a pod, pods of a workload: splitting needs two.
    pub parts: usize,
    pub cpu: Reference,
    pub memory: Reference,
}

impl MetricsSubject {
    fn reference(&self, metric: Metric) -> Option<Reference> {
        match metric {
            Metric::Cpu => Some(self.cpu),
            Metric::Memory => Some(self.memory),
            Metric::Network | Metric::Filesystem => None,
        }
    }

    fn namespace(&self) -> Option<&str> {
        match &self.target {
            Target::Pod { namespace, .. } | Target::Workload { namespace, .. } => Some(namespace),
            Target::Node { .. } => None,
        }
    }

    /// `By container` or `By pod`, when splitting makes sense.
    fn split_label(&self) -> Option<String> {
        match self.target.split_label()? {
            "container" if self.parts > 1 => Some(t!("metrics-by-container")),
            "pod" if self.parts > 1 => Some(t!("metrics-by-pod")),
            _ => None,
        }
    }
}

/// Where the numbers come from.
#[derive(Clone)]
pub enum MetricsSource {
    Prometheus(Prometheus),
    /// Polls shared with the Pods / Nodes table.
    Server(Box<MetricsServer>),
    /// No metrics-server, Prometheus still being looked up.
    Searching,
    None,
}

impl MetricsSource {
    /// Identity, to tell when the block must start over.
    pub fn key(&self) -> String {
        match self {
            Self::Prometheus(prometheus) => {
                format!("prometheus {}", prometheus.endpoint().describe())
            }
            Self::Server(_) => "metrics-server".into(),
            Self::Searching => "searching".into(),
            Self::None => "none".into(),
        }
    }
}

/// State of the metrics-server polling.
enum ServerState {
    Loading,
    Ready,
    Failed,
}

pub struct ResourceMetrics {
    /// Context of the object, for "Detect again" and "Settings".
    context: String,
    /// Why the chosen Prometheus is not used (it does not answer).
    hint: Option<String>,
    subject: MetricsSubject,
    source: MetricsSource,
    tab: Metric,
    split: bool,
    range: Range,
    /// The range was picked by hand: data found later does not change it.
    range_picked: bool,
    /// Why each of [`Range::ALL`] is disabled; `None` until known (or when
    /// Prometheus could not tell), and then every range is offered.
    coverage: Option<[Option<Message>; Range::ALL.len()]>,
    live_range: LiveRange,
    /// Prometheus charts: `None` while loading, `Some(None)` on failure.
    charts: Option<Option<PanelCharts>>,
    live: LiveHistory,
    server: ServerState,
    _task: Option<Task<()>>,
    _registry: Option<Subscription>,
}

impl ResourceMetrics {
    pub fn new(
        context: String,
        subject: MetricsSubject,
        source: MetricsSource,
        cx: &mut Context<Self>,
    ) -> Self {
        // "Detect again" shows its progress.
        let registry = PrometheusRegistry::try_global(cx)
            .map(|registry| cx.observe(&registry, |_, _, cx| cx.notify()));
        let mut this = Self {
            context,
            hint: None,
            subject,
            source,
            tab: Metric::Cpu,
            split: false,
            range: Range::default(),
            range_picked: false,
            coverage: None,
            live_range: LiveRange::default(),
            charts: None,
            live: LiveHistory::default(),
            server: ServerState::Loading,
            _task: None,
            _registry: registry,
        };
        this.start(cx);
        this
    }

    /// Whether this block charts `target` from `source`.
    pub fn shows(&self, target: &Target, source: &str) -> bool {
        &self.subject.target == target && self.source.key() == source
    }

    /// Shows (or with `None` hides) why the chosen Prometheus is not used.
    pub fn set_hint(&mut self, hint: Option<String>, cx: &mut Context<Self>) {
        if self.hint != hint {
            self.hint = hint;
            cx.notify();
        }
    }

    /// The problem with Prometheus to show above the charts: the chosen
    /// server failed to connect, or stopped answering queries.
    fn problem(&self) -> Option<String> {
        if let Some(hint) = &self.hint {
            return Some(hint.clone());
        }
        match (&self.source, &self.charts) {
            (MetricsSource::Prometheus(prometheus), Some(None)) => {
                Some(prometheus_link::doesnt_answer(prometheus.endpoint()))
            }
            _ => None,
        }
    }

    /// The problem with its actions: search the cluster again, or open the
    /// Prometheus settings.
    fn problem_bar(&self, cx: &App) -> Option<AnyElement> {
        let problem = self.problem()?;
        Some(prometheus_link::problem_bar(
            "metrics",
            &self.context,
            problem,
            cx,
        ))
    }

    /// Updates pods and references (pods come and go, specs change)
    /// without starting over.
    pub fn set_subject(&mut self, subject: MetricsSubject, cx: &mut Context<Self>) {
        if self.subject != subject {
            self.subject = subject;
            if self.subject.split_label().is_none() {
                self.split = false;
            }
            cx.notify();
        }
    }

    /// Whether some range is still disabled (or not known yet): then each
    /// refresh asks again how far back the object's data goes, so ranges
    /// open as a young pod ages. Once all are offered, no more asking.
    fn needs_coverage(&self) -> bool {
        self.coverage
            .as_ref()
            .is_none_or(|unavailable| unavailable.iter().any(Option::is_some))
    }

    /// Applies the oldest sample of the object (see
    /// [`coverage::unavailable`]); `true` when the default range had no
    /// more to show than a shorter one and was switched to it.
    fn set_coverage(&mut self, oldest: Option<f64>) -> bool {
        let unavailable = coverage::unavailable(oldest, unix_now() as f64);
        let fitted = coverage::fit(self.range, &unavailable);
        self.coverage = Some(unavailable);
        if self.range_picked || fitted == self.range {
            return false;
        }
        self.range = fitted;
        self.charts = None;
        true
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self._task = match self.source.clone() {
            MetricsSource::Prometheus(prometheus) => {
                let target = self.subject.target.clone();
                Some(cx.spawn(async move |this, cx| {
                    loop {
                        let Ok((range, coverage)) =
                            this.read_with(cx, |this, _| (this.range, this.needs_coverage()))
                        else {
                            break;
                        };
                        let (prometheus, target) = (prometheus.clone(), target.clone());
                        let handle = runtime::spawn(async move {
                            let oldest = async {
                                // One instant query; a failure offers every range.
                                match coverage {
                                    true => prometheus.oldest(&target).await.ok(),
                                    false => None,
                                }
                            };
                            futures::join!(prometheus.panel(&target, range), oldest)
                        });
                        let (charts, oldest) = match handle.await {
                            Ok((charts, oldest)) => (charts.ok(), oldest),
                            Err(_) => (None, None),
                        };
                        let updated = this.update(cx, |this, cx| {
                            if let Some(oldest) = oldest
                                && this.set_coverage(oldest)
                            {
                                // Drawn at once in the range that fits.
                                cx.notify();
                                return true;
                            }
                            if oldest.is_some() {
                                cx.notify();
                            }
                            // The same samples again: nothing to redraw.
                            if this.charts.as_ref() != Some(&charts) {
                                this.charts = Some(charts);
                                cx.notify();
                            }
                            false
                        });
                        match updated {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(_) => break,
                        }
                        cx.background_executor().timer(REFRESH_INTERVAL).await;
                    }
                }))
            }
            MetricsSource::Server(server) => {
                // Joins the table's poll when it covers the object.
                let scope = match &self.subject.target {
                    Target::Node { .. } => PollScope::Nodes,
                    _ => PollScope::pods(self.subject.namespace().map(|ns| vec![ns.to_owned()])),
                };
                let mut results = server.subscribe(scope).results();
                Some(cx.spawn(async move |this, cx| {
                    while let Some(result) = results.next().await {
                        if this.update(cx, |this, cx| this.add(&result, cx)).is_err() {
                            break;
                        }
                    }
                }))
            }
            MetricsSource::Searching | MetricsSource::None => None,
        };
    }

    fn add(&mut self, result: &PollResult, cx: &mut Context<Self>) {
        let now = unix_now() as f64;
        match result {
            Ok(polled) => {
                let sample = match (&self.subject.target, self.subject.namespace()) {
                    (Target::Node { name }, _) => {
                        polled.node(name).map(|node| LiveSample::of_node(node, now))
                    }
                    (_, Some(namespace)) => {
                        LiveSample::of_pods(polled.pods_in(namespace), &self.subject.pods, now)
                    }
                    (_, None) => None,
                };
                if let Some(sample) = sample {
                    self.live.push(sample);
                }
                self.server = ServerState::Ready;
            }
            Err(_) => {
                // Keep what was charted; a later poll may succeed.
                if self.live.is_empty() {
                    self.server = ServerState::Failed;
                }
            }
        }
        cx.notify();
    }

    fn set_range(&mut self, range: Range, cx: &mut Context<Self>) {
        if self.range != range {
            self.range = range;
            self.charts = None;
            self.start(cx);
            cx.notify();
        }
    }

    /// The chart shown for `metric`, if there is data for it.
    fn chart(&self, metric: Metric, split: bool) -> Option<Chart> {
        let chart = match &self.source {
            MetricsSource::Prometheus(_) => {
                let charts = self.charts.as_ref()?.as_ref()?;
                let list = if split {
                    &charts.splits
                } else {
                    &charts.totals
                };
                list.iter().find(|c| c.metric == metric).cloned()
            }
            MetricsSource::Server(_) => {
                let window = self.live.range_window(
                    self.live_range,
                    unix_now() as f64,
                    DEFAULT_POLL_INTERVAL.as_secs(),
                );
                self.live.chart(metric, split, window)
            }
            MetricsSource::Searching | MetricsSource::None => None,
        }?;
        (!chart.is_empty()).then_some(chart)
    }

    /// Why `metric` has no chart.
    fn missing(&self, metric: Metric) -> String {
        match (&self.source, metric) {
            (MetricsSource::Searching, _) => t!("metrics-looking-for-prometheus"),
            (MetricsSource::None, _) => t!("metrics-no-source"),
            (MetricsSource::Server(_), Metric::Network | Metric::Filesystem) => {
                t!("metrics-needs-prometheus")
            }
            (MetricsSource::Server(_), _) => match self.server {
                ServerState::Loading => t!("metrics-waiting-server"),
                ServerState::Failed => t!("metrics-server-no-answer"),
                ServerState::Ready => t!("metrics-server-no-sample"),
            },
            (MetricsSource::Prometheus(_), _) => match &self.charts {
                None => t!("metrics-loading-prometheus"),
                Some(None) => t!("metrics-prometheus-no-answer"),
                Some(Some(_)) => t!("metrics-no-data"),
            },
        }
    }

    fn source_note(&self) -> String {
        match &self.source {
            MetricsSource::Prometheus(prometheus) => {
                format!("Prometheus · {}", prometheus.endpoint().describe())
            }
            MetricsSource::Server(_) => match self.live.len() {
                0 | 1 => "metrics-server".into(),
                _ => t!("metrics-server-while-open"),
            },
            MetricsSource::Searching | MetricsSource::None => String::new(),
        }
    }
}

/// Theme slot of line `line` of `metric` when not split: CPU blue, memory
/// green, network and filesystem two lines each.
fn slot(metric: Metric, line: usize) -> usize {
    match metric {
        Metric::Cpu => 0,
        Metric::Memory => 1,
        Metric::Network | Metric::Filesystem => 2 + line,
    }
}

impl Render for ResourceMetrics {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tabs: Vec<SegmentTab> = Metric::ALL
            .iter()
            .map(|&metric| {
                let tab = SegmentTab::new(metric_title(metric));
                if self.chart(metric, false).is_some() {
                    tab
                } else {
                    tab.muted(true).tooltip(self.missing(metric))
                }
            })
            .collect();
        let active = Metric::ALL
            .iter()
            .position(|m| *m == self.tab)
            .unwrap_or_default();
        let metric_tabs = SegmentTabs::new("metrics-tabs", tabs, active).on_select(cx.listener(
            |this, ix: &usize, _, cx| {
                this.tab = Metric::ALL[*ix];
                cx.notify();
            },
        ));

        let split_label = self
            .subject
            .split_label()
            .filter(|_| matches!(self.tab, Metric::Cpu | Metric::Memory));
        let split = self.split && split_label.is_some();
        let split_tabs = split_label.map(|label| {
            SegmentTabs::new(
                "metrics-split",
                vec![SegmentTab::new(t!("metrics-total")), SegmentTab::new(label)],
                usize::from(split),
            )
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                this.split = *ix == 1;
                cx.notify();
            }))
        });
        let range_tab = |label: &'static str, why: Option<&Message>| {
            let tab = SegmentTab::new(label);
            match why {
                Some(why) => tab.disabled(true).tooltip(why.to_string()),
                None => tab,
            }
        };
        let range_tabs = match &self.source {
            MetricsSource::Prometheus(_) => {
                let active = Range::ALL
                    .iter()
                    .position(|r| *r == self.range)
                    .unwrap_or_default();
                let tabs = Range::ALL
                    .iter()
                    .enumerate()
                    .map(|(ix, r)| {
                        range_tab(
                            r.label(),
                            self.coverage.as_ref().and_then(|c| c[ix].as_ref()),
                        )
                    })
                    .collect();
                Some(
                    SegmentTabs::new("metrics-range", tabs, active).on_select(cx.listener(
                        |this, ix: &usize, _, cx| {
                            this.range_picked = true;
                            this.set_range(Range::ALL[*ix], cx)
                        },
                    )),
                )
            }
            MetricsSource::Server(_) if !self.live.is_empty() => {
                let active = LiveRange::ALL
                    .iter()
                    .position(|r| *r == self.live_range)
                    .unwrap_or_default();
                let unavailable = self.live.unavailable(unix_now() as f64);
                let tabs = LiveRange::ALL
                    .iter()
                    .zip(&unavailable)
                    .map(|(r, why)| range_tab(r.label(), why.as_ref()))
                    .collect();
                Some(
                    SegmentTabs::new("metrics-live-range", tabs, active).on_select(cx.listener(
                        |this, ix: &usize, _, cx| {
                            this.live_range = LiveRange::ALL[*ix];
                            cx.notify();
                        },
                    )),
                )
            }
            _ => None,
        };
        let options = (split_tabs.is_some() || range_tabs.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().children(split_tabs))
                .children(range_tabs)
        });

        let metric = self.tab;
        let chart = self.chart(metric, split);
        let body = match chart {
            Some(chart) => {
                let mut view = LineChart::new(
                    SharedString::from(format!("metrics-{}", metric.title())),
                    "",
                )
                .lines(chart.series.iter().enumerate().map(|(ix, series)| {
                    let line = ChartLine::new(
                        if split {
                            series.name.clone()
                        } else {
                            line_name(&series.name)
                        },
                        series.points.iter().map(|p| (p.time, p.value)).collect(),
                    );
                    if split {
                        line.slot(ix)
                    } else {
                        line.slot(slot(metric, ix))
                    }
                }))
                .x_range(chart.window.start as f64, chart.window.end as f64)
                .format(move |value| metric.format(value).into());
                if metric != Metric::Cpu {
                    // Bytes: the axis tops out at 2 GiB, not 1907.3 MiB.
                    view = view.binary();
                }
                view = match &self.source {
                    MetricsSource::Prometheus(_) => {
                        view.x_labels(format!("-{}", self.range.label()), t!("metrics-now"))
                    }
                    _ => view.x_labels(
                        format!(
                            "-{}",
                            format_age(chart.window.end.saturating_sub(chart.window.start))
                        ),
                        t!("metrics-now"),
                    ),
                };
                if !split && let Some(reference) = self.subject.reference(metric) {
                    let node = matches!(self.subject.target, Target::Node { .. });
                    if let Some(request) = reference.request {
                        view = view.threshold(t!("metrics-request"), request, Tone::Info);
                    }
                    if let Some(limit) = reference.limit {
                        let name = if node {
                            t!("metrics-allocatable")
                        } else {
                            t!("metrics-limit")
                        };
                        view = view.threshold(name, limit, Tone::Warning);
                    }
                }
                view.into_any_element()
            }
            None => div()
                .h(px(96.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_dashed()
                .border_color(theme.border)
                .px_4()
                .text_xs()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(self.missing(metric))
                .into_any_element(),
        };
        let note = self.source_note();
        let problem = self.problem_bar(cx);
        Panel::new()
            .title(t!("section-metrics"))
            .when(!note.is_empty(), |panel| panel.note(note))
            .children(problem)
            .child(metric_tabs)
            .children(options)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::prometheus::{Target, Workload};

    use super::{MetricsSubject, Reference};

    fn subject(target: Target, parts: usize) -> MetricsSubject {
        MetricsSubject {
            target,
            pods: Vec::new(),
            parts,
            cpu: Reference::default(),
            memory: Reference::default(),
        }
    }

    #[test]
    fn splits_only_with_several_parts() {
        let pod = Target::Pod {
            namespace: "ns".into(),
            name: "p".into(),
        };
        assert_eq!(subject(pod.clone(), 1).split_label(), None);
        assert_eq!(
            subject(pod, 3).split_label().as_deref(),
            Some("By container")
        );
        let workload = Target::Workload {
            kind: Workload::Deployment,
            namespace: "ns".into(),
            name: "d".into(),
        };
        assert_eq!(
            subject(workload, 2).split_label().as_deref(),
            Some("By pod")
        );
        let node = Target::Node { name: "n".into() };
        assert_eq!(subject(node, 5).split_label(), None);
    }
}
