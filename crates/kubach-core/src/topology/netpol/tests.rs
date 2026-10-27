use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use kube::api::DynamicObject;
use serde_json::{Value, json};

use super::{Cidr, Direction, PodGroup, Policy, PolicySet, Selector, Target, Verdict};
use crate::topology::{
    Access, BuildOptions, ContainerPort, EdgeKind, NodeId, PolicyId, PortRef, Protocol, SourceKind,
    TopologyInput, build,
};

fn policy(namespace: &str, name: &str, spec: Value) -> Policy {
    Policy::from_spec(
        PolicyId {
            namespace: namespace.into(),
            name: name.into(),
        },
        &spec,
    )
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn pods(namespace: &str, pairs: &[(&str, &str)]) -> Target {
    Target::Pods(PodGroup {
        namespace: namespace.into(),
        labels: labels(pairs),
        ports: vec![
            ContainerPort {
                name: Some("http".into()),
                port: 8080,
                protocol: Protocol::Tcp,
            },
            ContainerPort {
                name: Some("metrics".into()),
                port: 9090,
                protocol: Protocol::Tcp,
            },
        ],
    })
}

fn set(policies: Vec<Policy>) -> PolicySet {
    PolicySet::new(
        policies,
        [
            ("a".to_owned(), labels(&[("team", "a")])),
            ("b".to_owned(), labels(&[("team", "b"), ("env", "prod")])),
            ("c".to_owned(), labels(&[("team", "c"), ("env", "prod")])),
        ],
    )
}

fn reach(set: &PolicySet, from: &Target, to: &Target, port: i32) -> Verdict {
    set.can_reach(from, to, &PortRef::Number(port), Protocol::Tcp)
}

fn names(ids: &[PolicyId]) -> Vec<&str> {
    ids.iter().map(|id| id.name.as_str()).collect()
}

#[test]
fn no_policies_allow_everything() {
    let set = set(vec![]);
    let verdict = reach(&set, &pods("a", &[("app", "x")]), &pods("b", &[]), 80);
    assert!(verdict.allowed);
    assert!(!verdict.ingress_side.isolated());
    assert!(!verdict.egress_side.isolated());
    assert!(verdict.ingress_side.notes[0].contains("all ingress traffic is allowed"));
}

#[test]
fn default_deny_ingress() {
    let set = set(vec![policy(
        "a",
        "deny",
        json!({"podSelector": {}, "policyTypes": ["Ingress"]}),
    )]);
    let web = pods("a", &[("app", "web")]);
    let verdict = reach(&set, &pods("a", &[("app", "client")]), &web, 8080);
    assert!(!verdict.allowed);
    assert!(verdict.egress_side.allowed);
    assert_eq!(names(&verdict.ingress_side.isolated_by), ["deny"]);
    assert!(verdict.ingress_side.allowed_by.is_empty());
    assert!(
        verdict
            .ingress_side
            .notes
            .iter()
            .any(|note| note.contains("a/deny deny all ingress"))
    );
    // Egress of the web pods is not restricted.
    assert!(reach(&set, &web, &pods("b", &[]), 80).allowed);
    // Policy types default to Ingress when not set.
    let implicit = policy("a", "deny", json!({"podSelector": {}}));
    assert!(implicit.ingress.as_ref().is_some_and(Vec::is_empty));
    assert!(implicit.egress.is_none());
}

#[test]
fn default_deny_egress_hints_at_dns() {
    let set = set(vec![policy(
        "a",
        "deny-egress",
        json!({"podSelector": {}, "policyTypes": ["Egress"]}),
    )]);
    let verdict = reach(&set, &pods("a", &[("app", "web")]), &pods("b", &[]), 80);
    assert!(!verdict.allowed);
    assert!(verdict.ingress_side.allowed);
    assert_eq!(names(&verdict.egress_side.isolated_by), ["deny-egress"]);
    assert!(
        verdict
            .egress_side
            .notes
            .iter()
            .any(|note| note.contains("DNS"))
    );
    // With DNS allowed, there is no hint.
    let set = set_with_dns();
    let verdict = reach(&set, &pods("a", &[("app", "web")]), &pods("b", &[]), 80);
    assert!(!verdict.allowed);
    assert!(
        !verdict
            .egress_side
            .notes
            .iter()
            .any(|note| note.contains("DNS"))
    );
}

fn set_with_dns() -> PolicySet {
    set(vec![
        policy(
            "a",
            "deny-egress",
            json!({"podSelector": {}, "policyTypes": ["Egress"]}),
        ),
        policy(
            "a",
            "dns",
            json!({"podSelector": {}, "egress": [{
                "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                        "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                "ports": [{"port": 53, "protocol": "UDP"}, {"port": 53, "protocol": "TCP"}]
            }]}),
        ),
    ])
}

#[test]
fn egress_types_are_inferred_from_rules() {
    let with_rules = policy(
        "a",
        "out",
        json!({"podSelector": {}, "egress": [{"to": [{"ipBlock": {"cidr": "10.0.0.0/8"}}]}]}),
    );
    assert!(with_rules.ingress.as_ref().is_some_and(Vec::is_empty));
    assert_eq!(with_rules.egress.as_ref().map(Vec::len), Some(1));
}

#[test]
fn allow_from_namespace_by_label() {
    let set = set(vec![
        policy(
            "a",
            "deny",
            json!({"podSelector": {}, "policyTypes": ["Ingress"]}),
        ),
        policy(
            "a",
            "from-prod",
            json!({"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{
                "from": [{"namespaceSelector": {"matchLabels": {"env": "prod"}}}]
            }]}),
        ),
    ]);
    let web = pods("a", &[("app", "web")]);
    let verdict = reach(&set, &pods("b", &[("app", "any")]), &web, 8080);
    assert!(verdict.allowed);
    assert_eq!(names(&verdict.ingress_side.allowed_by), ["from-prod"]);
    assert_eq!(
        names(&verdict.ingress_side.isolated_by),
        ["deny", "from-prod"]
    );
    // Same namespace, but the namespace isn't `env=prod`.
    assert!(!reach(&set, &pods("a", &[("app", "x")]), &web, 8080).allowed);
    // By the name label every namespace has.
    let by_name = PolicySet::new(
        vec![policy(
            "a",
            "from-b",
            json!({"podSelector": {}, "ingress": [{
                "from": [{"namespaceSelector": {"matchExpressions": [
                    {"key": "kubernetes.io/metadata.name", "operator": "In", "values": ["b"]}
                ]}}]
            }]}),
        )],
        [],
    );
    assert!(reach(&by_name, &pods("b", &[]), &web, 80).allowed);
    assert!(!reach(&by_name, &pods("c", &[]), &web, 80).allowed);
}

