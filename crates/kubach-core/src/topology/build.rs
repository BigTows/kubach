//! Snapshot → graph: joins the facts of every object into nodes, edges and
//! diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use super::extract::{self, Body, Facts, Meta, OwnerRef, RefTarget, SpecFacts, WorkloadFacts};
use super::model::{
    Diagnostic, DiagnosticKind, Edge, EdgeAttrs, EdgeId, EdgeKind, EndpointCounts, ExternalKind,
    Node, NodeAttrs, NodeClass, NodeId, NodeKind, PodCounts, PortRef, Presence, RefUse, RefVia,
    RouteRule, ServiceInfo, ServiceType, TopologyGraph, WorkloadKind,
};
use super::source::{SourceKind, TopologyInput};

/// How much of a workload to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Pods are collapsed into their workload (a Deployment also takes its
    /// ReplicaSets, a CronJob its Jobs); pods without a controller stay.
    #[default]
    Workloads,
    /// Every pod is a node, owned by its workload.
    Pods,
}

/// Which nodes to keep. Every set part narrows the graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Namespaced nodes outside these are dropped (external addresses stay
    /// while something links to them).
    pub namespaces: Option<BTreeSet<String>>,
    pub kinds: Option<BTreeSet<NodeClass>>,
    /// Nodes whose labels (or pod labels) match, with everything upstream
    /// and downstream of them ([`TopologyGraph::neighborhood`]).
    pub labels: Option<LabelSelector>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildOptions {
    pub detail: Detail,
    pub filter: Filter,
    /// Also ConfigMaps, Secrets, PVCs and ServiceAccounts nothing refers to.
    pub include_unreferenced: bool,
    /// Adds the network policy layer ([`netpol::apply`](super::netpol::apply))
    /// from the input's NetworkPolicies and Namespaces.
    pub network_policies: bool,
}

/// An equality-based label selector: `app=web,tier!=db,canary,!legacy`
/// (`==` works as `=`; set-based `in`/`notin` are not supported).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelSelector {
    requirements: Vec<Requirement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Requirement {
    Equals(String, String),
    NotEquals(String, String),
    Exists(String),
    NotExists(String),
}

impl LabelSelector {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut requirements = Vec::new();
        for part in text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let requirement = if let Some((key, value)) = part.split_once("!=") {
                Requirement::NotEquals(key.trim().into(), value.trim().into())
            } else if let Some((key, value)) =
                part.split_once("==").or_else(|| part.split_once('='))
            {
                Requirement::Equals(key.trim().into(), value.trim().into())
            } else if let Some(key) = part.strip_prefix('!') {
                Requirement::NotExists(key.trim().into())
            } else if part.contains(char::is_whitespace) {
                return Err(format!("unsupported label requirement `{part}`"));
            } else {
                Requirement::Exists(part.into())
            };
            let key = match &requirement {
                Requirement::Equals(key, _)
                | Requirement::NotEquals(key, _)
                | Requirement::Exists(key)
                | Requirement::NotExists(key) => key,
            };
            if key.is_empty() {
                return Err(format!("empty label key in `{part}`"));
            }
            requirements.push(requirement);
        }
        Ok(Self { requirements })
    }

    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.requirements
            .iter()
            .all(|requirement| match requirement {
                Requirement::Equals(key, value) => labels.get(key) == Some(value),
                Requirement::NotEquals(key, value) => labels.get(key) != Some(value),
                Requirement::Exists(key) => labels.contains_key(key),
                Requirement::NotExists(key) => !labels.contains_key(key),
            })
    }
}

/// Builds the graph of `input` in one go. Use a [`TopologyBuilder`] to
/// rebuild it as the input changes.
pub fn build(input: &TopologyInput, options: &BuildOptions) -> TopologyGraph {
    TopologyBuilder::new().build(input, options)
}

type MemoKey = (SourceKind, String, String);

/// Rebuilds a graph cheaply: what each object contributes is read once per
/// `resourceVersion` and reused while the object does not change; only the
/// join runs on every build.
#[derive(Default)]
pub struct TopologyBuilder {
    memo: HashMap<MemoKey, (String, Arc<Facts>)>,
}

