//! Resource usage from metrics-server (`metrics.k8s.io/v1beta1`): pod and
//! node metrics, usage against requests, limits and allocatable, and
//! periodic polling.
//!
//! History from Prometheus is in [`prometheus`]. CPU is in nanocores and memory in bytes
//! throughout; see [`format_cpu`] and [`format_memory`] for display.

pub mod live;
pub mod overview;
pub mod prometheus;
mod quantity;
pub mod shared;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::ops::Add;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::channel::mpsc;
use futures::{SinkExt as _, Stream};
use k8s_openapi::api::core::v1::{Container, Node, Pod, ResourceRequirements};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};
use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::{DynamicObject, ListParams};
use serde::Deserialize;

use crate::discovery::{self, ApiCatalog, ResourceKind};
use crate::runtime::{self, BackgroundTask};

pub use quantity::{
    QuantityError, cpu_nanocores, format_cpu, format_memory, memory_bytes, parse_quantity,
};

/// API group served by metrics-server.
pub const METRICS_GROUP: &str = "metrics.k8s.io";

/// metrics-server scrapes kubelets about every 15 s; polling faster only
/// returns the same numbers.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// `metrics.k8s.io` is not served: metrics-server is not installed.
    #[error("metrics unavailable: metrics.k8s.io is not served (is metrics-server installed?)")]
    NotInstalled,
    /// Served but not answering (APIService down, still starting), or no
    /// metrics for this object yet.
    #[error("metrics unavailable: {0}")]
    Unavailable(String),
    #[error("failed to fetch metrics: {0}")]
    Api(#[source] Box<kube::Error>),
    #[error("invalid metrics object: {0}")]
    Invalid(String),
}

impl MetricsError {
    /// Whether the UI should show "metrics unavailable" rather than an error.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::NotInstalled | Self::Unavailable(_))
    }

    fn from_kube(error: kube::Error) -> Self {
        match &error {
            kube::Error::Api(status) if status.is_not_found() || status.code == 503 => {
                Self::Unavailable(status.message.clone())
            }
            _ => Self::Api(Box::new(error)),
        }
    }
}

/// CPU (nanocores) and memory (bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub cpu: u64,
    pub memory: u64,
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        Usage {
            cpu: self.cpu.saturating_add(rhs.cpu),
            memory: self.memory.saturating_add(rhs.memory),
        }
    }
}

impl std::iter::Sum for Usage {
    fn sum<I: Iterator<Item = Usage>>(iter: I) -> Usage {
        iter.fold(Usage::default(), Add::add)
    }
}

