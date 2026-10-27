//! Nodes, edges and diagnostics of a topology graph.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

/// Stable identity of a node: `Kind[.group]/namespace/name`, e.g.
/// `Deployment.apps/web/api`, `Service/web/api`, `External//203.0.113.7`.
///
/// Built from names, not UIDs: it survives a recreate, a rebuild and a
/// change of detail level (a workload keeps its id whether its pods are
/// shown or not), and a reference to an object that does not exist (an
/// Ingress backend by name) gets the id the object would have.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(Arc<str>);

impl NodeId {
    /// Id of a cluster object; `group` is empty for the core group,
    /// `namespace` is `None` for cluster-scoped objects.
    pub fn object(kind: &str, group: &str, namespace: Option<&str>, name: &str) -> Self {
        let namespace = namespace.unwrap_or_default();
        let id = if group.is_empty() {
            format!("{kind}/{namespace}/{name}")
        } else {
            format!("{kind}.{group}/{namespace}/{name}")
        };
        Self(id.into())
    }

    /// Id of an address outside the cluster (hostname or IP).
    pub fn external(address: &str) -> Self {
        Self(format!("External//{address}").into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.0)
    }
}

/// Stable identity of an edge: `kind:from->to`. There is at most one edge
/// of a kind between two nodes; several rules or references between them
/// are collected in its [`EdgeAttrs`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(Arc<str>);

impl EdgeId {
    pub fn new(kind: EdgeKind, from: &NodeId, to: &NodeId) -> Self {
        Self(format!("{}:{from}->{to}", kind.as_str()).into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.0)
    }
}

/// What a node stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    Ingress,
    /// `gateway.networking.k8s.io` Gateway.
    Gateway,
    HttpRoute,
    GrpcRoute,
    TlsRoute,
    Service(ServiceType),
    Pod,
    /// A workload; with [`Detail::Workloads`](super::Detail) its pods are
    /// collapsed into it.
    Workload(WorkloadKind),
    ConfigMap,
    Secret,
    PersistentVolumeClaim,
    ServiceAccount,
    /// An address outside the cluster.
    External(ExternalKind),
}

impl NodeKind {
    pub fn class(self) -> NodeClass {
        match self {
            NodeKind::Ingress => NodeClass::Ingress,
            NodeKind::Gateway => NodeClass::Gateway,
            NodeKind::HttpRoute | NodeKind::GrpcRoute | NodeKind::TlsRoute => NodeClass::Route,
            NodeKind::Service(_) => NodeClass::Service,
            NodeKind::Pod => NodeClass::Pod,
            NodeKind::Workload(_) => NodeClass::Workload,
            NodeKind::ConfigMap => NodeClass::ConfigMap,
            NodeKind::Secret => NodeClass::Secret,
            NodeKind::PersistentVolumeClaim => NodeClass::PersistentVolumeClaim,
            NodeKind::ServiceAccount => NodeClass::ServiceAccount,
            NodeKind::External(_) => NodeClass::External,
        }
    }

    /// Human name of the kind, e.g. `Service`, `HTTPRoute`.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Ingress => "Ingress",
            NodeKind::Gateway => "Gateway",
            NodeKind::HttpRoute => "HTTPRoute",
            NodeKind::GrpcRoute => "GRPCRoute",
            NodeKind::TlsRoute => "TLSRoute",
            NodeKind::Service(_) => "Service",
            NodeKind::Pod => "Pod",
            NodeKind::Workload(kind) => kind.label(),
            NodeKind::ConfigMap => "ConfigMap",
            NodeKind::Secret => "Secret",
            NodeKind::PersistentVolumeClaim => "PersistentVolumeClaim",
            NodeKind::ServiceAccount => "ServiceAccount",
            NodeKind::External(_) => "External",
        }
    }
}

