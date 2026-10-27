//! Topology of an application: how ingress, services, workloads and their
//! dependencies are connected (#321, [ADR 0016](../../../../docs/adr/0016-topology.md)).
//!
//! A pure model, without UI: [`build`] (or a [`TopologyBuilder`] for cheap
//! rebuilds) turns object lists by kind ([`TopologyInput`]) into a
//! [`TopologyGraph`] of [`Node`]s and typed [`Edge`]s with
//! [`Diagnostic`]s for dangling references. [`TopologySource`] feeds it
//! from the [`ResourceCache`](crate::cache::ResourceCache).
//!
//! - **Nodes**: Ingress, Gateway and routes (Gateway API, read as dynamic
//!   objects), Services, workloads (a Deployment with its ReplicaSets, a
//!   CronJob with its Jobs), pods ([`Detail::Pods`]) or pods collapsed into
//!   their workload ([`Detail::Workloads`]), the ConfigMaps, Secrets
//!   (names only), PVCs and ServiceAccounts something uses, and external
//!   addresses. Grouped by namespace.
//! - **Ids** are names (`Kind[.group]/namespace/name`), stable across
//!   rebuilds, recreates and detail levels.
//! - **Edges**: [`EdgeKind`] with attributes in [`EdgeAttrs`]; at most one
//!   edge of a kind between two nodes. Layers to come (network policies,
//!   traffic) add edge kinds and attribute fields and merge their edges
//!   with [`TopologyGraph::with_edges`].
//! - **Network policies** ([`netpol`], #325): which pods NetworkPolicies
//!   isolate, whether a Service's traffic gets to its pods, and "can A
//!   reach B on port P" with the policies that decide it.
//! - **Export** ([`export`], #327): Mermaid and Graphviz DOT of a graph.
//! - **Traffic** ([`traffic`], #326): what really flows between workloads,
//!   from the Prometheus metrics of Istio, Linkerd or Cilium Hubble, as
//!   [`EdgeKind::Traffic`] edges and [`EdgeAttrs::traffic`].

mod build;
pub mod export;
mod extract;
mod live;
mod model;
pub mod netpol;
mod source;
#[cfg(test)]
mod tests;
pub mod traffic;

pub use build::{BuildOptions, Detail, Filter, LabelSelector, TopologyBuilder, build};
pub use live::{TopologySnapshot, TopologySource, WatchErrors};
pub use model::{
    Access, ContainerPort, Diagnostic, DiagnosticKind, Edge, EdgeAttrs, EdgeId, EdgeKind,
    EdgePolicy, EdgeTraffic, EndpointCounts, ExternalKind, GraphDiff, Node, NodeAttrs, NodeClass,
    NodeId, NodeKind, NodePolicy, PodCounts, PolicyId, PortRef, Presence, Protocol, RefUse, RefVia,
    RouteRule, ServiceInfo, ServicePort, ServiceType, Severity, TopologyGraph, WorkloadKind,
};
pub use source::{GATEWAY_GROUP, SourceKind, TopologyInput};
