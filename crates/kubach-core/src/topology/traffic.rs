//! Traffic layer of the topology (#326, [ADR 0017](../../../../docs/adr/0017-traffic-layer.md)):
//! what really flows between workloads, read from the Prometheus metrics a
//! service mesh or CNI already exports. Nothing runs in the cluster
//! (ADR 0004) and nothing is collected by Kubach itself (ADR 0009).
//!
//! Sources are built-in plugins ([`TrafficPlugin`], a static list like
//! resource plugins, ADR 0003): Istio, Linkerd and Cilium Hubble. Each one
//!
//! - is a **candidate** when discovery (already cached) serves one of its
//!   API groups ([`candidates`]): no request at all;
//! - is **confirmed** by one small instant query ([`TrafficPlugin::probe`],
//!   answered by a single number) once per connection ([`confirm`]);
//! - gives **aggregating** queries ([`TrafficPlugin::queries`]): sums by
//!   source and destination workload, limited to the shown namespaces, so
//!   an answer has one series per pair of workloads that talk;
//! - names the two ends of each answer series ([`TrafficPlugin::flow`]).
//!
//! [`fetch`] runs the queries of one window with a timeout and a small
//! answer limit and parses them into [`TrafficSnapshot`] (only that window:
//! no history); [`apply`] lays a snapshot over a built [`TopologyGraph`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use super::model::{
    Edge, EdgeAttrs, EdgeId, EdgeKind, EdgeTraffic, ExternalKind, Node, NodeAttrs, NodeClass,
    NodeId, NodeKind, Presence, TopologyGraph,
};
use crate::metrics::prometheus::usage::Labels;
use crate::metrics::prometheus::{Prometheus, PrometheusError};

mod hubble;
mod istio;
mod linkerd;

pub use hubble::Hubble;
pub use istio::Istio;
pub use linkerd::Linkerd;

/// Largest answer of one traffic query. The queries sum by workload pair,
/// so even a big mesh answers in a few KiB; a bigger answer fails instead
/// of being read.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// A traffic query that takes longer is given up (the poll tries again).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// A probe that takes longer counts as "not confirmed".
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Flows kept per snapshot, the busiest first: the graph would be
/// unreadable long before, and memory stays bounded.
pub const MAX_FLOWS: usize = 1000;

/// Window the rates are computed over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TrafficWindow {
    OneMinute,
    #[default]
    FiveMinutes,
    OneHour,
}

impl TrafficWindow {
    pub const ALL: [TrafficWindow; 3] = [Self::OneMinute, Self::FiveMinutes, Self::OneHour];

    pub fn label(self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }

    /// PromQL range of `rate()`.
    pub fn range(self) -> &'static str {
        self.label()
    }

    /// How often the shown window is asked again: a long window changes
    /// slowly, so it is polled less.
    pub fn poll_interval(self) -> Duration {
        match self {
            Self::OneMinute => Duration::from_secs(15),
            Self::FiveMinutes => Duration::from_secs(30),
            Self::OneHour => Duration::from_secs(60),
        }
    }
}

/// What a query measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrafficMetric {
    /// Requests per second.
    Requests,
    /// Failed requests per second.
    Errors,
    /// p95 latency in milliseconds.
    LatencyP95,
    /// TCP bytes per second.
    Bytes,
    /// Forwarded flows per second.
    Flows,
    /// Dropped flows per second.
    Dropped,
}

impl TrafficMetric {
    /// Without it the layer has nothing to draw: its failure fails the
    /// poll. The others only add numbers to the edges.
    fn required(self) -> bool {
        matches!(self, Self::Requests | Self::Flows)
    }
}

/// One PromQL expression of a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficQuery {
    pub metric: TrafficMetric,
    pub promql: String,
}

/// A workload (or Service) as a mesh names it: namespace and name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Peer {
    pub namespace: String,
    pub name: String,
}

impl Peer {
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        }
    }

    /// From two labels; `None` when either is missing, empty or `unknown`.
    pub fn from_labels(labels: &Labels, namespace: &str, name: &str) -> Option<Self> {
        let known = |key: &str| {
            labels
                .get(key)
                .map(String::as_str)
                .filter(|v| !v.is_empty() && *v != "unknown")
        };
        Some(Self::new(known(namespace)?, known(name)?))
    }
}

/// The ends of a flow.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    /// `None`: outside the mesh (an unmeshed client, the internet).
    pub source: Option<Peer>,
    pub destination: Peer,
    /// The Service that was called, when the source says so.
    pub service: Option<Peer>,
}

/// Measured traffic between two workloads.
#[derive(Clone, Debug, PartialEq)]
pub struct TrafficFlow {
    pub key: FlowKey,
    pub traffic: EdgeTraffic,
}

