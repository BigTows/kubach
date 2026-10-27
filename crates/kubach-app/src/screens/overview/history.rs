//! The "CPU and memory" card of the Overview screen: usage of the pods in
//! scope over time, with the range picked on the card (#481).
//!
//! The card is an entity of its own and asks Prometheus for its two charts
//! on its own ([`Prometheus::overview_usage`]): a new range reloads only
//! them. The cards above and pods by phase below come from the screen's
//! own query over a range of their own, so they stay as they are. While the
//! new range loads, the charts of the old one stay, dimmed.
//!
//! Without Prometheus the charts grow from the metrics-server polls the
//! screen passes in ([`UsageHistory::push`]).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::metrics::DEFAULT_POLL_INTERVAL;
use kubach_core::metrics::live::{LiveHistory, LiveRange, LiveSample};
use kubach_core::metrics::overview::{self, HistorySource, ResourceTotals};
use kubach_core::metrics::prometheus::overview::UsageCharts;
use kubach_core::metrics::prometheus::{
    Chart, Metric, Prometheus, REFRESH_INTERVAL, Range, Window as ChartWindow, coverage,
};
use kubach_core::runtime;
use kubach_i18n::{Message, t};
use kubach_ui::{ChartLine, LineChart, Panel, SegmentTab, SegmentTabs, Tone, format_age, unix_now};

use super::super::metrics::{line_name, metric_title, with_arg};
use super::{Server, placeholder};

/// Charts Prometheus answered with, and the range they cover: the old
/// range's stay drawn while a new one loads.
#[derive(Clone, Debug, Default, PartialEq)]
struct Loaded {
    /// `None` until the first answer, `Some(None)` when it did not answer.
    charts: Option<Option<UsageCharts>>,
    /// Range of [`Self::charts`].
    range: Range,
}

impl Loaded {
    /// Takes an answer for `range`; `true` when it changes what is drawn.
    fn apply(&mut self, range: Range, charts: Option<UsageCharts>) -> bool {
        let charts = Some(charts);
        if self.range == range && self.charts == charts {
            return false;
        }
        self.range = range;
        self.charts = charts;
        true
    }

    /// Charts of another range than `selected` are drawn until its own
    /// come.
    fn stale(&self, selected: Range) -> bool {
        self.charts.is_some() && self.range != selected
    }
}

pub(super) struct UsageHistory {
    /// Namespaces queried (`None`: all) and how the screen names them.
    names: Option<Vec<String>>,
    scope_label: String,
    prometheus: Option<Prometheus>,
    loaded: Loaded,
    /// Prometheus range of the charts.
    range: Range,
    /// The range was picked by hand: data found later does not change it.
    range_picked: bool,
    /// Why each of [`Range::ALL`] is disabled; `None` until known.
    coverage: Option<[Option<Message>; Range::ALL.len()]>,
    /// Window of the charts without Prometheus.
    live_range: LiveRange,
    /// Usage of the pods in scope from metrics-server polls.
    session: LiveHistory,
    server: Server,
    /// CPU and memory requests and allocatable, drawn dashed.
    totals: Option<(ResourceTotals, ResourceTotals)>,
    _task: Option<Task<()>>,
}

impl UsageHistory {
    pub(super) fn new() -> Self {
        Self {
            names: None,
            scope_label: String::new(),
            prometheus: None,
            loaded: Loaded::default(),
            range: Range::default(),
            range_picked: false,
            coverage: None,
            live_range: LiveRange::default(),
            session: LiveHistory::default(),
            server: Server::Absent,
            totals: None,
            _task: None,
        }
    }

    /// Starts over for other namespaces (or another context).
    pub(super) fn set_scope(
        &mut self,
        names: Option<Vec<String>>,
        scope_label: String,
        cx: &mut Context<Self>,
    ) {
        *self = Self {
            names,
            scope_label,
            ..Self::new()
        };
        cx.notify();
    }

