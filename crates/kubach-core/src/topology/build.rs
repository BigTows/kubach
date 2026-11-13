//! Snapshot → graph: joins the facts of every object into nodes, edges and
//! diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use kube::api::DynamicObject;

use super::extract::{self, Body, Facts, Meta, OwnerRef, RefTarget, SpecFacts, WorkloadFacts};
use super::model::{
    Diagnostic, DiagnosticKind, Edge, EdgeAttrs, EdgeId, EdgeKind, EndpointCounts, ExternalKind,
    Node, NodeAttrs, NodeClass, NodeId, NodeKind, PodCounts, PortRef, Presence, RefUse, RefVia,
    RouteRule, ServiceInfo, ServiceType, TopologyGraph, WorkloadKind,
};
use super::netpol::PolicySet;
use super::source::{SourceKind, TopologyInput};
use crate::cache::Objects;

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

/// Rebuilds a graph cheaply as the input changes.
///
/// - Nothing changed (every list is the one of the last build, with the
///   same options): the last graph is handed back, not built again.
/// - A list that did not change is not looked at; in one that did, an
///   object is read again only when it is another object with another
///   `resourceVersion`.
/// - The join of the facts runs whenever anything changed.
#[derive(Default)]
pub struct TopologyBuilder {
    world: World,
    /// The last graph with its options: the answer while nothing changes.
    last: Option<(BuildOptions, TopologyGraph)>,
    policies: Option<ReadPolicies>,
    clock: Clock,
}

/// A kind as last read: its list and the facts of its objects, in order.
struct Read {
    /// `None`: the input had no list of the kind.
    objects: Option<Objects>,
    complete: bool,
    facts: Vec<Arc<Facts>>,
}

/// The policy set with the lists it was parsed from.
struct ReadPolicies {
    policies: Option<Objects>,
    namespaces: Option<Objects>,
    namespaces_complete: bool,
    set: Arc<PolicySet>,
}

/// Whether a list is the one read before: the same allocation, which the
/// builder keeps alive, so its address is not reused. A watch hands out
/// the same list until its objects change.
fn same_list(old: Option<&Objects>, new: Option<&Objects>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) if Arc::ptr_eq(old, new) => true,
        _ => old.is_none_or(|old| old.is_empty()) && new.is_none_or(|new| new.is_empty()),
    }
}

fn key_of(object: &DynamicObject) -> (&str, &str) {
    (
        object.metadata.namespace.as_deref().unwrap_or_default(),
        object.metadata.name.as_deref().unwrap_or_default(),
    )
}

/// Facts of `objects`, taking those of `old` for the objects it has: the
/// same object, or one of the same namespace, name and `resourceVersion`.
fn read_list(
    kind: SourceKind,
    old: Option<&Read>,
    objects: &[Arc<DynamicObject>],
) -> Vec<Arc<Facts>> {
    let (old_objects, old_facts): (&[Arc<DynamicObject>], &[Arc<Facts>]) = match old {
        Some(read) => (read.objects.as_deref().unwrap_or_default(), &read.facts),
        None => (&[], &[]),
    };
    // Lists mostly differ in a few places: the old one is walked alongside,
    // and looked up by name only where the two part.
    let mut by_key: Option<HashMap<(&str, &str), usize>> = None;
    let mut cursor = 0;
    objects
        .iter()
        .map(|object| {
            if old_objects
                .get(cursor)
                .is_some_and(|old| Arc::ptr_eq(old, object))
            {
                cursor += 1;
                return Arc::clone(&old_facts[cursor - 1]);
            }
            let by_key = by_key.get_or_insert_with(|| {
                old_objects
                    .iter()
                    .enumerate()
                    .map(|(ix, old)| (key_of(old), ix))
                    .collect()
            });
            if let Some(&ix) = by_key.get(&key_of(object)) {
                cursor = ix + 1;
                let old = &old_objects[ix];
                let same_version = matches!(
                    (&old.metadata.resource_version, &object.metadata.resource_version),
                    (Some(old), Some(new)) if old == new
                );
                if same_version || Arc::ptr_eq(old, object) {
                    return Arc::clone(&old_facts[ix]);
                }
            }
            Arc::new(extract::facts(kind, object))
        })
        .collect()
}

