//! Topology model → what the graph canvas draws: columns by node class,
//! namespaces as groups, tones from readiness and diagnostics, edge styles
//! by edge kind; measured traffic (#326) as thickness and traffic colors.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use gpui_kit::assets::IconName;
use kubach_core::metrics::prometheus::format_bytes;
use kubach_core::topology::{
    Access, Diagnostic, DiagnosticKind, Edge, EdgeKind, EdgeTraffic, Node, NodeClass, NodeId,
    NodeKind, Presence, ServiceType, Severity, TopologyGraph, WorkloadKind, export,
};
use kubach_i18n::t;
use kubach_ui::{EdgeStyle, GraphData, GraphEdge, GraphGroup, GraphNode, Tone, TrafficLevel};

use super::super::access::Reveal;
use super::super::browser::OpenObject;

/// Node classes in the order of the filter chips.
pub const CLASSES: [NodeClass; 11] = [
    NodeClass::Ingress,
    NodeClass::Gateway,
    NodeClass::Route,
    NodeClass::Service,
    NodeClass::Workload,
    NodeClass::Pod,
    NodeClass::ConfigMap,
    NodeClass::Secret,
    NodeClass::PersistentVolumeClaim,
    NodeClass::ServiceAccount,
    NodeClass::External,
];

/// The label of a filter chip: kind names stay as they are.
pub fn class_label(class: NodeClass) -> String {
    match class {
        NodeClass::Ingress => "Ingress".to_owned(),
        NodeClass::Gateway => "Gateway".to_owned(),
        NodeClass::Route => t!("topology-class-routes"),
        NodeClass::Service => "Services".to_owned(),
        NodeClass::Workload => t!("topology-class-workloads"),
        NodeClass::Pod => "Pods".to_owned(),
        NodeClass::ConfigMap => "ConfigMaps".to_owned(),
        NodeClass::Secret => "Secrets".to_owned(),
        NodeClass::PersistentVolumeClaim => "PVCs".to_owned(),
        NodeClass::ServiceAccount => "ServiceAccounts".to_owned(),
        NodeClass::External => t!("topology-class-external"),
    }
}

pub fn icon(kind: NodeKind) -> IconName {
    match kind {
        NodeKind::Ingress => IconName::LogIn,
        NodeKind::Gateway => IconName::Waypoints,
        NodeKind::HttpRoute | NodeKind::GrpcRoute | NodeKind::TlsRoute => IconName::Route,
        NodeKind::Service(_) => IconName::Network,
        NodeKind::Pod => IconName::Box,
        NodeKind::Workload(WorkloadKind::StatefulSet) => IconName::Database,
        NodeKind::Workload(WorkloadKind::DaemonSet) => IconName::Server,
        NodeKind::Workload(WorkloadKind::Job) => IconName::Clock,
        NodeKind::Workload(WorkloadKind::CronJob) => IconName::Calendar,
        NodeKind::Workload(_) => IconName::Layers,
        NodeKind::ConfigMap => IconName::FileText,
        NodeKind::Secret => IconName::Lock,
        NodeKind::PersistentVolumeClaim => IconName::HardDrive,
        NodeKind::ServiceAccount => IconName::CircleUser,
        NodeKind::External(_) => IconName::Globe,
    }
}