impl TopologyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(&mut self, input: &TopologyInput, options: &BuildOptions) -> TopologyGraph {
        let mut memo = HashMap::with_capacity(self.memo.len());
        let mut world = World::default();
        for kind in SourceKind::ALL {
            let objects = input.objects(kind);
            let mut facts = Vec::with_capacity(objects.len());
            for object in objects {
                let key = (
                    kind,
                    object.metadata.namespace.clone().unwrap_or_default(),
                    object.metadata.name.clone().unwrap_or_default(),
                );
                let version = object.metadata.resource_version.as_deref();
                let read = match (version, self.memo.remove(&key)) {
                    (Some(version), Some((seen, read))) if seen == version => read,
                    _ => Arc::new(extract::facts(kind, object)),
                };
                if let Some(version) = version {
                    memo.insert(key, (version.to_owned(), Arc::clone(&read)));
                }
                facts.push(read);
            }
            world.facts.insert(kind, facts);
        }
        self.memo = memo;
        let graph = Assembler::new(&world, input, options).run();
        let graph = apply_filter(graph, &options.filter);
        if options.network_policies {
            super::netpol::apply(graph, &super::netpol::PolicySet::from_input(input))
        } else {
            graph
        }
    }
}

#[derive(Default)]
struct World {
    facts: HashMap<SourceKind, Vec<Arc<Facts>>>,
}

impl World {
    fn of(&self, kind: SourceKind) -> &[Arc<Facts>] {
        self.facts.get(&kind).map_or(&[], Vec::as_slice)
    }
}

const WORKLOADS: [SourceKind; 6] = [
    // Controllers first, so a collapsed ReplicaSet or Job finds its node.
    SourceKind::Deployment,
    SourceKind::StatefulSet,
    SourceKind::DaemonSet,
    SourceKind::CronJob,
    SourceKind::ReplicaSet,
    SourceKind::Job,
];

const POD_TEMPLATE_HASH: &str = "pod-template-hash";

fn id_of(kind: SourceKind, namespace: Option<&str>, name: &str) -> NodeId {
    NodeId::object(kind.kind(), kind.group(), namespace, name)
}

fn node_kind(kind: SourceKind, facts: Option<&Facts>) -> NodeKind {
    match kind {
        SourceKind::Pod => NodeKind::Pod,
        SourceKind::Deployment
        | SourceKind::ReplicaSet
        | SourceKind::StatefulSet
        | SourceKind::DaemonSet
        | SourceKind::Job
        | SourceKind::CronJob => {
            NodeKind::Workload(WorkloadKind::from_api(kind.group(), kind.kind()))
        }
        SourceKind::Service => NodeKind::Service(match facts.map(|facts| &facts.body) {
            Some(Body::Service(service)) => service.ty,
            _ => ServiceType::ClusterIp,
        }),
        SourceKind::EndpointSlice => NodeKind::Service(ServiceType::ClusterIp),
        SourceKind::Ingress => NodeKind::Ingress,
        SourceKind::Gateway => NodeKind::Gateway,
        SourceKind::HttpRoute => NodeKind::HttpRoute,
        SourceKind::GrpcRoute => NodeKind::GrpcRoute,
        SourceKind::TlsRoute => NodeKind::TlsRoute,
        SourceKind::ConfigMap => NodeKind::ConfigMap,
        SourceKind::Secret => NodeKind::Secret,
        SourceKind::PersistentVolumeClaim => NodeKind::PersistentVolumeClaim,
        SourceKind::ServiceAccount => NodeKind::ServiceAccount,
        // Not drawn: read by the network policy layer only.
        SourceKind::NetworkPolicy | SourceKind::Namespace => NodeKind::ConfigMap,
    }
}

/// The node a controller owner stands for.
#[derive(Clone, Debug)]
struct OwnerTarget {
    id: NodeId,
    group: String,
    kind: String,
    name: String,
}

impl OwnerTarget {
    fn of(namespace: Option<&str>, owner: &OwnerRef) -> Self {
        Self {
            id: NodeId::object(&owner.kind, &owner.group, namespace, &owner.name),
            group: owner.group.clone(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
        }
    }
}

type Key<'w> = (SourceKind, &'w str, &'w str);

type Labels = BTreeMap<String, String>;

struct Assembler<'w> {
    world: &'w World,
    input: &'w TopologyInput,
    detail: Detail,
    include_unreferenced: bool,
    index: HashMap<Key<'w>, &'w Facts>,
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
    diagnostics: Vec<Diagnostic>,
}

