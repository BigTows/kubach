//! Overview screen of a connected cluster (#414): counters of nodes and
//! pods, CPU and memory usage against requests, limits and allocatable,
//! charts of usage and of pods by phase over time, and the cluster facts.
//!
//! Counters come from the object cache: Pods of the selected namespaces and
//! Nodes, subscribed only while the screen is shown (the watches are shared
//! with the Pods and Nodes tables, see [`ResourceCache::subscribe`]). Usage
//! comes from metrics-server and the context's Prometheus as the tables
//! and details pick them (ADR 0009, [`overview::sources`]): metrics-server
//! for the current value and Prometheus for history with ranges; without
//! Prometheus the charts grow from metrics-server polls while the screen is
//! open. Pods by phase come from kube-state-metrics in Prometheus
//! (`kube_pod_status_phase`), else from the cache, sampled while the screen
//! is open. Nothing polls while the screen is hidden.
//!
//! The "CPU and memory" card is an entity of its own with its own range and
//! query ([`history`]): picking a range reloads only its charts (#481). The
//! cards and pods by phase cover [`STATUS_RANGE`].

mod history;

use futures::StreamExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::cache::{ResourceCache, Subscription, SubscriptionSet, WatchStatus};
use kubach_core::metrics::live::{LiveRange, LiveSample};
use kubach_core::metrics::overview::{
    self, ClusterStats, CurrentSource, HistorySource, PhaseChart, PhaseHistory, PhaseSource,
    PodPhase, ResourceTotals, RestartTracker, share,
};
use kubach_core::metrics::prometheus::format_bytes;
use kubach_core::metrics::prometheus::overview::OverviewStatus;
use kubach_core::metrics::prometheus::{Metric, Prometheus, REFRESH_INTERVAL, Range};
use kubach_core::metrics::shared::{PollResult, PollScope};
use kubach_core::metrics::{DEFAULT_POLL_INTERVAL, Usage};
use kubach_core::runtime;
use kubach_i18n::t;
use kubach_ui::{
    ChartLine, Donut, DonutColor, DonutSegment, KeyValueList, LineChart, PageHeader, Panel,
    StatCard, StatusCounts, Tone, format_age, unix_now,
};

use super::activity::{ScreenActivity, watch_set};
use super::metrics::metric_title;
use super::namespace_picker::Namespaces;
use super::namespaces::selected_names;
use super::prometheus_link::{self, PrometheusRegistry};
use crate::cluster::{CatalogState, Cluster};
use crate::settings::{SettingsTarget, open_settings};
use history::UsageHistory;

/// Range of the screen's own Prometheus data: pods by phase and recent
/// restarts. The "CPU and memory" card picks its own.
const STATUS_RANGE: Range = Range::Hour;

/// The cache watches of the screen: Pods of the selected namespaces and
/// all Nodes.
struct Watches {
    cache: ResourceCache,
    pods: SubscriptionSet,
    nodes: Subscription,
    _listener: Task<()>,
}

/// metrics-server, as the screen polls it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Server {
    /// The cluster does not serve `metrics.k8s.io`.
    Absent,
    Loading,
    Ready(Usage),
    /// No poll answered yet.
    Failed,
}

impl Server {
    fn available(self) -> bool {
        matches!(self, Self::Loading | Self::Ready(_))
    }
}

/// The context's Prometheus, as the screen uses it.
struct PrometheusFeed {
    prometheus: Prometheus,
    /// `None` while loading, `Some(None)` when it did not answer.
    status: Option<Option<OverviewStatus>>,
    _task: Task<()>,
}

/// Data of the context and namespaces shown; replaced when they change.
#[derive(Default)]
struct Data {
    /// `None` until the Pods and Nodes watches loaded.
    stats: Option<ClusterStats>,
    /// Why Nodes or Pods can't be listed (forbidden, unreachable).
    nodes_error: Option<String>,
    pods_error: Option<String>,
    restarts: RestartTracker,
    phases: PhaseHistory,
}

pub struct OverviewScreen {
    cluster: Entity<Cluster>,
    namespaces: Entity<Namespaces>,
    /// On screen: watches and polls run only then.
    active: bool,
    /// Context and namespaces (`None`: all) of [`Self::data`].
    scope: Option<(String, Option<Vec<String>>)>,
    /// Bumped with every new scope: results of older tasks are dropped.
    epoch: u64,
    data: Data,
    watches: Option<Watches>,
    computing: bool,
    dirty: bool,
    _compute: Option<Task<()>>,
    server: Server,
    _server_task: Option<Task<()>>,
    prometheus: Option<PrometheusFeed>,
    /// The "CPU and memory" card, with its range.
    history: Entity<UsageHistory>,
    /// Samples the phases while nothing changes, so the chart keeps going.
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription_>,
}

