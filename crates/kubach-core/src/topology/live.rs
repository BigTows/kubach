//! The topology of what the [`ResourceCache`] watches: a thin layer that
//! subscribes to every [`SourceKind`] and feeds snapshots to a
//! [`TopologyBuilder`].

use std::sync::{Arc, Mutex};

use kube::discovery::verbs;

use super::build::{BuildOptions, TopologyBuilder};
use super::model::TopologyGraph;
use super::netpol::PolicySet;
use super::source::{SourceKind, TopologyInput};
use crate::cache::{
    MergedChanges, ResourceCache, SubscriptionSet, WatchError, WatchFilter, WatchStatus,
};
use crate::discovery::ApiCatalog;
use crate::sync::lock;

/// Failing watches by kind and namespace (`None`: all namespaces).
pub type WatchErrors = Vec<(SourceKind, Option<String>, WatchError)>;

/// Watches of every kind a topology reads, kept alive while this lives.
pub struct TopologySource {
    parts: Vec<(SourceKind, SubscriptionSet)>,
    unavailable: Vec<SourceKind>,
    all: SubscriptionSet,
    builder: Mutex<TopologyBuilder>,
}

/// A graph with the state of the watches it was built from.
#[derive(Clone, Debug)]
pub struct TopologySnapshot {
    pub graph: TopologyGraph,
    /// Some watch is still loading: the graph may be incomplete.
    pub loading: bool,
    /// Failing watches (no access, lasting outage), by kind and namespace.
    pub errors: WatchErrors,
    /// Kinds the cluster does not serve or that cannot be watched (Gateway
    /// API without its CRDs).
    pub unavailable: Vec<SourceKind>,
    /// NetworkPolicies and namespace labels the graph's network policy
    /// layer came from, for queries ([`PolicySet::can_reach`]); `None`
    /// unless [`BuildOptions::network_policies`].
    pub policies: Option<Arc<PolicySet>>,
}

impl TopologySource {
    /// Subscribes to every [`SourceKind`] the cluster serves, in
    /// `namespaces` (all when `None`). ConfigMaps, Secrets and
    /// ServiceAccounts are watched as metadata only.
    ///
    /// The kinds of the network policy layer ([`SourceKind::LAYERS`]) are
    /// watched always, whether the layer is shown or not: NetworkPolicies
    /// are few and small, the Namespace watch is the one the namespace
    /// picker already holds, and the layer then switches on without a
    /// load. Their errors are reported only while the layer is on.
    ///
    /// Must be called with the I/O [`runtime`](crate::runtime) available,
    /// like [`ResourceCache::subscribe`].
    pub fn subscribe(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        namespaces: Option<&[String]>,
    ) -> Self {
        let mut parts = Vec::new();
        let mut unavailable = Vec::new();
        for source in SourceKind::every() {
            let kind = catalog
                .find(source.group(), source.kind())
                .filter(|kind| kind.supports(verbs::WATCH));
            match kind {
                Some(kind) => {
                    let filter = WatchFilter {
                        metadata_only: source.metadata_only(),
                        ..WatchFilter::default()
                    };
                    parts.push((source, cache.subscribe_many(kind, namespaces, filter)));
                }
                None => unavailable.push(source),
            }
        }
        let all = SubscriptionSet::new(
            parts
                .iter()
                .flat_map(|(_, set)| set.subscriptions().iter().cloned())
                .collect(),
        );
        Self {
            parts,
            unavailable,
            all,
            builder: Mutex::new(TopologyBuilder::new()),
        }
    }

    /// Fires when any watched kind changes; the first `next` resolves at
    /// once.
    pub fn changes(&self) -> MergedChanges {
        self.all.changes()
    }

    /// Current objects. A kind is complete only when all its watches are
    /// loaded without errors.
    pub fn input(&self) -> (TopologyInput, bool, WatchErrors) {
        let mut input = TopologyInput::new();
        let mut loading = false;
        let mut errors = Vec::new();
        for (source, set) in &self.parts {
            let snapshot = set.snapshot();
            let complete = snapshot.status == WatchStatus::Ready && snapshot.errors.is_empty();
            loading |= snapshot.status == WatchStatus::Loading;
            errors.extend(
                snapshot
                    .errors
                    .into_iter()
                    .map(|(namespace, error)| (*source, namespace, error)),
            );
            if complete {
                input.set(*source, snapshot.objects);
            } else {
                input.set_partial(*source, snapshot.objects);
            }
        }
        (input, loading, errors)
    }

    /// Builds the graph of the current objects, reusing what was read from
    /// unchanged objects. CPU work proportional to the cluster: call it off
    /// the UI thread for big clusters.
    pub fn snapshot(&self, options: &BuildOptions) -> TopologySnapshot {
        let (input, loading, mut errors) = self.input();
        let graph = lock(&self.builder).build(&input, options);
        let layer = |kind: &SourceKind| SourceKind::LAYERS.contains(kind);
        if !options.network_policies {
            errors.retain(|(kind, _, _)| !layer(kind));
        }
        TopologySnapshot {
            graph,
            loading,
            errors,
            unavailable: self
                .unavailable
                .iter()
                .copied()
                .filter(|kind| options.network_policies || !layer(kind))
                .collect(),
            policies: options
                .network_policies
                .then(|| Arc::new(PolicySet::from_input(&input))),
        }
    }
}