/// Time per phase of a build, measured only with the `perf` feature.
#[derive(Default)]
struct Clock {
    #[cfg(feature = "perf")]
    at: Option<std::time::Instant>,
    #[cfg(feature = "perf")]
    laps: Vec<(&'static str, std::time::Duration)>,
}

impl Clock {
    fn start(&mut self) {
        #[cfg(feature = "perf")]
        {
            self.laps.clear();
            self.at = Some(std::time::Instant::now());
        }
    }

    /// Ends the phase `_name`: the time since the previous one.
    fn lap(&mut self, _name: &'static str) {
        #[cfg(feature = "perf")]
        {
            let now = std::time::Instant::now();
            if let Some(at) = self.at.replace(now) {
                self.laps.push((_name, now - at));
            }
        }
    }
}

impl TopologyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Phases of the last [`build`](Self::build) with the time each took.
    #[cfg(feature = "perf")]
    pub fn phases(&self) -> &[(&'static str, std::time::Duration)] {
        &self.clock.laps
    }

    pub fn build(&mut self, input: &TopologyInput, options: &BuildOptions) -> TopologyGraph {
        self.build_with_policies(input, options).0
    }

    /// [`build`](Self::build), with the policy set the network policy
    /// layer came from (`None` unless [`BuildOptions::network_policies`]):
    /// parsed once, and again only when NetworkPolicies or Namespaces change.
    pub fn build_with_policies(
        &mut self,
        input: &TopologyInput,
        options: &BuildOptions,
    ) -> (TopologyGraph, Option<Arc<PolicySet>>) {
        self.clock.start();
        let mut changed = false;
        for kind in SourceKind::ALL {
            changed |= self.world.read(kind, input);
        }
        let policies = options
            .network_policies
            .then(|| self.policy_set(input, &mut changed));
        self.clock.lap("facts");
        if !changed
            && let Some((built, graph)) = &self.last
            && built == options
        {
            return (graph.clone(), policies);
        }
        let graph = Assembler::new(&self.world, input, options).run(&mut self.clock);
        let graph = apply_filter(graph, &options.filter);
        self.clock.lap("filter");
        let graph = match &policies {
            Some(policies) => super::netpol::apply(graph, policies),
            None => graph,
        };
        self.clock.lap("network policies");
        self.last = Some((options.clone(), graph.clone()));
        (graph, policies)
    }

    fn policy_set(&mut self, input: &TopologyInput, changed: &mut bool) -> Arc<PolicySet> {
        let policies = input.list(SourceKind::NetworkPolicy);
        let namespaces = input.list(SourceKind::Namespace);
        let namespaces_complete = input.is_complete(SourceKind::Namespace);
        if let Some(read) = &self.policies
            && same_list(read.policies.as_ref(), policies)
            && same_list(read.namespaces.as_ref(), namespaces)
            && read.namespaces_complete == namespaces_complete
        {
            return Arc::clone(&read.set);
        }
        *changed = true;
        let set = Arc::new(PolicySet::from_input(input));
        self.policies = Some(ReadPolicies {
            policies: policies.cloned(),
            namespaces: namespaces.cloned(),
            namespaces_complete,
            set: Arc::clone(&set),
        });
        set
    }
}

#[derive(Default)]
struct World {
    kinds: HashMap<SourceKind, Read>,
}

impl World {
    fn of(&self, kind: SourceKind) -> &[Arc<Facts>] {
        self.kinds
            .get(&kind)
            .map_or(&[], |read| read.facts.as_slice())
    }