impl Usage {
    fn parse(map: &BTreeMap<String, Quantity>) -> Result<Self, QuantityError> {
        Ok(Self {
            cpu: map
                .get("cpu")
                .map(|q| cpu_nanocores(&q.0))
                .transpose()?
                .unwrap_or(0),
            memory: map
                .get("memory")
                .map(|q| memory_bytes(&q.0))
                .transpose()?
                .unwrap_or(0),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerMetrics {
    pub name: String,
    pub usage: Usage,
}

/// A `PodMetrics` sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodMetrics {
    pub name: String,
    pub namespace: String,
    /// When the sample was taken.
    pub timestamp: Option<Timestamp>,
    /// Sampling window, as reported (e.g. `"10.5s"`).
    pub window: Option<String>,
    pub containers: Vec<ContainerMetrics>,
}

impl PodMetrics {
    pub fn total(&self) -> Usage {
        self.containers.iter().map(|c| c.usage).sum()
    }
}

/// A `NodeMetrics` sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeMetrics {
    pub name: String,
    pub timestamp: Option<Timestamp>,
    pub window: Option<String>,
    pub usage: Usage,
}

#[derive(Deserialize)]
struct RawPodMetrics {
    metadata: ObjectMeta,
    timestamp: Option<Time>,
    window: Option<String>,
    #[serde(default)]
    containers: Vec<RawContainerMetrics>,
}

#[derive(Deserialize)]
struct RawContainerMetrics {
    name: String,
    #[serde(default)]
    usage: BTreeMap<String, Quantity>,
}

#[derive(Deserialize)]
struct RawNodeMetrics {
    metadata: ObjectMeta,
    timestamp: Option<Time>,
    window: Option<String>,
    #[serde(default)]
    usage: BTreeMap<String, Quantity>,
}

fn invalid(error: impl std::fmt::Display) -> MetricsError {
    MetricsError::Invalid(error.to_string())
}

impl PodMetrics {
    /// Parses a `PodMetrics` object (JSON as served by the API).
    pub fn from_value(value: serde_json::Value) -> Result<Self, MetricsError> {
        let raw: RawPodMetrics = serde_json::from_value(value).map_err(invalid)?;
        let containers = raw
            .containers
            .into_iter()
            .map(|c| {
                Ok(ContainerMetrics {
                    usage: Usage::parse(&c.usage).map_err(invalid)?,
                    name: c.name,
                })
            })
            .collect::<Result<_, MetricsError>>()?;
        Ok(Self {
            name: raw.metadata.name.unwrap_or_default(),
            namespace: raw.metadata.namespace.unwrap_or_default(),
            timestamp: raw.timestamp.map(|t| t.0),
            window: raw.window,
            containers,
        })
    }
}

impl NodeMetrics {
    /// Parses a `NodeMetrics` object (JSON as served by the API).
    pub fn from_value(value: serde_json::Value) -> Result<Self, MetricsError> {
        let raw: RawNodeMetrics = serde_json::from_value(value).map_err(invalid)?;
        Ok(Self {
            name: raw.metadata.name.unwrap_or_default(),
            timestamp: raw.timestamp.map(|t| t.0),
            window: raw.window,
            usage: Usage::parse(&raw.usage).map_err(invalid)?,
        })
    }
}

fn to_value(obj: &DynamicObject) -> Result<serde_json::Value, MetricsError> {
    serde_json::to_value(obj).map_err(invalid)
}

/// Access to metrics-server for one connection. Cheap to clone.
#[derive(Clone)]
pub struct MetricsApi {
    client: Client,
    pods: ResourceKind,
    nodes: ResourceKind,
}

impl MetricsApi {
    /// Whether the cluster serves `metrics.k8s.io` (metrics-server or a
    /// compatible adapter is installed).
    pub fn is_available(catalog: &ApiCatalog) -> bool {
        catalog.find(METRICS_GROUP, "PodMetrics").is_some()
    }

    /// Fails with [`MetricsError::NotInstalled`] when metrics are not served.
    pub fn new(client: &Client, catalog: &ApiCatalog) -> Result<Self, MetricsError> {
        let pods = catalog.find(METRICS_GROUP, "PodMetrics");
        let nodes = catalog.find(METRICS_GROUP, "NodeMetrics");
        match (pods, nodes) {
            (Some(pods), Some(nodes)) => Ok(Self {
                client: client.clone(),
                pods: pods.clone(),
                nodes: nodes.clone(),
            }),
            _ => Err(MetricsError::NotInstalled),
        }
    }

    /// Pod metrics in `namespace`, or in all namespaces when `None`.
    pub async fn pods(&self, namespace: Option<&str>) -> Result<Vec<PodMetrics>, MetricsError> {
        let list = discovery::api(&self.client, &self.pods, namespace)
            .list(&ListParams::default())
            .await
            .map_err(MetricsError::from_kube)?;
        list.items
            .iter()
            .map(|o| PodMetrics::from_value(to_value(o)?))
            .collect()
    }

    /// Metrics of one pod; [`MetricsError::Unavailable`] when there is no
    /// sample yet (e.g. the pod just started).
    pub async fn pod(&self, namespace: &str, name: &str) -> Result<PodMetrics, MetricsError> {
        let obj = discovery::api(&self.client, &self.pods, Some(namespace))
            .get(name)
            .await
            .map_err(MetricsError::from_kube)?;
        PodMetrics::from_value(to_value(&obj)?)
    }

    pub async fn nodes(&self) -> Result<Vec<NodeMetrics>, MetricsError> {
        let list = discovery::api(&self.client, &self.nodes, None)
            .list(&ListParams::default())
            .await
            .map_err(MetricsError::from_kube)?;
        list.items
            .iter()
            .map(|o| NodeMetrics::from_value(to_value(o)?))
            .collect()
    }

    pub async fn node(&self, name: &str) -> Result<NodeMetrics, MetricsError> {
        let obj = discovery::api(&self.client, &self.nodes, None)
            .get(name)
            .await
            .map_err(MetricsError::from_kube)?;
        NodeMetrics::from_value(to_value(&obj)?)
    }

    /// [`pods`](Self::pods) every `interval`.
    pub fn poll_pods(
        &self,
        namespace: Option<String>,
        interval: Duration,
    ) -> Polling<Vec<PodMetrics>> {
        let api = self.clone();
        poll(interval, move || {
            let api = api.clone();
            let namespace = namespace.clone();
            async move { api.pods(namespace.as_deref()).await }
        })
    }

    /// [`nodes`](Self::nodes) every `interval`.
    pub fn poll_nodes(&self, interval: Duration) -> Polling<Vec<NodeMetrics>> {
        let api = self.clone();
        poll(interval, move || {
            let api = api.clone();
            async move { api.nodes().await }
        })
    }
}

/// Results of `fetch` called right away and then every `interval`, until
/// dropped. Errors are yielded too, and polling continues after them.
/// Works on any executor: fetching runs on the I/O [`runtime`].
pub struct Polling<T> {
    rx: mpsc::Receiver<Result<T, MetricsError>>,
    _task: BackgroundTask,
}

impl<T> Stream for Polling<T> {
    type Item = Result<T, MetricsError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }
}

/// Calls `fetch` now and then every `interval` on the I/O runtime. A slow
/// consumer delays the next fetch instead of queueing results.
pub fn poll<T, F, Fut>(interval: Duration, mut fetch: F) -> Polling<T>
where
    T: Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, MetricsError>> + Send,
{
    let (mut tx, rx) = mpsc::channel(0);
    let task = runtime::spawn(async move {
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if tx.send(fetch().await).await.is_err() {
                break;
            }
        }
    });
    Polling { rx, _task: task }
}