#[test]
fn pod_and_namespace_selectors_in_one_peer_are_and_in_two_are_or() {
    let and = set(vec![policy(
        "a",
        "and",
        json!({"podSelector": {}, "ingress": [{"from": [{
            "namespaceSelector": {"matchLabels": {"team": "b"}},
            "podSelector": {"matchLabels": {"role": "client"}}
        }]}]}),
    )]);
    let or = set(vec![policy(
        "a",
        "or",
        json!({"podSelector": {}, "ingress": [{"from": [
            {"namespaceSelector": {"matchLabels": {"team": "b"}}},
            {"podSelector": {"matchLabels": {"role": "client"}}}
        ]}]}),
    )]);
    let web = pods("a", &[("app", "web")]);
    let client_b = pods("b", &[("role", "client")]);
    let other_b = pods("b", &[("role", "other")]);
    let client_a = pods("a", &[("role", "client")]);
    let client_c = pods("c", &[("role", "client")]);
    assert!(reach(&and, &client_b, &web, 80).allowed);
    assert!(!reach(&and, &other_b, &web, 80).allowed);
    assert!(!reach(&and, &client_a, &web, 80).allowed);
    assert!(reach(&or, &client_b, &web, 80).allowed);
    assert!(reach(&or, &other_b, &web, 80).allowed);
    // A podSelector alone means the policy's namespace.
    assert!(reach(&or, &client_a, &web, 80).allowed);
    assert!(!reach(&or, &client_c, &web, 80).allowed);
}

