//! What a topology is built from: object lists by kind.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use kube::api::DynamicObject;

use crate::cache::Objects;

/// A kind of object the topology reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKind {
    Pod,
    Deployment,
    ReplicaSet,
    StatefulSet,
    DaemonSet,
    Job,
    CronJob,
    Service,
    EndpointSlice,
    Ingress,
    Gateway,
    HttpRoute,
    GrpcRoute,
    TlsRoute,
    ConfigMap,
    Secret,
    PersistentVolumeClaim,
    ServiceAccount,
    /// `networking.k8s.io/v1` NetworkPolicy: the network policy layer.
    NetworkPolicy,
    /// Namespace labels, for NetworkPolicy `namespaceSelector`s.
    Namespace,
}

/// API group of Gateway API kinds.
pub const GATEWAY_GROUP: &str = "gateway.networking.k8s.io";

impl SourceKind {
    /// Kinds the graph is built from (and drawn on it).
    pub const ALL: [SourceKind; 18] = [
        SourceKind::Pod,
        SourceKind::Deployment,
        SourceKind::ReplicaSet,
        SourceKind::StatefulSet,
        SourceKind::DaemonSet,
        SourceKind::Job,
        SourceKind::CronJob,
        SourceKind::Service,
        SourceKind::EndpointSlice,
        SourceKind::Ingress,
        SourceKind::Gateway,
        SourceKind::HttpRoute,
        SourceKind::GrpcRoute,
        SourceKind::TlsRoute,
        SourceKind::ConfigMap,
        SourceKind::Secret,
        SourceKind::PersistentVolumeClaim,
        SourceKind::ServiceAccount,
    ];

    /// Kinds layers read on top of the graph (not nodes themselves).
    pub const LAYERS: [SourceKind; 2] = [SourceKind::NetworkPolicy, SourceKind::Namespace];

    /// [`ALL`](Self::ALL) and [`LAYERS`](Self::LAYERS).
    pub fn every() -> impl Iterator<Item = SourceKind> {
        Self::ALL.into_iter().chain(Self::LAYERS)
    }

    pub fn group(self) -> &'static str {
        match self {
            SourceKind::Deployment
            | SourceKind::ReplicaSet
            | SourceKind::StatefulSet
            | SourceKind::DaemonSet => "apps",
            SourceKind::Job | SourceKind::CronJob => "batch",
            SourceKind::EndpointSlice => "discovery.k8s.io",
            SourceKind::Ingress | SourceKind::NetworkPolicy => "networking.k8s.io",
            SourceKind::Gateway
            | SourceKind::HttpRoute
            | SourceKind::GrpcRoute
            | SourceKind::TlsRoute => GATEWAY_GROUP,
            SourceKind::Pod
            | SourceKind::Service
            | SourceKind::ConfigMap
            | SourceKind::Secret
            | SourceKind::PersistentVolumeClaim
            | SourceKind::ServiceAccount
            | SourceKind::Namespace => "",
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            SourceKind::Pod => "Pod",
            SourceKind::Deployment => "Deployment",
            SourceKind::ReplicaSet => "ReplicaSet",
            SourceKind::StatefulSet => "StatefulSet",
            SourceKind::DaemonSet => "DaemonSet",
            SourceKind::Job => "Job",
            SourceKind::CronJob => "CronJob",
            SourceKind::Service => "Service",
            SourceKind::EndpointSlice => "EndpointSlice",
            SourceKind::Ingress => "Ingress",
            SourceKind::Gateway => "Gateway",
            SourceKind::HttpRoute => "HTTPRoute",
            SourceKind::GrpcRoute => "GRPCRoute",
            SourceKind::TlsRoute => "TLSRoute",
            SourceKind::ConfigMap => "ConfigMap",
            SourceKind::Secret => "Secret",
            SourceKind::PersistentVolumeClaim => "PersistentVolumeClaim",
            SourceKind::ServiceAccount => "ServiceAccount",
            SourceKind::NetworkPolicy => "NetworkPolicy",
            SourceKind::Namespace => "Namespace",
        }
    }

    /// A kind the graph draws ([`ALL`](Self::ALL)); layer kinds are not.
    pub fn from_api(group: &str, kind: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|source| source.group() == group && source.kind() == kind)
    }

    /// Only names and labels are used: the watch can skip bodies.
    pub fn metadata_only(self) -> bool {
        matches!(
            self,
            SourceKind::ConfigMap | SourceKind::Secret | SourceKind::ServiceAccount
        )
    }
}

/// Snapshots of the objects to build a topology from, by kind.
///
/// A kind is *complete* when its list is known to be whole (the watch is
/// loaded): only then a reference to an object of that kind that is not
/// in the list is reported as missing. Kinds never set are neither.
#[derive(Clone, Debug, Default)]
pub struct TopologyInput {
    objects: HashMap<SourceKind, Objects>,
    complete: HashSet<SourceKind>,
}

impl TopologyInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the whole list of `kind`.
    pub fn set(&mut self, kind: SourceKind, objects: impl Into<Objects>) {
        self.objects.insert(kind, objects.into());
        self.complete.insert(kind);
    }

    /// Sets what is known of `kind` without claiming it is all (loading,
    /// failed watch, cached list).
    pub fn set_partial(&mut self, kind: SourceKind, objects: impl Into<Objects>) {
        self.objects.insert(kind, objects.into());
        self.complete.remove(&kind);
    }

    /// Sorts objects (e.g. parsed from manifests) by their `apiVersion` and
    /// `kind`; every [`SourceKind`] is then complete, the objects being
    /// the whole world (also [`SourceKind::LAYERS`]). Objects of other kinds
    /// are ignored.
    pub fn from_objects(objects: impl IntoIterator<Item = DynamicObject>) -> Self {
        let mut by_kind: HashMap<SourceKind, Vec<Arc<DynamicObject>>> = HashMap::new();
        for object in objects {
            let Some(types) = &object.types else {
                continue;
            };
            let group = types
                .api_version
                .split_once('/')
                .map_or("", |(group, _)| group);
            if let Some(kind) =
                SourceKind::every().find(|kind| kind.group() == group && kind.kind() == types.kind)
            {
                by_kind.entry(kind).or_default().push(Arc::new(object));
            }
        }
        let mut input = Self::new();
        for kind in SourceKind::every() {
            input.set(kind, by_kind.remove(&kind).unwrap_or_default());
        }
        input
    }

    pub fn objects(&self, kind: SourceKind) -> &[Arc<DynamicObject>] {
        self.objects.get(&kind).map_or(&[], |objects| objects)
    }

    pub fn is_complete(&self, kind: SourceKind) -> bool {
        self.complete.contains(&kind)
    }
}