/// Coarse node kinds, for filters and legends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeClass {
    Ingress,
    Gateway,
    Route,
    Service,
    Pod,
    Workload,
    ConfigMap,
    Secret,
    PersistentVolumeClaim,
    ServiceAccount,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServiceType {
    ClusterIp,
    /// `clusterIP: None`.
    Headless,
    NodePort,
    LoadBalancer,
    ExternalName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkloadKind {
    Deployment,
    /// Only a ReplicaSet without a controller; one owned by a Deployment is
    /// part of the Deployment's node.
    ReplicaSet,
    StatefulSet,
    DaemonSet,
    /// Only a Job without a CronJob; one created by a CronJob is part of
    /// the CronJob's node.
    Job,
    CronJob,
    /// A controller known only from `ownerReferences` (an Argo Rollout, an
    /// operator's resource); [`Node::api_kind`] names it.
    Other,
}

impl WorkloadKind {
    pub fn from_api(group: &str, kind: &str) -> Self {
        match (group, kind) {
            ("apps", "Deployment") => WorkloadKind::Deployment,
            ("apps", "ReplicaSet") => WorkloadKind::ReplicaSet,
            ("apps", "StatefulSet") => WorkloadKind::StatefulSet,
            ("apps", "DaemonSet") => WorkloadKind::DaemonSet,
            ("batch", "Job") => WorkloadKind::Job,
            ("batch", "CronJob") => WorkloadKind::CronJob,
            _ => WorkloadKind::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "Deployment",
            WorkloadKind::ReplicaSet => "ReplicaSet",
            WorkloadKind::StatefulSet => "StatefulSet",
            WorkloadKind::DaemonSet => "DaemonSet",
            WorkloadKind::Job => "Job",
            WorkloadKind::CronJob => "CronJob",
            WorkloadKind::Other => "Workload",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExternalKind {
    Hostname,
    Ip,
    /// A workload or client known only from traffic metrics (#326): in a
    /// namespace that is not shown, or outside the mesh.
    Peer,
}

/// Whether the object behind a node exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Presence {
    #[default]
    Present,
    /// Referenced, but its kind is loaded and it is not there (a dangling
    /// reference; there is a [`Diagnostic`] unless the reference is optional).
    Missing,
    /// Referenced, but its kind is not loaded (no access, still loading or
    /// not served), so whether it exists is unknown.
    Unknown,
}

/// A node of the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// `None` for external addresses.
    pub namespace: Option<String>,
    /// Object name; the address for external nodes.
    pub name: String,
    /// API kind and group of the object (`Rollout`, `argoproj.io` for a
    /// [`WorkloadKind::Other`]); empty for external nodes.
    pub api_kind: String,
    pub api_group: String,
    pub presence: Presence,
    /// The object's own labels.
    pub labels: BTreeMap<String, String>,
    pub attrs: NodeAttrs,
}

/// Kind-specific facts about a node. New layers add fields here (all
/// `Default`), so existing code keeps compiling.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeAttrs {
    /// Labels of the pods: a pod's own, a workload's pod template. What
    /// Service and NetworkPolicy selectors match.
    pub pod_labels: BTreeMap<String, String>,
    /// Pods of a workload (collapsed or shown) and how many are ready.
    pub pods: Option<PodCounts>,
    /// `spec.replicas` of a workload.
    pub desired_replicas: Option<i64>,
    /// Pod phase (`Running`, `Pending`…).
    pub phase: Option<String>,
    /// Pod `Ready` condition.
    pub ready: Option<bool>,
    pub service: Option<ServiceInfo>,
    /// Hostnames of an Ingress (rules and TLS), Gateway listeners or a route.
    pub hosts: Vec<String>,
    /// `ingressClassName` or `gatewayClassName`.
    pub class_name: Option<String>,
    /// Ports the containers of a pod (or a workload's pod template)
    /// declare: what named ports of NetworkPolicies and Services resolve to.
    pub container_ports: Vec<ContainerPort>,
    /// Set by the network policy layer
    /// ([`netpol::apply`](super::netpol::apply)) on pods and workloads: the
    /// policies that isolate them.
    pub network_policy: Option<NodePolicy>,
}

/// A port a container declares (`containers[].ports[]`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContainerPort {
    pub name: Option<String>,
    pub port: i32,
    pub protocol: Protocol,
}