impl<'w> Assembler<'w> {
    fn new(world: &'w World, input: &'w TopologyInput, options: &BuildOptions) -> Self {
        let mut index = HashMap::new();
        for kind in SourceKind::ALL {
            for facts in world.of(kind) {
                let meta = &facts.meta;
                index.insert(
                    (
                        kind,
                        meta.namespace.as_deref().unwrap_or_default(),
                        meta.name.as_str(),
                    ),
                    &**facts,
                );
            }
        }
        Self {
            world,
            input,
            detail: options.detail,
            include_unreferenced: options.include_unreferenced,
            index,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    fn get(&self, kind: SourceKind, namespace: Option<&str>, name: &str) -> Option<&'w Facts> {
        self.index
            .get(&(kind, namespace.unwrap_or_default(), name))
            .copied()
    }

    fn presence(&self, kind: SourceKind, namespace: Option<&str>, name: &str) -> Presence {
        if self.get(kind, namespace, name).is_some() {
            Presence::Present
        } else if self.input.is_complete(kind) {
            Presence::Missing
        } else {
            Presence::Unknown
        }
    }

    fn run(mut self) -> TopologyGraph {
        let pod_display = self.workloads_and_pods();
        self.services(&pod_display);
        self.ingresses();
        self.gateways();
        self.routes();
        let world = self.world;
        if self.include_unreferenced {
            for kind in [
                SourceKind::ConfigMap,
                SourceKind::Secret,
                SourceKind::PersistentVolumeClaim,
                SourceKind::ServiceAccount,
            ] {
                for facts in world.of(kind) {
                    self.named(kind, facts.meta.namespace.as_deref(), &facts.meta.name);
                }
            }
        }
        let mut edges: Vec<Edge> = self.edges.into_values().collect();
        for edge in &mut edges {
            edge.attrs.refs.sort();
            edge.attrs.refs.dedup();
        }
        TopologyGraph::new(self.nodes.into_values().collect(), edges, self.diagnostics)
    }

    // --- nodes and edges -------------------------------------------------

    fn edge(&mut self, kind: EdgeKind, from: &NodeId, to: &NodeId) -> &mut EdgeAttrs {
        let id = EdgeId::new(kind, from, to);
        &mut self
            .edges
            .entry(id.clone())
            .or_insert_with(|| Edge {
                id,
                from: from.clone(),
                to: to.clone(),
                kind,
                attrs: EdgeAttrs::default(),
            })
            .attrs
    }

    fn route(&mut self, from: &NodeId, to: &NodeId, rule: RouteRule) {
        let routes = &mut self.edge(EdgeKind::RoutesTo, from, to).routes;
        if !routes.contains(&rule) {
            routes.push(rule);
        }
    }

    fn diagnose(&mut self, node: &NodeId, target: Option<&NodeId>, kind: DiagnosticKind) {
        self.diagnostics.push(Diagnostic {
            node: node.clone(),
            target: target.cloned(),
            kind,
        });
    }

    fn object_node(&mut self, kind: SourceKind, facts: &Facts) -> NodeId {
        let meta = &facts.meta;
        let id = id_of(kind, meta.namespace.as_deref(), &meta.name);
        if !self.nodes.contains_key(&id) {
            self.nodes.insert(
                id.clone(),
                Node {
                    id: id.clone(),
                    kind: node_kind(kind, Some(facts)),
                    namespace: meta.namespace.clone(),
                    name: meta.name.clone(),
                    api_kind: kind.kind().into(),
                    api_group: kind.group().into(),
                    presence: Presence::Present,
                    labels: meta.labels.clone(),
                    attrs: NodeAttrs::default(),
                },
            );
            self.owner_edge(&id, meta);
        }
        id
    }

    /// The node of an object referenced by name, created as present,
    /// missing or unknown.
    fn named(
        &mut self,
        kind: SourceKind,
        namespace: Option<&str>,
        name: &str,
    ) -> (NodeId, Presence) {
        if let Some(facts) = self.get(kind, namespace, name) {
            return (self.object_node(kind, facts), Presence::Present);
        }
        let id = id_of(kind, namespace, name);
        let presence = self.presence(kind, namespace, name);
        self.nodes.entry(id.clone()).or_insert_with(|| Node {
            id: id.clone(),
            kind: node_kind(kind, None),
            namespace: namespace.map(str::to_owned),
            name: name.to_owned(),
            api_kind: kind.kind().into(),
            api_group: kind.group().into(),
            presence,
            labels: BTreeMap::new(),
            attrs: NodeAttrs::default(),
        });
        (id, presence)
    }

    fn external(&mut self, address: &str) -> NodeId {
        let id = NodeId::external(address);
        self.nodes.entry(id.clone()).or_insert_with(|| Node {
            id: id.clone(),
            kind: NodeKind::External(if address.parse::<IpAddr>().is_ok() {
                ExternalKind::Ip
            } else {
                ExternalKind::Hostname
            }),
            namespace: None,
            name: address.to_owned(),
            api_kind: String::new(),
            api_group: String::new(),
            presence: Presence::Present,
            labels: BTreeMap::new(),
            attrs: NodeAttrs::default(),
        });
        id
    }

    /// `Owns` from the node's controller, if it is in the graph already
    /// (no placeholder is made for owners of non-pods).
    fn owner_edge(&mut self, id: &NodeId, meta: &Meta) {
        let Some(owner) = &meta.owner else {
            return;
        };
        if let Some(target) = self.owner_target(meta.namespace.as_deref(), owner, None, 0)
            && &target.id != id
            && self.nodes.contains_key(&target.id)
        {
            self.edge(EdgeKind::Owns, &target.id, id);
        }
    }

    /// The node a controller owner stands for: ReplicaSets and Jobs with a
    /// controller of their own collapse into it. `None` for owners that are
    /// not workloads (a Node owns static pods).
    fn owner_target(
        &self,
        namespace: Option<&str>,
        owner: &OwnerRef,
        pod_hash: Option<&str>,
        depth: u8,
    ) -> Option<OwnerTarget> {
        if owner.group.is_empty() && owner.kind == "Node" {
            return None;
        }
        let source = SourceKind::from_api(&owner.group, &owner.kind);
        if depth < 4 && matches!(source, Some(SourceKind::ReplicaSet | SourceKind::Job)) {
            let kind = source.unwrap_or(SourceKind::ReplicaSet);
            if let Some(facts) = self.get(kind, namespace, &owner.name) {
                if let Some(parent) = &facts.meta.owner
                    && let Some(target) = self.owner_target(namespace, parent, None, depth + 1)
                {
                    return Some(target);
                }
            } else if kind == SourceKind::ReplicaSet
                && let Some(hash) = pod_hash
                && let Some(deployment) = owner.name.strip_suffix(&format!("-{hash}"))
            {
                // ReplicaSets not loaded: `<deployment>-<pod-template-hash>`.
                return Some(OwnerTarget::of(
                    namespace,
                    &OwnerRef {
                        group: "apps".into(),
                        kind: "Deployment".into(),
                        name: deployment.into(),
                    },
                ));
            }
        }
        Some(OwnerTarget::of(namespace, owner))
    }

    /// A workload node for an owner that is not among the objects.
    fn owner_node(&mut self, target: &OwnerTarget, namespace: Option<&str>) {
        if self.nodes.contains_key(&target.id) {
            return;
        }
        let source = SourceKind::from_api(&target.group, &target.kind);
        if let Some(source) = source
            && let Some(facts) = self.get(source, namespace, &target.name)
        {
            self.object_node(source, facts);
            return;
        }
        let presence = match source {
            Some(source) if self.input.is_complete(source) => Presence::Missing,
            _ => Presence::Unknown,
        };
        self.nodes.insert(
            target.id.clone(),
            Node {
                id: target.id.clone(),
                kind: NodeKind::Workload(WorkloadKind::from_api(&target.group, &target.kind)),
                namespace: namespace.map(str::to_owned),
                name: target.name.clone(),
                api_kind: target.kind.clone(),
                api_group: target.group.clone(),
                presence,
                labels: BTreeMap::new(),
                attrs: NodeAttrs::default(),
            },
        );
    }

    /// Edges from a pod spec to what it uses.
    fn spec_refs(&mut self, from: &NodeId, namespace: Option<&str>, spec: &SpecFacts) {
        for reference in &spec.refs {
            self.reference(
                from,
                reference.target,
                namespace,
                &reference.name,
                reference.edge,
                reference.use_.clone(),
            );
        }
        if let Some(account) = &spec.service_account {
            let (id, presence) = self.named(SourceKind::ServiceAccount, namespace, account);
            self.edge(EdgeKind::UsesServiceAccount, from, &id);
            if presence == Presence::Missing {
                self.diagnose(from, Some(&id), DiagnosticKind::MissingTarget);
            }
        }
    }

    fn reference(
        &mut self,
        from: &NodeId,
        target: RefTarget,
        namespace: Option<&str>,
        name: &str,
        edge: EdgeKind,
        use_: RefUse,
    ) {
        let optional = use_.optional;
        let (id, presence) = self.named(target.source(), namespace, name);
        self.edge(edge, from, &id).refs.push(use_);
        if presence == Presence::Missing && !optional {
            self.diagnose(from, Some(&id), DiagnosticKind::MissingTarget);
        }
    }

    // --- workloads and pods ----------------------------------------------

    /// Creates workload and pod nodes; returns, per pod (in input order),
    /// the node it is drawn as.
    fn workloads_and_pods(&mut self) -> Vec<NodeId> {
        let world = self.world;
        // Workload nodes with the facts of their pod template.
        let mut templates: Vec<(NodeId, Option<&str>, &'w WorkloadFacts)> = Vec::new();
        for kind in WORKLOADS {
            for facts in world.of(kind) {
                let Body::Workload(workload) = &facts.body else {
                    continue;
                };
                let namespace = facts.meta.namespace.as_deref();
                let own = id_of(kind, namespace, &facts.meta.name);
                let collapsed = matches!(kind, SourceKind::ReplicaSet | SourceKind::Job)
                    .then(|| facts.meta.owner.as_ref())
                    .flatten()
                    .and_then(|owner| self.owner_target(namespace, owner, None, 0))
                    .filter(|target| target.id != own);
                match collapsed {
                    Some(target) => {
                        // A controller we only know from ownerReferences takes
                        // its template from the ReplicaSet or Job.
                        let known = self.nodes.contains_key(&target.id);
                        self.owner_node(&target, namespace);
                        if !known && self.nodes[&target.id].presence != Presence::Present {
                            let node = self.nodes.get_mut(&target.id).expect("just added");
                            if node.attrs.container_ports.is_empty() {
                                node.attrs.container_ports = workload.spec.ports.clone();
                            }
                            if node.attrs.pod_labels.is_empty() {
                                node.attrs.pod_labels = workload
                                    .template_labels
                                    .iter()
                                    .filter(|(key, _)| key.as_str() != POD_TEMPLATE_HASH)
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect();
                            }
                            templates.push((target.id, namespace, workload));
                        }
                    }
                    None => {
                        let id = self.object_node(kind, facts);
                        let node = self.nodes.get_mut(&id).expect("just added");
                        node.attrs.pod_labels = workload.template_labels.clone();
                        node.attrs.container_ports = workload.spec.ports.clone();
                        node.attrs.desired_replicas = workload.replicas;
                        templates.push((id, namespace, workload));
                    }
                }
            }
        }

        let mut counts: HashMap<NodeId, PodCounts> = HashMap::new();
        let mut with_pod_nodes: HashSet<NodeId> = HashSet::new();
        let mut display = Vec::with_capacity(world.of(SourceKind::Pod).len());
        for facts in world.of(SourceKind::Pod) {
            let Body::Pod(pod) = &facts.body else {
                display.push(id_of(SourceKind::Pod, None, ""));
                continue;
            };
            let meta = &facts.meta;
            let namespace = meta.namespace.as_deref();
            let hash = meta.labels.get(POD_TEMPLATE_HASH).map(String::as_str);
            let owner = meta
                .owner
                .as_ref()
                .and_then(|owner| self.owner_target(namespace, owner, hash, 0));
            if let Some(owner) = &owner {
                self.owner_node(owner, namespace);
                let count = counts.entry(owner.id.clone()).or_default();
                count.total += 1;
                count.ready += u32::from(pod.ready);
            }
            let shown = match (self.detail, owner) {
                (Detail::Workloads, Some(owner)) => owner.id,
                (_, owner) => {
                    let id = self.object_node(SourceKind::Pod, facts);
                    let node = self.nodes.get_mut(&id).expect("just added");
                    node.attrs.pod_labels = meta.labels.clone();
                    node.attrs.container_ports = pod.spec.ports.clone();
                    node.attrs.phase = pod.phase.clone();
                    node.attrs.ready = Some(pod.ready);
                    if let Some(owner) = owner {
                        self.edge(EdgeKind::Owns, &owner.id, &id);
                        with_pod_nodes.insert(owner.id);
                    }
                    id
                }
            };
            self.spec_refs(&shown, namespace, &pod.spec);
            display.push(shown);
        }

        let pods_known =
            self.input.is_complete(SourceKind::Pod) || !world.of(SourceKind::Pod).is_empty();
        for (id, namespace, workload) in templates {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.attrs.pods = if pods_known {
                    Some(counts.get(&id).copied().unwrap_or_default())
                } else {
                    workload.replicas.map(|total| PodCounts {
                        total: u32::try_from(total).unwrap_or_default(),
                        ready: workload
                            .ready_replicas
                            .and_then(|ready| u32::try_from(ready).ok())
                            .unwrap_or_default(),
                    })
                };
            }
            if self.detail == Detail::Pods && with_pod_nodes.contains(&id) {
                continue;
            }
            self.spec_refs(&id, namespace, &workload.spec);
            self.claim_templates(&id, namespace, workload);
        }
        // Placeholders (orphans' missing owners) still get their counts.
        for (id, count) in counts {
            if let Some(node) = self.nodes.get_mut(&id)
                && node.attrs.pods.is_none()
            {
                node.attrs.pods = Some(count);
            }
        }
        display
    }

    /// StatefulSet → PVCs made from its `volumeClaimTemplates`
    /// (`<template>-<set>-<ordinal>`).
    fn claim_templates(&mut self, id: &NodeId, namespace: Option<&str>, workload: &WorkloadFacts) {
        if workload.source != SourceKind::StatefulSet || workload.claim_templates.is_empty() {
            return;
        }
        let Some(set) = self.nodes.get(id).map(|node| node.name.clone()) else {
            return;
        };
        let world = self.world;
        for claim in world.of(SourceKind::PersistentVolumeClaim) {
            if claim.meta.namespace.as_deref() != namespace {
                continue;
            }
            for template in &workload.claim_templates {
                let ordinal = claim
                    .meta
                    .name
                    .strip_prefix(&format!("{template}-{set}-"))
                    .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
                if ordinal.is_some() {
                    let claim_id = self.object_node(SourceKind::PersistentVolumeClaim, claim);
                    self.edge(EdgeKind::Mounts, id, &claim_id)
                        .refs
                        .push(RefUse {
                            via: RefVia::ClaimTemplate,
                            at: format!("volumeClaimTemplate {template}"),
                            key: None,
                            optional: false,
                        });
                }
            }
        }
    }

    // --- services --------------------------------------------------------

    fn services(&mut self, pod_display: &[NodeId]) {
        let world = self.world;
        let pods = world.of(SourceKind::Pod);
        // (namespace, key, value) → pods with that label.
        let mut by_label: HashMap<(&str, &str, &str), Vec<usize>> = HashMap::new();
        let mut by_name: HashMap<(&str, &str), usize> = HashMap::new();
        for (ix, facts) in pods.iter().enumerate() {
            let namespace = facts.meta.namespace.as_deref().unwrap_or_default();
            by_name.insert((namespace, facts.meta.name.as_str()), ix);
            for (key, value) in &facts.meta.labels {
                by_label
                    .entry((namespace, key.as_str(), value.as_str()))
                    .or_default()
                    .push(ix);
            }
        }
        let mut slices: HashMap<(&str, &str), Vec<&extract::SliceFacts>> = HashMap::new();
        for facts in world.of(SourceKind::EndpointSlice) {
            if let Body::EndpointSlice(slice) = &facts.body
                && let Some(service) = &slice.service
            {
                slices
                    .entry((
                        facts.meta.namespace.as_deref().unwrap_or_default(),
                        service.as_str(),
                    ))
                    .or_default()
                    .push(slice);
            }
        }
        // Workload nodes by namespace, for selectors that match templates.
        let mut workloads: HashMap<Option<String>, Vec<(NodeId, Labels)>> = HashMap::new();
        for node in self.nodes.values() {
            if matches!(node.kind, NodeKind::Workload(_)) && !node.attrs.pod_labels.is_empty() {
                workloads
                    .entry(node.namespace.clone())
                    .or_default()
                    .push((node.id.clone(), node.attrs.pod_labels.clone()));
            }
        }
        let pods_complete = self.input.is_complete(SourceKind::Pod);

        for facts in world.of(SourceKind::Service) {
            let Body::Service(service) = &facts.body else {
                continue;
            };
            let id = self.object_node(SourceKind::Service, facts);
            if let Some(node) = self.nodes.get_mut(&id) {
                node.attrs.service = Some(ServiceInfo {
                    cluster_ip: service.cluster_ip.clone(),
                    ports: service.ports.clone(),
                    external_name: service.external_name.clone(),
                });
            }
            let namespace = facts.meta.namespace.as_deref().unwrap_or_default();
            let service_slices = slices
                .get(&(namespace, facts.meta.name.as_str()))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut endpoints: BTreeMap<NodeId, EndpointCounts> = BTreeMap::new();
            let mut count = |target: NodeId, ready: bool| {
                let counts = endpoints.entry(target).or_default();
                if ready {
                    counts.ready += 1;
                } else {
                    counts.not_ready += 1;
                }
            };
            if let Some(selector) = &service.selector {
                // Readiness as the EndpointSlices see it, if there are any.
                let slice_ready: Option<HashMap<&str, bool>> =
                    (!service_slices.is_empty()).then(|| {
                        service_slices
                            .iter()
                            .flat_map(|slice| &slice.endpoints)
                            .filter_map(|endpoint| Some((endpoint.pod.as_deref()?, endpoint.ready)))
                            .collect()
                    });
                let candidates = selector
                    .iter()
                    .map(|(key, value)| {
                        by_label
                            .get(&(namespace, key.as_str(), value.as_str()))
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                    })
                    .min_by_key(|candidates| candidates.len())
                    .unwrap_or_default();
                let mut matched = 0;
                for &ix in candidates {
                    let pod = &pods[ix];
                    if !selector
                        .iter()
                        .all(|(key, value)| pod.meta.labels.get(key) == Some(value))
                    {
                        continue;
                    }
                    matched += 1;
                    let ready = match (&slice_ready, &pod.body) {
                        (Some(ready), _) => {
                            ready.get(pod.meta.name.as_str()).copied().unwrap_or(false)
                        }
                        (None, Body::Pod(pod)) => pod.ready,
                        _ => false,
                    };
                    count(pod_display[ix].clone(), ready);
                }
                let mut template_matches = 0;
                for (workload, labels) in workloads
                    .get(&facts.meta.namespace)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    if selector
                        .iter()
                        .all(|(key, value)| labels.get(key) == Some(value))
                    {
                        template_matches += 1;
                        // Without pods of its own on the graph (scaled to zero,
                        // or pods collapsed into it), the workload is the target.
                        if self.detail == Detail::Workloads
                            || !endpoints.keys().any(|id| {
                                self.edges
                                    .contains_key(&EdgeId::new(EdgeKind::Owns, workload, id))
                            })
                        {
                            endpoints.entry(workload.clone()).or_default();
                        }
                    }
                }
                let ready: u32 = endpoints.values().map(|counts| counts.ready).sum();
                let not_ready: u32 = endpoints.values().map(|counts| counts.not_ready).sum();
                if matched == 0 && template_matches == 0 {
                    if pods_complete {
                        self.diagnose(&id, None, DiagnosticKind::SelectorMatchesNothing);
                    }
                } else if ready == 0 && (pods_complete || matched > 0) {
                    self.diagnose(&id, None, DiagnosticKind::NoReadyEndpoints { not_ready });
                }
            } else if let Some(name) = &service.external_name {
                let external = self.external(name);
                self.edge(EdgeKind::Exposes, &id, &external);
            } else {
                // Endpoints managed by hand: whatever the slices point at.
                for endpoint in service_slices.iter().flat_map(|slice| &slice.endpoints) {
                    match endpoint
                        .pod
                        .as_deref()
                        .and_then(|pod| by_name.get(&(namespace, pod)))
                    {
                        Some(&ix) => count(pod_display[ix].clone(), endpoint.ready),
                        None if endpoint.pod.is_none() => {
                            for address in &endpoint.addresses {
                                let external = self.external(address);
                                count(external, endpoint.ready);
                            }
                        }
                        None => {}
                    }
                }
            }
            for (target, counts) in endpoints {
                self.edge(EdgeKind::Selects, &id, &target).endpoints = Some(counts);
            }
            for address in &service.load_balancer {
                let external = self.external(address);
                self.edge(EdgeKind::Exposes, &id, &external);
            }
        }
    }

