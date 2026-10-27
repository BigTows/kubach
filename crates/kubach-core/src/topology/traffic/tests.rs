use std::sync::Arc;

use kube::api::DynamicObject;
use serde_json::json;

use super::{
    FlowKey, Hubble, Istio, Linkerd, MAX_FLOWS, NamespaceScope, PLUGINS, Peer, TrafficFlow,
    TrafficMetric, TrafficPlugin, TrafficSnapshot, TrafficWindow, apply, candidates, outside_id,
    parse, plugin, probe_confirms,
};
use crate::metrics::prometheus::usage::{Labels, parse_vector};
use crate::topology::{
    BuildOptions, Detail, EdgeKind, EdgeTraffic, ExternalKind, NodeId, NodeKind, SourceKind,
    TopologyGraph, TopologyInput, build, export,
};

/// The answers of a fixture, by metric (the keys are [`TrafficMetric`]
/// names), parsed as Prometheus vectors.
fn answers(fixture: &str) -> Vec<(TrafficMetric, Vec<(Labels, f64)>)> {
    let all: serde_json::Map<String, serde_json::Value> = serde_json::from_str(fixture).unwrap();
    all.into_iter()
        .map(|(name, body)| {
            let metric = match name.as_str() {
                "Requests" => TrafficMetric::Requests,
                "Errors" => TrafficMetric::Errors,
                "LatencyP95" => TrafficMetric::LatencyP95,
                "Bytes" => TrafficMetric::Bytes,
                "Flows" => TrafficMetric::Flows,
                "Dropped" => TrafficMetric::Dropped,
                other => panic!("unknown metric {other}"),
            };
            (metric, parse_vector(&body.to_string()).unwrap())
        })
        .collect()
}

fn flows(plugin: &dyn TrafficPlugin, fixture: &str) -> Vec<TrafficFlow> {
    let (flows, truncated) = parse(plugin, answers(fixture));
    assert!(!truncated);
    flows
}

fn key(source: Option<(&str, &str)>, destination: (&str, &str)) -> impl Fn(&TrafficFlow) -> bool {
    let source = source.map(|(ns, name)| Peer::new(ns, name));
    let destination = Peer::new(destination.0, destination.1);
    move |flow| flow.key.source == source && flow.key.destination == destination
}

fn find(flows: &[TrafficFlow], pred: impl Fn(&TrafficFlow) -> bool) -> &TrafficFlow {
    flows.iter().find(|flow| pred(flow)).expect("flow")
}

#[test]
fn istio_answers_become_flows() {
    let flows = flows(&Istio, include_str!("fixtures/istio.json"));
    // Busiest first; the idle flow, the unknown destination and the NaN
    // latency are left out.
    assert_eq!(flows.len(), 5);
    assert_eq!(flows[0].traffic.requests, Some(120.));
    let cart = find(&flows, key(Some(("shop", "frontend")), ("shop", "cart")));
    assert_eq!(cart.traffic.requests, Some(42.5));
    assert_eq!(cart.traffic.errors, Some(4.25));
    assert_eq!(cart.traffic.error_ratio(), Some(0.1));
    assert_eq!(cart.traffic.p95_ms, Some(87.5));
    assert_eq!(cart.key.service, Some(Peer::new("shop", "cart")));
    let gateway = find(
        &flows,
        key(
            Some(("istio-system", "istio-ingressgateway")),
            ("shop", "frontend"),
        ),
    );
    assert_eq!(gateway.traffic.p95_ms, None);
    // A caller outside the mesh has no source.
    let outside = find(&flows, key(None, ("shop", "frontend")));
    assert_eq!(outside.traffic.requests, Some(3.));
    // Passthrough traffic names no Service.
    let legacy = find(&flows, key(Some(("shop", "frontend")), ("shop", "legacy")));
    assert_eq!(legacy.key.service, None);
    // TCP only: bytes, no requests.
    let redis = find(&flows, key(Some(("shop", "cart")), ("shop", "redis")));
    assert_eq!(redis.traffic.requests, None);
    assert_eq!(redis.traffic.bytes, Some(20480.));
    assert_eq!(export::traffic_text(&redis.traffic), "20Ki/s");
}