/// Requests or limits: `None` means not set (for limits: unbounded).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    pub cpu: Option<u64>,
    pub memory: Option<u64>,
}

fn resource(map: Option<&BTreeMap<String, Quantity>>, name: &str) -> Option<u64> {
    let q = map?.get(name)?;
    if name == "cpu" {
        cpu_nanocores(&q.0).ok()
    } else {
        memory_bytes(&q.0).ok()
    }
}

/// Requests and limits of a container. Requests default to limits, as the
/// API server does.
pub fn container_resources(container: &Container) -> (Resources, Resources) {
    resources_of(container.resources.as_ref())
}

fn resources_of(r: Option<&ResourceRequirements>) -> (Resources, Resources) {
    let requests = r.and_then(|r| r.requests.as_ref());
    let limits = r.and_then(|r| r.limits.as_ref());
    let limits = Resources {
        cpu: resource(limits, "cpu"),
        memory: resource(limits, "memory"),
    };
    let requests = Resources {
        cpu: resource(requests, "cpu").or(limits.cpu),
        memory: resource(requests, "memory").or(limits.memory),
    };
    (requests, limits)
}

/// Effective pod requests and limits, like the scheduler computes them:
/// the larger of (app containers + sidecars) and any single init container,
/// plus pod overhead. Limits are `None` when any container is unbounded.
pub fn pod_resources(pod: &Pod) -> (Resources, Resources) {
    let Some(spec) = pod.spec.as_ref() else {
        return Default::default();
    };
    let inits = spec.init_containers.as_deref().unwrap_or_default();
    let is_sidecar = |c: &&Container| c.restart_policy.as_deref() == Some("Always");
    let long_running: Vec<_> = spec
        .containers
        .iter()
        .chain(inits.iter().filter(is_sidecar))
        .collect();
    let one_shot: Vec<_> = inits.iter().filter(|c| !is_sidecar(c)).collect();
    let overhead = spec.overhead.as_ref();

    let pick = |res: (Resources, Resources), limit: bool, cpu: bool| {
        let r = if limit { res.1 } else { res.0 };
        if cpu { r.cpu } else { r.memory }
    };
    let total = |limit: bool, cpu: bool| -> Option<u64> {
        let values: Vec<Option<u64>> = long_running
            .iter()
            .map(|c| pick(container_resources(c), limit, cpu))
            .collect();
        let sum: Option<u64> = if limit {
            // Any unbounded container makes the pod unbounded.
            values.iter().copied().sum()
        } else {
            values.iter().flatten().copied().reduce(u64::saturating_add)
        };
        let init_max = one_shot
            .iter()
            .filter_map(|c| pick(container_resources(c), limit, cpu))
            .max();
        let base = match (sum, init_max) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, _) if limit => None,
            (a, b) => a.or(b),
        };
        let extra = resource(overhead, if cpu { "cpu" } else { "memory" }).unwrap_or(0);
        base.map(|v| v.saturating_add(extra))
    };
    (
        Resources {
            cpu: total(false, true),
            memory: total(false, false),
        },
        Resources {
            cpu: total(true, true),
            memory: total(true, false),
        },
    )
}

