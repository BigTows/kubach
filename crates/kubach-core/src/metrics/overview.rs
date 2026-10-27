//! Cluster statistics of the Overview screen (#414): nodes, pods by phase,
//! restarts, CPU and memory requests / limits against allocatable, and the
//! history kept while the screen is open.
//!
//! Counters come from the object cache (Pods of the selected namespaces,
//! Nodes of the whole cluster); usage from metrics-server or Prometheus,
//! picked by [`sources`]. Prometheus queries are in
//! [`prometheus::overview`](super::prometheus::overview).

use std::collections::{HashMap, VecDeque};

use k8s_openapi::api::core::v1::{Pod, PodSpec};
use kube::api::DynamicObject;
use serde_json::Value;

use super::live::{LIVE_CAPACITY, LiveRange};
use super::prometheus::{Point, Window};
use super::{Usage, cpu_nanocores, memory_bytes, pod_is_active, pod_resources};

/// Phase of a pod, as `status.phase` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PodPhase {
    Running,
    Pending,
    Failed,
    Succeeded,
    Unknown,
}

impl PodPhase {
    /// In the order the screen shows them.
    pub const ALL: [PodPhase; 5] = [
        Self::Running,
        Self::Pending,
        Self::Failed,
        Self::Succeeded,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Pending => "Pending",
            Self::Failed => "Failed",
            Self::Succeeded => "Succeeded",
            Self::Unknown => "Unknown",
        }
    }

    /// `status.phase`; a pod without one is still pending.
    pub fn parse(phase: Option<&str>) -> Self {
        match phase {
            Some("Running") => Self::Running,
            None | Some("Pending") | Some("") => Self::Pending,
            Some("Failed") => Self::Failed,
            Some("Succeeded") => Self::Succeeded,
            Some(_) => Self::Unknown,
        }
    }

    pub fn of(pod: &DynamicObject) -> Self {
        Self::parse(pod.data.pointer("/status/phase").and_then(Value::as_str))
    }
}

/// Pods per phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseCounts {
    counts: [usize; PodPhase::ALL.len()],
}

impl PhaseCounts {
    fn index(phase: PodPhase) -> usize {
        PodPhase::ALL
            .iter()
            .position(|p| *p == phase)
            .unwrap_or_default()
    }

    pub fn get(&self, phase: PodPhase) -> usize {
        self.counts[Self::index(phase)]
    }

    pub fn add(&mut self, phase: PodPhase) {
        self.counts[Self::index(phase)] += 1;
    }

    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }
}

/// Ready nodes out of all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeCounts {
    pub ready: usize,
    pub total: usize,
    /// Pods the nodes take (`status.allocatable.pods`, summed).
    pub pod_capacity: u64,
}

impl NodeCounts {
    pub fn not_ready(&self) -> usize {
        self.total.saturating_sub(self.ready)
    }
}

/// One resource (CPU in nanocores, memory in bytes) summed over the
/// running pods in scope, and what the nodes of the whole cluster offer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceTotals {
    pub requests: u64,
    /// Sum of the limits that are set.
    pub limits: u64,
    /// Running pods with no limit for this resource: the sum of limits
    /// does not bound them.
    pub unlimited: usize,
    /// Allocatable of all nodes (whole cluster, whatever the namespaces).
    pub allocatable: u64,
}

/// Share of `value` in `of` (`None` when `of` is zero), may exceed 1.
pub fn share(value: u64, of: u64) -> Option<f64> {
    (of > 0).then(|| value as f64 / of as f64)
}

/// Everything the counters of the screen show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterStats {
    pub nodes: NodeCounts,
    pub pods: PhaseCounts,
    /// Container restarts of the pods in scope, over their lifetime.
    pub restarts: u64,
    pub cpu: ResourceTotals,
    pub memory: ResourceTotals,
}

/// Whether a node reports `Ready=True`.
pub fn node_ready(node: &DynamicObject) -> bool {
    node.data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("Ready")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
}