#[test]
fn ip_block_with_except() {
    let set = set(vec![policy(
        "a",
        "egress-cidr",
        json!({"podSelector": {}, "policyTypes": ["Egress"], "egress": [{
            "to": [{"ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.1.0.0/16"]}}]
        }]}),
    )]);
    let web = pods("a", &[]);
    let ip = |text: &str| Target::Address(text.parse::<IpAddr>().unwrap());
    let verdict = reach(&set, &web, &ip("10.2.3.4"), 443);
    assert!(verdict.allowed);
    assert!(verdict.ingress_side.notes[0].contains("Not a pod"));
    assert!(!reach(&set, &web, &ip("10.1.2.3"), 443).allowed);
    assert!(!reach(&set, &web, &ip("192.168.1.1"), 443).allowed);
    // ipBlock doesn't match pods.
    let to_pod = reach(&set, &web, &pods("b", &[]), 443);
    assert!(!to_pod.allowed);
    assert!(
        to_pod
            .egress_side
            .notes
            .iter()
            .any(|n| n.contains("ipBlock"))
    );
    let v6 = Cidr::parse("2001:db8::/32").unwrap();
    assert!(v6.contains("2001:db8::1".parse().unwrap()));
    assert!(!v6.contains("2001:db9::1".parse().unwrap()));
    assert!(!v6.contains("10.0.0.1".parse().unwrap()));
    assert!(
        Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains("8.8.8.8".parse().unwrap())
    );
    assert_eq!(Cidr::parse("10.0.0.0/33"), None);
}

#[test]
fn named_port_resolves_on_the_destination() {
    let set = set(vec![policy(
        "a",
        "http-only",
        json!({"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{
            "ports": [{"port": "http"}]
        }]}),
    )]);
    let web = pods("a", &[("app", "web")]);
    let client = pods("b", &[]);
    assert!(reach(&set, &client, &web, 8080).allowed);
    assert!(!reach(&set, &client, &web, 9090).allowed);
    // The query may name the port too.
    let named = set.can_reach(&client, &web, &PortRef::Name("http".into()), Protocol::Tcp);
    assert!(named.allowed);
    assert_eq!(named.port, Some(8080));
    let missing = set.can_reach(&client, &web, &PortRef::Name("grpc".into()), Protocol::Tcp);
    assert!(!missing.allowed);
    assert_eq!(missing.port, None);
    // UDP 8080 is another port.
    assert!(
        !set.can_reach(&client, &web, &PortRef::Number(8080), Protocol::Udp)
            .allowed
    );
}

#[test]
fn end_port_range() {
    let set = set(vec![policy(
        "a",
        "range",
        json!({"podSelector": {}, "ingress": [{
            "ports": [{"port": 8000, "endPort": 8100, "protocol": "TCP"}]
        }]}),
    )]);
    let web = pods("a", &[]);
    let client = pods("b", &[]);
    assert!(reach(&set, &client, &web, 8000).allowed);
    assert!(reach(&set, &client, &web, 8100).allowed);
    assert!(reach(&set, &client, &web, 8050).allowed);
    assert!(!reach(&set, &client, &web, 8101).allowed);
    assert!(!reach(&set, &client, &web, 7999).allowed);
}

#[test]
fn several_policies_are_a_union() {
    let set = set(vec![
        policy(
            "a",
            "from-b",
            json!({"podSelector": {}, "ingress": [{
                "from": [{"namespaceSelector": {"matchLabels": {"team": "b"}}}],
                "ports": [{"port": 80}]
            }]}),
        ),
        policy(
            "a",
            "metrics",
            json!({"podSelector": {}, "ingress": [{"ports": [{"port": 9090}]}]}),
        ),
    ]);
    let web = pods("a", &[]);
    let from_b = reach(&set, &pods("b", &[]), &web, 80);
    assert!(from_b.allowed);
    assert_eq!(names(&from_b.ingress_side.allowed_by), ["from-b"]);
    assert!(reach(&set, &pods("c", &[]), &web, 9090).allowed);
    assert!(!reach(&set, &pods("c", &[]), &web, 80).allowed);
    let union = reach(&set, &pods("b", &[]), &web, 9090);
    assert_eq!(names(&union.ingress_side.allowed_by), ["metrics"]);
}

#[test]
fn policy_in_another_namespace_does_not_apply() {
    let set = set(vec![policy(
        "b",
        "deny",
        json!({"podSelector": {}, "policyTypes": ["Ingress", "Egress"]}),
    )]);
    let a1 = pods("a", &[("app", "one")]);
    let a2 = pods("a", &[("app", "two")]);
    assert!(reach(&set, &a1, &a2, 80).allowed);
    assert!(!reach(&set, &a1, &pods("b", &[]), 80).allowed);
}