/// GPUI's subscription, named apart from the cache's.
type Subscription_ = gpui_kit::Subscription;

impl OverviewScreen {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<Namespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
        ];
        // Prometheus found, connected, picked in the settings.
        if let Some(registry) = PrometheusRegistry::try_global(cx) {
            subscriptions.push(cx.observe(&registry, |this, _, cx| {
                this.sync(cx);
                cx.notify();
            }));
        }
        Self {
            cluster,
            namespaces,
            active: false,
            scope: None,
            epoch: 0,
            data: Data::default(),
            watches: None,
            computing: false,
            dirty: false,
            _compute: None,
            server: Server::Absent,
            _server_task: None,
            prometheus: None,
            history: cx.new(|_| UsageHistory::new()),
            _ticker: None,
            _subscriptions: subscriptions,
        }
    }

    /// Context and namespaces shown now, when connected.
    fn current_scope(&self, cx: &App) -> Option<(String, Option<Vec<String>>)> {
        let cluster = self.cluster.read(cx);
        if !matches!(cluster.state(), ConnectionState::Connected { .. }) {
            return None;
        }
        let context = cluster.active()?.name.clone();
        let names = selected_names(&self.namespaces.read(cx).scope()?.selection);
        Some((context, names))
    }

    /// Stops every watch and poll.
    fn stop(&mut self, cx: &mut Context<Self>) {
        self.watches = None;
        self._compute = None;
        self.computing = false;
        self._server_task = None;
        self.set_prometheus(None, cx);
        self._ticker = None;
    }

    /// The Prometheus of the screen and of the card.
    fn set_prometheus(&mut self, feed: Option<PrometheusFeed>, cx: &mut Context<Self>) {
        let prometheus = feed.as_ref().map(|feed| feed.prometheus.clone());
        self.prometheus = feed;
        self.history
            .update(cx, |history, cx| history.set_prometheus(prometheus, cx));
    }

    /// metrics-server as polled, for the cards and the card's charts.
    fn set_server(&mut self, server: Server, cx: &mut Context<Self>) {
        self.server = server;
        self.history
            .update(cx, |history, cx| history.set_server(server, cx));
    }

    /// Brings watches and polls in line with the connection, namespaces,
    /// Prometheus and whether the screen is shown.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let before = self.signature(cx);
        self.sync_inner(cx);
        // The cluster notifies often while it loads: redraw only when
        // something shown changed.
        if self.signature(cx) != before {
            cx.notify();
        }
    }

    /// What [`Self::sync`] changes that the screen shows: scope, sources,
    /// polls, and the cluster facts.
    fn signature(&self, cx: &App) -> String {
        let cluster = self.cluster.read(cx);
        let catalog = match cluster.catalog() {
            CatalogState::Pending => "pending".to_owned(),
            CatalogState::Error(error) => error.clone(),
            CatalogState::Ready(catalog) => catalog.kinds().len().to_string(),
        };
        format!(
            "{:?} {} {} {} {:?} {:?} {:?} {catalog}",
            self.scope,
            self.active,
            self.watches.is_some(),
            self._server_task.is_some(),
            self.server,
            self.prometheus
                .as_ref()
                .map(|feed| feed.prometheus.endpoint().describe()),
            cluster.state(),
        )
    }

    fn sync_inner(&mut self, cx: &mut Context<Self>) {
        let scope = self.current_scope(cx);
        if scope != self.scope {
            self.stop(cx);
            self.scope = scope.clone();
            self.epoch += 1;
            self.data = Data::default();
            self.server = Server::Absent;
            let names = scope.as_ref().and_then(|(_, names)| names.clone());
            let label = self.scope_label();
            self.history
                .update(cx, |history, cx| history.set_scope(names, label, cx));
        }
        let Some((context, names)) = scope.filter(|_| self.active) else {
            self.stop(cx);
            return;
        };
        let cluster = self.cluster.read(cx);
        let cache = cluster.cache().cloned();
        let catalog = match cluster.catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        let metrics_server = cluster.metrics_server();
        if let (Some(cache), Some(catalog)) = (cache, catalog)
            && !self
                .watches
                .as_ref()
                .is_some_and(|watches| watches.cache.same_as(&cache))
            && let (Some(pod), Some(node)) = (catalog.find("", "Pod"), catalog.find("", "Node"))
        {
            let pods = cache.subscribe_many(pod, names.as_deref(), Default::default());
            let nodes = cache.subscribe(node, None);
            let mut all = pods.subscriptions().to_vec();
            all.push(nodes.clone());
            let listener = watch_set(&SubscriptionSet::new(all), cx, Self::schedule_stats);
            self.watches = Some(Watches {
                cache,
                pods,
                nodes,
                _listener: listener,
            });
        }
        match metrics_server {
            Some(server) if self._server_task.is_none() => {
                self.set_server(Server::Loading, cx);
                let mut results = server.subscribe(PollScope::pods(names.clone())).results();
                self._server_task = Some(cx.spawn(async move |this, cx| {
                    while let Some(result) = results.next().await {
                        if this
                            .update(cx, |this, cx| this.add_poll(&result, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Some(_) => {}
            None => {
                self.set_server(Server::Absent, cx);
                self._server_task = None;
            }
        }
        self.sync_prometheus(&context, names, cx);
        if self._ticker.is_none() {
            self._ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(DEFAULT_POLL_INTERVAL).await;
                    let sampled = this.update(cx, |this, cx| {
                        if let Some(stats) = &this.data.stats {
                            this.data.phases.push(unix_now() as f64, stats.pods);
                            cx.notify();
                        }
                    });
                    if sampled.is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// Follows the context's Prometheus: polls the one ready, stops when
    /// there is none (Off, not found, doesn't answer).
    fn sync_prometheus(
        &mut self,
        context: &str,
        names: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let Some(registry) = PrometheusRegistry::try_global(cx) else {
            return;
        };
        let status = registry.update(cx, |registry, cx| registry.ensure(context, cx));
        match status.prometheus() {
            Some(prometheus)
                if self
                    .prometheus
                    .as_ref()
                    .is_some_and(|feed| feed.prometheus.endpoint() == prometheus.endpoint()) => {}
            Some(prometheus) => {
                let task = self.poll_prometheus(prometheus.clone(), names, cx);
                let feed = PrometheusFeed {
                    prometheus: prometheus.clone(),
                    status: None,
                    _task: task,
                };
                self.set_prometheus(Some(feed), cx);
            }
            None => self.set_prometheus(None, cx),
        }
    }

    /// Cards' numbers and pods by phase over [`STATUS_RANGE`] now and
    /// every [`REFRESH_INTERVAL`]; the card asks for its charts itself.
    fn poll_prometheus(
        &self,
        prometheus: Prometheus,
        names: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let (prometheus, names) = (prometheus.clone(), names.clone());
                let handle = runtime::spawn(async move {
                    prometheus
                        .overview_status(names.as_deref(), STATUS_RANGE)
                        .await
                });
                let status = handle.await.ok().and_then(Result::ok);
                let updated = this.update(cx, |this, cx| {
                    if let Some(feed) = &mut this.prometheus
                        && feed.status.as_ref() != Some(&status)
                    {
                        feed.status = Some(status);
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH_INTERVAL).await;
            }
        })
    }

    fn add_poll(&mut self, result: &PollResult, cx: &mut Context<Self>) {
        match result {
            Ok(polled) => {
                let names = self.scope.as_ref().and_then(|(_, names)| names.as_deref());
                let usage = overview::pods_usage(&polled.pods, names);
                let sample = LiveSample {
                    time: unix_now() as f64,
                    total: usage,
                    parts: Vec::new(),
                };
                self.history
                    .update(cx, |history, cx| history.push(sample, cx));
                self.set_server(Server::Ready(usage), cx);
            }
            // Keep what was charted; a later poll may succeed.
            Err(_) if !matches!(self.server, Server::Ready(_)) => {
                self.set_server(Server::Failed, cx)
            }
            Err(_) => {}
        }
        cx.notify();
    }

    /// Recounts after a change of the watches, off the UI thread; changes
    /// during a count are counted once more after it.
    fn schedule_stats(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        if self.computing {
            return;
        }
        self.computing = true;
        let epoch = self.epoch;
        self._compute = Some(cx.spawn(async move |this, cx| {
            loop {
                let input = this.update(cx, |this, _| {
                    this.dirty = false;
                    let watches = this.watches.as_ref()?;
                    Some((
                        watches.nodes.snapshot(),
                        watches.pods.snapshot(),
                        this.data.restarts.clone(),
                    ))
                });
                let Ok(Some((nodes, pods, mut restarts))) = input else {
                    return;
                };
                let counted = cx
                    .background_spawn(async move {
                        let loaded = nodes.status != WatchStatus::Loading
                            && pods.status != WatchStatus::Loading;
                        let stats = overview::cluster_stats(
                            nodes.objects.iter().map(AsRef::as_ref),
                            pods.objects.iter().map(AsRef::as_ref),
                        );
                        restarts.update(pods.objects.iter().map(AsRef::as_ref));
                        let error = |status: &WatchStatus| match status {
                            WatchStatus::Error(error) if error.forbidden => {
                                Some(t!("overview-not-allowed"))
                            }
                            WatchStatus::Error(_) => Some(t!("overview-cant-list")),
                            _ => None,
                        };
                        let errors = (error(&nodes.status), error(&pods.status));
                        (loaded, stats, restarts, errors)
                    })
                    .await;
                let again = this.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return false;
                    }
                    let (loaded, stats, restarts, (nodes_error, pods_error)) = counted;
                    this.data.restarts = restarts;
                    this.data.nodes_error = nodes_error;
                    this.data.pods_error = pods_error;
                    if loaded {
                        this.data.phases.push(unix_now() as f64, stats.pods);
                        this.history.update(cx, |history, cx| {
                            history.set_totals(stats.cpu, stats.memory, cx)
                        });
                        this.data.stats = Some(stats);
                    }
                    cx.notify();
                    if !this.dirty {
                        this.computing = false;
                    }
                    this.dirty
                });
                if !matches!(again, Ok(true)) {
                    return;
                }
            }
        }));
    }

    fn status(&self) -> Option<&OverviewStatus> {
        self.prometheus.as_ref()?.status.as_ref()?.as_ref()
    }

    fn sources(&self) -> overview::Sources {
        let prometheus = self
            .prometheus
            .as_ref()
            .is_some_and(|feed| !matches!(feed.status, Some(None)));
        overview::sources(self.server.available(), prometheus)
    }

    /// Current usage of the pods in scope, and where it comes from.
    fn current_usage(&self) -> Option<(Usage, &'static str)> {
        match self.server {
            Server::Ready(usage) => Some((usage, "metrics-server")),
            _ => self
                .status()
                .and_then(|status| status.usage)
                .map(|usage| (usage, "Prometheus")),
        }
    }

    /// "2 namespaces", "kube-system", or "all namespaces".
    fn scope_label(&self) -> String {
        match self.scope.as_ref().and_then(|(_, names)| names.as_ref()) {
            None => t!("overview-all-namespaces"),
            Some(names) if names.len() == 1 => names[0].clone(),
            Some(names) => t!("overview-namespaces", count = names.len()),
        }
    }

    fn context(&self) -> Option<&str> {
        self.scope.as_ref().map(|(context, _)| context.as_str())
    }
}

impl ScreenActivity for OverviewScreen {
    /// Shown, the screen starts its watches and polls; hidden, it stops
    /// them (the counts shown last stay until it is shown again).
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

/// Theme slot of a phase line: running green, pending amber, failed pink,
/// succeeded blue, unknown purple.
fn phase_slot(phase: PodPhase) -> usize {
    match phase {
        PodPhase::Succeeded => 0,
        PodPhase::Running => 1,
        PodPhase::Pending => 2,
        PodPhase::Unknown => 3,
        PodPhase::Failed => 4,
    }
}

/// `"34%"` of `share`, `"—"` without one.
fn percent(share: Option<f64>) -> String {
    share.map_or_else(|| "—".to_owned(), |share| format!("{:.0}%", share * 100.0))
}

/// CPU in cores: whole numbers without decimals (`10`), else `1.25`.
fn cores(value: f64) -> SharedString {
    if (value - value.round()).abs() < 0.005 {
        format!("{value:.0}").into()
    } else if value >= 10.0 {
        format!("{value:.1}").into()
    } else {
        format!("{value:.2}").into()
    }
}

/// Bytes in a readable binary unit: `20 GiB`, `512 MiB`.
fn bytes(value: f64) -> SharedString {
    let text = format_bytes(value);
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    match unit {
        "B" | "" => format!("{number} B").into(),
        unit => format!("{number} {unit}B").into(),
    }
}

/// `"1.2 / 10 cores"`, `"3.4 GiB / 20 GiB"`.
fn out_of(metric: Metric, value: f64, max: f64) -> String {
    match metric {
        Metric::Cpu => t!(
            "overview-cores",
            value = cores(value).to_string(),
            max = cores(max).to_string()
        ),
        _ => format!("{} / {}", bytes(value), bytes(max)),
    }
}

/// The center of the CPU or memory ring: the share used of allocatable
/// when usage is known, else the share requested.
fn usage_headline(used: Option<u64>, totals: &ResourceTotals) -> (String, String) {
    match used {
        Some(used) => (
            percent(share(used, totals.allocatable)),
            t!("overview-used"),
        ),
        None => (
            percent(share(totals.requests, totals.allocatable)),
            t!("overview-requested"),
        ),
    }
}

/// `"3 pods without a limit"`, when some have none.
fn unlimited_note(totals: &ResourceTotals) -> Option<String> {
    match totals.unlimited {
        0 => None,
        n => Some(t!("overview-unlimited", count = n)),
    }
}

/// A dashed box with why a chart is missing.
fn placeholder(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .h(px(140.))
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
        .child(text.into())
        .into_any_element()
}

/// A muted line under a ring; `warning` tints it.
fn card_note(text: impl Into<SharedString>, warning: bool, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .text_xs()
        .text_color(if warning {
            theme.warning
        } else {
            theme.muted_foreground
        })
        .child(text.into())
}

impl OverviewScreen {
    /// Why the cards have no current usage, and what to do, in one line.
    fn usage_missing(&self, cx: &App) -> Option<String> {
        if self.current_usage().is_some() {
            return None;
        }
        if self.server == Server::Failed && self.prometheus.is_none() {
            return Some(t!("overview-usage-server-no-answer"));
        }
        if self.server == Server::Loading {
            return Some(t!("metrics-waiting-server"));
        }
        if let Some(feed) = &self.prometheus {
            return Some(match &feed.status {
                None => t!("overview-usage-loading-prometheus"),
                Some(None) => t!("overview-usage-prometheus-no-answer"),
                Some(Some(_)) => t!("overview-usage-no-samples", scope = self.scope_label()),
            });
        }
        let status = self.context().and_then(|context| {
            Some(
                PrometheusRegistry::try_global(cx)?
                    .read(cx)
                    .status(context, cx),
            )
        });
        Some(match status {
            Some(status) if status.pending() => t!("metrics-looking-for-prometheus"),
            Some(status) if status.problem().is_some() => t!("overview-usage-see-problem"),
            _ => t!("overview-usage-none"),
        })
    }

    fn render_cards(&self, cx: &App) -> impl IntoElement + use<> {
        let scope = self.scope_label();
        let stats = self.data.stats.as_ref();
        let loading = || "…".to_owned();
        let whole = self
            .scope
            .as_ref()
            .is_some_and(|(_, names)| names.is_none());

        let nodes = match stats {
            Some(stats) if self.data.nodes_error.is_none() => {
                let card = StatCard::new(
                    "Nodes",
                    format!("{}/{}", stats.nodes.ready, stats.nodes.total),
                )
                .caption(t!("overview-ready"))
                .scope(t!("overview-whole-cluster"))
                .child(
                    StatusCounts::new()
                        .keep_zero()
                        .item(stats.nodes.ready, t!("overview-ready"), Tone::Success)
                        .item(
                            stats.nodes.not_ready(),
                            t!("overview-not-ready"),
                            Tone::Danger,
                        ),
                );
                if stats.nodes.not_ready() > 0 {
                    card.tone(Tone::Danger)
                } else {
                    card
                }
            }
            _ => StatCard::new("Nodes", "—")
                .caption(self.data.nodes_error.clone().unwrap_or_else(loading))
                .scope(t!("overview-whole-cluster")),
        };

        let restarts_line =
            stats.map(
                |stats| match self.status().and_then(|status| status.restarts) {
                    Some(recent) => t!(
                        "overview-restarts-recent",
                        count = stats.restarts,
                        recent = recent.round() as u64,
                        range = STATUS_RANGE.label()
                    ),
                    None if self.data.restarts.since() > 0 => t!(
                        "overview-restarts-since",
                        count = stats.restarts,
                        since = self.data.restarts.since()
                    ),
                    None => t!("overview-restarts", count = stats.restarts),
                },
            );
        let pods = match stats {
            Some(stats) if self.data.pods_error.is_none() => {
                let counts = PodPhase::ALL
                    .into_iter()
                    .filter(|phase| *phase != PodPhase::Unknown || stats.pods.get(*phase) > 0)
                    .fold(StatusCounts::new().keep_zero(), |counts, phase| {
                        let tone = match phase {
                            PodPhase::Running => Tone::Success,
                            PodPhase::Pending => Tone::Warning,
                            PodPhase::Failed => Tone::Danger,
                            PodPhase::Succeeded => Tone::Info,
                            PodPhase::Unknown => Tone::Neutral,
                        };
                        counts.item(stats.pods.get(phase), phase.label().to_lowercase(), tone)
                    });
                // Running and pending pods take the nodes' pod slots.
                let running = stats.pods.get(PodPhase::Running) as f64;
                let pending = stats.pods.get(PodPhase::Pending) as f64;
                let capacity = stats.nodes.pod_capacity as f64;
                let donut = Donut::new(capacity)
                    .center(percent(share(
                        (running + pending) as u64,
                        stats.nodes.pod_capacity,
                    )))
                    .caption(t!("overview-of-slots"))
                    .footer(t!(
                        "overview-pods-of",
                        count = running + pending,
                        capacity = capacity
                    ))
                    .ring([
                        DonutSegment::new(
                            "Running",
                            Some(running),
                            DonutColor::Tone(Tone::Success),
                        ),
                        DonutSegment::new(
                            "Pending",
                            Some(pending),
                            DonutColor::Tone(Tone::Warning),
                        ),
                    ])
                    .format(|value| format!("{value:.0}").into());
                StatCard::new("Pods", stats.pods.total().to_string())
                    .caption(t!("overview-in-scope", scope = scope.clone()))
                    .child(donut)
                    .child(counts)
                    .children(restarts_line.map(|line| card_note(line, false, cx)))
                    .when(!whole, |card| {
                        card.child(card_note(t!("overview-pod-slots-note"), false, cx))
                    })
            }
            _ => StatCard::new("Pods", "—")
                .caption(self.data.pods_error.clone().unwrap_or_else(loading)),
        };

        let current = self.current_usage();
        let missing = self.usage_missing(cx);
        let resource = |title: String, metric: Metric| {
            let Some(stats) = stats else {
                return StatCard::new(title, "—").caption(loading());
            };
            let (totals, used) = match metric {
                Metric::Cpu => (stats.cpu, current.map(|(usage, _)| usage.cpu)),
                _ => (stats.memory, current.map(|(usage, _)| usage.memory)),
            };
            let (center, caption) = usage_headline(used, &totals);
            let scale = |value: u64| match metric {
                Metric::Cpu => value as f64 / 1e9,
                _ => value as f64,
            };
            let max = scale(totals.allocatable);
            let shown = used.unwrap_or(totals.requests);
            let mut limits = DonutSegment::new(
                t!("overview-limits"),
                Some(scale(totals.limits)),
                DonutColor::Limit,
            );
            if let Some(note) = unlimited_note(&totals) {
                limits = limits.note(note);
            }
            let mut donut = Donut::new(max)
                .center(center.clone())
                .caption(caption)
                .footer(out_of(metric, scale(shown), max));
            if let Some(used) = used {
                donut = donut.ring([DonutSegment::new(
                    t!("overview-segment-used"),
                    Some(scale(used)),
                    DonutColor::Tone(Tone::Success),
                )]);
            }
            donut = donut
                .ring([DonutSegment::new(
                    t!("overview-requests"),
                    Some(scale(totals.requests)),
                    DonutColor::Request,
                )])
                .ring([limits]);
            donut = match metric {
                Metric::Cpu => donut.format(cores),
                _ => donut.format(bytes),
            };
            let headline = match used {
                Some(_) => center,
                None => "—".to_owned(),
            };
            StatCard::new(title, headline)
                .caption(if used.is_some() {
                    t!("overview-used-of-allocatable")
                } else {
                    t!("overview-usage-unknown")
                })
                .scope(t!("overview-pods-scope", scope = scope.clone()))
                .child(donut)
                .child(match (current, &missing) {
                    (Some((_, source)), _) => {
                        card_note(t!("overview-usage-from", source = source), false, cx)
                    }
                    (None, Some(missing)) => card_note(missing.clone(), true, cx),
                    (None, None) => card_note(t!("overview-max-allocatable"), false, cx),
                })
        };

        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(nodes)
            .child(pods)
            .child(resource(metric_title(Metric::Cpu), Metric::Cpu))
            .child(resource(metric_title(Metric::Memory), Metric::Memory))
    }

    /// Pods by phase over time and where the chart comes from.
    fn phase_chart(&self) -> (PhaseChart, PhaseSource) {
        let prometheus = self.status().and_then(|status| status.phases.clone());
        match overview::phase_source(prometheus.as_ref().map(|_| true)) {
            PhaseSource::Prometheus => (
                prometheus.unwrap_or_else(|| self.session_phases()),
                PhaseSource::Prometheus,
            ),
            PhaseSource::Session => (self.session_phases(), PhaseSource::Session),
        }
    }

    /// Every sample of the session: the card's range is its own.
    fn session_phases(&self) -> PhaseChart {
        let window = self.data.phases.range_window(
            LiveRange::All,
            unix_now() as f64,
            DEFAULT_POLL_INTERVAL.as_secs(),
        );
        self.data.phases.chart(window)
    }

    fn render_phases(&self, cx: &App) -> impl IntoElement + use<> {
        let (chart, source) = self.phase_chart();
        let scope = self.scope_label();
        let note = match source {
            PhaseSource::Prometheus => t!("overview-phases-prometheus", scope = scope),
            PhaseSource::Session => t!("overview-phases-session", scope = scope),
        };
        // One sample draws no line: wait for the second.
        let enough = chart.series.iter().any(|(_, points)| points.len() > 1);
        let body = if enough {
            let window = chart.window;
            let span = match source {
                PhaseSource::Prometheus => STATUS_RANGE.label().to_owned(),
                PhaseSource::Session => format_age(window.end.saturating_sub(window.start)),
            };
            LineChart::new("overview-phases", "")
                .lines(chart.series.iter().map(|(phase, points)| {
                    ChartLine::new(
                        phase.label(),
                        points.iter().map(|p| (p.time, p.value)).collect(),
                    )
                    .slot(phase_slot(*phase))
                }))
                .x_range(window.start as f64, window.end as f64)
                .x_labels(format!("-{span}"), t!("metrics-now"))
                .format(|value| format!("{value:.0}").into())
                .into_any_element()
        } else {
            placeholder(t!("overview-phases-collecting"), cx)
        };
        Panel::new()
            .title(t!("overview-pods-by-phase"))
            .note(note)
            .child(body)
    }

    /// Without metrics-server and Prometheus: why, and where to set one up.
    /// A problem with the chosen Prometheus has its own bar.
    fn render_hint(&self, cx: &App) -> Option<AnyElement> {
        let context = self.context()?;
        let registry = PrometheusRegistry::try_global(cx)?;
        let status = registry.read(cx).status(context, cx);
        if let Some(problem) = status.problem().or_else(|| {
            let feed = self.prometheus.as_ref()?;
            matches!(feed.status, Some(None))
                .then(|| prometheus_link::doesnt_answer(feed.prometheus.endpoint()))
        }) {
            return Some(prometheus_link::problem_bar(
                "overview", context, problem, cx,
            ));
        }
        if !self.sources().none() || status.pending() || self.server == Server::Loading {
            return None;
        }
        let theme = cx.theme();
        let settings_context = context.to_owned();
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(t!("overview-metrics-unavailable")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t!("overview-metrics-unavailable-description")),
                        ),
                )
                .child(
                    Button::new("overview-prometheus-settings")
                        .outline()
                        .small()
                        .icon(IconName::Settings)
                        .label(t!("overview-prometheus-settings"))
                        .on_click(move |_, _, cx| {
                            open_settings(SettingsTarget::prometheus(settings_context.clone()), cx)
                        }),
                )
                .into_any_element(),
        )
    }

    /// Context, cluster, user, namespace, version and API resources; CRDs.
    fn render_facts(&self, cx: &App) -> impl IntoElement + use<> {
        let cluster = self.cluster.read(cx);
        let server_version = match cluster.state() {
            ConnectionState::Connected { server_version } => server_version.clone(),
            _ => String::new(),
        };
        let api_resources: SharedString = match cluster.catalog() {
            CatalogState::Pending => t!("overview-discovering").into(),
            CatalogState::Error(error) => error.clone().into(),
            CatalogState::Ready(catalog) => t!(
                "overview-api-resources",
                kinds = catalog.kinds().len(),
                groups = catalog.by_group().len()
            )
            .into(),
        };
        let facts = cluster.active().map(|context| {
            Panel::new().title(t!("overview-cluster")).child(
                KeyValueList::new()
                    .row(t!("overview-context"), context.name.clone())
                    .row(t!("overview-cluster"), context.cluster.clone())
                    .row(
                        t!("overview-user"),
                        context.user.clone().unwrap_or_else(|| "—".into()),
                    )
                    .row(
                        t!("column-namespace"),
                        context
                            .namespace
                            .clone()
                            .unwrap_or_else(|| "default".into()),
                    )
                    .row(t!("overview-server-version"), server_version)
                    .row(t!("overview-api-resources-title"), api_resources),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(facts)
            .children(custom_resources(cluster.catalog()))
    }
}

/// CRD-backed kinds by group; hidden while there are none.
fn custom_resources(catalog: &CatalogState) -> Option<impl IntoElement + use<>> {
    let CatalogState::Ready(catalog) = catalog else {
        return None;
    };
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for kind in catalog.custom_kinds() {
        match groups.last_mut() {
            Some((group, kinds)) if *group == kind.group_label() => kinds.push(&kind.api.kind),
            _ => groups.push((kind.group_label(), vec![&kind.api.kind])),
        }
    }
    if groups.is_empty() {
        return None;
    }
    let list = groups
        .into_iter()
        .fold(KeyValueList::new(), |list, (group, kinds)| {
            list.row(group.to_owned(), kinds.join(", "))
        });
    Some(
        Panel::new()
            .title(t!("overview-custom-resources"))
            .child(list),
    )
}

impl Render for OverviewScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hint = self.render_hint(cx);
        let cards = self.render_cards(cx);
        let phases = self.render_phases(cx);
        let facts = self.render_facts(cx);
        let show_metrics = !matches!(self.sources().current, CurrentSource::None)
            || !matches!(self.sources().history, HistorySource::None)
            || self.server == Server::Loading;
        div()
            .id("overview-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .p_8()
                    .child(PageHeader::new(t!("nav-overview")))
                    .children(hint)
                    .child(cards)
                    .when(show_metrics, |page| page.child(self.history.clone()))
                    .child(phases)
                    .child(facts),
            )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::overview::{PodPhase, ResourceTotals};

    use kubach_core::metrics::prometheus::Metric;

    use super::{
        Server, bytes, cores, out_of, percent, phase_slot, unlimited_note, usage_headline,
    };

    #[test]
    fn values_read_in_handy_units() {
        assert_eq!(cores(10.0).as_ref(), "10");
        assert_eq!(cores(1.25).as_ref(), "1.25");
        assert_eq!(cores(12.34).as_ref(), "12.3");
        assert_eq!(bytes(21_470_134_272.0).as_ref(), "20 GiB");
        assert_eq!(bytes(512.0 * 1024.0 * 1024.0).as_ref(), "512 MiB");
        assert_eq!(bytes(100.0).as_ref(), "100 B");
        assert_eq!(out_of(Metric::Cpu, 1.2, 10.0), "1.20 / 10 cores");
        assert_eq!(
            out_of(
                Metric::Memory,
                3.5 * 1024.0 * 1024.0 * 1024.0,
                21_470_134_272.0
            ),
            "3.5 GiB / 20 GiB"
        );
    }

    #[test]
    fn cards_show_usage_or_requests() {
        let totals = ResourceTotals {
            requests: 2,
            limits: 6,
            unlimited: 0,
            allocatable: 8,
        };
        assert_eq!(
            usage_headline(Some(4), &totals),
            ("50%".into(), "used".into())
        );
        assert_eq!(
            usage_headline(None, &totals),
            ("25%".into(), "requested".into())
        );
        assert_eq!(percent(None), "—");
        assert_eq!(unlimited_note(&totals), None);
        let unbounded = ResourceTotals {
            unlimited: 3,
            ..totals
        };
        assert_eq!(
            unlimited_note(&unbounded).as_deref(),
            Some("3 pods without a limit")
        );
    }

    #[test]
    fn phases_have_distinct_colors() {
        let mut slots: Vec<usize> = PodPhase::ALL.into_iter().map(phase_slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), PodPhase::ALL.len());
        assert!(Server::Loading.available());
        assert!(!Server::Failed.available());
        assert!(!Server::Absent.available());
    }
}