/// L4 protocol of a port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
    Sctp,
}

impl Protocol {
    /// `TCP`, `UDP`, `SCTP` (case-insensitive); anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_uppercase().as_str() {
            "TCP" => Some(Protocol::Tcp),
            "UDP" => Some(Protocol::Udp),
            "SCTP" => Some(Protocol::Sctp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
            Protocol::Sctp => "SCTP",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A NetworkPolicy by namespace and name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PolicyId {
    pub namespace: String,
    pub name: String,
}

impl fmt::Display for PolicyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

/// The NetworkPolicies selecting a pod (or a workload's pods), by
/// direction. A direction with a policy is *isolated*: only what some rule
/// of those policies allows gets through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodePolicy {
    pub ingress: Vec<PolicyId>,
    pub egress: Vec<PolicyId>,
}

impl NodePolicy {
    pub fn isolated_ingress(&self) -> bool {
        !self.ingress.is_empty()
    }

    pub fn isolated_egress(&self) -> bool {
        !self.egress.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PodCounts {
    pub total: u32,
    pub ready: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceInfo {
    pub cluster_ip: Option<String>,
    pub ports: Vec<ServicePort>,
    pub external_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServicePort {
    pub name: Option<String>,
    pub port: i32,
    pub protocol: Option<String>,
    pub target_port: Option<PortRef>,
    pub node_port: Option<i32>,
}

/// A port by number or by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PortRef {
    Number(i32),
    Name(String),
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortRef::Number(number) => write!(f, "{number}"),
            PortRef::Name(name) => f.write_str(name),
        }
    }
}

/// How two nodes are related. New layers (network policies, traffic) add
/// variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum EdgeKind {
    /// Ingress or route → Service; Gateway → route. [`EdgeAttrs::routes`].
    RoutesTo,
    /// Service → workload, pod or external endpoint address.
    /// [`EdgeAttrs::endpoints`].
    Selects,
    /// Controller → owned object (`ownerReferences`).
    Owns,
    /// Workload or pod → ConfigMap, Secret or PVC through a volume (also a
    /// projected volume or an image pull secret). [`EdgeAttrs::refs`].
    Mounts,
    /// Workload or pod → ConfigMap or Secret through `env` or `envFrom`.
    /// [`EdgeAttrs::refs`].
    EnvRef,
    UsesServiceAccount,
    /// Service, Ingress or Gateway → external address (ExternalName,
    /// load balancer IP or hostname).
    Exposes,
    /// Ingress or Gateway → its TLS certificate Secret.
    Tls,
    /// Network policy layer: workload or pod → an ingress-isolated workload
    /// or pod a rule explicitly admits it to (a `podSelector` peer), egress
    /// permitting. [`EdgeAttrs::policy`].
    PolicyAllows,
    /// Traffic layer (#326): requests or flows a mesh or CNI measured,
    /// workload → the Service it called (or → the workload, when no
    /// Service explains it). [`EdgeAttrs::traffic`].
    Traffic,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::RoutesTo => "routes-to",
            EdgeKind::Selects => "selects",
            EdgeKind::Owns => "owns",
            EdgeKind::Mounts => "mounts",
            EdgeKind::EnvRef => "env-ref",
            EdgeKind::UsesServiceAccount => "uses-sa",
            EdgeKind::Exposes => "exposes",
            EdgeKind::Tls => "tls",
            EdgeKind::PolicyAllows => "policy-allows",
            EdgeKind::Traffic => "traffic",
        }
    }
}

/// An edge between two nodes of the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub attrs: EdgeAttrs,
}