    /// Checks a backend port against the Service's ports.
    fn check_port(&mut self, from: &NodeId, service: &NodeId, port: Option<&PortRef>) {
        let Some(port) = port else {
            return;
        };
        let Some(info) = self
            .nodes
            .get(service)
            .and_then(|node| node.attrs.service.as_ref())
        else {
            return;
        };
        let found = info.ports.iter().any(|candidate| match port {
            PortRef::Number(number) => candidate.port == *number,
            PortRef::Name(name) => candidate.name.as_deref() == Some(name),
        });
        if !found {
            self.diagnose(
                from,
                Some(service),
                DiagnosticKind::MissingPort { port: port.clone() },
            );
        }
    }

    fn backend(&mut self, from: &NodeId, namespace: Option<&str>, backend: &extract::Backend) {
        let namespace = backend.namespace.as_deref().or(namespace);
        let (service, presence) = self.named(SourceKind::Service, namespace, &backend.service);
        self.route(from, &service, backend.rule.clone());
        match presence {
            Presence::Missing => self.diagnose(from, Some(&service), DiagnosticKind::MissingTarget),
            Presence::Present => self.check_port(from, &service, backend.rule.port.as_ref()),
            Presence::Unknown => {}
        }
    }

    fn tls(&mut self, from: &NodeId, namespace: Option<&str>, secret: &str) {
        let (id, presence) = self.named(SourceKind::Secret, namespace, secret);
        self.edge(EdgeKind::Tls, from, &id);
        if presence == Presence::Missing {
            self.diagnose(from, Some(&id), DiagnosticKind::MissingTarget);
        }
    }