/// The traffic of one window, as last polled. Replaced, never accumulated.
#[derive(Clone, Debug, PartialEq)]
pub struct TrafficSnapshot {
    /// Name of the source plugin.
    pub source: &'static str,
    pub window: TrafficWindow,
    /// Busiest first.
    pub flows: Vec<TrafficFlow>,
    /// More flows were measured than [`MAX_FLOWS`].
    pub truncated: bool,
    /// Optional queries that failed (latency, bytes): their numbers are
    /// missing, the rest is shown.
    pub partial: Vec<String>,
}

/// Namespaces the queries are limited to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NamespaceScope {
    All,
    Only(Vec<String>),
}

impl NamespaceScope {
    pub fn of(namespaces: Option<&[String]>) -> Self {
        match namespaces {
            Some(names) => {
                let mut names = names.to_vec();
                names.sort();
                names.dedup();
                Self::Only(names)
            }
            None => Self::All,
        }
    }

    /// `,label=~"a|b"` (empty for all namespaces).
    fn matcher(&self, label: &str) -> String {
        match self {
            Self::All => String::new(),
            Self::Only(names) => {
                let names: Vec<String> = names.iter().map(|n| regex_literal(n)).collect();
                format!(r#",{label}=~"{}""#, quote(&names.join("|")))
            }
        }
    }

    /// `expr(destination in scope) or expr(source in scope)`: flows with
    /// either end in the shown namespaces. `expr` gets the extra matcher
    /// to put in its selector. One expression for all namespaces.
    pub fn either(
        &self,
        destination_label: &str,
        source_label: &str,
        expr: impl Fn(&str) -> String,
    ) -> String {
        match self {
            Self::All => expr(""),
            Self::Only(_) => format!(
                "{} or {}",
                expr(&self.matcher(destination_label)),
                expr(&self.matcher(source_label))
            ),
        }
    }
}

/// `value` as a PromQL string literal body.
fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `value` matched literally inside a PromQL regex.
fn regex_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A built-in traffic source: detection, queries and naming of the ends.
/// Plugins are a static list ([`PLUGINS`]); none is loaded at run time.
pub trait TrafficPlugin: Send + Sync + 'static {
    /// Stable id, e.g. `istio`.
    fn id(&self) -> &'static str;
    /// Name shown in the UI, e.g. `Istio`.
    fn name(&self) -> &'static str;
    /// API groups (from discovery) that mean the component is installed.
    fn api_groups(&self) -> &'static [&'static str];
    /// Instant query whose non-empty answer (a single count) confirms the
    /// metrics are in Prometheus with the labels the queries need.
    fn probe(&self) -> &'static str;
    /// Aggregating queries of `window` for flows touching `scope`.
    fn queries(&self, scope: &NamespaceScope, window: TrafficWindow) -> Vec<TrafficQuery>;
    /// Ends of the flow an answer series stands for; `None` skips it.
    fn flow(&self, labels: &Labels) -> Option<FlowKey>;

    /// Whether discovery suggests the component: one of its API groups is
    /// served. Reads the cached catalog only.
    fn detect(&self, groups: &BTreeSet<&str>) -> bool {
        self.api_groups().iter().any(|group| groups.contains(group))
    }
}

/// Every traffic source, in order of preference: a mesh measures requests
/// (with errors and latency), Hubble flows.
pub static PLUGINS: [&dyn TrafficPlugin; 3] = [&Istio, &Linkerd, &Hubble];

/// The plugin with `id`.
pub fn plugin(id: &str) -> Option<&'static dyn TrafficPlugin> {
    PLUGINS.iter().copied().find(|plugin| plugin.id() == id)
}

/// Plugins whose API groups are among `groups` (from the discovery
/// catalog), in order of preference.
pub fn candidates<'a>(
    groups: impl IntoIterator<Item = &'a str>,
) -> Vec<&'static dyn TrafficPlugin> {
    let groups: BTreeSet<&str> = groups.into_iter().collect();
    PLUGINS
        .iter()
        .copied()
        .filter(|plugin| plugin.detect(&groups))
        .collect()
}

/// Whether a probe answer confirms the metrics: some series with a value
/// above zero.
pub fn probe_confirms(samples: &[(Labels, f64)]) -> bool {
    samples.iter().any(|(_, value)| *value > 0.)
}