/// CPU (nanocores) and memory (bytes) allocatable on `node`.
pub fn node_allocatable(node: &DynamicObject) -> Usage {
    let value = |name: &str| {
        node.data
            .pointer(&format!("/status/allocatable/{name}"))
            .and_then(Value::as_str)
    };
    Usage {
        cpu: value("cpu")
            .and_then(|q| cpu_nanocores(q).ok())
            .unwrap_or(0),
        memory: value("memory")
            .and_then(|q| memory_bytes(q).ok())
            .unwrap_or(0),
    }
}

/// Pods `node` takes (`status.allocatable.pods`), 0 when not reported.
pub fn node_pod_capacity(node: &DynamicObject) -> u64 {
    match node.data.pointer("/status/allocatable/pods") {
        Some(Value::String(count)) => count.parse().unwrap_or(0),
        Some(Value::Number(count)) => count.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// Container restarts of a pod (init containers included).
pub fn pod_restarts(pod: &DynamicObject) -> u64 {
    ["/status/containerStatuses", "/status/initContainerStatuses"]
        .iter()
        .filter_map(|path| pod.data.pointer(path).and_then(Value::as_array))
        .flatten()
        .filter_map(|status| status.get("restartCount").and_then(Value::as_u64))
        .sum()
}

/// The pod's spec only, enough for [`pod_resources`].
fn spec_only(pod: &DynamicObject) -> Option<Pod> {
    let spec: PodSpec = serde_json::from_value(pod.data.get("spec")?.clone()).ok()?;
    Some(Pod {
        spec: Some(spec),
        ..Pod::default()
    })
}

/// Counters of `nodes` (the whole cluster) and `pods` (the namespaces in
/// scope). Requests and limits count running pods only (not Succeeded or
/// Failed), as the scheduler does.
pub fn cluster_stats<'a>(
    nodes: impl IntoIterator<Item = &'a DynamicObject>,
    pods: impl IntoIterator<Item = &'a DynamicObject>,
) -> ClusterStats {
    let mut stats = ClusterStats::default();
    for node in nodes {
        stats.nodes.total += 1;
        if node_ready(node) {
            stats.nodes.ready += 1;
        }
        stats.nodes.pod_capacity = stats
            .nodes
            .pod_capacity
            .saturating_add(node_pod_capacity(node));
        let allocatable = node_allocatable(node);
        stats.cpu.allocatable = stats.cpu.allocatable.saturating_add(allocatable.cpu);
        stats.memory.allocatable = stats.memory.allocatable.saturating_add(allocatable.memory);
    }
    for pod in pods {
        stats.pods.add(PodPhase::of(pod));
        stats.restarts = stats.restarts.saturating_add(pod_restarts(pod));
        if !pod_is_active(pod) {
            continue;
        }
        let Some(typed) = spec_only(pod) else {
            continue;
        };
        let (requests, limits) = pod_resources(&typed);
        for (totals, request, limit) in [
            (&mut stats.cpu, requests.cpu, limits.cpu),
            (&mut stats.memory, requests.memory, limits.memory),
        ] {
            totals.requests = totals.requests.saturating_add(request.unwrap_or(0));
            match limit {
                Some(limit) => totals.limits = totals.limits.saturating_add(limit),
                None => totals.unlimited += 1,
            }
        }
    }
    stats
}

/// Sum of the usage of the pods in `namespaces` (all when `None`) in a
/// metrics-server poll; a shared poll may cover more namespaces.
pub fn pods_usage<'a>(
    pods: impl IntoIterator<Item = &'a super::PodMetrics>,
    namespaces: Option<&[String]>,
) -> Usage {
    pods.into_iter()
        .filter(|pod| namespaces.is_none_or(|names| names.contains(&pod.namespace)))
        .map(super::PodMetrics::total)
        .sum()
}

/// Restarts since the screen opened, without Prometheus: per pod against
/// the count first seen; pods that appeared later count all theirs.
#[derive(Clone, Debug, Default)]
pub struct RestartTracker {
    /// Restarts by pod uid when first seen; `None` until the first update.
    baseline: Option<HashMap<String, u64>>,
    since: u64,
}