/// `Deployment`, `Service · headless`: the kind under the name, in the
/// language of the interface (the exports keep
/// [`export::kind_text`]).
pub fn subtitle(node: &Node) -> String {
    let kind = if node.api_kind.is_empty() {
        node.kind.label()
    } else {
        &node.api_kind
    };
    let variant = match node.kind {
        NodeKind::Service(ServiceType::Headless) => Some("headless".to_owned()),
        NodeKind::Service(ServiceType::NodePort) => Some("NodePort".to_owned()),
        NodeKind::Service(ServiceType::LoadBalancer) => Some("LoadBalancer".to_owned()),
        NodeKind::Service(ServiceType::ExternalName) => Some("ExternalName".to_owned()),
        _ => None,
    };
    let presence = match node.presence {
        Presence::Present => None,
        Presence::Missing => Some(t!("topology-presence-missing")),
        Presence::Unknown => Some(t!("topology-presence-unknown")),
    };
    [Some(kind.to_owned()), variant, presence]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `Kind namespace/name` of `id`, or the id when it is not in the graph.
fn describe(graph: &TopologyGraph, id: &NodeId) -> String {
    match graph.node(id) {
        Some(node) => node.describe(),
        None => id.to_string(),
    }
}

/// A problem of the graph in one line, in the language of the interface
/// (the MCP agent gets [`Diagnostic::message`]).
pub fn diagnostic_text(diagnostic: &Diagnostic, graph: &TopologyGraph) -> String {
    let node = describe(graph, &diagnostic.node);
    let target = diagnostic
        .target
        .as_ref()
        .map(|id| describe(graph, id))
        .unwrap_or_default();
    match &diagnostic.kind {
        DiagnosticKind::NoReadyEndpoints { not_ready: 0 } => {
            t!("topology-diagnostic-no-ready", node = node)
        }
        DiagnosticKind::NoReadyEndpoints { not_ready } => t!(
            "topology-diagnostic-not-ready",
            node = node,
            count = *not_ready
        ),
        DiagnosticKind::SelectorMatchesNothing => {
            t!("topology-diagnostic-selector", node = node)
        }
        DiagnosticKind::MissingTarget => t!(
            "topology-diagnostic-missing-target",
            node = node,
            target = target
        ),
        DiagnosticKind::MissingPort { port } => t!(
            "topology-diagnostic-missing-port",
            node = node,
            port = port.to_string(),
            target = target
        ),
    }
}

/// Column of a node: entry points left, dependencies right. Pods owned by
/// a drawn workload get a column of their own.
fn base_layer(node: &Node, owned: bool) -> Option<usize> {
    match node.kind.class() {
        NodeClass::Ingress | NodeClass::Gateway => Some(0),
        NodeClass::Route => Some(1),
        NodeClass::Service => Some(2),
        NodeClass::Workload => Some(3),
        NodeClass::Pod => Some(if owned { 4 } else { 3 }),
        NodeClass::ConfigMap
        | NodeClass::Secret
        | NodeClass::PersistentVolumeClaim
        | NodeClass::ServiceAccount => Some(5),
        // Load balancer addresses and ExternalName targets follow edges.
        NodeClass::External => None,
    }
}

/// Worst diagnostic of a node.
pub fn severity(graph: &TopologyGraph, id: &NodeId) -> Option<Severity> {
    graph.diagnostics_for(id).map(|d| d.severity()).max()
}

pub fn severity_tone(severity: Severity) -> Tone {
    match severity {
        Severity::Warning => Tone::Warning,
        Severity::Error => Tone::Danger,
    }
}

/// Ready of total: all ready is fine, none is a problem.
fn ready_tone(ready: u32, total: u32) -> Tone {
    if total == 0 {
        Tone::Neutral
    } else if ready == total {
        Tone::Success
    } else if ready == 0 {
        Tone::Danger
    } else {
        Tone::Warning
    }
}

/// State of a node on its own, before diagnostics.
fn state_tone(graph: &TopologyGraph, node: &Node) -> Tone {
    match node.presence {
        Presence::Missing => return Tone::Danger,
        Presence::Unknown => return Tone::Neutral,
        Presence::Present => {}
    }
    match node.kind {
        NodeKind::Pod => match (node.attrs.phase.as_deref(), node.attrs.ready) {
            (Some("Succeeded"), _) => Tone::Neutral,
            (Some("Failed"), _) => Tone::Danger,
            (_, Some(true)) => Tone::Success,
            (Some("Pending"), _) => Tone::Info,
            _ => Tone::Warning,
        },
        NodeKind::Workload(WorkloadKind::CronJob | WorkloadKind::Job) => {
            node.attrs.pods.map_or(Tone::Neutral, |p| match p.total {
                0 => Tone::Neutral,
                _ if p.ready > 0 => Tone::Success,
                _ => Tone::Info,
            })
        }
        NodeKind::Workload(_) => match node.attrs.pods {
            Some(pods) => ready_tone(pods.ready, pods.total),
            None => Tone::Neutral,
        },
        NodeKind::Service(_) => {
            let ready: u32 = graph
                .edges_from(&node.id)
                .filter(|edge| edge.kind == EdgeKind::Selects)
                .filter_map(|edge| edge.attrs.endpoints)
                .map(|endpoints| endpoints.ready)
                .sum();
            if ready > 0 {
                Tone::Success
            } else {
                Tone::Neutral
            }
        }
        _ => Tone::Neutral,
    }
}

/// Tone of the node's dot: its diagnostics win over its state.
pub fn tone(graph: &TopologyGraph, node: &Node) -> Tone {
    match severity(graph, &node.id) {
        Some(severity) => severity_tone(severity),
        None => state_tone(graph, node),
    }
}

/// Text at the middle of an edge, in the language of the interface (the
/// exports keep [`export::edge_label`]): route, endpoints, policy verdict
/// or measured traffic.
pub fn edge_label(edge: &Edge) -> Option<String> {
    if let Some(policy) = &edge.attrs.policy {
        match (edge.kind, policy.access) {
            (EdgeKind::PolicyAllows, _) => {
                return Some(if policy.ports.is_empty() {
                    t!("topology-edge-allowed")
                } else {
                    t!("topology-edge-allows", ports = policy.ports.join(", "))
                });
            }
            (_, Access::Blocked) => return Some(t!("topology-edge-blocked")),
            _ => {}
        }
    }
    if let Some(traffic) = &edge.attrs.traffic {
        return Some(traffic_text(traffic));
    }
    match edge.kind {
        // Hosts, paths and ports only.
        EdgeKind::RoutesTo => export::edge_label(edge),
        EdgeKind::Selects => edge.attrs.endpoints.map(|e| match e.not_ready {
            0 => t!("topology-edge-ready", count = e.ready),
            n => t!(
                "topology-edge-ready-of",
                count = e.ready,
                total = e.ready + n
            ),
        }),
        _ => None,
    }
}

/// `12 req/s · 2.1% err · p95 84 ms`: the numbers of a traffic edge, the
/// load first.
pub fn traffic_text(traffic: &EdgeTraffic) -> String {
    let mut parts = Vec::new();
    if let Some(requests) = traffic.requests.filter(|r| *r > 0.) {
        parts.push(t!(
            "topology-traffic-requests",
            rate = export::rate_text(requests)
        ));
    } else if let Some(flows) = traffic.flows.filter(|f| *f > 0.) {
        parts.push(t!(
            "topology-traffic-flows",
            rate = export::rate_text(flows)
        ));
    } else if let Some(bytes) = traffic.bytes.filter(|b| *b > 0.) {
        parts.push(format!("{}/s", format_bytes(bytes)));
    }
    if let Some(ratio) = traffic.error_ratio().filter(|r| *r > 0.) {
        let percent = ratio * 100.;
        let text = if percent >= 10. {
            format!("{percent:.0}")
        } else if percent >= 0.1 {
            format!("{percent:.1}")
        } else {
            "<0.1".to_owned()
        };
        parts.push(if traffic.requests.is_some_and(|r| r > 0.) {
            t!("topology-traffic-errors", percent = text)
        } else {
            t!("topology-traffic-dropped", percent = text)
        });
    }
    if let Some(p95) = traffic.p95_ms {
        parts.push(if p95 >= 1000. {
            format!("p95 {:.1} s", p95 / 1000.)
        } else {
            format!("p95 {p95:.0} ms")
        });
    }
    if parts.is_empty() {
        t!("topology-traffic-idle")
    } else {
        parts.join(" · ")
    }
}

/// `routes to`, `mounts`: an edge kind in a sentence.
pub fn edge_kind_label(kind: EdgeKind) -> String {
    match kind {
        EdgeKind::RoutesTo => t!("topology-edge-routes-to"),
        EdgeKind::Selects => t!("topology-edge-selects"),
        EdgeKind::Owns => t!("topology-edge-owns"),
        EdgeKind::Mounts => t!("topology-edge-mounts"),
        EdgeKind::EnvRef => t!("topology-edge-env-from"),
        EdgeKind::UsesServiceAccount => t!("topology-edge-service-account"),
        EdgeKind::Exposes => t!("topology-edge-exposes"),
        EdgeKind::Tls => t!("topology-edge-tls"),
        EdgeKind::PolicyAllows => t!("topology-edge-policy-allows"),
        EdgeKind::Traffic => t!("topology-edge-traffic"),
        _ => t!("topology-edge-links-to"),
    }
}

/// Traffic paths are solid and heavier, references dashed or dotted,
/// ownership thin.
fn edge_look(kind: EdgeKind) -> (EdgeStyle, Option<f32>) {
    match kind {
        EdgeKind::RoutesTo | EdgeKind::Selects => (EdgeStyle::Solid, Some(0.25)),
        EdgeKind::Exposes => (EdgeStyle::Solid, None),
        EdgeKind::Owns => (EdgeStyle::Solid, None),
        EdgeKind::Mounts | EdgeKind::EnvRef => (EdgeStyle::Dashed, None),
        _ => (EdgeStyle::Dotted, None),
    }
}

fn edge_tone(graph: &TopologyGraph, edge: &Edge) -> Tone {
    match edge.attrs.policy.as_ref().map(|policy| policy.access) {
        Some(Access::Blocked) => return Tone::Danger,
        _ if edge.kind == EdgeKind::PolicyAllows => return Tone::Success,
        _ => {}
    }
    if graph
        .node(&edge.to)
        .is_some_and(|node| node.presence == Presence::Missing)
    {
        return Tone::Danger;
    }
    match edge.attrs.endpoints {
        Some(e) if e.ready == 0 && e.not_ready > 0 => Tone::Warning,
        _ => Tone::Neutral,
    }
}

/// `isolated`, `isolated in`, `isolated out`: what the policy layer says
/// about a pod or workload; `None` when it isn't isolated or the layer is
/// off.
pub fn isolation_label(node: &Node) -> Option<String> {
    let policy = node.attrs.network_policy.as_ref()?;
    match (policy.isolated_ingress(), policy.isolated_egress()) {
        (true, true) => Some(t!("topology-isolated")),
        (true, false) => Some(t!("topology-isolated-in")),
        (false, true) => Some(t!("topology-isolated-out")),
        (false, false) => None,
    }
}

/// Badges of a node: pods ready of total, `missing`, policy isolation.
fn badges(node: &Node) -> Vec<(String, Tone)> {
    let mut badges = Vec::new();
    if node.presence == Presence::Missing {
        badges.push((t!("topology-presence-missing"), Tone::Danger));
    }
    if let Some(label) = isolation_label(node) {
        badges.push((label, Tone::Info));
    }
    if let (NodeKind::Workload(_), Some(pods)) = (node.kind, node.attrs.pods) {
        let total = node
            .attrs
            .desired_replicas
            .map_or(pods.total, |desired| desired.max(0) as u32);
        badges.push((
            format!("{}/{}", pods.ready, total),
            ready_tone(pods.ready, total),
        ));
    }
    badges
}

/// Thinnest drawn traffic: an idle edge is still visible.
const MIN_TRAFFIC_WEIGHT: f32 = 0.08;

/// Thickness of measured traffic, 0..=1 relative to the busiest edge of
/// the graph, on a log scale (one hot path doesn't flatten the rest).
pub fn traffic_weight(load: f64, busiest: f64) -> f32 {
    if busiest <= 0. || load <= 0. {
        return MIN_TRAFFIC_WEIGHT;
    }
    let weight = ((1. + load).ln() / (1. + busiest).ln()) as f32;
    weight.clamp(MIN_TRAFFIC_WEIGHT, 1.)
}

/// Options of [`graph_data`].
#[derive(Clone, Copy, Default)]
pub struct MapOptions {
    /// Namespaces become groups (off for a one-namespace mini graph).
    pub groups: bool,
}

/// The canvas data of `graph`.
pub fn graph_data(graph: &TopologyGraph, options: MapOptions) -> GraphData {
    let owned: HashSet<&NodeId> = graph
        .edges()
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Owns)
        .map(|edge| &edge.to)
        .collect();
    // Dense columns: no empty column for a class that isn't shown.
    let layers: BTreeMap<&NodeId, Option<usize>> = graph
        .nodes()
        .iter()
        .map(|node| (&node.id, base_layer(node, owned.contains(&node.id))))
        .collect();
    let busiest = graph
        .edges()
        .iter()
        .filter_map(|edge| edge.attrs.traffic.as_ref())
        .map(|traffic| traffic.load())
        .fold(0f64, f64::max);
    let used: BTreeSet<usize> = layers.values().flatten().copied().collect();
    let dense: BTreeMap<usize, usize> = used.into_iter().enumerate().map(|(i, l)| (l, i)).collect();
    let nodes = graph
        .nodes()
        .iter()
        .map(|node| {
            let mut out = GraphNode::new(node.id.as_str().to_owned(), node.name.clone())
                .subtitle(subtitle(node))
                .icon(if isolation_label(node).is_some() {
                    IconName::Shield
                } else {
                    icon(node.kind)
                })
                .tone(tone(graph, node));
            for (label, tone) in badges(node) {
                out = out.badge(label, tone);
            }
            if let Some(layer) = layers[&node.id].and_then(|layer| dense.get(&layer)) {
                out = out.layer(*layer);
            }
            if options.groups
                && let Some(namespace) = &node.namespace
            {
                out = out.group(namespace.clone());
            }
            out
        })
        .collect();
    let edges = graph
        .edges()
        .iter()
        .map(|edge| {
            let (mut style, weight) = edge_look(edge.kind);
            if edge
                .attrs
                .policy
                .as_ref()
                .is_some_and(|policy| policy.access == Access::Blocked)
            {
                style = EdgeStyle::Dashed;
            }
            let mut out = GraphEdge::new(
                edge.id.as_str().to_owned(),
                edge.from.as_str().to_owned(),
                edge.to.as_str().to_owned(),
            )
            .style(style)
            .tone(edge_tone(graph, edge));
            if let Some(weight) = weight {
                out = out.weight(weight);
            }
            if let Some(traffic) = &edge.attrs.traffic {
                let weight = traffic_weight(traffic.load(), busiest);
                let level = TrafficLevel::of(weight, traffic.error_ratio().map(|r| r as f32));
                out = out
                    .weight(weight)
                    .traffic(level)
                    .style(if traffic.unmapped {
                        EdgeStyle::LongDash
                    } else {
                        EdgeStyle::Solid
                    });
            }
            if let Some(label) = edge_label(edge) {
                out = out.label(label);
            }
            out
        })
        .collect();
    let groups = if options.groups {
        graph
            .namespaces()
            .into_iter()
            .map(|namespace| GraphGroup::new(namespace.to_owned(), namespace.to_owned()))
            .collect()
    } else {
        Vec::new()
    };
    GraphData {
        nodes,
        edges,
        groups,
    }
}