/// Facts about an edge; which are set depends on its kind. New layers add
/// fields here (all `Default`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeAttrs {
    /// [`EdgeKind::RoutesTo`]: the rules behind it, in source order.
    pub routes: Vec<RouteRule>,
    /// [`EdgeKind::Selects`]: endpoints behind it.
    pub endpoints: Option<EndpointCounts>,
    /// [`EdgeKind::Mounts`], [`EdgeKind::EnvRef`]: the uses, sorted.
    pub refs: Vec<RefUse>,
    /// Set by the network policy layer on [`EdgeKind::Selects`] (Service →
    /// its pods, on the Service's target ports) and
    /// [`EdgeKind::PolicyAllows`].
    pub policy: Option<EdgePolicy>,
    /// Set by the traffic layer on [`EdgeKind::Traffic`] and on the
    /// [`EdgeKind::Selects`] edges the measured traffic went through.
    pub traffic: Option<EdgeTraffic>,
}

/// Measured traffic along an edge over the chosen window (#326). Rates are
/// per second; what is unknown to the source stays `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeTraffic {
    /// Requests (HTTP/gRPC) per second.
    pub requests: Option<f64>,
    /// Failed requests per second (5xx, or failures as the mesh classifies them).
    pub errors: Option<f64>,
    /// 95th percentile of request latency, milliseconds.
    pub p95_ms: Option<f64>,
    /// TCP bytes per second, both directions.
    pub bytes: Option<f64>,
    /// Forwarded L3/L4 flows per second (Cilium Hubble).
    pub flows: Option<f64>,
    /// Dropped flows per second (Cilium Hubble).
    pub dropped: Option<f64>,
    /// No Service in the graph explains the traffic: pod to pod, or to a
    /// workload no Service selects.
    pub unmapped: bool,
}

impl EdgeTraffic {
    /// Share of failed requests (or dropped flows), 0..=1.
    pub fn error_ratio(&self) -> Option<f64> {
        match (self.requests, self.errors) {
            (Some(requests), errors) if requests > 0. => {
                Some((errors.unwrap_or(0.) / requests).clamp(0., 1.))
            }
            _ => {
                let dropped = self.dropped?;
                let total = self.flows.unwrap_or(0.) + dropped;
                (total > 0.).then(|| (dropped / total).clamp(0., 1.))
            }
        }
    }

    /// How busy the edge is, to compare edges: requests per second, else
    /// flows per second, else KiB per second.
    pub fn load(&self) -> f64 {
        let positive = |v: Option<f64>| v.filter(|v| *v > 0.);
        positive(self.requests)
            .or(positive(self.flows.map(|f| f + self.dropped.unwrap_or(0.))))
            .or(positive(self.dropped))
            .or(positive(self.bytes.map(|b| b / 1024.)))
            .unwrap_or(0.)
    }

    /// Adds `other` (another flow along the same edge): rates add up, the
    /// slower p95 wins.
    pub fn merge(&mut self, other: &EdgeTraffic) {
        fn add(a: &mut Option<f64>, b: Option<f64>) {
            if let Some(b) = b {
                *a = Some(a.unwrap_or(0.) + b);
            }
        }
        add(&mut self.requests, other.requests);
        add(&mut self.errors, other.errors);
        add(&mut self.bytes, other.bytes);
        add(&mut self.flows, other.flows);
        add(&mut self.dropped, other.dropped);
        if let Some(p95) = other.p95_ms {
            self.p95_ms = Some(self.p95_ms.map_or(p95, |own| own.max(p95)));
        }
        self.unmapped |= other.unmapped;
    }
}

/// What NetworkPolicies say about the traffic along an edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EdgePolicy {
    pub access: Access,
    /// Policies that decide it: those admitting the traffic, or, when
    /// [`Access::Blocked`], those isolating the target.
    pub policies: Vec<PolicyId>,
    /// Ports it is about (`TCP 8080`), empty: any port.
    pub ports: Vec<String>,
}

/// How open a path is under NetworkPolicies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Access {
    /// No policy isolates the target: everything gets in.
    #[default]
    Open,
    /// Isolated, but a rule admits traffic from anywhere on these ports.
    Allowed,
    /// Isolated; rules admit only some sources on these ports.
    Restricted,
    /// Isolated and no rule admits anything on these ports.
    Blocked,
}

