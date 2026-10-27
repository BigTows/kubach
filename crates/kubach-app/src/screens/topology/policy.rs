//! The network policy layer of the topology screen (#325): what the details
//! of a pod or workload say about its NetworkPolicies, and the "Can reach…"
//! query, on top of `kubach_core::topology::netpol`.

use kubach_core::topology::netpol::{Direction, PodGroup, PolicySet, Target, Verdict};
use kubach_core::topology::{EdgeKind, Node, NodeClass, NodeId, PortRef, Protocol, TopologyGraph};
use kubach_i18n::t;

/// The caveat shown wherever the layer is.
pub fn scope_note() -> String {
    t!("topology-policies-scope")
}

/// Parses a port query: `8080`, `http`, `53/udp`, `udp 53`, `TCP/443`.
pub fn parse_port(text: &str) -> Result<(PortRef, Protocol), String> {
    let parts: Vec<&str> = text
        .split(|c: char| c == '/' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let (port, protocol) = match parts.as_slice() {
        [] => return Err(t!("topology-port-empty")),
        [port] => (*port, Protocol::Tcp),
        [a, b] => match (Protocol::parse(a), Protocol::parse(b)) {
            (Some(protocol), None) => (*b, protocol),
            (None, Some(protocol)) => (*a, protocol),
            _ => return Err(t!("topology-port-invalid", text = text)),
        },
        _ => return Err(t!("topology-port-invalid", text = text)),
    };
    let port = match port.parse::<i32>() {
        Ok(number) if (1..=65535).contains(&number) => PortRef::Number(number),
        Ok(_) => return Err(t!("topology-port-out-of-range", port = port)),
        Err(_) if port.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
            PortRef::Name(port.to_owned())
        }
        Err(_) => return Err(t!("topology-port-invalid", text = port)),
    };
    Ok((port, protocol))
}

/// What "Can reach…" can point at: pods, workloads and Services other than
/// `source`, as `(label, id)` sorted by label.
pub fn targets(graph: &TopologyGraph, source: &NodeId) -> Vec<(String, NodeId)> {
    let mut out: Vec<(String, NodeId)> = graph
        .nodes()
        .iter()
        .filter(|node| &node.id != source && node.namespace.is_some())
        .filter(|node| {
            matches!(
                node.kind.class(),
                NodeClass::Pod | NodeClass::Workload | NodeClass::Service
            )
        })
        .map(|node| (node.describe(), node.id.clone()))
        .collect();
    out.sort();
    out
}

/// One answer of [`reach`]: for a Service target, per pod behind it.
pub struct Reach {
    /// The pods checked when the target is a Service (`Deployment a/web`).
    pub via: Option<String>,
    pub verdict: Verdict,
}

/// Whether `source`'s pods can open a connection to `target` on `port`. A
/// Service is checked at the pods it selects, its port mapped to their
/// target port; empty when it selects none.
pub fn reach(
    graph: &TopologyGraph,
    policies: &PolicySet,
    source: &Node,
    target: &Node,
    port: &PortRef,
    protocol: Protocol,
) -> Vec<Reach> {
    let Some(from) = PodGroup::of(source).map(Target::Pods) else {
        return Vec::new();
    };
    if let Some(to) = PodGroup::of(target) {
        return vec![Reach {
            via: None,
            verdict: policies.can_reach(&from, &Target::Pods(to), port, protocol),
        }];
    }
    let Some(service) = &target.attrs.service else {
        return Vec::new();
    };
    // The Service port asked for, by number or name, and what it maps to.
    let mapped = service.ports.iter().find(|candidate| match port {
        PortRef::Number(number) => candidate.port == *number,
        PortRef::Name(name) => candidate.name.as_deref() == Some(name),
    });
    let (backend_port, backend_protocol) = match mapped {
        Some(mapped) => (
            mapped
                .target_port
                .clone()
                .unwrap_or(PortRef::Number(mapped.port)),
            mapped
                .protocol
                .as_deref()
                .and_then(Protocol::parse)
                .unwrap_or_default(),
        ),
        None => (port.clone(), protocol),
    };
    graph
        .edges_from(&target.id)
        .filter(|edge| edge.kind == EdgeKind::Selects)
        .filter_map(|edge| graph.node(&edge.to))
        .filter_map(|backend| {
            let to = PodGroup::of(backend)?;
            Some(Reach {
                via: Some(backend.describe()),
                verdict: policies.can_reach(
                    &from,
                    &Target::Pods(to),
                    &backend_port,
                    backend_protocol,
                ),
            })
        })
        .collect()
}

/// `Ingress: isolated by a/deny`, `Egress: not isolated, all allowed`.
pub fn isolation_summary(policies: &PolicySet, pods: &PodGroup, direction: Direction) -> String {
    let policy = policies.node_policy(pods);
    let ids = match direction {
        Direction::Ingress => policy.ingress,
        Direction::Egress => policy.egress,
    };
    if ids.is_empty() {
        t!("topology-not-isolated", direction = direction.to_string())
    } else {
        let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
        t!("topology-isolated-by", policies = names.join(", "))
    }
}