    // --- ingresses and Gateway API ---------------------------------------

    fn ingresses(&mut self) {
        let world = self.world;
        for facts in world.of(SourceKind::Ingress) {
            let Body::Ingress(ingress) = &facts.body else {
                continue;
            };
            let namespace = facts.meta.namespace.as_deref();
            let id = self.object_node(SourceKind::Ingress, facts);
            if let Some(node) = self.nodes.get_mut(&id) {
                node.attrs.hosts = ingress.hosts.clone();
                node.attrs.class_name = ingress.class.clone();
            }
            for backend in &ingress.backends {
                self.backend(&id, namespace, backend);
            }
            for secret in &ingress.tls_secrets {
                self.tls(&id, namespace, secret);
            }
            for address in &ingress.load_balancer {
                let external = self.external(address);
                self.edge(EdgeKind::Exposes, &id, &external);
            }
        }
    }

    fn gateways(&mut self) {
        let world = self.world;
        for facts in world.of(SourceKind::Gateway) {
            let Body::Gateway(gateway) = &facts.body else {
                continue;
            };
            let namespace = facts.meta.namespace.as_deref();
            let id = self.object_node(SourceKind::Gateway, facts);
            if let Some(node) = self.nodes.get_mut(&id) {
                node.attrs.hosts = gateway.hosts.clone();
                node.attrs.class_name = gateway.class.clone();
            }
            for (secret_namespace, secret) in &gateway.certificates {
                self.tls(&id, secret_namespace.as_deref().or(namespace), secret);
            }
            for address in &gateway.addresses {
                let external = self.external(address);
                self.edge(EdgeKind::Exposes, &id, &external);
            }
        }
    }