/// One routing rule: `host` + `path` → `port` of the target.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteRule {
    /// Ingress rule host or route hostnames; empty: any host.
    pub hosts: Vec<String>,
    /// HTTP path (Ingress, HTTPRoute) or gRPC `service/method` (GRPCRoute).
    pub path: Option<String>,
    /// Target port (backend port, or a Gateway listener port).
    pub port: Option<PortRef>,
    /// Gateway listener (`sectionName`) a route attaches to.
    pub section: Option<String>,
}

/// Endpoints of a Service behind a [`EdgeKind::Selects`] edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointCounts {
    pub ready: u32,
    pub not_ready: u32,
}

/// One use of a ConfigMap, Secret or PVC by a pod spec.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefUse {
    pub via: RefVia,
    /// Container (`env`, `envFrom`) or volume it comes through.
    pub at: String,
    /// The key it needs (`configMapKeyRef.key`, a volume item).
    pub key: Option<String>,
    /// `optional: true`: the pod starts without it.
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefVia {
    /// `env[].valueFrom`.
    Env,
    /// `envFrom[]`.
    EnvFrom,
    /// `volumes[].configMap`, `.secret`, `.persistentVolumeClaim`.
    Volume,
    /// `volumes[].projected.sources[]`.
    Projected,
    ImagePullSecret,
    /// A StatefulSet's `volumeClaimTemplates` (PVC `<template>-<set>-<n>`).
    ClaimTemplate,
}

/// A problem with a node's references.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diagnostic {
    /// The node with the problem.
    pub node: NodeId,
    /// The node it points to, if the problem is about a reference.
    pub target: Option<NodeId>,
    pub kind: DiagnosticKind,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticKind {
    /// A Service whose selector matches pods (or workload templates), none
    /// of them ready.
    NoReadyEndpoints { not_ready: u32 },
    /// A Service whose selector matches no pod and no workload.
    SelectorMatchesNothing,
    /// A reference (Ingress backend, route backend or parent, volume, env,
    /// service account, StatefulSet `serviceName`) to an object that does
    /// not exist; `target` is its [`Presence::Missing`] node.
    MissingTarget,
    /// An Ingress or route backend port the Service does not have.
    MissingPort { port: PortRef },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl Diagnostic {
    pub fn severity(&self) -> Severity {
        match self.kind {
            DiagnosticKind::NoReadyEndpoints { .. } | DiagnosticKind::SelectorMatchesNothing => {
                Severity::Warning
            }
            DiagnosticKind::MissingTarget | DiagnosticKind::MissingPort { .. } => Severity::Error,
        }
    }

    /// One-line English description, e.g. `Service web/api has no ready
    /// endpoints (2 not ready)`.
    pub fn message(&self, graph: &TopologyGraph) -> String {
        let describe = |id: &NodeId| match graph.node(id) {
            Some(node) => node.describe(),
            None => id.to_string(),
        };
        let node = describe(&self.node);
        let target = self.target.as_ref().map(describe).unwrap_or_default();
        match &self.kind {
            DiagnosticKind::NoReadyEndpoints { not_ready: 0 } => {
                format!("{node} has no ready endpoints")
            }
            DiagnosticKind::NoReadyEndpoints { not_ready } => {
                format!("{node} has no ready endpoints ({not_ready} not ready)")
            }
            DiagnosticKind::SelectorMatchesNothing => {
                format!("{node} selector matches no pods")
            }
            DiagnosticKind::MissingTarget => format!("{node} refers to missing {target}"),
            DiagnosticKind::MissingPort { port } => {
                format!("{node} refers to port {port} that {target} does not have")
            }
        }
    }
}

impl Node {
    /// `Kind namespace/name`, e.g. `Service web/api`.
    pub fn describe(&self) -> String {
        let kind = if self.api_kind.is_empty() {
            self.kind.label()
        } else {
            &self.api_kind
        };
        match &self.namespace {
            Some(namespace) => format!("{kind} {namespace}/{}", self.name),
            None => format!("{kind} {}", self.name),
        }
    }
}

/// A built topology: nodes and edges sorted by id, with diagnostics.
#[derive(Clone, Debug, Default)]
pub struct TopologyGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    diagnostics: Vec<Diagnostic>,
    index: HashMap<NodeId, usize>,
    outgoing: HashMap<NodeId, Vec<usize>>,
    incoming: HashMap<NodeId, Vec<usize>>,
}