impl RestartTracker {
    /// Takes in the pods now in scope; returns restarts since the first
    /// update.
    pub fn update<'a>(&mut self, pods: impl IntoIterator<Item = &'a DynamicObject>) -> u64 {
        let first = self.baseline.is_none();
        let baseline = self.baseline.get_or_insert_with(HashMap::new);
        let mut since = 0u64;
        for pod in pods {
            let Some(uid) = pod.metadata.uid.as_deref() else {
                continue;
            };
            let restarts = pod_restarts(pod);
            let base = *baseline
                .entry(uid.to_owned())
                .or_insert(if first { restarts } else { 0 });
            since = since.saturating_add(restarts.saturating_sub(base));
        }
        self.since = since;
        since
    }

    pub fn since(&self) -> u64 {
        self.since
    }
}

/// Pods per phase over time, as the screen charts them.
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChart {
    pub window: Window,
    /// Phases with at least one point, in [`PodPhase::ALL`] order.
    pub series: Vec<(PodPhase, Vec<Point>)>,
}

impl PhaseChart {
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }
}

/// Pod counts per phase sampled while the screen is open, for clusters
/// without kube-state-metrics in Prometheus.
#[derive(Clone, Debug, Default)]
pub struct PhaseHistory {
    samples: VecDeque<(f64, PhaseCounts)>,
}

impl PhaseHistory {
    pub fn push(&mut self, time: f64, counts: PhaseCounts) {
        if self.samples.back().is_some_and(|(last, _)| *last >= time) {
            return;
        }
        if self.samples.len() == LIVE_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back((time, counts));
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The window of `range` ending at `now`, as
    /// [`LiveHistory::range_window`](super::live::LiveHistory::range_window).
    pub fn range_window(&self, range: LiveRange, now: f64, step: u64) -> Window {
        let start = self.samples.front().map_or(now, |(time, _)| *time);
        let end = now.max(start) as u64;
        Window {
            start: range
                .seconds()
                .map_or(start as u64, |seconds| end.saturating_sub(seconds)),
            end,
            step,
        }
    }

    /// Phases seen in `window`: Running, Pending, Failed and Succeeded
    /// always; Unknown only when some pod was in it.
    pub fn chart(&self, window: Window) -> PhaseChart {
        let shown: Vec<&(f64, PhaseCounts)> = self
            .samples
            .iter()
            .filter(|(time, _)| *time >= window.start as f64)
            .collect();
        let series = PodPhase::ALL
            .into_iter()
            .filter(|phase| {
                *phase != PodPhase::Unknown || shown.iter().any(|(_, c)| c.get(*phase) > 0)
            })
            .map(|phase| {
                let points = shown
                    .iter()
                    .map(|(time, counts)| Point {
                        time: *time,
                        value: counts.get(phase) as f64,
                    })
                    .collect::<Vec<_>>();
                (phase, points)
            })
            .filter(|(_, points)| !points.is_empty())
            .collect();
        PhaseChart { window, series }
    }
}

/// Where the current usage of the cards comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrentSource {
    MetricsServer,
    Prometheus,
    None,
}

/// Where the usage charts come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// History with ranges (15m … 7d), also from before the app started.
    Prometheus,
    /// metrics-server polls collected while the screen is open.
    Session,
    None,
}

/// Where the pods-by-phase chart comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseSource {
    /// `kube_pod_status_phase` of kube-state-metrics.
    Prometheus,
    /// Counts from the cache, sampled while the screen is open.
    Session,
}

/// Sources of the screen's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sources {
    pub current: CurrentSource,
    pub history: HistorySource,
}

impl Sources {
    /// No usage at all: the screen says "metrics unavailable".
    pub fn none(&self) -> bool {
        self.current == CurrentSource::None && self.history == HistorySource::None
    }
}