/// Requests and limits of a set of pods (a workload's), from their
/// [`pod_resources`]: a request sums the pods that set it (`None` when none
/// does); a limit is unbounded (`None`) as soon as one pod has none, and when
/// there are no pods. See ADR 0009.
pub fn sum_resources(all: &[(Resources, Resources)]) -> (Resources, Resources) {
    let request = |pick: fn(&Resources) -> Option<u64>| {
        all.iter()
            .filter_map(|(r, _)| pick(r))
            .reduce(u64::saturating_add)
    };
    let limit = |pick: fn(&Resources) -> Option<u64>| {
        if all.is_empty() {
            return None;
        }
        all.iter()
            .map(|(_, l)| pick(l))
            .try_fold(0u64, |sum, v| v.map(|v| sum.saturating_add(v)))
    };
    (
        Resources {
            cpu: request(|r| r.cpu),
            memory: request(|r| r.memory),
        },
        Resources {
            cpu: limit(|r| r.cpu),
            memory: limit(|r| r.memory),
        },
    )
}

/// Whether a pod still runs and so counts towards its workload's requests
/// and limits: every phase but `Succeeded` and `Failed` (a pod without a
/// phase is pending).
pub fn pod_is_active(pod: &DynamicObject) -> bool {
    !matches!(
        pod.data
            .pointer("/status/phase")
            .and_then(serde_json::Value::as_str),
        Some("Succeeded" | "Failed")
    )
}

fn percent(used: u64, of: Option<u64>) -> Option<f64> {
    of.filter(|&of| of > 0)
        .map(|of| used as f64 * 100.0 / of as f64)
}

/// Usage of one resource against requests and limits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageStat {
    pub used: u64,
    pub request: Option<u64>,
    pub limit: Option<u64>,
}

impl UsageStat {
    /// Usage as a percentage of the request; may exceed 100.
    pub fn request_percent(&self) -> Option<f64> {
        percent(self.used, self.request)
    }