/// Rules of a policy for the pods' details: `ingress: from … · TCP 80`,
/// `egress: deny all`.
pub fn policy_lines(policies: &PolicySet, pods: &PodGroup) -> Vec<(String, Vec<String>)> {
    policies
        .selecting(pods)
        .map(|policy| {
            let mut lines = Vec::new();
            for direction in [Direction::Ingress, Direction::Egress] {
                let Some(rules) = policy.rules(direction) else {
                    continue;
                };
                if rules.is_empty() {
                    lines.push(t!(
                        "topology-policy-deny-all",
                        direction = direction.to_string()
                    ));
                }
                for rule in rules {
                    lines.push(format!("{direction}: {}", rule.describe(direction)));
                }
            }
            (policy.id.to_string(), lines)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::topology::netpol::PolicySet;
    use kubach_core::topology::{
        BuildOptions, NodeId, PortRef, Protocol, SourceKind, TopologyInput, build,
    };
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{parse_port, policy_lines, reach, targets};

    #[test]
    fn parses_port_queries() {
        assert_eq!(
            parse_port("8080"),
            Ok((PortRef::Number(8080), Protocol::Tcp))
        );
        assert_eq!(
            parse_port("53/udp"),
            Ok((PortRef::Number(53), Protocol::Udp))
        );
        assert_eq!(
            parse_port("UDP 53"),
            Ok((PortRef::Number(53), Protocol::Udp))
        );
        assert_eq!(
            parse_port(" http "),
            Ok((PortRef::Name("http".into()), Protocol::Tcp))
        );
        assert!(parse_port("").is_err());
        assert!(parse_port("70000").is_err());
        assert!(parse_port("80/icmp").is_err());
    }

    fn object(value: serde_json::Value) -> Arc<DynamicObject> {
        Arc::new(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn reaches_a_service_through_its_pods() {
        let mut input = TopologyInput::new();
        for kind in SourceKind::every() {
            input.set(kind, Vec::<Arc<DynamicObject>>::new());
        }
        let deployment = |name: &str| {
            object(json!({
                "apiVersion": "apps/v1", "kind": "Deployment",
                "metadata": {"name": name, "namespace": "a"},
                "spec": {"selector": {"matchLabels": {"app": name}},
                    "template": {"metadata": {"labels": {"app": name}}, "spec": {"containers": [{
                        "name": name, "ports": [{"name": "http", "containerPort": 8080}]
                    }]}}}
            }))
        };
        input.set(
            SourceKind::Deployment,
            vec![deployment("web"), deployment("client")],
        );
        input.set(
            SourceKind::Service,
            vec![object(json!({
                "apiVersion": "v1", "kind": "Service",
                "metadata": {"name": "web", "namespace": "a"},
                "spec": {"selector": {"app": "web"}, "ports": [{"port": 80, "targetPort": "http"}]}
            }))],
        );
        input.set(
            SourceKind::NetworkPolicy,
            vec![object(json!({
                "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "web", "namespace": "a"},
                "spec": {"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{
                    "from": [{"podSelector": {"matchLabels": {"app": "client"}}}],
                    "ports": [{"port": "http"}]
                }]}
            }))],
        );
        let graph = build(
            &input,
            &BuildOptions {
                network_policies: true,
                ..BuildOptions::default()
            },
        );
        let policies = PolicySet::from_input(&input);
        let client = graph
            .node(&NodeId::object("Deployment", "apps", Some("a"), "client"))
            .unwrap();
        let service = graph
            .node(&NodeId::object("Service", "", Some("a"), "web"))
            .unwrap();
        let answers = reach(
            &graph,
            &policies,
            client,
            service,
            &PortRef::Number(80),
            Protocol::Tcp,
        );
        assert_eq!(answers.len(), 1);
        assert!(answers[0].verdict.allowed);
        assert_eq!(answers[0].verdict.port, Some(8080));
        assert_eq!(answers[0].via.as_deref(), Some("Deployment a/web"));
        // The other way round web isn't admitted: client isn't isolated.
        let web = graph
            .node(&NodeId::object("Deployment", "apps", Some("a"), "web"))
            .unwrap();
        let back = reach(
            &graph,
            &policies,
            web,
            client,
            &PortRef::Number(8080),
            Protocol::Tcp,
        );
        assert!(back[0].verdict.allowed);
        let labels: Vec<String> = targets(&graph, &client.id)
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(labels, ["Deployment a/web", "Service a/web"]);
        let lines = policy_lines(
            &policies,
            &kubach_core::topology::netpol::PodGroup::of(web).unwrap(),
        );
        assert_eq!(lines[0].0, "a/web");
        assert_eq!(
            lines[0].1,
            ["ingress: from pods app=client (same namespace) · TCP http"]
        );
    }
}
