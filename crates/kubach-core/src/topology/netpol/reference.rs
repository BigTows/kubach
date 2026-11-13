//! The graph layer as it was first written, kept for tests: what
//! [`apply`](super::apply) must keep returning. Every policy is tried on
//! every node, and every drawn node as a source of every isolated one.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::*;
use crate::topology::NodeId;

pub fn apply(graph: TopologyGraph, policies: &PolicySet) -> TopologyGraph {
    // Workloads whose pods are drawn: the pods carry the layer's edges.
    let with_pods: BTreeSet<&NodeId> = graph
        .edges()
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Owns
                && graph
                    .node(&edge.to)
                    .is_some_and(|node| node.kind == NodeKind::Pod)
        })
        .map(|edge| &edge.from)
        .collect();
    let groups: Vec<(NodeId, PodGroup)> = graph
        .nodes()
        .iter()
        .filter_map(|node| Some((node.id.clone(), PodGroup::of(node)?)))
        .collect();
    let mut node_policies: HashMap<NodeId, NodePolicy> = groups
        .iter()
        .map(|(id, pods)| (id.clone(), policies.node_policy(pods)))
        .collect();
    let by_id: HashMap<&NodeId, &PodGroup> = groups.iter().map(|(id, pods)| (id, pods)).collect();

    let mut edge_policies: HashMap<EdgeId, EdgePolicy> = HashMap::new();
    for edge in graph.edges() {
        if edge.kind != EdgeKind::Selects {
            continue;
        }
        let (Some(pods), Some(service)) = (
            by_id.get(&edge.to),
            graph
                .node(&edge.from)
                .and_then(|node| node.attrs.service.as_ref()),
        ) else {
            continue;
        };
        let mut ports: Vec<(i32, Protocol)> = service
            .ports
            .iter()
            .filter_map(|port| service_target(port, pods.view()))
            .collect();
        ports.sort();
        ports.dedup();
        edge_policies.insert(edge.id.clone(), policies.admission(pods, &ports));
    }

    let mut allows: BTreeMap<EdgeId, Edge> = BTreeMap::new();
    let drawn: Vec<&(NodeId, PodGroup)> = groups
        .iter()
        .filter(|(id, _)| !with_pods.contains(id))
        .collect();
    for (target_id, target) in &drawn {
        if !node_policies
            .get(target_id)
            .is_some_and(NodePolicy::isolated_ingress)
        {
            continue;
        }
        let destination = Target::Pods(target.clone());
        let mut sources = 0;
        for policy in policies.selecting(target) {
            for rule in policy.ingress.as_deref().unwrap_or_default() {
                let ports = rule_ports(rule, target.view());
                for peer in &rule.peers {
                    let Peer::Pods {
                        pods: Some(selector),
                        ..
                    } = peer
                    else {
                        continue;
                    };
                    if selector.is_empty() {
                        continue;
                    }
                    for (source_id, source) in &drawn {
                        if source_id == target_id || sources >= MAX_ALLOWED_SOURCES {
                            continue;
                        }
                        let from = Target::Pods(source.clone());
                        if !policies.peer_matches(policy, peer, &from) {
                            continue;
                        }
                        // The source's egress must let it out on some port.
                        let open: Vec<String> = ports
                            .iter()
                            .filter(|(port, protocol)| {
                                policies
                                    .side(
                                        Direction::Egress,
                                        &from,
                                        &destination,
                                        *port,
                                        *protocol,
                                        Some(target),
                                    )
                                    .allowed
                            })
                            .map(|(port, protocol)| format!("{protocol} {port}"))
                            .collect();
                        let egress_open = !node_policies
                            .get(source_id)
                            .is_some_and(NodePolicy::isolated_egress);
                        if open.is_empty() && !(ports.is_empty() && egress_open) {
                            continue;
                        }
                        let id = EdgeId::new(EdgeKind::PolicyAllows, source_id, target_id);
                        let edge = allows.entry(id.clone()).or_insert_with(|| {
                            sources += 1;
                            Edge {
                                id,
                                from: source_id.clone(),
                                to: target_id.clone(),
                                kind: EdgeKind::PolicyAllows,
                                attrs: EdgeAttrs {
                                    policy: Some(EdgePolicy {
                                        access: Access::Allowed,
                                        policies: Vec::new(),
                                        ports: Vec::new(),
                                    }),
                                    ..EdgeAttrs::default()
                                },
                            }
                        });
                        if let Some(edge_policy) = &mut edge.attrs.policy {
                            if !edge_policy.policies.contains(&policy.id) {
                                edge_policy.policies.push(policy.id.clone());
                            }
                            for port in open {
                                if !edge_policy.ports.contains(&port) {
                                    edge_policy.ports.push(port);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    graph
        .map(
            |node| {
                if let Some(policy) = node_policies.remove(&node.id) {
                    node.attrs.network_policy = Some(policy);
                }
            },
            |edge| {
                if let Some(policy) = edge_policies.remove(&edge.id) {
                    edge.attrs.policy = Some(policy);
                }
            },
        )
        .with_edges(allows.into_values())
}