#[test]
fn linkerd_answers_become_flows() {
    let flows = flows(&Linkerd, include_str!("fixtures/linkerd.json"));
    assert_eq!(flows.len(), 3);
    // `dst_service` as a name and as an FQDN is the same Service: summed.
    let cart = find(&flows, key(Some(("shop", "frontend")), ("shop", "cart")));
    assert_eq!(cart.traffic.requests, Some(40.));
    assert_eq!(cart.traffic.errors, Some(0.4));
    assert_eq!(cart.traffic.p95_ms, Some(25.));
    assert_eq!(cart.key.service, Some(Peer::new("shop", "cart")));
    assert_eq!(
        export::traffic_text(&cart.traffic),
        "40 req/s · 1.0% err · p95 25 ms"
    );
    // StatefulSets name the workload by their own label.
    let db = find(&flows, key(Some(("shop", "worker")), ("shop", "db")));
    assert_eq!(db.traffic.requests, Some(5.));
    assert_eq!(db.key.service, None);
    // An unmeshed caller (no workload label) has no source.
    let unmeshed = find(&flows, key(None, ("shop", "frontend")));
    assert_eq!(unmeshed.traffic.requests, Some(2.));
}

#[test]
fn hubble_answers_become_flows() {
    let flows = flows(&Hubble, include_str!("fixtures/hubble.json"));
    assert_eq!(flows.len(), 4);
    let cart = find(&flows, key(Some(("shop", "frontend")), ("shop", "cart")));
    assert_eq!(cart.traffic.requests, Some(8.));
    assert_eq!(cart.traffic.error_ratio(), Some(0.25));
    assert_eq!(cart.traffic.p95_ms, Some(240.));
    assert_eq!(cart.traffic.flows, Some(16.));
    // L4 only: flows and drops.
    let redis = find(&flows, key(Some(("shop", "cart")), ("shop", "redis")));
    assert_eq!(redis.traffic.requests, None);
    assert_eq!(redis.traffic.load(), 8.);
    assert_eq!(redis.traffic.error_ratio(), Some(0.25));
    assert_eq!(
        export::traffic_text(&redis.traffic),
        "6 flows/s · 25% dropped"
    );
    // Only dropped: still drawn.
    let ledger = find(
        &flows,
        key(Some(("shop", "frontend")), ("billing", "ledger")),
    );
    assert_eq!(ledger.traffic.error_ratio(), Some(1.));
    // The world or the host: no source.
    assert_eq!(
        find(&flows, key(None, ("shop", "frontend"))).traffic.flows,
        Some(1.5)
    );
    assert!(flows.iter().all(|flow| flow.key.service.is_none()));
}

#[test]
fn queries_aggregate_by_workload_pair_in_the_shown_namespaces() {
    let scope = NamespaceScope::of(Some(&["shop".to_owned(), "a.b".to_owned()]));
    for plugin in PLUGINS {
        let queries = plugin.queries(&scope, TrafficWindow::FiveMinutes);
        assert!(
            queries.iter().any(|q| q.metric.required()),
            "{}",
            plugin.id()
        );
        for query in &queries {
            assert!(query.promql.contains("sum by ("), "{}", query.promql);
            assert!(query.promql.contains("[5m]"), "{}", query.promql);
            // Either end in the shown namespaces.
            assert!(query.promql.contains(" or "), "{}", query.promql);
            assert!(
                query.promql.contains(r#"=~"a\\.b|shop""#),
                "{}",
                query.promql
            );
        }
    }
    let all = Istio.queries(&NamespaceScope::All, TrafficWindow::OneHour);
    assert_eq!(
        all[0].promql,
        r#"sum by (source_workload_namespace, source_workload, destination_workload_namespace, destination_workload, destination_service_namespace, destination_service_name) (rate(istio_requests_total{reporter="destination"}[1h]))"#
    );
    assert!(
        all[2]
            .promql
            .starts_with("histogram_quantile(0.95, sum by (le, ")
    );
    let hubble = Hubble.queries(&NamespaceScope::All, TrafficWindow::OneMinute);
    assert!(hubble[2].promql.ends_with("* 1000"));
}

#[test]
fn detection_reads_discovery_groups_only() {
    let ids = |groups: &[&str]| -> Vec<&str> {
        candidates(groups.iter().copied())
            .into_iter()
            .map(|p| p.id())
            .collect()
    };
    assert!(ids(&["apps", "networking.k8s.io"]).is_empty());
    assert_eq!(ids(&["networking.istio.io"]), ["istio"]);
    assert_eq!(ids(&["policy.linkerd.io"]), ["linkerd"]);
    assert_eq!(ids(&["cilium.io"]), ["hubble"]);
    // Order of preference: a mesh before Hubble.
    assert_eq!(
        ids(&["cilium.io", "linkerd.io", "security.istio.io"]),
        ["istio", "linkerd", "hubble"]
    );
    assert_eq!(plugin("linkerd").map(|p| p.name()), Some("Linkerd"));
    assert!(plugin("nope").is_none());
    let count = |value: f64| vec![(Labels::new(), value)];
    assert!(probe_confirms(&count(12.)));
    assert!(!probe_confirms(&count(0.)));
    assert!(!probe_confirms(&[]));
}

#[test]
fn flows_are_capped_busiest_first() {
    let samples: Vec<(Labels, f64)> = (0..MAX_FLOWS + 10)
        .map(|i| {
            let labels: Labels = [
                ("source_namespace", "a".to_owned()),
                ("source_workload", format!("w{i}")),
                ("destination_namespace", "a".to_owned()),
                ("destination_workload", "db".to_owned()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
            (labels, i as f64 + 1.)
        })
        .collect();
    let (flows, truncated) = parse(&Hubble, vec![(TrafficMetric::Flows, samples)]);
    assert!(truncated);
    assert_eq!(flows.len(), MAX_FLOWS);
    assert_eq!(flows[0].traffic.flows, Some((MAX_FLOWS + 10) as f64));
}

fn object(value: serde_json::Value) -> Arc<DynamicObject> {
    Arc::new(serde_json::from_value(value).unwrap())
}

fn deployment(name: &str) -> Arc<DynamicObject> {
    object(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name, "namespace": "shop", "uid": name},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": name}},
            "template": {"metadata": {"labels": {"app": name}},
                "spec": {"containers": [{"name": name}]}}},
        "status": {"replicas": 1, "readyReplicas": 1}
    }))
}