    /// Takes the list of `kind` from `input`; whether anything the join
    /// reads of it changed.
    fn read(&mut self, kind: SourceKind, input: &TopologyInput) -> bool {
        let objects = input.list(kind);
        let complete = input.is_complete(kind);
        if let Some(read) = self.kinds.get_mut(&kind)
            && same_list(read.objects.as_ref(), objects)
        {
            let changed = read.complete != complete;
            read.complete = complete;
            return changed;
        }
        let old = self.kinds.remove(&kind);
        let facts = read_list(kind, old.as_ref(), objects.map_or(&[], |objects| objects));
        self.kinds.insert(
            kind,
            Read {
                objects: objects.cloned(),
                complete,
                facts,
            },
        );
        true
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
struct OwnerTarget<'w> {
    id: NodeId,
    group: &'w str,
    kind: &'w str,
    name: &'w str,
}

impl<'w> OwnerTarget<'w> {
    fn of(namespace: Option<&str>, group: &'w str, kind: &'w str, name: &'w str) -> Self {
        Self {
            id: NodeId::object(kind, group, namespace, name),
            group,
            kind,
            name,
        }
    }
}

/// Whether two pod specs point at the same objects in the same ways:
/// [`Assembler::spec_refs`] of one adds nothing to that of the other.
fn same_refs(a: &SpecFacts, b: &SpecFacts) -> bool {
    std::ptr::eq(a, b) || a.service_account == b.service_account && a.refs == b.refs
}

type Key<'w> = (SourceKind, &'w str, &'w str);

type Labels = BTreeMap<String, String>;

/// PVCs by namespace and name without the ordinal, as positions in their list.
type Claims<'w> = HashMap<(Option<&'w str>, &'w str), Vec<usize>>;

struct Assembler<'w> {
    world: &'w World,
    input: &'w TopologyInput,
    detail: Detail,
    include_unreferenced: bool,
    index: HashMap<Key<'w>, &'w Facts>,
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
    diagnostics: Vec<Diagnostic>,
    /// What [`named`](Self::named) answered, by what it was asked: the
    /// pods of an app ask for the same ConfigMaps and Secrets.
    named: HashMap<Key<'w>, (NodeId, Presence)>,
    /// The edge asked for last: several uses of one object by one pod
    /// (a Secret in a few env variables) come in a row.
    last_edge: Option<(EdgeKind, NodeId, NodeId, EdgeId)>,
    /// PVCs named `<prefix>-<ordinal>` by namespace and prefix, as
    /// positions in the PVC list; made for the first StatefulSet with
    /// `volumeClaimTemplates`.
    claims: Option<Claims<'w>>,
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
            named: HashMap::new(),
            last_edge: None,
            claims: None,
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

    fn run(mut self, clock: &mut Clock) -> TopologyGraph {
        clock.lap("index");
        let pod_display = self.workloads_and_pods();
        clock.lap("workloads and pods");
        self.services(&pod_display);
        clock.lap("services");
        self.ingresses();
        self.gateways();
        self.routes();
        clock.lap("ingresses and routes");
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
        clock.lap("edges");
        let graph = TopologyGraph::new(self.nodes.into_values().collect(), edges, self.diagnostics);
        clock.lap("graph");
        graph
    }

    // --- nodes and edges -------------------------------------------------

    fn edge(&mut self, kind: EdgeKind, from: &NodeId, to: &NodeId) -> &mut EdgeAttrs {
        let id = match &self.last_edge {
            Some((last_kind, last_from, last_to, id))
                if *last_kind == kind && last_from == from && last_to == to =>
            {
                id.clone()
            }
            _ => {
                let id = EdgeId::new(kind, from, to);
                self.last_edge = Some((kind, from.clone(), to.clone(), id.clone()));
                id
            }
        };
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

    fn object_node(&mut self, kind: SourceKind, facts: &'w Facts) -> NodeId {
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
        namespace: Option<&'w str>,
        name: &'w str,
    ) -> (NodeId, Presence) {
        // Asked before: the node is there and stays as it is.
        let key = (kind, namespace.unwrap_or_default(), name);
        if let Some(answer) = self.named.get(&key) {
            return answer.clone();
        }
        let answer = self.named_new(kind, namespace, name);
        self.named.insert(key, answer.clone());
        answer
    }

    fn named_new(
        &mut self,
        kind: SourceKind,
        namespace: Option<&'w str>,
        name: &'w str,
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
    fn owner_edge(&mut self, id: &NodeId, meta: &'w Meta) {
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
        owner: &'w OwnerRef,
        pod_hash: Option<&str>,
        depth: u8,
    ) -> Option<OwnerTarget<'w>> {
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
                return Some(OwnerTarget::of(namespace, "apps", "Deployment", deployment));
            }
        }
        Some(OwnerTarget::of(
            namespace,
            &owner.group,
            &owner.kind,
            &owner.name,
        ))
    }