/// Opens the node's object in the resource browser; `None` for external
/// addresses and objects that don't exist.
pub fn reveal_of(node: &Node) -> Option<Reveal> {
    if node.presence == Presence::Missing || node.kind.class() == NodeClass::External {
        return None;
    }
    let kind = if node.api_kind.is_empty() {
        node.kind.label().to_owned()
    } else {
        node.api_kind.clone()
    };
    Some(Reveal {
        group: node.api_group.clone(),
        kind,
        namespace: node.namespace.clone(),
        name: node.name.clone(),
    })
}

/// The object a node stands for, to open its details in place (#394);
/// `None` for nodes without one in the cluster (external addresses, peers
/// seen only in traffic, dangling references).
pub fn open_object(node: &Node) -> Option<OpenObject> {
    let Reveal {
        group,
        kind,
        namespace,
        name,
    } = reveal_of(node)?;
    Some(OpenObject {
        group,
        kind,
        namespace,
        name,
    })
}

/// `seed` with the nodes up to `depth` edges upstream and downstream.
pub fn near(graph: &TopologyGraph, seed: &NodeId, depth: usize) -> BTreeSet<NodeId> {
    let mut seen: BTreeSet<NodeId> = BTreeSet::new();
    if graph.node(seed).is_none() {
        return seen;
    }
    seen.insert(seed.clone());
    for forward in [true, false] {
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::from([(seed.clone(), 0)]);
        let mut visited: BTreeSet<NodeId> = BTreeSet::new();
        while let Some((id, distance)) = queue.pop_front() {
            if distance == depth || !visited.insert(id.clone()) {
                continue;
            }
            let next: Vec<NodeId> = if forward {
                graph.edges_from(&id).map(|e| e.to.clone()).collect()
            } else {
                graph.edges_to(&id).map(|e| e.from.clone()).collect()
            };
            for id in next {
                seen.insert(id.clone());
                queue.push_back((id, distance + 1));
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use kubach_core::topology::{BuildOptions, Detail, NodeId, SourceKind, TopologyInput, build};
    use kubach_ui::{EdgeStyle, Tone};
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::super::super::browser::OpenObject;
    use super::{MapOptions, graph_data, near, open_object, reveal_of};

    fn object(value: serde_json::Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    /// Ingress → Service → Deployment (1 of 2 ready) → ConfigMap, and an
    /// Ingress to a Service that does not exist.
    fn input() -> TopologyInput {
        let labels = json!({"app": "web"});
        let mut input = TopologyInput::new();
        for kind in SourceKind::ALL {
            input.set(kind, Vec::<std::sync::Arc<DynamicObject>>::new());
        }
        input.set(
            SourceKind::Ingress,
            vec![
                std::sync::Arc::new(object(json!({
                    "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
                    "metadata": {"name": "web", "namespace": "shop"},
                    "spec": {"rules": [{"host": "shop.local", "http": {"paths": [{
                        "path": "/api", "pathType": "Prefix",
                        "backend": {"service": {"name": "web", "port": {"number": 80}}}
                    }]}}]}
                }))),
                std::sync::Arc::new(object(json!({
                    "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
                    "metadata": {"name": "broken", "namespace": "shop"},
                    "spec": {"defaultBackend": {"service": {"name": "ghost", "port": {"number": 80}}}}
                }))),
            ],
        );
        input.set(
            SourceKind::Service,
            vec![std::sync::Arc::new(object(json!({
                "apiVersion": "v1", "kind": "Service",
                "metadata": {"name": "web", "namespace": "shop"},
                "spec": {"selector": labels, "ports": [{"port": 80, "targetPort": 8080}]}
            })))],
        );
        input.set(
            SourceKind::Deployment,
            vec![std::sync::Arc::new(object(json!({
                "apiVersion": "apps/v1", "kind": "Deployment",
                "metadata": {"name": "web", "namespace": "shop", "uid": "d1"},
                "spec": {"replicas": 2, "selector": {"matchLabels": labels},
                    "template": {"metadata": {"labels": labels}, "spec": {"containers": [{
                        "name": "web", "envFrom": [{"configMapRef": {"name": "web-config"}}]
                    }]}}},
                "status": {"replicas": 2, "readyReplicas": 1}
            })))],
        );
        input.set(
            SourceKind::ConfigMap,
            vec![std::sync::Arc::new(object(json!({
                "apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "web-config", "namespace": "shop"}
            })))],
        );
        input
    }

    #[test]
    fn maps_layers_groups_tones_and_edges() {
        let graph = build(&input(), &BuildOptions::default());
        let data = graph_data(&graph, MapOptions { groups: true });
        let node = |id: &str| data.nodes.iter().find(|n| n.id.as_ref() == id).unwrap();
        let ingress = node("Ingress.networking.k8s.io/shop/web");
        let service = node("Service/shop/web");
        let ghost = node("Service/shop/ghost");
        let config = node("ConfigMap/shop/web-config");
        assert_eq!(ingress.layer, Some(0));
        assert!(service.layer.unwrap() > ingress.layer.unwrap());
        assert!(config.layer.unwrap() > service.layer.unwrap());
        assert_eq!(service.group.as_deref(), Some("shop"));
        assert_eq!(data.groups.len(), 1);
        // The missing backend and the Ingress pointing at it are errors.
        assert_eq!(ghost.tone, Tone::Danger);
        assert_eq!(
            node("Ingress.networking.k8s.io/shop/broken").tone,
            Tone::Danger
        );
        assert!(ghost.badges.iter().any(|b| b.label.as_ref() == "missing"));
        let route = data
            .edges
            .iter()
            .find(|e| e.from.as_ref() == "Ingress.networking.k8s.io/shop/web")
            .unwrap();
        assert_eq!(route.style, EdgeStyle::Solid);
        assert_eq!(route.label.as_deref(), Some("shop.local/api → 80"));
        let env = data
            .edges
            .iter()
            .find(|e| e.to.as_ref() == "ConfigMap/shop/web-config")
            .unwrap();
        assert_eq!(env.style, EdgeStyle::Dashed);
        // Without groups, the mini graph has none.
        assert!(graph_data(&graph, MapOptions::default()).groups.is_empty());
    }

    #[test]
    fn policy_layer_marks_blocked_edges_and_isolated_nodes() {
        let mut input = input();
        input.set(
            SourceKind::NetworkPolicy,
            vec![std::sync::Arc::new(object(json!({
                "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "deny", "namespace": "shop"},
                "spec": {"podSelector": {}, "policyTypes": ["Ingress"]}
            })))],
        );
        let graph = build(
            &input,
            &BuildOptions {
                network_policies: true,
                ..BuildOptions::default()
            },
        );
        let data = graph_data(&graph, MapOptions { groups: true });
        let edge = data
            .edges
            .iter()
            .find(|e| e.from.as_ref() == "Service/shop/web")
            .unwrap();
        assert_eq!(edge.style, EdgeStyle::Dashed);
        assert_eq!(edge.tone, Tone::Danger);
        assert_eq!(
            edge.label.as_deref(),
            Some(kubach_core::topology::export::BLOCKED_LABEL)
        );
        let web = data
            .nodes
            .iter()
            .find(|n| n.id.as_ref() == "Deployment.apps/shop/web")
            .unwrap();
        assert!(web.badges.iter().any(|b| b.label.as_ref() == "isolated in"));
        // Without the layer the edge is a plain traffic path.
        let plain = graph_data(
            &build(&input, &BuildOptions::default()),
            MapOptions::default(),
        );
        let edge = plain
            .edges
            .iter()
            .find(|e| e.from.as_ref() == "Service/shop/web")
            .unwrap();
        assert_eq!(edge.style, EdgeStyle::Solid);
    }

    /// Ingress → Service frontend → frontend, Services cart and redis
    /// with their Deployments, and legacy without a Service; traffic from
    /// a gateway in another namespace, from outside the mesh, frontend →
    /// cart with errors, cart → redis over TCP and frontend → legacy.
    fn traffic_graph() -> kubach_core::topology::TopologyGraph {
        use kubach_core::topology::EdgeTraffic;
        use kubach_core::topology::traffic::{
            FlowKey, Peer, TrafficFlow, TrafficSnapshot, TrafficWindow, apply,
        };
        let deployment = |name: &str| {
            std::sync::Arc::new(object(json!({
                "apiVersion": "apps/v1", "kind": "Deployment",
                "metadata": {"name": name, "namespace": "shop", "uid": name},
                "spec": {"replicas": 2, "selector": {"matchLabels": {"app": name}},
                    "template": {"metadata": {"labels": {"app": name}},
                        "spec": {"containers": [{"name": name}]}}},
                "status": {"replicas": 2, "readyReplicas": 2}
            })))
        };
        let service = |name: &str| {
            std::sync::Arc::new(object(json!({
                "apiVersion": "v1", "kind": "Service",
                "metadata": {"name": name, "namespace": "shop"},
                "spec": {"selector": {"app": name}, "ports": [{"port": 80}]}
            })))
        };
        let mut input = TopologyInput::new();
        for kind in SourceKind::ALL {
            input.set(kind, Vec::<std::sync::Arc<DynamicObject>>::new());
        }
        input.set(
            SourceKind::Ingress,
            vec![std::sync::Arc::new(object(json!({
                "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
                "metadata": {"name": "shop", "namespace": "shop"},
                "spec": {"rules": [{"host": "shop.example.com", "http": {"paths": [{
                    "path": "/", "pathType": "Prefix",
                    "backend": {"service": {"name": "frontend", "port": {"number": 80}}}
                }]}}]}
            })))],
        );
        input.set(
            SourceKind::Deployment,
            ["frontend", "cart", "redis", "legacy"]
                .map(deployment)
                .to_vec(),
        );
        input.set(
            SourceKind::Service,
            ["frontend", "cart", "redis"].map(service).to_vec(),
        );
        let graph = build(&input, &BuildOptions::default());
        let flow =
            |source: Option<(&str, &str)>, destination: &str, traffic: EdgeTraffic| TrafficFlow {
                key: FlowKey {
                    source: source.map(|(ns, name)| Peer::new(ns, name)),
                    destination: Peer::new("shop", destination),
                    service: None,
                },
                traffic,
            };
        let requests = |rps: f64, errors: f64, p95: f64| EdgeTraffic {
            requests: Some(rps),
            errors: Some(errors),
            p95_ms: Some(p95),
            ..EdgeTraffic::default()
        };
        let snapshot = TrafficSnapshot {
            source: "Istio",
            window: TrafficWindow::FiveMinutes,
            flows: vec![
                flow(
                    Some(("istio-system", "istio-ingressgateway")),
                    "frontend",
                    requests(120., 0., 35.),
                ),
                flow(
                    Some(("shop", "frontend")),
                    "cart",
                    requests(42.5, 4.25, 87.),
                ),
                flow(
                    Some(("shop", "cart")),
                    "redis",
                    EdgeTraffic {
                        bytes: Some(20480.),
                        ..EdgeTraffic::default()
                    },
                ),
                flow(
                    Some(("shop", "frontend")),
                    "legacy",
                    requests(0.75, 0., 12.),
                ),
                flow(None, "frontend", requests(3., 0.06, 50.)),
            ],
            truncated: false,
            partial: Vec::new(),
        };
        apply(graph, &snapshot)
    }

    #[test]
    fn traffic_edges_get_weight_level_and_style() {
        use kubach_ui::TrafficLevel;
        let graph = traffic_graph();
        let data = graph_data(&graph, MapOptions { groups: true });
        let edge = |from: &str, to: &str| {
            data.edges
                .iter()
                .find(|e| e.from.as_ref() == from && e.to.as_ref() == to)
                .unwrap_or_else(|| panic!("{from} -> {to}"))
        };
        let gateway = edge(
            "Peer/istio-system/istio-ingressgateway",
            "Service/shop/frontend",
        );
        // The busiest edge (into frontend: gateway and outside callers)
        // is the thickest.
        assert!(gateway.weight.unwrap() > 0.99);
        let into_frontend = edge("Service/shop/frontend", "Deployment.apps/shop/frontend");
        assert_eq!(into_frontend.weight, Some(1.));
        assert_eq!(gateway.traffic, Some(TrafficLevel::High));
        assert_eq!(gateway.style, EdgeStyle::Solid);
        assert_eq!(gateway.label.as_deref(), Some("120 req/s · p95 35 ms"));
        // 10% errors: failing, whatever the load.
        let cart = edge("Deployment.apps/shop/frontend", "Service/shop/cart");
        assert_eq!(cart.traffic, Some(TrafficLevel::Failing));
        assert!(cart.weight.unwrap() < 1.);
        // The Service's edge to its pods carries the same traffic.
        let selects = edge("Service/shop/cart", "Deployment.apps/shop/cart");
        assert_eq!(selects.traffic, Some(TrafficLevel::Failing));
        // No Service selects legacy: long dashes.
        let legacy = edge(
            "Deployment.apps/shop/frontend",
            "Deployment.apps/shop/legacy",
        );
        assert_eq!(legacy.style, EdgeStyle::LongDash);
        assert_eq!(legacy.traffic, Some(TrafficLevel::Low));
        // The gateway's namespace becomes a group of its own.
        assert!(data.groups.iter().any(|g| g.id.as_ref() == "istio-system"));
        // Peers open nothing in the browser.
        let peer = graph
            .nodes()
            .iter()
            .find(|n| n.name == "istio-ingressgateway")
            .unwrap();
        assert_eq!(reveal_of(peer), None);
        // Nor do they open details on the topology (#394).
        assert_eq!(open_object(peer), None);
        assert!(super::traffic_weight(0., 10.) > 0.);
    }

    /// Edge labels of the traffic layer sit clear of the node boxes and of
    /// each other; long ones are shortened, keeping the full text (#383).
    #[test]
    fn traffic_labels_stay_clear_of_nodes_and_each_other() {
        let data = graph_data(&traffic_graph(), MapOptions { groups: true });
        let geometry = kubach_ui::graph_geometry(&data);
        let labelled = data.edges.iter().filter(|e| e.label.is_some()).count();
        assert!(
            geometry.labels.len() + 1 >= labelled,
            "{} of {labelled} labels placed",
            geometry.labels.len()
        );
        for label in &geometry.labels {
            for node in &geometry.nodes {
                assert!(
                    !label.rect.intersects(node),
                    "{:?} covers {node:?}",
                    label.text
                );
            }
            assert!(
                label.full.starts_with(label.text.trim_end_matches('…')),
                "{:?} is not a start of {:?}",
                label.text,
                label.full
            );
        }
        for (i, a) in geometry.labels.iter().enumerate() {
            for b in &geometry.labels[i + 1..] {
                assert!(
                    !a.rect.intersects(&b.rect),
                    "{:?} and {:?} overlap",
                    a.text,
                    b.text
                );
            }
        }
        // The failing edge keeps at least its request rate.
        assert!(
            geometry
                .labels
                .iter()
                .any(|l| l.full.starts_with("42 req/s") && l.text.starts_with("42 req/s"))
        );
    }

    /// Writes the traffic graph as a PNG (Kubach Dark colors, the canvas
    /// layout) to `$KUBACH_TRAFFIC_PNG`, to look at the layer. Ignored:
    /// `KUBACH_TRAFFIC_PNG=/tmp/traffic.png cargo test -p kubach-app
    /// traffic_graph_image -- --ignored`.
    #[test]
    #[ignore]
    fn traffic_graph_image() {
        use gpui_kit::hsla;
        use kubach_ui::theme::{DARK, traffic_palette_of};
        let path = std::env::var("KUBACH_TRAFFIC_PNG").expect("KUBACH_TRAFFIC_PNG");
        let data = graph_data(&traffic_graph(), MapOptions { groups: true });
        let gray = |l: f32| hsla(0., 0., l, 1.);
        let palette = kubach_ui::SvgPalette {
            background: gray(0.04),
            node: gray(0.09),
            node_border: hsla(0., 0., 1., 0.14),
            text: gray(0.93),
            muted: gray(0.63),
            edge: hsla(0., 0., 1., 0.24),
            group: hsla(0., 0., 1., 0.03),
            group_border: hsla(0., 0., 1., 0.12),
            success: hsla(0.39, 0.7, 0.45, 1.),
            warning: hsla(0.1, 0.9, 0.55, 1.),
            danger: hsla(0., 0.85, 0.6, 1.),
            info: hsla(0.59, 1., 0.6, 1.),
            traffic: traffic_palette_of(DARK),
            font: "Geist".into(),
        };
        let svg = kubach_ui::graph_svg(&data, &palette);
        let png = kubach_ui::svg_to_png(&svg, 2.).unwrap();
        std::fs::write(&path, png).unwrap();
    }

    #[test]
    fn neighbors_within_depth_and_reveal() {
        let graph = build(
            &input(),
            &BuildOptions {
                detail: Detail::Workloads,
                ..BuildOptions::default()
            },
        );
        let service = NodeId::object("Service", "", Some("shop"), "web");
        let one = near(&graph, &service, 1);
        assert!(one.contains(&NodeId::object(
            "Ingress",
            "networking.k8s.io",
            Some("shop"),
            "web"
        )));
        assert!(!one.contains(&NodeId::object("ConfigMap", "", Some("shop"), "web-config")));
        let two = near(&graph, &service, 2);
        assert!(two.contains(&NodeId::object("ConfigMap", "", Some("shop"), "web-config")));
        let ghost = graph
            .node(&NodeId::object("Service", "", Some("shop"), "ghost"))
            .unwrap();
        assert_eq!(reveal_of(ghost), None);
        assert_eq!(open_object(ghost), None);
        let reveal = reveal_of(graph.node(&service).unwrap()).unwrap();
        assert_eq!(
            (reveal.kind.as_str(), reveal.group.as_str()),
            ("Service", "")
        );
    }

    /// What "Open details" opens (#394): the node's object by kind,
    /// group, namespace and name; external addresses open nothing.
    #[test]
    fn nodes_with_an_object_open_their_details() {
        let graph = build(
            &input(),
            &BuildOptions {
                detail: Detail::Workloads,
                ..BuildOptions::default()
            },
        );
        let deployment = graph
            .node(&NodeId::object("Deployment", "apps", Some("shop"), "web"))
            .unwrap();
        assert_eq!(
            open_object(deployment),
            Some(OpenObject {
                group: "apps".into(),
                kind: "Deployment".into(),
                namespace: Some("shop".into()),
                name: "web".into(),
            })
        );
        let service = graph
            .node(&NodeId::object("Service", "", Some("shop"), "web"))
            .unwrap();
        assert_eq!(
            open_object(service).map(|o| (o.group, o.kind)),
            Some((String::new(), "Service".into()))
        );
        let mut external = service.clone();
        external.kind = kubach_core::topology::NodeKind::External(
            kubach_core::topology::ExternalKind::Hostname,
        );
        external.namespace = None;
        assert_eq!(open_object(&external), None);
    }
}