fn service(name: &str, app: &str) -> Arc<DynamicObject> {
    object(json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": name, "namespace": "shop"},
        "spec": {"selector": {"app": app}, "ports": [{"port": 80}]}
    }))
}

/// Services frontend, cart (selecting cart) and redis; Deployments
/// frontend, cart, redis and legacy (no Service).
fn shop(detail: Detail) -> TopologyGraph {
    let mut input = TopologyInput::new();
    for kind in SourceKind::ALL {
        input.set(kind, Vec::new());
    }
    input.set(
        SourceKind::Deployment,
        ["frontend", "cart", "redis", "legacy"]
            .map(deployment)
            .to_vec(),
    );
    input.set(
        SourceKind::Service,
        vec![
            service("frontend", "frontend"),
            service("cart", "cart"),
            service("redis", "redis"),
        ],
    );
    build(
        &input,
        &BuildOptions {
            detail,
            ..BuildOptions::default()
        },
    )
}

fn snapshot(flows: Vec<TrafficFlow>) -> TrafficSnapshot {
    TrafficSnapshot {
        source: "Istio",
        window: TrafficWindow::FiveMinutes,
        flows,
        truncated: false,
        partial: Vec::new(),
    }
}

fn workload(name: &str) -> NodeId {
    NodeId::object("Deployment", "apps", Some("shop"), name)
}

fn svc(name: &str) -> NodeId {
    NodeId::object("Service", "", Some("shop"), name)
}