    /// A workload node for an owner that is not among the objects.
    fn owner_node(&mut self, target: &OwnerTarget<'w>, namespace: Option<&'w str>) {
        if self.nodes.contains_key(&target.id) {
            return;
        }
        let source = SourceKind::from_api(target.group, target.kind);
        if let Some(source) = source
            && let Some(facts) = self.get(source, namespace, target.name)
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
                kind: NodeKind::Workload(WorkloadKind::from_api(target.group, target.kind)),
                namespace: namespace.map(str::to_owned),
                name: target.name.to_owned(),
                api_kind: target.kind.to_owned(),
                api_group: target.group.to_owned(),
                presence,
                labels: BTreeMap::new(),
                attrs: NodeAttrs::default(),
            },
        );
    }

    /// Edges from a pod spec to what it uses.
    fn spec_refs(&mut self, from: &NodeId, namespace: Option<&'w str>, spec: &'w SpecFacts) {
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
        namespace: Option<&'w str>,
        name: &'w str,
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
        let mut templates: Vec<(NodeId, Option<&'w str>, &'w WorkloadFacts)> = Vec::new();
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
        // The owner of the previous pod: pods of a ReplicaSet come in a row.
        type Owner<'w> = (Option<&'w str>, &'w OwnerRef, Option<&'w str>);
        let mut last_owner: Option<(Owner<'w>, Option<OwnerTarget<'w>>)> = None;
        // Specs already joined to a workload its pods are collapsed into.
        let mut joined: HashMap<NodeId, Vec<&'w SpecFacts>> = HashMap::new();
        for facts in world.of(SourceKind::Pod) {
            let Body::Pod(pod) = &facts.body else {
                display.push(id_of(SourceKind::Pod, None, ""));
                continue;
            };
            let meta = &facts.meta;
            let namespace = meta.namespace.as_deref();
            let hash = meta.labels.get(POD_TEMPLATE_HASH).map(String::as_str);
            let owner = meta.owner.as_ref().and_then(|owner| {
                let asked = (namespace, owner, hash);
                match &last_owner {
                    Some((last, target)) if *last == asked => target.clone(),
                    _ => {
                        let target = self.owner_target(namespace, owner, hash, 0);
                        last_owner = Some((asked, target.clone()));
                        target
                    }
                }
            });
            if let Some(owner) = &owner {
                self.owner_node(owner, namespace);
                let count = counts.entry(owner.id.clone()).or_default();
                count.total += 1;
                count.ready += u32::from(pod.ready);
            }
            let collapsed = self.detail == Detail::Workloads && owner.is_some();
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
            // Replicas have the same spec: joining it again would add the
            // same nodes, uses and diagnostics, which the graph has once.
            let again = collapsed && {
                let specs = joined.entry(shown.clone()).or_default();
                let again = specs.iter().any(|spec| same_refs(spec, &pod.spec));
                if !again {
                    specs.push(&pod.spec);
                }
                again
            };
            if !again {
                self.spec_refs(&shown, namespace, &pod.spec);
            }
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
        let all = world.of(SourceKind::PersistentVolumeClaim);
        let claims: &HashMap<_, _> = self.claims.get_or_insert_with(|| {
            let mut claims: Claims<'w> = HashMap::new();
            for (ix, claim) in all.iter().enumerate() {
                if let Some((prefix, ordinal)) = claim.meta.name.rsplit_once('-')
                    && !ordinal.is_empty()
                    && ordinal.bytes().all(|b| b.is_ascii_digit())
                {
                    claims
                        .entry((claim.meta.namespace.as_deref(), prefix))
                        .or_default()
                        .push(ix);
                }
            }
            claims
        });
        let mut found: Vec<(usize, &String)> = Vec::new();
        for template in &workload.claim_templates {
            let prefix = format!("{template}-{set}");
            if let Some(claims) = claims.get(&(namespace, prefix.as_str())) {
                found.extend(claims.iter().map(|&ix| (ix, template)));
            }
        }
        for (ix, template) in found {
            let claim_id = self.object_node(SourceKind::PersistentVolumeClaim, &all[ix]);
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

    // --- services --------------------------------------------------------

    fn services(&mut self, pod_display: &[NodeId]) {
        let world = self.world;
        let pods = world.of(SourceKind::Pod);
        // Label keys some Service selects by: only those are indexed.
        let mut keys: HashSet<&str> = HashSet::new();
        for facts in world.of(SourceKind::Service) {
            if let Body::Service(service) = &facts.body
                && let Some(selector) = &service.selector
            {
                keys.extend(selector.keys().map(String::as_str));
            }
        }
        // (namespace, key, value) → pods with that label.
        let mut by_label: HashMap<(&str, &str, &str), Vec<usize>> = HashMap::new();
        let mut by_name: HashMap<(&str, &str), usize> = HashMap::new();
        for (ix, facts) in pods.iter().enumerate() {
            let namespace = facts.meta.namespace.as_deref().unwrap_or_default();
            by_name.insert((namespace, facts.meta.name.as_str()), ix);
            for (key, value) in &facts.meta.labels {
                if !keys.contains(key.as_str()) {
                    continue;
                }
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
        // Workload nodes, for selectors that match templates: by namespace,
        // and by namespace and label (of the keys Services select by).
        let workloads: Vec<(Option<String>, NodeId, Labels)> = self
            .nodes
            .values()
            .filter(|node| {
                matches!(node.kind, NodeKind::Workload(_)) && !node.attrs.pod_labels.is_empty()
            })
            .map(|node| {
                (
                    node.namespace.clone(),
                    node.id.clone(),
                    node.attrs.pod_labels.clone(),
                )
            })
            .collect();
        let mut workloads_in: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        let mut workloads_by_label: HashMap<(Option<&str>, &str, &str), Vec<usize>> =
            HashMap::new();
        for (ix, (namespace, _, labels)) in workloads.iter().enumerate() {
            let namespace = namespace.as_deref();
            workloads_in.entry(namespace).or_default().push(ix);
            for (key, value) in labels {
                if keys.contains(key.as_str()) {
                    workloads_by_label
                        .entry((namespace, key.as_str(), value.as_str()))
                        .or_default()
                        .push(ix);
                }
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
                // An empty selector matches every workload of the namespace.
                let own = facts.meta.namespace.as_deref();
                let candidates = if selector.is_empty() {
                    workloads_in.get(&own)
                } else {
                    selector
                        .iter()
                        .map(|(key, value)| {
                            workloads_by_label.get(&(own, key.as_str(), value.as_str()))
                        })
                        .min_by_key(|candidates| candidates.map_or(0, Vec::len))
                        .flatten()
                };
                for &ix in candidates.map(Vec::as_slice).unwrap_or_default() {
                    let (_, workload, labels) = &workloads[ix];
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

    fn backend(
        &mut self,
        from: &NodeId,
        namespace: Option<&'w str>,
        backend: &'w extract::Backend,
    ) {
        let namespace = backend.namespace.as_deref().or(namespace);
        let (service, presence) = self.named(SourceKind::Service, namespace, &backend.service);
        self.route(from, &service, backend.rule.clone());
        match presence {
            Presence::Missing => self.diagnose(from, Some(&service), DiagnosticKind::MissingTarget),
            Presence::Present => self.check_port(from, &service, backend.rule.port.as_ref()),
            Presence::Unknown => {}
        }
    }

    fn tls(&mut self, from: &NodeId, namespace: Option<&'w str>, secret: &'w str) {
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