    pub fn limit_percent(&self) -> Option<f64> {
        percent(self.used, self.limit)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContainerUsage {
    pub name: String,
    pub cpu: UsageStat,
    pub memory: UsageStat,
}

/// A pod's usage against its requests and limits.
#[derive(Clone, Debug, PartialEq)]
pub struct PodUsage {
    pub cpu: UsageStat,
    pub memory: UsageStat,
    /// Containers present in the metrics sample, in its order.
    pub containers: Vec<ContainerUsage>,
}

/// Combines a pod spec with its metrics sample.
pub fn pod_usage(pod: &Pod, metrics: &PodMetrics) -> PodUsage {
    let (requests, limits) = pod_resources(pod);
    let spec_containers: Vec<&Container> = pod
        .spec
        .iter()
        .flat_map(|s| {
            s.containers
                .iter()
                .chain(s.init_containers.iter().flatten())
        })
        .collect();
    let containers = metrics
        .containers
        .iter()
        .map(|m| {
            let (req, lim) = spec_containers
                .iter()
                .find(|c| c.name == m.name)
                .map(|c| container_resources(c))
                .unwrap_or_default();
            ContainerUsage {
                name: m.name.clone(),
                cpu: UsageStat {
                    used: m.usage.cpu,
                    request: req.cpu,
                    limit: lim.cpu,
                },
                memory: UsageStat {
                    used: m.usage.memory,
                    request: req.memory,
                    limit: lim.memory,
                },
            }
        })
        .collect();
    let total = metrics.total();
    PodUsage {
        cpu: UsageStat {
            used: total.cpu,
            request: requests.cpu,
            limit: limits.cpu,
        },
        memory: UsageStat {
            used: total.memory,
            request: requests.memory,
            limit: limits.memory,
        },
        containers,
    }
}

/// Usage of one node resource against allocatable and capacity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NodeStat {
    pub used: u64,
    pub allocatable: Option<u64>,
    pub capacity: Option<u64>,
}

impl NodeStat {
    /// Usage as a percentage of allocatable (what `kubectl top node` shows).
    pub fn percent(&self) -> Option<f64> {
        percent(self.used, self.allocatable)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NodeUsage {
    pub cpu: NodeStat,
    pub memory: NodeStat,
}

/// CPU and memory a node offers to pods.
pub fn node_allocatable(node: &Node) -> Resources {
    let allocatable = node.status.as_ref().and_then(|s| s.allocatable.as_ref());
    Resources {
        cpu: resource(allocatable, "cpu"),
        memory: resource(allocatable, "memory"),
    }
}

/// Combines a node with its metrics sample.
pub fn node_usage(node: &Node, metrics: &NodeMetrics) -> NodeUsage {
    let status = node.status.as_ref();
    let allocatable = status.and_then(|s| s.allocatable.as_ref());
    let capacity = status.and_then(|s| s.capacity.as_ref());
    NodeUsage {
        cpu: NodeStat {
            used: metrics.usage.cpu,
            allocatable: resource(allocatable, "cpu"),
            capacity: resource(capacity, "cpu"),
        },
        memory: NodeStat {
            used: metrics.usage.memory,
            allocatable: resource(allocatable, "memory"),
            capacity: resource(capacity, "memory"),
        },
    }
}

/// Latest samples by object, to join listed Pods and Nodes with their
/// metrics.
#[derive(Clone, Debug, Default)]
pub struct MetricsIndex {
    pods: HashMap<(String, String), PodMetrics>,
    nodes: HashMap<String, NodeMetrics>,
}

impl MetricsIndex {
    pub fn from_pods(pods: impl IntoIterator<Item = PodMetrics>) -> Self {
        Self {
            pods: pods
                .into_iter()
                .map(|m| ((m.namespace.clone(), m.name.clone()), m))
                .collect(),
            nodes: HashMap::new(),
        }
    }

    pub fn from_nodes(nodes: impl IntoIterator<Item = NodeMetrics>) -> Self {
        Self {
            pods: HashMap::new(),
            nodes: nodes.into_iter().map(|m| (m.name.clone(), m)).collect(),
        }
    }

    pub fn pod(&self, namespace: &str, name: &str) -> Option<&PodMetrics> {
        self.pods.get(&(namespace.to_owned(), name.to_owned()))
    }

    pub fn node(&self, name: &str) -> Option<&NodeMetrics> {
        self.nodes.get(name)
    }

    /// Current usage of a listed object: a Pod when it has a namespace,
    /// else a Node. `None` without a sample.
    pub fn usage_of(&self, obj: &DynamicObject) -> Option<Usage> {
        let name = obj.metadata.name.as_deref()?;
        match obj.metadata.namespace.as_deref() {
            Some(namespace) => self.pod(namespace, name).map(PodMetrics::total),
            None => self.node(name).map(|m| m.usage),
        }
    }

    /// A listed Pod's usage against its requests and limits.
    pub fn pod_usage_of(&self, obj: &DynamicObject) -> Option<PodUsage> {
        let metrics = self.pod(
            obj.metadata.namespace.as_deref()?,
            obj.metadata.name.as_deref()?,
        )?;
        Some(pod_usage(&decode(obj)?, metrics))
    }

    /// A listed Node's usage against its allocatable resources.
    pub fn node_usage_of(&self, obj: &DynamicObject) -> Option<NodeUsage> {
        let metrics = self.node(obj.metadata.name.as_deref()?)?;
        Some(node_usage(&decode(obj)?, metrics))
    }
}

fn decode<K: serde::de::DeserializeOwned>(obj: &DynamicObject) -> Option<K> {
    serde_json::from_value(serde_json::to_value(obj).ok()?).ok()
}

impl UsageStat {
    /// Share of the limit, else of the request, in `0..=1` for a meter.
    pub fn fill(&self) -> Option<f64> {
        self.limit_percent()
            .or_else(|| self.request_percent())
            .map(|p| (p / 100.0).clamp(0.0, 1.0))
    }

    /// `"120m · 48% of 250m request · no limit"`, units by `format`.
    pub fn describe(&self, format: impl Fn(u64) -> String) -> String {
        let against = |value: Option<u64>, pct: Option<f64>, what: &str| match (value, pct) {
            (Some(v), Some(p)) => format!("{p:.0}% of {} {what}", format(v)),
            (Some(v), None) => format!("{} {what}", format(v)),
            (None, _) => format!("no {what}"),
        };
        format!(
            "{} · {} · {}",
            format(self.used),
            against(self.request, self.request_percent(), "request"),
            against(self.limit, self.limit_percent(), "limit"),
        )
    }
}

impl NodeStat {
    /// Share of allocatable in `0..=1`, for a meter.
    pub fn fill(&self) -> Option<f64> {
        self.percent().map(|p| (p / 100.0).clamp(0.0, 1.0))
    }

    /// `"950m · 25% of 3.8 allocatable"`, units by `format`.
    pub fn describe(&self, format: impl Fn(u64) -> String) -> String {
        match (self.allocatable, self.percent()) {
            (Some(a), Some(p)) => {
                format!(
                    "{} · {p:.0}% of {} allocatable",
                    format(self.used),
                    format(a)
                )
            }
            _ => format(self.used),
        }
    }
}

#[cfg(test)]
mod tests;