#[test]
fn both_sides_must_allow() {
    let set = set(vec![
        policy(
            "a",
            "egress-to-b",
            json!({"podSelector": {}, "egress": [{"to": [{"namespaceSelector": {"matchLabels": {"team": "b"}}}]}]}),
        ),
        policy(
            "b",
            "from-c",
            json!({"podSelector": {}, "ingress": [{"from": [{"namespaceSelector": {"matchLabels": {"team": "c"}}}]}]}),
        ),
    ]);
    let verdict = reach(&set, &pods("a", &[]), &pods("b", &[]), 80);
    assert!(!verdict.allowed);
    assert!(verdict.egress_side.allowed);
    assert!(!verdict.ingress_side.allowed);
}

#[test]
fn selector_expressions() {
    let selector = Selector::parse(&json!({
        "matchLabels": {"app": "web"},
        "matchExpressions": [
            {"key": "tier", "operator": "In", "values": ["front", "back"]},
            {"key": "legacy", "operator": "DoesNotExist"},
            {"key": "env", "operator": "NotIn", "values": ["dev"]}
        ]
    }));
    assert!(selector.matches(&labels(&[("app", "web"), ("tier", "front")])));
    assert!(!selector.matches(&labels(&[("app", "web"), ("tier", "db")])));
    assert!(!selector.matches(&labels(&[
        ("app", "web"),
        ("tier", "back"),
        ("legacy", "1")
    ])));
    assert!(!selector.matches(&labels(&[("app", "web"), ("tier", "back"), ("env", "dev")])));
    assert_eq!(
        selector.to_string(),
        "app=web,tier in (front,back),!legacy,env notin (dev)"
    );
    assert!(Selector::default().matches(&labels(&[("x", "y")])));
    let bad = Selector::parse(&json!({"matchExpressions": [{"key": "a", "operator": "Like"}]}));
    assert!(!bad.matches(&labels(&[("a", "b")])));
}

#[test]
fn admission_of_service_ports() {
    let set = set(vec![
        policy(
            "a",
            "deny",
            json!({"podSelector": {}, "policyTypes": ["Ingress"]}),
        ),
        policy(
            "a",
            "http-from-b",
            json!({"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{
                "from": [{"namespaceSelector": {"matchLabels": {"team": "b"}}}],
                "ports": [{"port": "http"}]
            }]}),
        ),
        policy(
            "a",
            "metrics",
            json!({"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{"ports": [{"port": 9090}]}]}),
        ),
    ]);
    let Target::Pods(web) = pods("a", &[("app", "web")]) else {
        unreachable!()
    };
    let Target::Pods(db) = pods("a", &[("app", "db")]) else {
        unreachable!()
    };
    let http = set.admission(&web, &[(8080, Protocol::Tcp)]);
    assert_eq!(http.access, Access::Restricted);
    assert_eq!(names(&http.policies), ["http-from-b"]);
    assert_eq!(
        set.admission(&web, &[(9090, Protocol::Tcp)]).access,
        Access::Allowed
    );
    let blocked = set.admission(&web, &[(5432, Protocol::Tcp)]);
    assert_eq!(blocked.access, Access::Blocked);
    assert_eq!(names(&blocked.policies), ["deny", "http-from-b", "metrics"]);
    assert_eq!(
        set.admission(&db, &[(5432, Protocol::Tcp)]).access,
        Access::Blocked
    );
    assert_eq!(
        set.admission(&web, &[(8080, Protocol::Tcp), (5432, Protocol::Tcp)])
            .access,
        Access::Restricted
    );
    assert_eq!(
        PolicySet::default().admission(&db, &[]).access,
        Access::Open
    );
}

#[test]
fn explanations_name_directions() {
    let set = set_with_dns();
    let verdict = reach(&set, &pods("a", &[]), &pods("b", &[]), 80);
    assert_eq!(verdict.egress_side.direction, Direction::Egress);
    assert_eq!(verdict.ingress_side.direction, Direction::Ingress);
    let rule = &set.policies()[1].egress.as_ref().unwrap()[0];
    assert_eq!(
        rule.describe(Direction::Egress),
        "to pods k8s-app=kube-dns in namespaces kubernetes.io/metadata.name=kube-system · UDP 53, TCP 53"
    );
}