/// Picks the sources as the tables and details do (ADR 0009):
/// metrics-server for the current value, Prometheus for history; each
/// falls back to the other.
pub fn sources(metrics_server: bool, prometheus: bool) -> Sources {
    Sources {
        current: match (metrics_server, prometheus) {
            (true, _) => CurrentSource::MetricsServer,
            (false, true) => CurrentSource::Prometheus,
            (false, false) => CurrentSource::None,
        },
        history: match (prometheus, metrics_server) {
            (true, _) => HistorySource::Prometheus,
            (false, true) => HistorySource::Session,
            (false, false) => HistorySource::None,
        },
    }
}

/// kube-state-metrics series (`Some(true)`) chart the phases; without them,
/// or before Prometheus answered, the session's samples do.
pub fn phase_source(prometheus_phases: Option<bool>) -> PhaseSource {
    match prometheus_phases {
        Some(true) => PhaseSource::Prometheus,
        _ => PhaseSource::Session,
    }
}

#[cfg(test)]
mod tests {
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{
        CurrentSource, HistorySource, PhaseHistory, PhaseSource, PodPhase, RestartTracker,
        cluster_stats, phase_source, share, sources,
    };
    use crate::metrics::live::LiveRange;
    use crate::metrics::prometheus::Window;

    fn object(value: serde_json::Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn node(name: &str, ready: bool, cpu: &str, memory: &str) -> DynamicObject {
        object(json!({
            "apiVersion": "v1", "kind": "Node",
            "metadata": {"name": name},
            "status": {
                "allocatable": {"cpu": cpu, "memory": memory, "pods": "110"},
                "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}],
            },
        }))
    }

    fn pod(
        name: &str,
        phase: Option<&str>,
        resources: serde_json::Value,
        restarts: u64,
    ) -> DynamicObject {
        let mut status = json!({
            "containerStatuses": [{"name": "app", "restartCount": restarts}],
        });
        if let Some(phase) = phase {
            status["phase"] = json!(phase);
        }
        object(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "web", "uid": format!("uid-{name}")},
            "spec": {"containers": [{"name": "app", "image": "nginx", "resources": resources}]},
            "status": status,
        }))
    }

    #[test]
    fn sums_requests_limits_and_allocatable() {
        let nodes = [
            node("a", true, "4", "8Gi"),
            node("b", false, "3800m", "4Gi"),
        ];
        let pods = [
            pod(
                "api",
                Some("Running"),
                json!({"requests": {"cpu": "250m", "memory": "256Mi"},
                       "limits": {"cpu": "1", "memory": "512Mi"}}),
                2,
            ),
            // Requests default to limits.
            pod(
                "worker",
                Some("Pending"),
                json!({"limits": {"cpu": "500m", "memory": "1Gi"}}),
                0,
            ),
            // No limits: unbounded.
            pod("batch", None, json!({"requests": {"cpu": "100m"}}), 1),
            // Done: counted in phases and restarts, not in resources.
            pod(
                "job",
                Some("Succeeded"),
                json!({"requests": {"cpu": "2"}}),
                3,
            ),
            pod("crash", Some("Failed"), json!({}), 0),
        ];
        let stats = cluster_stats(&nodes, &pods);
        assert_eq!((stats.nodes.ready, stats.nodes.total), (1, 2));
        assert_eq!(stats.nodes.not_ready(), 1);
        assert_eq!(stats.nodes.pod_capacity, 220);
        assert_eq!(stats.cpu.allocatable, 7_800_000_000);
        assert_eq!(stats.memory.allocatable, 12 << 30);
        assert_eq!(stats.cpu.requests, 850_000_000);
        assert_eq!(stats.cpu.limits, 1_500_000_000);
        assert_eq!(stats.cpu.unlimited, 1);
        assert_eq!(stats.memory.requests, (256 << 20) + (1 << 30));
        assert_eq!(stats.memory.limits, (512 << 20) + (1 << 30));
        assert_eq!(stats.memory.unlimited, 1);
        assert_eq!(stats.restarts, 6);
        assert_eq!(stats.pods.get(PodPhase::Running), 1);
        // No phase yet: pending.
        assert_eq!(stats.pods.get(PodPhase::Pending), 2);
        assert_eq!(stats.pods.get(PodPhase::Succeeded), 1);
        assert_eq!(stats.pods.get(PodPhase::Failed), 1);
        assert_eq!(stats.pods.total(), 5);
        assert_eq!(
            share(stats.cpu.requests, stats.cpu.allocatable),
            Some(0.85 / 7.8)
        );
        assert_eq!(share(1, 0), None);
    }

    #[test]
    fn phases_parse_like_the_api() {
        assert_eq!(PodPhase::parse(Some("Running")), PodPhase::Running);
        assert_eq!(PodPhase::parse(None), PodPhase::Pending);
        assert_eq!(PodPhase::parse(Some("Weird")), PodPhase::Unknown);
    }

    #[test]
    fn restarts_count_since_the_first_update() {
        let mut tracker = RestartTracker::default();
        assert_eq!(
            tracker.update(&[pod("a", Some("Running"), json!({}), 5)]),
            0
        );
        let later = [
            pod("a", Some("Running"), json!({}), 7),
            // New since: all its restarts count.
            pod("b", Some("Running"), json!({}), 1),
        ];
        assert_eq!(tracker.update(&later), 3);
        assert_eq!(tracker.since(), 3);
    }

    #[test]
    fn phase_history_charts_seen_phases() {
        let mut history = PhaseHistory::default();
        let mut counts = super::PhaseCounts::default();
        counts.add(PodPhase::Running);
        history.push(10.0, counts);
        counts.add(PodPhase::Pending);
        history.push(25.0, counts);
        // Out of order: ignored.
        history.push(20.0, counts);
        assert_eq!(history.len(), 2);
        let window = Window {
            start: 0,
            end: 30,
            step: 15,
        };
        let chart = history.chart(window);
        let phases: Vec<_> = chart.series.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            phases,
            [
                PodPhase::Running,
                PodPhase::Pending,
                PodPhase::Failed,
                PodPhase::Succeeded
            ]
        );
        assert_eq!(chart.series[1].1[1].value, 1.0);
        let later = Window {
            start: 20,
            end: 30,
            step: 15,
        };
        assert_eq!(history.chart(later).series[0].1.len(), 1);
        let all = history.range_window(LiveRange::All, 40.0, 15);
        assert_eq!((all.start, all.end), (10, 40));
        let five = history.range_window(LiveRange::FiveMinutes, 400.0, 15);
        assert_eq!((five.start, five.end), (100, 400));
    }

    #[test]
    fn usage_counts_the_namespaces_in_scope() {
        use crate::metrics::{ContainerMetrics, PodMetrics, Usage};
        let pod = |namespace: &str, cpu: u64| PodMetrics {
            name: "p".into(),
            namespace: namespace.into(),
            timestamp: None,
            window: None,
            containers: vec![ContainerMetrics {
                name: "c".into(),
                usage: Usage { cpu, memory: cpu },
            }],
        };
        let pods = [pod("web", 1), pod("api", 2), pod("db", 4)];
        assert_eq!(super::pods_usage(&pods, None).cpu, 7);
        let scope = ["web".to_owned(), "db".to_owned()];
        assert_eq!(super::pods_usage(&pods, Some(&scope)).cpu, 5);
    }

    #[test]
    fn picks_sources_like_the_tables() {
        let both = sources(true, true);
        assert_eq!(both.current, CurrentSource::MetricsServer);
        assert_eq!(both.history, HistorySource::Prometheus);
        let server = sources(true, false);
        assert_eq!(server.history, HistorySource::Session);
        let prometheus = sources(false, true);
        assert_eq!(prometheus.current, CurrentSource::Prometheus);
        assert!(!prometheus.none());
        assert!(sources(false, false).none());
        assert_eq!(phase_source(Some(true)), PhaseSource::Prometheus);
        assert_eq!(phase_source(Some(false)), PhaseSource::Session);
        assert_eq!(phase_source(None), PhaseSource::Session);
    }
}