/// Asks Prometheus which of `candidates` really have metrics: one small
/// query each, at once. Returns the ids of the confirmed ones, in order.
pub async fn confirm(
    prometheus: &Prometheus,
    candidates: &[&'static dyn TrafficPlugin],
) -> Vec<&'static str> {
    let probes = candidates.iter().map(|plugin| async move {
        let answer = tokio::time::timeout(
            PROBE_TIMEOUT,
            prometheus.query_limited(plugin.probe(), 64 * 1024),
        )
        .await;
        match answer {
            Ok(Ok(samples)) if probe_confirms(&samples) => Some(plugin.id()),
            _ => None,
        }
    });
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Parses the answers of a plugin's queries into flows, busiest first, at
/// most [`MAX_FLOWS`]. Series a plugin can't name are skipped; series of
/// the same flow add up (p95: the slowest).
pub fn parse(
    plugin: &dyn TrafficPlugin,
    answers: Vec<(TrafficMetric, Vec<(Labels, f64)>)>,
) -> (Vec<TrafficFlow>, bool) {
    let mut flows: HashMap<FlowKey, EdgeTraffic> = HashMap::new();
    for (metric, samples) in answers {
        for (labels, value) in samples {
            if !value.is_finite() || value < 0. {
                continue;
            }
            let Some(key) = plugin.flow(&labels) else {
                continue;
            };
            let mut traffic = EdgeTraffic::default();
            let slot = match metric {
                TrafficMetric::Requests => &mut traffic.requests,
                TrafficMetric::Errors => &mut traffic.errors,
                TrafficMetric::LatencyP95 => &mut traffic.p95_ms,
                TrafficMetric::Bytes => &mut traffic.bytes,
                TrafficMetric::Flows => &mut traffic.flows,
                TrafficMetric::Dropped => &mut traffic.dropped,
            };
            *slot = Some(value);
            flows.entry(key).or_default().merge(&traffic);
        }
    }
    let mut flows: Vec<TrafficFlow> = flows
        .into_iter()
        // Latency or errors alone, without any load, draw nothing.
        .filter(|(_, traffic)| traffic.load() > 0.)
        .map(|(key, traffic)| TrafficFlow { key, traffic })
        .collect();
    flows.sort_by(|a, b| {
        b.traffic
            .load()
            .total_cmp(&a.traffic.load())
            .then_with(|| a.key.cmp(&b.key))
    });
    let truncated = flows.len() > MAX_FLOWS;
    flows.truncate(MAX_FLOWS);
    (flows, truncated)
}

/// Polls the traffic of `window` in `scope` from `plugin`'s metrics: its
/// queries at once, each within [`QUERY_TIMEOUT`] and
/// [`MAX_RESPONSE_BYTES`]. Fails when the load query fails; optional
/// queries that fail are listed in [`TrafficSnapshot::partial`].
pub async fn fetch(
    prometheus: &Prometheus,
    plugin: &'static dyn TrafficPlugin,
    scope: &NamespaceScope,
    window: TrafficWindow,
) -> Result<TrafficSnapshot, PrometheusError> {
    let queries = plugin.queries(scope, window);
    let answers = futures::future::join_all(queries.into_iter().map(|query| async move {
        let answer = tokio::time::timeout(
            QUERY_TIMEOUT,
            prometheus.query_limited(&query.promql, MAX_RESPONSE_BYTES),
        )
        .await
        .unwrap_or_else(|_| Err(PrometheusError::Request("no answer in time".to_owned())));
        (query.metric, answer)
    }))
    .await;
    let mut parsed = Vec::new();
    let mut partial = Vec::new();
    let mut loaded = false;
    let mut failure = None;
    for (metric, answer) in answers {
        match answer {
            Ok(samples) => {
                loaded |= metric.required();
                parsed.push((metric, samples));
            }
            Err(error) if metric.required() => failure = Some(error),
            Err(error) => partial.push(format!("{metric:?}: {error}")),
        }
    }
    if let (false, Some(error)) = (loaded, failure) {
        return Err(error);
    }
    let (flows, truncated) = parse(plugin, parsed);
    Ok(TrafficSnapshot {
        source: plugin.name(),
        window,
        flows,
        truncated,
        partial,
    })
}

/// Id of a peer known only from traffic: a workload in a namespace that is
/// not shown.
fn peer_id(peer: &Peer) -> NodeId {
    NodeId::object("Peer", "", Some(&peer.namespace), &peer.name)
}

/// Id of the node standing for everything outside the mesh.
pub fn outside_id() -> NodeId {
    NodeId::external("outside the mesh")
}

fn peer_node(id: NodeId, namespace: Option<&str>, name: &str) -> Node {
    Node {
        id,
        kind: NodeKind::External(ExternalKind::Peer),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        api_kind: "Seen in traffic".to_owned(),
        api_group: String::new(),
        presence: Presence::Present,
        labels: BTreeMap::new(),
        attrs: NodeAttrs::default(),
    }
}

/// Lays `snapshot` over `graph`:
///
/// - a flow to a Service the graph has (named by the source, or the only
///   Service selecting the destination) becomes a [`EdgeKind::Traffic`]
///   edge source → Service, and adds to the Service's `Selects` edge to
///   the destination;
/// - other flows become source → destination edges, `unmapped` when no
///   Service in the graph selects the destination;
/// - ends the graph doesn't show (another namespace, outside the mesh)
///   become [`ExternalKind::Peer`] nodes.
pub fn apply(graph: TopologyGraph, snapshot: &TrafficSnapshot) -> TopologyGraph {
    if snapshot.flows.is_empty() {
        return graph;
    }
    // Workloads by (namespace, name); a Deployment wins over a ReplicaSet
    // of the same name (there is none, but be deterministic).
    let mut workloads: HashMap<(&str, &str), &NodeId> = HashMap::new();
    let mut services: HashMap<(&str, &str), &NodeId> = HashMap::new();
    for node in graph.nodes() {
        let Some(namespace) = node.namespace.as_deref() else {
            continue;
        };
        match node.kind.class() {
            NodeClass::Workload => {
                workloads
                    .entry((namespace, node.name.as_str()))
                    .or_insert(&node.id);
            }
            NodeClass::Service => {
                services.insert((namespace, node.name.as_str()), &node.id);
            }
            _ => {}
        }
    }
    // Which workloads each Service selects: directly, or through the pods
    // they own (detail "Pods").
    let mut selected_by: HashMap<&NodeId, Vec<&NodeId>> = HashMap::new();
    for edge in graph.edges() {
        if edge.kind != EdgeKind::Selects {
            continue;
        }
        let Some(target) = graph.node(&edge.to) else {
            continue;
        };
        let owners: Vec<&NodeId> = match target.kind.class() {
            NodeClass::Workload => vec![&target.id],
            NodeClass::Pod => graph
                .edges_to(&target.id)
                .filter(|e| e.kind == EdgeKind::Owns)
                .map(|e| &e.from)
                .collect(),
            _ => Vec::new(),
        };
        for owner in owners {
            let list = selected_by.entry(owner).or_default();
            if !list.contains(&&edge.from) {
                list.push(&edge.from);
            }
        }
    }

    let mut extra: BTreeMap<NodeId, Node> = BTreeMap::new();
    let mut resolve = |peer: &Peer| -> NodeId {
        if let Some(id) = workloads.get(&(peer.namespace.as_str(), peer.name.as_str())) {
            return (*id).clone();
        }
        let id = peer_id(peer);
        extra
            .entry(id.clone())
            .or_insert_with(|| peer_node(id.clone(), Some(&peer.namespace), &peer.name));
        id
    };
    let mut traffic_edges: BTreeMap<(NodeId, NodeId), EdgeTraffic> = BTreeMap::new();
    let mut through: HashMap<(NodeId, NodeId), EdgeTraffic> = HashMap::new();
    let mut outside = false;
    for flow in &snapshot.flows {
        let destination = resolve(&flow.key.destination);
        let source = match &flow.key.source {
            Some(peer) => resolve(peer),
            None => {
                outside = true;
                outside_id()
            }
        };
        if source == destination {
            continue;
        }
        let selecting = selected_by
            .get(&destination)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let named = flow.key.service.as_ref().and_then(|service| {
            services
                .get(&(service.namespace.as_str(), service.name.as_str()))
                .map(|id| (*id).clone())
        });
        let service = named.or_else(|| match selecting {
            [only] => Some((*only).clone()),
            _ => None,
        });
        match service {
            Some(service) => {
                traffic_edges
                    .entry((source, service.clone()))
                    .or_default()
                    .merge(&flow.traffic);
                through
                    .entry((service, destination))
                    .or_default()
                    .merge(&flow.traffic);
            }
            None => {
                let mut traffic = flow.traffic.clone();
                traffic.unmapped = selecting.is_empty();
                traffic_edges
                    .entry((source, destination))
                    .or_default()
                    .merge(&traffic);
            }
        }
    }

    let mut nodes = graph.nodes().to_vec();
    nodes.extend(extra.into_values());
    if outside {
        nodes.push(peer_node(outside_id(), None, "outside the mesh"));
    }
    let mut edges: Vec<Edge> = graph
        .edges()
        .iter()
        .cloned()
        .map(|mut edge| {
            if edge.kind == EdgeKind::Selects
                && let Some(traffic) = through.remove(&(edge.from.clone(), edge.to.clone()))
            {
                edge.attrs.traffic = Some(traffic);
            }
            edge
        })
        .collect();
    edges.extend(traffic_edges.into_iter().map(|((from, to), traffic)| Edge {
        id: EdgeId::new(EdgeKind::Traffic, &from, &to),
        from,
        to,
        kind: EdgeKind::Traffic,
        attrs: EdgeAttrs {
            traffic: Some(traffic),
            ..EdgeAttrs::default()
        },
    }));
    TopologyGraph::new(nodes, edges, graph.diagnostics().to_vec())
}

#[cfg(test)]
mod tests;