#[test]
fn traffic_lays_over_the_structure() {
    let flows = flows(&Istio, include_str!("fixtures/istio.json"));
    let graph = apply(shop(Detail::Workloads), &snapshot(flows));
    let traffic = |from: &NodeId, to: &NodeId| {
        graph
            .edges_from(from)
            .find(|e| e.kind == EdgeKind::Traffic && &e.to == to)
            .and_then(|e| e.attrs.traffic.clone())
    };
    // Through the named Service: frontend → Service cart, and the
    // Service's edge to its pods carries the numbers too.
    let to_cart = traffic(&workload("frontend"), &svc("cart")).unwrap();
    assert_eq!(to_cart.requests, Some(42.5));
    assert!(!to_cart.unmapped);
    let selects = graph
        .edges_from(&svc("cart"))
        .find(|e| e.kind == EdgeKind::Selects)
        .unwrap();
    assert_eq!(
        selects.attrs.traffic.as_ref().and_then(|t| t.requests),
        Some(42.5)
    );
    assert_eq!(
        export::edge_label(selects).as_deref(),
        Some("42 req/s · 10% err · p95 88 ms")
    );
    // No Service selects legacy: drawn straight, unmapped.
    let legacy = traffic(&workload("frontend"), &workload("legacy")).unwrap();
    assert!(legacy.unmapped);
    // The gateway's namespace is not shown: a peer node of its own.
    let gateway = NodeId::object("Peer", "", Some("istio-system"), "istio-ingressgateway");
    let node = graph.node(&gateway).unwrap();
    assert_eq!(node.kind, NodeKind::External(ExternalKind::Peer));
    assert_eq!(node.namespace.as_deref(), Some("istio-system"));
    assert_eq!(
        traffic(&gateway, &svc("frontend")).and_then(|t| t.requests),
        Some(120.)
    );
    // Callers outside the mesh share one node.
    assert!(graph.node(&outside_id()).is_some());
    assert!(traffic(&outside_id(), &svc("frontend")).is_some());
    // TCP to redis goes through its Service.
    assert_eq!(
        traffic(&workload("cart"), &svc("redis")).and_then(|t| t.bytes),
        Some(20480.)
    );
    // The structure is untouched.
    let base = shop(Detail::Workloads);
    for edge in base.edges() {
        assert!(graph.edges().iter().any(|e| e.id == edge.id));
    }
}

#[test]
fn flows_without_a_named_service_use_the_only_one_selecting() {
    let flows = flows(&Hubble, include_str!("fixtures/hubble.json"));
    let graph = apply(shop(Detail::Workloads), &snapshot(flows));
    // Hubble names no Service; cart has exactly one.
    assert!(
        graph
            .edges_from(&workload("frontend"))
            .any(|e| e.kind == EdgeKind::Traffic && e.to == svc("cart"))
    );
    // A destination in another namespace becomes a peer.
    let ledger = NodeId::object("Peer", "", Some("billing"), "ledger");
    let edge = graph
        .edges_from(&workload("frontend"))
        .find(|e| e.kind == EdgeKind::Traffic && e.to == ledger)
        .unwrap();
    assert!(edge.attrs.traffic.as_ref().unwrap().unmapped);
}

#[test]
fn pods_detail_keeps_traffic_on_workloads() {
    let flows = flows(&Linkerd, include_str!("fixtures/linkerd.json"));
    let graph = apply(shop(Detail::Pods), &snapshot(flows));
    // Service cart selects pods of cart: still known to select the
    // workload, so the flow is not unmapped.
    let edge = graph
        .edges_from(&workload("frontend"))
        .find(|e| e.kind == EdgeKind::Traffic)
        .unwrap();
    assert_eq!(edge.to, svc("cart"));
    assert!(!edge.attrs.traffic.as_ref().unwrap().unmapped);
}

#[test]
fn an_empty_snapshot_changes_nothing() {
    let graph = shop(Detail::Workloads);
    assert_eq!(apply(graph.clone(), &snapshot(Vec::new())), graph);
}

#[test]
fn merging_adds_rates_and_keeps_the_slowest_p95() {
    let mut a = EdgeTraffic {
        requests: Some(10.),
        errors: Some(1.),
        p95_ms: Some(20.),
        ..EdgeTraffic::default()
    };
    a.merge(&EdgeTraffic {
        requests: Some(30.),
        p95_ms: Some(50.),
        bytes: Some(10.),
        unmapped: true,
        ..EdgeTraffic::default()
    });
    assert_eq!(a.requests, Some(40.));
    assert_eq!(a.errors, Some(1.));
    assert_eq!(a.p95_ms, Some(50.));
    assert_eq!(a.bytes, Some(10.));
    assert!(a.unmapped);
    assert_eq!(a.error_ratio(), Some(0.025));
    assert_eq!(export::rate_text(1234.), "1.2k");
    assert_eq!(export::rate_text(0.04), "0.04");
    assert_eq!(export::traffic_text(&EdgeTraffic::default()), "idle");
    let rare = EdgeTraffic {
        requests: Some(1000.),
        errors: Some(0.2),
        ..EdgeTraffic::default()
    };
    assert_eq!(export::traffic_text(&rare), "1k req/s · <0.1% err");
    let key = FlowKey {
        source: None,
        destination: Peer::new("a", "b"),
        service: None,
    };
    assert_eq!(key.clone(), key);
}