impl PartialEq for TopologyGraph {
    fn eq(&self, other: &Self) -> bool {
        self.nodes == other.nodes
            && self.edges == other.edges
            && self.diagnostics == other.diagnostics
    }
}

impl TopologyGraph {
    /// Builds a graph from parts in any order. Edges whose ends are not
    /// among the nodes and duplicate ids are dropped (the first wins).
    pub fn new(nodes: Vec<Node>, edges: Vec<Edge>, diagnostics: Vec<Diagnostic>) -> Self {
        let mut nodes = nodes;
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        nodes.dedup_by(|a, b| a.id == b.id);
        let index: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(ix, node)| (node.id.clone(), ix))
            .collect();
        let mut edges: Vec<_> = edges
            .into_iter()
            .filter(|edge| index.contains_key(&edge.from) && index.contains_key(&edge.to))
            .collect();
        edges.sort_by(|a, b| a.id.cmp(&b.id));
        edges.dedup_by(|a, b| a.id == b.id);
        let mut diagnostics: Vec<_> = diagnostics
            .into_iter()
            .filter(|diagnostic| index.contains_key(&diagnostic.node))
            .collect();
        diagnostics.sort();
        diagnostics.dedup();
        let mut outgoing: HashMap<NodeId, Vec<usize>> = HashMap::new();
        let mut incoming: HashMap<NodeId, Vec<usize>> = HashMap::new();
        for (ix, edge) in edges.iter().enumerate() {
            outgoing.entry(edge.from.clone()).or_default().push(ix);
            incoming.entry(edge.to.clone()).or_default().push(ix);
        }
        Self {
            nodes,
            edges,
            diagnostics,
            index,
            outgoing,
            incoming,
        }
    }

    /// Sorted by id.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Sorted by id.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Sorted by node.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.index.get(id).map(|&ix| &self.nodes[ix])
    }

    pub fn edges_from(&self, id: &NodeId) -> impl Iterator<Item = &Edge> {
        self.outgoing
            .get(id)
            .into_iter()
            .flatten()
            .map(|&ix| &self.edges[ix])
    }

    pub fn edges_to(&self, id: &NodeId) -> impl Iterator<Item = &Edge> {
        self.incoming
            .get(id)
            .into_iter()
            .flatten()
            .map(|&ix| &self.edges[ix])
    }

    pub fn diagnostics_for<'a>(&'a self, id: &'a NodeId) -> impl Iterator<Item = &'a Diagnostic> {
        self.diagnostics
            .iter()
            .filter(move |diagnostic| &diagnostic.node == id)
    }

    /// Namespaces of the nodes, sorted (the groups to draw).
    pub fn namespaces(&self) -> BTreeSet<&str> {
        self.nodes
            .iter()
            .filter_map(|node| node.namespace.as_deref())
            .collect()
    }

    /// `seeds` with everything upstream (following edges backwards) and
    /// downstream (forwards) of them: an app's path from its Ingress to its
    /// ConfigMaps, without the siblings that only share a node with it.
    pub fn neighborhood<'a>(
        &self,
        seeds: impl IntoIterator<Item = &'a NodeId>,
    ) -> BTreeSet<NodeId> {
        self.neighborhood_within(seeds, None)
    }

    /// [`neighborhood`](Self::neighborhood) at most `steps` edges up and
    /// down from the seeds; `None`: unbounded.
    pub fn neighborhood_within<'a>(
        &self,
        seeds: impl IntoIterator<Item = &'a NodeId>,
        steps: Option<usize>,
    ) -> BTreeSet<NodeId> {
        let seeds: Vec<_> = seeds
            .into_iter()
            .filter(|id| self.index.contains_key(*id))
            .cloned()
            .collect();
        let mut seen: BTreeSet<NodeId> = seeds.iter().cloned().collect();
        for forward in [true, false] {
            let mut visited: BTreeSet<&NodeId> = BTreeSet::new();
            let mut queue: VecDeque<(&NodeId, usize)> = seeds.iter().map(|id| (id, 0)).collect();
            while let Some((id, depth)) = queue.pop_front() {
                if !visited.insert(id) || steps.is_some_and(|steps| depth >= steps) {
                    continue;
                }
                let next: Box<dyn Iterator<Item = &NodeId>> = if forward {
                    Box::new(self.edges_from(id).map(|edge| &edge.to))
                } else {
                    Box::new(self.edges_to(id).map(|edge| &edge.from))
                };
                for id in next {
                    seen.insert(id.clone());
                    queue.push_back((id, depth + 1));
                }
            }
        }
        seen
    }

    /// Keeps the nodes `keep` accepts, with the edges and diagnostics
    /// between them.
    pub fn retain(self, mut keep: impl FnMut(&Node) -> bool) -> Self {
        let nodes: Vec<_> = self.nodes.into_iter().filter(|node| keep(node)).collect();
        Self::new(nodes, self.edges, self.diagnostics)
    }

    /// Changes nodes and edges in place (a layer's attributes); ids must
    /// stay as they are.
    pub fn map(self, mut node: impl FnMut(&mut Node), mut edge: impl FnMut(&mut Edge)) -> Self {
        let mut nodes = self.nodes;
        let mut edges = self.edges;
        nodes.iter_mut().for_each(&mut node);
        edges.iter_mut().for_each(&mut edge);
        Self::new(nodes, edges, self.diagnostics)
    }

    /// Adds edges of another layer (network policies, traffic). Edges to
    /// unknown nodes are dropped; an edge with an existing id replaces it.
    pub fn with_edges(self, edges: impl IntoIterator<Item = Edge>) -> Self {
        let mut all: Vec<_> = edges.into_iter().collect();
        let ids: BTreeSet<_> = all.iter().map(|edge| edge.id.clone()).collect();
        all.extend(
            self.edges
                .into_iter()
                .filter(|edge| !ids.contains(&edge.id)),
        );
        Self::new(self.nodes, all, self.diagnostics)
    }

    /// What changed from `self` to `newer`, by id.
    pub fn diff(&self, newer: &TopologyGraph) -> GraphDiff {
        fn split<'a, T: PartialEq, K: Ord + Clone + 'a>(
            old: &'a [T],
            new: &'a [T],
            id: impl Fn(&T) -> &K,
        ) -> (Vec<K>, Vec<K>, Vec<K>) {
            let old: BTreeMap<&K, &T> = old.iter().map(|item| (id(item), item)).collect();
            let new: BTreeMap<&K, &T> = new.iter().map(|item| (id(item), item)).collect();
            let added = new
                .keys()
                .filter(|key| !old.contains_key(*key))
                .map(|key| (*key).clone())
                .collect();
            let removed = old
                .keys()
                .filter(|key| !new.contains_key(*key))
                .map(|key| (*key).clone())
                .collect();
            let changed = new
                .iter()
                .filter(|(key, item)| old.get(*key).is_some_and(|before| before != *item))
                .map(|(key, _)| (*key).clone())
                .collect();
            (added, removed, changed)
        }
        let (added_nodes, removed_nodes, changed_nodes) =
            split(&self.nodes, &newer.nodes, |node: &Node| &node.id);
        let (added_edges, removed_edges, changed_edges) =
            split(&self.edges, &newer.edges, |edge: &Edge| &edge.id);
        GraphDiff {
            added_nodes,
            removed_nodes,
            changed_nodes,
            added_edges,
            removed_edges,
            changed_edges,
        }
    }
}

/// Ids that differ between two graphs, sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphDiff {
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub changed_nodes: Vec<NodeId>,
    pub added_edges: Vec<EdgeId>,
    pub removed_edges: Vec<EdgeId>,
    pub changed_edges: Vec<EdgeId>,
}

impl GraphDiff {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.changed_nodes.is_empty()
            && self.added_edges.is_empty()
            && self.removed_edges.is_empty()
            && self.changed_edges.is_empty()
    }
}