    /// Follows the screen's Prometheus: charts from the one ready, none
    /// without (or while the screen is hidden).
    pub(super) fn set_prometheus(
        &mut self,
        prometheus: Option<Prometheus>,
        cx: &mut Context<Self>,
    ) {
        let same = match (&self.prometheus, &prometheus) {
            (Some(old), Some(new)) => old.endpoint() == new.endpoint(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.prometheus = prometheus;
        self.loaded = Loaded::default();
        self.poll(cx);
        cx.notify();
    }

    pub(super) fn set_server(&mut self, server: Server, cx: &mut Context<Self>) {
        if self.server != server {
            self.server = server;
            cx.notify();
        }
    }

    /// A metrics-server sample, for the charts without Prometheus.
    pub(super) fn push(&mut self, sample: LiveSample, cx: &mut Context<Self>) {
        self.session.push(sample);
        cx.notify();
    }

    /// Requests and allocatable of CPU and memory.
    pub(super) fn set_totals(
        &mut self,
        cpu: ResourceTotals,
        memory: ResourceTotals,
        cx: &mut Context<Self>,
    ) {
        if self.totals != Some((cpu, memory)) {
            self.totals = Some((cpu, memory));
            cx.notify();
        }
    }

    /// Charts of the selected range now and every [`REFRESH_INTERVAL`],
    /// with the oldest sample while some range is still disabled (#376).
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(prometheus) = self.prometheus.clone() else {
            self._task = None;
            return;
        };
        let names = self.names.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((range, coverage)) =
                    this.read_with(cx, |this, _| (this.range, this.needs_coverage()))
                else {
                    break;
                };
                let (prometheus, names) = (prometheus.clone(), names.clone());
                let handle = runtime::spawn(async move {
                    let oldest = async {
                        match coverage {
                            true => prometheus.overview_oldest(names.as_deref()).await.ok(),
                            false => None,
                        }
                    };
                    futures::join!(prometheus.overview_usage(names.as_deref(), range), oldest)
                });
                let (charts, oldest) = match handle.await {
                    Ok((charts, oldest)) => (charts.ok(), oldest),
                    Err(_) => (None, None),
                };
                let again = this.update(cx, |this, cx| {
                    let refit = oldest.is_some_and(|oldest| this.set_coverage(oldest));
                    // Drawn (dimmed, if refitted) until the fitted range comes.
                    if this.loaded.apply(range, charts) || oldest.is_some() {
                        cx.notify();
                    }
                    refit
                });
                match again {
                    // At once in the range that fits.
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => break,
                }
                cx.background_executor().timer(REFRESH_INTERVAL).await;
            }
        }));
    }

    fn needs_coverage(&self) -> bool {
        self.coverage
            .as_ref()
            .is_none_or(|unavailable| unavailable.iter().any(Option::is_some))
    }

    /// Applies the oldest sample of the scope; `true` when the default
    /// range had nothing more to show than a shorter one and was switched.
    fn set_coverage(&mut self, oldest: Option<f64>) -> bool {
        let unavailable = coverage::unavailable(oldest, unix_now() as f64)
            .map(|why| why.map(|why| with_arg(&why, "subject", "pods")));
        let fitted = coverage::fit(self.range, &unavailable);
        self.coverage = Some(unavailable);
        if self.range_picked || fitted == self.range {
            return false;
        }
        self.range = fitted;
        true
    }

    /// A range picked on the card: only its charts reload, the old ones
    /// stay until then.
    fn set_range(&mut self, range: Range, cx: &mut Context<Self>) {
        if self.range == range {
            return;
        }
        self.range = range;
        self.range_picked = true;
        // A fresh poll: the old range's answer in flight is dropped.
        self.poll(cx);
        cx.notify();
    }

    fn history(&self) -> HistorySource {
        let prometheus = self.prometheus.is_some() && !matches!(self.loaded.charts, Some(None));
        overview::sources(self.server.available(), prometheus).history
    }

    /// The chart of `metric` from the history source, and the span its x
    /// axis covers (`1h`).
    fn chart(&self, metric: Metric) -> Result<(Chart, String), String> {
        match self.history() {
            HistorySource::Prometheus => {
                if self.prometheus.is_none() {
                    return Err(t!("metrics-looking-for-prometheus"));
                }
                match &self.loaded.charts {
                    None => Err(t!("metrics-loading-prometheus")),
                    Some(None) => Err(t!("metrics-prometheus-no-answer")),
                    Some(Some(charts)) => {
                        let chart = match metric {
                            Metric::Cpu => &charts.cpu,
                            _ => &charts.memory,
                        };
                        if chart.is_empty() {
                            Err(t!("metrics-no-data"))
                        } else {
                            Ok((chart.clone(), self.loaded.range.label().to_owned()))
                        }
                    }
                }
            }
            HistorySource::Session => {
                let window = self.session.range_window(
                    self.live_range,
                    unix_now() as f64,
                    DEFAULT_POLL_INTERVAL.as_secs(),
                );
                self.session
                    .chart(metric, false, window)
                    .filter(|chart| !chart.is_empty())
                    .map(|chart| {
                        let span = span(chart.window);
                        (chart, span)
                    })
                    .ok_or_else(|| match self.server {
                        Server::Failed => t!("metrics-server-no-answer"),
                        _ => t!("metrics-waiting-server"),
                    })
            }
            HistorySource::None => Err(t!("metrics-unavailable")),
        }
    }

    fn render_chart(&self, metric: Metric, cx: &App) -> AnyElement {
        let title = metric_title(metric);
        let body = match self.chart(metric) {
            Ok((chart, span)) => {
                let slot = if metric == Metric::Cpu { 0 } else { 1 };
                let mut view = LineChart::new(
                    SharedString::from(format!("overview-{}", metric.title().to_lowercase())),
                    "",
                )
                .lines(chart.series.iter().map(|series| {
                    ChartLine::new(
                        line_name(&series.name),
                        series.points.iter().map(|p| (p.time, p.value)).collect(),
                    )
                    .slot(slot)
                }))
                .x_range(chart.window.start as f64, chart.window.end as f64)
                .x_labels(format!("-{span}"), t!("metrics-now"))
                .format(move |value| metric.format(value).into());
                if metric != Metric::Cpu {
                    view = view.binary();
                }
                if let Some((cpu, memory)) = self.totals {
                    let (totals, scale) = match metric {
                        Metric::Cpu => (cpu, 1e9),
                        _ => (memory, 1.0),
                    };
                    if totals.requests > 0 {
                        view = view.threshold(
                            t!("overview-requests"),
                            totals.requests as f64 / scale,
                            Tone::Info,
                        );
                    }
                    if totals.allocatable > 0 {
                        view = view.threshold(
                            t!("metrics-allocatable"),
                            totals.allocatable as f64 / scale,
                            Tone::Warning,
                        );
                    }
                }
                view.into_any_element()
            }
            Err(why) => placeholder(why, cx),
        };
        div()
            .flex_1()
            .min_w(px(320.))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .child(body)
            .into_any_element()
    }

    /// Range tabs of the history source, if it has ranges.
    fn range_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = |label: &'static str, why: Option<&Message>| {
            let tab = SegmentTab::new(label);
            match why {
                Some(why) => tab.disabled(true).tooltip(why.to_string()),
                None => tab,
            }
        };
        match self.history() {
            HistorySource::Prometheus => {
                let active = Range::ALL
                    .iter()
                    .position(|r| *r == self.range)
                    .unwrap_or_default();
                let tabs = Range::ALL
                    .iter()
                    .enumerate()
                    .map(|(ix, r)| {
                        tab(
                            r.label(),
                            self.coverage.as_ref().and_then(|c| c[ix].as_ref()),
                        )
                    })
                    .collect();
                Some(
                    SegmentTabs::new("overview-range", tabs, active)
                        .on_select(cx.listener(|this, ix: &usize, _, cx| {
                            this.set_range(Range::ALL[*ix], cx)
                        }))
                        .into_any_element(),
                )
            }
            HistorySource::Session if !self.session.is_empty() => {
                let active = LiveRange::ALL
                    .iter()
                    .position(|r| *r == self.live_range)
                    .unwrap_or_default();
                let unavailable = self
                    .session
                    .unavailable(unix_now() as f64)
                    .map(|why| why.map(|why| with_arg(&why, "place", "screen")));
                let tabs = LiveRange::ALL
                    .iter()
                    .zip(&unavailable)
                    .map(|(r, why)| tab(r.label(), why.as_ref()))
                    .collect();
                Some(
                    SegmentTabs::new("overview-live-range", tabs, active)
                        .on_select(cx.listener(|this, ix: &usize, _, cx| {
                            this.live_range = LiveRange::ALL[*ix];
                            cx.notify();
                        }))
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    fn note(&self) -> String {
        let scope = self.scope_label.clone();
        match (self.history(), &self.prometheus) {
            (HistorySource::Prometheus, Some(prometheus)) => t!(
                "overview-history-prometheus",
                scope = scope,
                endpoint = prometheus.endpoint().describe()
            ),
            (HistorySource::Session, _) => t!("overview-history-session", scope = scope),
            _ => t!("overview-history", scope = scope),
        }
    }
}

/// `5m` for a window five minutes long.
fn span(window: ChartWindow) -> String {
    format_age(window.end.saturating_sub(window.start))
}

impl Render for UsageHistory {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.range_tabs(cx);
        let stale = self.history() == HistorySource::Prometheus && self.loaded.stale(self.range);
        Panel::new()
            .title(t!("overview-cpu-memory"))
            .note(self.note())
            .children(tabs.map(|tabs| div().flex().justify_end().child(tabs)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_4()
                    .when(stale, |charts| charts.opacity(0.5))
                    .child(self.render_chart(Metric::Cpu, cx))
                    .child(self.render_chart(Metric::Memory, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::prometheus::overview::UsageCharts;
    use kubach_core::metrics::prometheus::{Chart, Metric, Range, Window};

    use super::Loaded;

    fn charts(start: u64) -> UsageCharts {
        let window = Window {
            start,
            end: start + 60,
            step: 15,
        };
        let chart = |metric| Chart {
            metric,
            window,
            series: Vec::new(),
        };
        UsageCharts {
            cpu: chart(Metric::Cpu),
            memory: chart(Metric::Memory),
        }
    }

    #[test]
    fn old_range_stays_until_the_new_one_loads() {
        let mut loaded = Loaded::default();
        assert!(!loaded.stale(Range::Hour), "nothing drawn yet");
        assert!(loaded.apply(Range::Hour, Some(charts(0))));
        assert!(!loaded.stale(Range::Hour));
        // The same answer again: nothing to redraw.
        assert!(!loaded.apply(Range::Hour, Some(charts(0))));

        // 6h picked: the 1h charts stay, dimmed, until 6h answers.
        assert!(loaded.stale(Range::SixHours));
        assert_eq!(loaded.charts, Some(Some(charts(0))));
        assert!(loaded.apply(Range::SixHours, Some(charts(100))));
        assert!(!loaded.stale(Range::SixHours));
        assert_eq!(loaded.range, Range::SixHours);

        // No answer for the new range: said so, not the old charts.
        assert!(loaded.apply(Range::Day, None));
        assert_eq!(loaded.charts, Some(None));
    }
}