fn object(value: Value) -> Arc<DynamicObject> {
    Arc::new(serde_json::from_value(value).unwrap())
}

/// `a`: web (port http 8080) behind Service web, a client and a db; a
/// default deny, web open to the client on http, db open to nobody.
fn layer_input() -> TopologyInput {
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
        vec![deployment("web"), deployment("client"), deployment("db")],
    );
    let service = |name: &str, target: Value| {
        object(json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": {"name": name, "namespace": "a"},
            "spec": {"selector": {"app": name}, "ports": [{"port": 80, "targetPort": target}]}
        }))
    };
    input.set(
        SourceKind::Service,
        vec![service("web", json!("http")), service("db", json!(5432))],
    );
    input.set(
        SourceKind::NetworkPolicy,
        vec![
            object(json!({
                "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "deny", "namespace": "a"},
                "spec": {"podSelector": {}, "policyTypes": ["Ingress"]}
            })),
            object(json!({
                "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "web-from-client", "namespace": "a"},
                "spec": {"podSelector": {"matchLabels": {"app": "web"}}, "ingress": [{
                    "from": [{"podSelector": {"matchLabels": {"app": "client"}}}],
                    "ports": [{"port": "http"}]
                }]}
            })),
        ],
    );
    input.set(
        SourceKind::Namespace,
        vec![object(json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": "a", "labels": {"team": "a"}}
        }))],
    );
    input
}

#[test]
fn layer_marks_isolation_service_edges_and_allowed_paths() {
    let input = layer_input();
    let graph = build(
        &input,
        &BuildOptions {
            network_policies: true,
            ..BuildOptions::default()
        },
    );
    let id = |kind: &str, group: &str, name: &str| NodeId::object(kind, group, Some("a"), name);
    let web = id("Deployment", "apps", "web");
    let db = id("Deployment", "apps", "db");
    let client = id("Deployment", "apps", "client");
    let node = |id: &NodeId| graph.node(id).unwrap();
    let web_policy = node(&web).attrs.network_policy.clone().unwrap();
    assert!(web_policy.isolated_ingress());
    assert!(!web_policy.isolated_egress());
    assert_eq!(names(&web_policy.ingress), ["deny", "web-from-client"]);
    assert_eq!(node(&web).attrs.container_ports[0].port, 8080);
    let selects = |service: &str, to: &NodeId| {
        graph
            .edges()
            .iter()
            .find(|edge| {
                edge.kind == EdgeKind::Selects
                    && edge.from == id("Service", "", service)
                    && &edge.to == to
            })
            .and_then(|edge| edge.attrs.policy.clone())
            .unwrap()
    };
    let to_web = selects("web", &web);
    assert_eq!(to_web.access, Access::Restricted);
    assert_eq!(to_web.ports, ["TCP 8080"]);
    let to_db = selects("db", &db);
    assert_eq!(to_db.access, Access::Blocked);
    assert_eq!(names(&to_db.policies), ["deny"]);
    let allows: Vec<_> = graph
        .edges()
        .iter()
        .filter(|edge| edge.kind == EdgeKind::PolicyAllows)
        .collect();
    assert_eq!(allows.len(), 1);
    assert_eq!((&allows[0].from, &allows[0].to), (&client, &web));
    let policy = allows[0].attrs.policy.as_ref().unwrap();
    assert_eq!(policy.ports, ["TCP 8080"]);
    assert_eq!(names(&policy.policies), ["web-from-client"]);

    // Without the option, nothing of the layer.
    let plain = build(&input, &BuildOptions::default());
    assert!(plain.node(&web).unwrap().attrs.network_policy.is_none());
    assert!(
        plain
            .edges()
            .iter()
            .all(|edge| edge.kind != EdgeKind::PolicyAllows && edge.attrs.policy.is_none())
    );
    // Namespace labels come from the input.
    let set = PolicySet::from_input(&input);
    assert_eq!(
        set.namespace_labels("a").get("team").map(String::as_str),
        Some("a")
    );
    assert_eq!(set.policies().len(), 2);
}