    fn routes(&mut self) {
        let world = self.world;
        for kind in [
            SourceKind::HttpRoute,
            SourceKind::GrpcRoute,
            SourceKind::TlsRoute,
        ] {
            for facts in world.of(kind) {
                let Body::Route(route) = &facts.body else {
                    continue;
                };
                let namespace = facts.meta.namespace.as_deref();
                let id = self.object_node(kind, facts);
                if let Some(node) = self.nodes.get_mut(&id) {
                    node.attrs.hosts = route.hosts.clone();
                }
                for parent in &route.parents {
                    let (gateway, presence) = self.named(
                        SourceKind::Gateway,
                        parent.namespace.as_deref().or(namespace),
                        &parent.name,
                    );
                    self.route(
                        &gateway,
                        &id,
                        RouteRule {
                            hosts: route.hosts.clone(),
                            path: None,
                            port: parent.port.map(PortRef::Number),
                            section: parent.section.clone(),
                        },
                    );
                    if presence == Presence::Missing {
                        self.diagnose(&id, Some(&gateway), DiagnosticKind::MissingTarget);
                    }
                }
                for backend in &route.backends {
                    self.backend(&id, namespace, backend);
                }
            }
        }
    }
}

fn apply_filter(graph: TopologyGraph, filter: &Filter) -> TopologyGraph {
    let mut graph = graph;
    let mut narrowed = false;
    if let Some(namespaces) = &filter.namespaces {
        graph = graph.retain(|node| {
            node.namespace
                .as_ref()
                .is_none_or(|namespace| namespaces.contains(namespace))
        });
        narrowed = true;
    }
    if let Some(kinds) = &filter.kinds {
        graph = graph.retain(|node| kinds.contains(&node.kind.class()));
        narrowed = true;
    }
    if let Some(selector) = &filter.labels {
        let seeds: Vec<NodeId> = graph
            .nodes()
            .iter()
            .filter(|node| {
                matches!(node.kind, NodeKind::Pod | NodeKind::Workload(_))
                    && selector.matches(&node.attrs.pod_labels)
                    || !node.labels.is_empty() && selector.matches(&node.labels)
            })
            .map(|node| node.id.clone())
            .collect();
        let keep = graph.neighborhood(&seeds);
        graph = graph.retain(|node| keep.contains(&node.id));
        narrowed = true;
    }
    if narrowed {
        // External addresses nothing links to any more.
        let linked: HashSet<NodeId> = graph
            .edges()
            .iter()
            .flat_map(|edge| [edge.from.clone(), edge.to.clone()])
            .collect();
        graph = graph.retain(|node| {
            !matches!(node.kind, NodeKind::External(_)) || linked.contains(&node.id)
        });
    }
    graph
}
