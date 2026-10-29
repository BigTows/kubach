use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kube::api::DynamicObject;
use serde_json::{Value, json};

use super::*;
use crate::apply::yaml;

const APP: &str = include_str!("fixtures/app.yaml");
const STATEFULSET: &str = include_str!("fixtures/statefulset.yaml");
const GATEWAY: &str = include_str!("fixtures/gateway.yaml");
const BROKEN: &str = include_str!("fixtures/broken.yaml");

fn objects(text: &str) -> Vec<DynamicObject> {
    yaml::parse(text)
        .unwrap()
        .into_iter()
        .map(|manifest| serde_json::from_value(manifest.value).unwrap())
        .collect()
}

fn fixture(text: &str) -> TopologyInput {
    TopologyInput::from_objects(objects(text))
}

fn graph(text: &str, detail: Detail) -> TopologyGraph {
    build(
        &fixture(text),
        &BuildOptions {
            detail,
            ..BuildOptions::default()
        },
    )
}

fn id(text: &str) -> NodeId {
    let (kind, rest) = text.split_once('/').unwrap();
    let (namespace, name) = rest.split_once('/').unwrap();
    let (kind, group) = kind.split_once('.').unwrap_or((kind, ""));
    let namespace = (!namespace.is_empty()).then_some(namespace);
    NodeId::object(kind, group, namespace, name)
}

fn edge<'a>(graph: &'a TopologyGraph, kind: EdgeKind, from: &str, to: &str) -> &'a Edge {
    let (from, to) = (id(from), id(to));
    graph
        .edges()
        .iter()
        .find(|edge| edge.kind == kind && edge.from == from && edge.to == to)
        .unwrap_or_else(|| panic!("no {kind:?} edge {from} -> {to}; edges: {:#?}", ids(graph)))
}

fn has_edge(graph: &TopologyGraph, kind: EdgeKind, from: &str, to: &str) -> bool {
    let (from, to) = (id(from), id(to));
    graph
        .edges()
        .iter()
        .any(|edge| edge.kind == kind && edge.from == from && edge.to == to)
}

fn ids(graph: &TopologyGraph) -> Vec<String> {
    graph
        .edges()
        .iter()
        .map(|edge| edge.id.to_string())
        .collect()
}

fn node<'a>(graph: &'a TopologyGraph, text: &str) -> &'a Node {
    graph
        .node(&id(text))
        .unwrap_or_else(|| panic!("no node {text}"))
}

fn messages(graph: &TopologyGraph) -> Vec<String> {
    graph
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message(graph))
        .collect()
}

#[test]
fn typical_app_by_workload() {
    let graph = graph(APP, Detail::Workloads);

    let web = node(&graph, "Deployment.apps/shop/web");
    assert_eq!(web.kind, NodeKind::Workload(WorkloadKind::Deployment));
    assert_eq!(web.attrs.pods, Some(PodCounts { total: 2, ready: 1 }));
    assert_eq!(web.attrs.desired_replicas, Some(2));
    // ReplicaSets and pods are collapsed into the Deployment.
    assert!(graph.nodes().iter().all(|node| {
        node.kind != NodeKind::Pod && node.kind != NodeKind::Workload(WorkloadKind::ReplicaSet)
    }));
    // The api pod's ReplicaSet is not listed; pod-template-hash finds the Deployment.
    assert_eq!(
        node(&graph, "Deployment.apps/shop/api").attrs.pods,
        Some(PodCounts { total: 1, ready: 1 })
    );

    let routes = &edge(
        &graph,
        EdgeKind::RoutesTo,
        "Ingress.networking.k8s.io/shop/shop",
        "Service/shop/web",
    )
    .attrs
    .routes;
    assert_eq!(
        routes,
        &[RouteRule {
            hosts: vec!["shop.example.com".into()],
            path: Some("/".into()),
            port: Some(PortRef::Number(80)),
            section: None,
        }]
    );
    let api = &edge(
        &graph,
        EdgeKind::RoutesTo,
        "Ingress.networking.k8s.io/shop/shop",
        "Service/shop/api",
    )
    .attrs
    .routes;
    assert_eq!(api[0].port, Some(PortRef::Name("http".into())));

    // Readiness from the EndpointSlice.
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/shop/web",
            "Deployment.apps/shop/web"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts {
            ready: 1,
            not_ready: 1
        })
    );
    assert_eq!(
        node(&graph, "Service/shop/web").kind,
        NodeKind::Service(ServiceType::LoadBalancer)
    );
    let refs = &edge(
        &graph,
        EdgeKind::EnvRef,
        "Deployment.apps/shop/web",
        "ConfigMap/shop/web-config",
    )
    .attrs
    .refs;
    assert_eq!(refs[0].via, RefVia::EnvFrom);
    edge(
        &graph,
        EdgeKind::Mounts,
        "Deployment.apps/shop/web",
        "Secret/shop/web-secret",
    );
    edge(
        &graph,
        EdgeKind::Mounts,
        "Deployment.apps/shop/web",
        "PersistentVolumeClaim/shop/web-data",
    );
    edge(
        &graph,
        EdgeKind::UsesServiceAccount,
        "Deployment.apps/shop/web",
        "ServiceAccount/shop/web",
    );
    let token = &edge(
        &graph,
        EdgeKind::EnvRef,
        "Deployment.apps/shop/api",
        "Secret/shop/api-secret",
    )
    .attrs
    .refs;
    assert_eq!(token[0].key.as_deref(), Some("token"));
    edge(
        &graph,
        EdgeKind::Tls,
        "Ingress.networking.k8s.io/shop/shop",
        "Secret/shop/shop-tls",
    );
    edge(
        &graph,
        EdgeKind::Exposes,
        "Service/shop/web",
        "External//203.0.113.10",
    );
    edge(
        &graph,
        EdgeKind::Exposes,
        "Ingress.networking.k8s.io/shop/shop",
        "External//lb.example.net",
    );
    assert_eq!(
        node(&graph, "External//203.0.113.10").kind,
        NodeKind::External(ExternalKind::Ip)
    );

    // The token volume's root CA and unreferenced objects stay out.
    assert!(graph.node(&id("ConfigMap/shop/kube-root-ca.crt")).is_none());
    assert!(graph.node(&id("ConfigMap/shop/unused")).is_none());
    assert!(graph.diagnostics().is_empty(), "{:?}", messages(&graph));
    assert_eq!(graph.namespaces(), BTreeSet::from(["shop"]));
}

#[test]
fn typical_app_by_pod() {
    let graph = graph(APP, Detail::Pods);
    edge(
        &graph,
        EdgeKind::Owns,
        "Deployment.apps/shop/web",
        "Pod/shop/web-5d4f-a",
    );
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/shop/web",
            "Pod/shop/web-5d4f-a"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts {
            ready: 1,
            not_ready: 0
        })
    );
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/shop/web",
            "Pod/shop/web-5d4f-b"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts {
            ready: 0,
            not_ready: 1
        })
    );
    // Uses hang off the pods, not off their workload too.
    edge(
        &graph,
        EdgeKind::Mounts,
        "Pod/shop/web-5d4f-b",
        "Secret/shop/web-secret",
    );
    assert!(!has_edge(
        &graph,
        EdgeKind::Mounts,
        "Deployment.apps/shop/web",
        "Secret/shop/web-secret"
    ));
    assert!(!has_edge(
        &graph,
        EdgeKind::Selects,
        "Service/shop/web",
        "Deployment.apps/shop/web"
    ));
    assert_eq!(
        node(&graph, "Pod/shop/web-5d4f-a").attrs.phase.as_deref(),
        Some("Running")
    );
    assert!(graph.diagnostics().is_empty(), "{:?}", messages(&graph));
}

#[test]
fn ids_are_stable_across_rebuilds_and_detail_levels() {
    let workloads = graph(APP, Detail::Workloads);
    let pods = graph(APP, Detail::Pods);
    assert_eq!(workloads, graph(APP, Detail::Workloads));
    assert!(workloads.diff(&graph(APP, Detail::Workloads)).is_empty());
    for id in [
        "Deployment.apps/shop/web",
        "Service/shop/web",
        "Ingress.networking.k8s.io/shop/shop",
        "ConfigMap/shop/web-config",
    ] {
        assert!(workloads.node(&self::id(id)).is_some(), "{id}");
        assert!(pods.node(&self::id(id)).is_some(), "{id}");
    }
    assert_eq!(
        id("Deployment.apps/shop/web").as_str(),
        "Deployment.apps/shop/web"
    );
    assert_eq!(NodeId::external("1.2.3.4").as_str(), "External//1.2.3.4");
    assert_eq!(
        EdgeId::new(
            EdgeKind::Selects,
            &id("Service/shop/web"),
            &id("Pod/shop/a")
        )
        .as_str(),
        "selects:Service/shop/web->Pod/shop/a"
    );
    // Objects in any order build the same graph.
    let mut reversed = objects(APP);
    reversed.reverse();
    assert_eq!(
        build(
            &TopologyInput::from_objects(reversed),
            &BuildOptions::default()
        ),
        workloads
    );
}

#[test]
fn statefulset_with_headless_service_and_external_name() {
    let graph = graph(STATEFULSET, Detail::Workloads);
    assert_eq!(
        node(&graph, "Service/db/postgres").kind,
        NodeKind::Service(ServiceType::Headless)
    );
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/db/postgres",
            "StatefulSet.apps/db/postgres"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts {
            ready: 2,
            not_ready: 0
        })
    );
    let claim = &edge(
        &graph,
        EdgeKind::Mounts,
        "StatefulSet.apps/db/postgres",
        "PersistentVolumeClaim/db/data-postgres-1",
    )
    .attrs
    .refs;
    assert!(claim.iter().any(|use_| use_.via == RefVia::ClaimTemplate));
    assert!(claim.iter().any(|use_| use_.via == RefVia::Volume));
    assert!(
        graph
            .node(&id("PersistentVolumeClaim/db/data-postgres-old"))
            .is_none()
    );

    assert_eq!(
        node(&graph, "Service/db/payments").kind,
        NodeKind::Service(ServiceType::ExternalName)
    );
    edge(
        &graph,
        EdgeKind::Exposes,
        "Service/db/payments",
        "External//api.payments.example.com",
    );
    assert_eq!(
        node(&graph, "External//api.payments.example.com").kind,
        NodeKind::External(ExternalKind::Hostname)
    );
    assert!(graph.diagnostics().is_empty(), "{:?}", messages(&graph));

    // By pod: each pod mounts its own claim.
    let pods = self::graph(STATEFULSET, Detail::Pods);
    edge(
        &pods,
        EdgeKind::Mounts,
        "Pod/db/postgres-0",
        "PersistentVolumeClaim/db/data-postgres-0",
    );
    edge(
        &pods,
        EdgeKind::Owns,
        "StatefulSet.apps/db/postgres",
        "Pod/db/postgres-1",
    );
}

#[test]
fn gateway_api_routes() {
    let graph = graph(GATEWAY, Detail::Workloads);
    let gateway = "Gateway.gateway.networking.k8s.io/edge/public";
    let store = "HTTPRoute.gateway.networking.k8s.io/edge/store";
    let rpc = "GRPCRoute.gateway.networking.k8s.io/edge/rpc";
    assert_eq!(
        node(&graph, gateway).attrs.class_name.as_deref(),
        Some("istio")
    );
    assert_eq!(node(&graph, gateway).attrs.hosts, ["*.example.com"]);
    let attach = &edge(&graph, EdgeKind::RoutesTo, gateway, store)
        .attrs
        .routes;
    assert_eq!(attach[0].section.as_deref(), Some("https"));
    assert_eq!(attach[0].hosts, ["store.example.com"]);
    let cart = &edge(&graph, EdgeKind::RoutesTo, store, "Service/edge/cart")
        .attrs
        .routes;
    assert_eq!(cart[0].path.as_deref(), Some("/cart"));
    assert_eq!(cart[0].port, Some(PortRef::Number(8080)));
    edge(&graph, EdgeKind::RoutesTo, store, "Service/edge/web");
    let grpc = &edge(&graph, EdgeKind::RoutesTo, rpc, "Service/edge/cart")
        .attrs
        .routes;
    assert_eq!(grpc[0].path.as_deref(), Some("cart.v1.Cart/Add"));
    edge(&graph, EdgeKind::Tls, gateway, "Secret/edge/public-cert");
    edge(&graph, EdgeKind::Exposes, gateway, "External//198.51.100.5");

    // Scaled to zero: the Service still points at the Deployment, with
    // nothing ready.
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/edge/cart",
            "Deployment.apps/edge/cart"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts::default())
    );
    assert_eq!(
        messages(&graph),
        [
            "Service edge/cart has no ready endpoints",
            "TLSRoute edge/passthrough refers to missing Gateway edge/internal",
        ]
    );
    assert_eq!(
        node(&graph, "Gateway.gateway.networking.k8s.io/edge/internal").presence,
        Presence::Missing
    );
}

#[test]
fn dangling_references() {
    let graph = graph(BROKEN, Detail::Workloads);
    assert_eq!(
        messages(&graph),
        [
            "Deployment broken/web refers to missing ConfigMap broken/nope",
            "Deployment broken/web refers to missing PersistentVolumeClaim broken/gone",
            "Deployment broken/web refers to missing ServiceAccount broken/robot",
            "Ingress broken/front refers to missing Service broken/ghost",
            "Ingress broken/front refers to port 9090 that Service broken/web does not have",
            "Ingress broken/front refers to port https that Service broken/web does not have",
            "Service broken/lonely selector matches no pods",
            "Service broken/web has no ready endpoints (1 not ready)",
        ]
    );
    // An optional reference is drawn as missing, without a diagnostic.
    assert_eq!(
        node(&graph, "Secret/broken/maybe").presence,
        Presence::Missing
    );
    assert!(
        graph
            .diagnostics()
            .iter()
            .all(|d| d.target != Some(id("Secret/broken/maybe")))
    );
    let severities: BTreeSet<_> = graph
        .diagnostics()
        .iter()
        .map(Diagnostic::severity)
        .collect();
    assert_eq!(
        severities,
        BTreeSet::from([Severity::Warning, Severity::Error])
    );
}

#[test]
fn kinds_not_loaded_are_unknown_not_missing() {
    let mut input = TopologyInput::new();
    let all = objects(BROKEN);
    let pick = |kind: &str| -> Vec<Arc<DynamicObject>> {
        all.iter()
            .filter(|object| object.types.as_ref().unwrap().kind == kind)
            .cloned()
            .map(Arc::new)
            .collect()
    };
    input.set(SourceKind::Deployment, pick("Deployment"));
    input.set_partial(SourceKind::Pod, pick("Pod"));
    input.set(SourceKind::Service, pick("Service"));
    input.set(SourceKind::Ingress, pick("Ingress"));
    let graph = build(&input, &BuildOptions::default());
    assert_eq!(
        node(&graph, "ConfigMap/broken/nope").presence,
        Presence::Unknown
    );
    assert_eq!(
        node(&graph, "ServiceAccount/broken/robot").presence,
        Presence::Unknown
    );
    let messages = messages(&graph);
    assert!(
        messages
            .iter()
            .all(|m| !m.contains("ConfigMap") && !m.contains("robot")),
        "{messages:?}"
    );
    // Pods are not complete: a selector matching nothing is not reported.
    assert!(
        !messages.iter().any(|m| m.contains("lonely")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("missing Service broken/ghost"))
    );
}

#[test]
fn filters_by_namespace_kind_and_labels() {
    let mut all = objects(APP);
    all.extend(objects(STATEFULSET));
    let input = TopologyInput::from_objects(all);

    let only_db = build(
        &input,
        &BuildOptions {
            filter: Filter {
                namespaces: Some(BTreeSet::from(["db".to_string()])),
                ..Filter::default()
            },
            ..BuildOptions::default()
        },
    );
    assert_eq!(only_db.namespaces(), BTreeSet::from(["db"]));
    // The ExternalName target stays, the shop load balancer goes.
    assert!(
        only_db
            .node(&NodeId::external("api.payments.example.com"))
            .is_some()
    );
    assert!(only_db.node(&NodeId::external("203.0.113.10")).is_none());

    let network = build(
        &input,
        &BuildOptions {
            filter: Filter {
                kinds: Some(BTreeSet::from([NodeClass::Ingress, NodeClass::Service])),
                ..Filter::default()
            },
            ..BuildOptions::default()
        },
    );
    assert!(
        network
            .nodes()
            .iter()
            .all(|node| { matches!(node.kind.class(), NodeClass::Ingress | NodeClass::Service) })
    );
    assert!(has_edge(
        &network,
        EdgeKind::RoutesTo,
        "Ingress.networking.k8s.io/shop/shop",
        "Service/shop/web"
    ));

    let web = build(
        &input,
        &BuildOptions {
            filter: Filter {
                labels: Some(LabelSelector::parse("app=web").unwrap()),
                ..Filter::default()
            },
            ..BuildOptions::default()
        },
    );
    for present in [
        "Ingress.networking.k8s.io/shop/shop",
        "Service/shop/web",
        "Deployment.apps/shop/web",
        "ConfigMap/shop/web-config",
        "External//203.0.113.10",
    ] {
        assert!(web.node(&id(present)).is_some(), "{present}");
    }
    // Downstream of the shared Ingress, but not of the web app.
    for absent in [
        "Deployment.apps/shop/api",
        "Secret/shop/api-secret",
        "Service/db/postgres",
    ] {
        assert!(web.node(&id(absent)).is_none(), "{absent}");
    }
}

#[test]
fn label_selector_syntax() {
    let selector = LabelSelector::parse("app = web, tier!=db, canary, !legacy").unwrap();
    let labels = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    };
    assert!(selector.matches(&labels(&[("app", "web"), ("canary", "")])));
    assert!(!selector.matches(&labels(&[("app", "web"), ("canary", ""), ("tier", "db")])));
    assert!(!selector.matches(&labels(&[("app", "web"), ("canary", ""), ("legacy", "1")])));
    assert!(!selector.matches(&labels(&[("app", "web")])));
    assert!(LabelSelector::parse("app in (a, b)").is_err());
    assert!(LabelSelector::parse("=x").is_err());
}

#[test]
fn unreferenced_objects_on_request() {
    let graph = build(
        &fixture(APP),
        &BuildOptions {
            include_unreferenced: true,
            ..BuildOptions::default()
        },
    );
    assert!(graph.node(&id("ConfigMap/shop/unused")).is_some());
}

#[test]
fn layers_add_edges_by_id() {
    let graph = graph(APP, Detail::Workloads);
    let before = graph.edges().len();
    let from = id("Deployment.apps/shop/api");
    let to = id("Deployment.apps/shop/web");
    let layered = graph.clone().with_edges([
        Edge {
            id: EdgeId::new(EdgeKind::Owns, &from, &to),
            from: from.clone(),
            to: to.clone(),
            kind: EdgeKind::Owns,
            attrs: EdgeAttrs::default(),
        },
        Edge {
            id: EdgeId::new(EdgeKind::Owns, &from, &id("Pod/x/nowhere")),
            from,
            to: id("Pod/x/nowhere"),
            kind: EdgeKind::Owns,
            attrs: EdgeAttrs::default(),
        },
    ]);
    assert_eq!(layered.edges().len(), before + 1);
    let diff = graph.diff(&layered);
    assert_eq!(diff.added_edges.len(), 1);
    assert!(diff.added_nodes.is_empty() && diff.removed_edges.is_empty());
}

// --- incremental builds and scale ---------------------------------------

fn object(value: Value) -> Arc<DynamicObject> {
    Arc::new(serde_json::from_value(value).unwrap())
}

fn meta(namespace: &str, name: &str, labels: Value, owner: Option<(&str, &str, &str)>) -> Value {
    let mut meta = json!({
        "name": name, "namespace": namespace, "labels": labels, "resourceVersion": "1",
    });
    if let Some((api_version, kind, owner)) = owner {
        meta["ownerReferences"] = json!([{
            "apiVersion": api_version, "kind": kind, "name": owner, "uid": "u", "controller": true,
        }]);
    }
    meta
}

/// `namespaces` × `apps` Deployments with `pods` pods each, a Service, an
/// EndpointSlice, a ConfigMap and a Secret per app, an Ingress per
/// namespace.
fn synthetic(namespaces: usize, apps: usize, pods: usize) -> TopologyInput {
    let mut by_kind: Vec<(SourceKind, Vec<Arc<DynamicObject>>)> = SourceKind::ALL
        .iter()
        .map(|kind| (*kind, Vec::new()))
        .collect();
    let mut push = |kind: SourceKind, value: Value| {
        by_kind
            .iter_mut()
            .find(|(candidate, _)| *candidate == kind)
            .unwrap()
            .1
            .push(object(value));
    };
    for n in 0..namespaces {
        let ns = format!("ns-{n}");
        let mut paths = Vec::new();
        for a in 0..apps {
            let app = format!("app-{a}");
            let labels = json!({"app": app});
            let spec = json!({
                "containers": [{
                    "name": "main",
                    "envFrom": [{"configMapRef": {"name": format!("{app}-config")}}],
                    "env": [{"name": "T", "valueFrom": {"secretKeyRef": {"name": format!("{app}-secret"), "key": "t"}}}],
                }],
            });
            push(
                SourceKind::Deployment,
                json!({
                    "metadata": meta(&ns, &app, labels.clone(), None),
                    "spec": {"replicas": pods, "template": {"metadata": {"labels": labels}, "spec": spec}},
                }),
            );
            let rs = format!("{app}-abc");
            push(
                SourceKind::ReplicaSet,
                json!({
                    "metadata": meta(&ns, &rs, labels.clone(), Some(("apps/v1", "Deployment", &app))),
                    "spec": {"template": {"metadata": {"labels": labels}, "spec": spec}},
                }),
            );
            let mut endpoints = Vec::new();
            for p in 0..pods {
                let pod = format!("{rs}-{p}");
                let ready = p % 7 != 0;
                endpoints.push(json!({
                    "addresses": [format!("10.{n}.{a}.{p}")],
                    "conditions": {"ready": ready},
                    "targetRef": {"kind": "Pod", "name": pod},
                }));
                push(
                    SourceKind::Pod,
                    json!({
                        "metadata": meta(&ns, &pod, json!({"app": app, "pod-template-hash": "abc"}), Some(("apps/v1", "ReplicaSet", &rs))),
                        "spec": spec,
                        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}]},
                    }),
                );
            }
            push(
                SourceKind::Service,
                json!({
                    "metadata": meta(&ns, &app, json!({}), None),
                    "spec": {"selector": labels, "ports": [{"name": "http", "port": 80}]},
                }),
            );
            push(
                SourceKind::EndpointSlice,
                json!({
                    "metadata": meta(&ns, &format!("{app}-x"), json!({"kubernetes.io/service-name": app}), None),
                    "endpoints": endpoints,
                }),
            );
            push(
                SourceKind::ConfigMap,
                json!({"metadata": meta(&ns, &format!("{app}-config"), json!({}), None)}),
            );
            push(
                SourceKind::Secret,
                json!({"metadata": meta(&ns, &format!("{app}-secret"), json!({}), None)}),
            );
            paths.push(json!({"path": format!("/{app}"), "backend": {"service": {"name": app, "port": {"number": 80}}}}));
        }
        push(
            SourceKind::Ingress,
            json!({
                "metadata": meta(&ns, "ingress", json!({}), None),
                "spec": {"rules": [{"host": format!("{ns}.example.com"), "http": {"paths": paths}}]},
            }),
        );
    }
    let mut input = TopologyInput::new();
    for (kind, objects) in by_kind {
        input.set(kind, objects);
    }
    input
}

/// Wall time of the fastest of `runs` calls.
fn fastest(runs: usize, mut work: impl FnMut()) -> Duration {
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn three_thousand_pods_rebuild_quickly() {
    let input = synthetic(30, 10, 10);
    assert_eq!(input.objects(SourceKind::Pod).len(), 3000);

    let mut builder = TopologyBuilder::new();
    let options = BuildOptions::default();
    let cold = fastest(1, || {
        builder.build(&input, &options);
    });
    let graph = builder.build(&input, &options);
    // 300 Deployments, Services, ConfigMaps, Secrets and 30 Ingresses.
    assert_eq!(graph.nodes().len(), 1230);
    assert!(graph.diagnostics().is_empty());
    assert_eq!(
        edge(
            &graph,
            EdgeKind::Selects,
            "Service/ns-3/app-4",
            "Deployment.apps/ns-3/app-4"
        )
        .attrs
        .endpoints,
        Some(EndpointCounts {
            ready: 8,
            not_ready: 2
        })
    );
    let warm = fastest(3, || {
        builder.build(&input, &options);
    });
    let pods = fastest(1, || {
        builder.build(
            &input,
            &BuildOptions {
                detail: Detail::Pods,
                ..BuildOptions::default()
            },
        );
    });
    // Generous bounds for unoptimized test builds on slow machines; the
    // real numbers are in the ADR.
    assert!(cold < Duration::from_secs(5), "cold build took {cold:?}");
    assert!(warm < Duration::from_secs(3), "warm build took {warm:?}");
    assert!(
        pods < Duration::from_secs(5),
        "pod-level build took {pods:?}"
    );
    assert!(
        warm < cold,
        "reusing facts is not faster: {warm:?} vs {cold:?}"
    );
}

#[test]
fn changed_objects_are_read_again() {
    let mut input = synthetic(1, 1, 2);
    let mut builder = TopologyBuilder::new();
    let before = builder.build(&input, &BuildOptions::default());

    // Same resourceVersion, other content: the remembered facts are used.
    let mut pods: Vec<_> = input.objects(SourceKind::Pod).to_vec();
    let mut changed = (*pods[0]).clone();
    changed.data["status"]["conditions"][0]["status"] = json!("True");
    pods[0] = Arc::new(changed.clone());
    input.set(SourceKind::Pod, pods.clone());
    assert_eq!(builder.build(&input, &BuildOptions::default()), before);

    // A new resourceVersion is read again.
    changed.metadata.resource_version = Some("2".into());
    pods[0] = Arc::new(changed);
    input.set(SourceKind::Pod, pods);
    let after = builder.build(&input, &BuildOptions::default());
    let diff = before.diff(&after);
    assert!(
        diff.changed_nodes
            .contains(&id("Deployment.apps/ns-0/app-0")),
        "{diff:?}"
    );
}

// --- the cache helper -----------------------------------------------------

mod live {
    use futures::StreamExt as _;
    use kube::api::{ApiResource, GroupVersionKind};
    use kube::discovery::{ApiCapabilities, Scope, verbs};
    use kube::runtime::watcher::Event;

    use super::*;
    use crate::cache::ResourceCache;
    use crate::discovery::ApiCatalog;

    fn resource(
        group: &str,
        version: &str,
        kind: &str,
        plural: &str,
    ) -> (ApiResource, ApiCapabilities) {
        let gvk = GroupVersionKind::gvk(group, version, kind);
        (
            ApiResource::from_gvk_with_plural(&gvk, plural),
            ApiCapabilities {
                scope: Scope::Namespaced,
                subresources: Vec::new(),
                operations: vec![verbs::LIST.into(), verbs::WATCH.into()],
            },
        )
    }

    #[tokio::test]
    async fn builds_from_cache_watches() {
        let all = objects(APP);
        let cache = ResourceCache::with_source(move |kind, _key| {
            let mut events = vec![Ok(Event::Init)];
            events.extend(
                all.iter()
                    .filter(|object| object.types.as_ref().unwrap().kind == kind.api.kind)
                    .cloned()
                    .map(|object| Ok(Event::InitApply(object))),
            );
            events.push(Ok(Event::InitDone));
            futures::stream::iter(events)
                .chain(futures::stream::pending())
                .boxed()
        });
        // No Gateway API, no EndpointSlices in this cluster.
        let catalog = ApiCatalog::from_resources([
            resource("", "v1", "Pod", "pods"),
            resource("", "v1", "Service", "services"),
            resource("", "v1", "ConfigMap", "configmaps"),
            resource("", "v1", "Secret", "secrets"),
            resource("apps", "v1", "Deployment", "deployments"),
            resource("apps", "v1", "ReplicaSet", "replicasets"),
            resource("networking.k8s.io", "v1", "Ingress", "ingresses"),
        ]);
        let source = TopologySource::subscribe(&cache, &catalog, Some(&["shop".to_string()]));
        let mut changes = source.changes();
        assert!(changes.next().await);

        let deadline = Instant::now() + Duration::from_secs(5);
        let snapshot = loop {
            let snapshot = source.snapshot(&BuildOptions::default());
            if !snapshot.loading || Instant::now() > deadline {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(!snapshot.loading);
        assert!(snapshot.errors.is_empty());
        assert!(snapshot.unavailable.contains(&SourceKind::HttpRoute));
        assert!(snapshot.unavailable.contains(&SourceKind::EndpointSlice));
        let graph = snapshot.graph;
        // Without EndpointSlices readiness comes from the pods.
        assert_eq!(
            edge(
                &graph,
                EdgeKind::Selects,
                "Service/shop/web",
                "Deployment.apps/shop/web"
            )
            .attrs
            .endpoints,
            Some(EndpointCounts {
                ready: 1,
                not_ready: 1
            })
        );
        edge(
            &graph,
            EdgeKind::RoutesTo,
            "Ingress.networking.k8s.io/shop/shop",
            "Service/shop/web",
        );
        // ServiceAccounts and PVCs are not served here: unknown, not missing.
        assert_eq!(
            node(&graph, "ServiceAccount/shop/web").presence,
            Presence::Unknown
        );
        assert!(graph.diagnostics().is_empty(), "{:?}", messages(&graph));
    }
}

mod exports {
    use super::*;

    const POLICY: &str = include_str!("fixtures/policy.yaml");

    /// Compares with `fixtures/export/<name>`; `UPDATE_SNAPSHOTS=1` writes it.
    fn snapshot(name: &str, actual: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/topology/fixtures/export")
            .join(name);
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("no snapshot {name}; run with UPDATE_SNAPSHOTS=1"))
            // A Windows checkout may turn the snapshot's line ends into CRLF.
            .replace("\r\n", "\n");
        assert_eq!(actual, expected, "snapshot {name} differs");
    }

    fn with_policies() -> TopologyGraph {
        let mut all = objects(APP);
        all.extend(objects(POLICY));
        build(
            &TopologyInput::from_objects(all),
            &BuildOptions {
                network_policies: true,
                ..BuildOptions::default()
            },
        )
    }

    #[test]
    fn mermaid_snapshots() {
        snapshot(
            "app.mmd",
            &export::to_mermaid(&graph(APP, Detail::Workloads)),
        );
        snapshot(
            "broken.mmd",
            &export::to_mermaid(&graph(BROKEN, Detail::Workloads)),
        );
        snapshot("policy.mmd", &export::to_mermaid(&with_policies()));
    }

    #[test]
    fn dot_snapshots() {
        snapshot("app.dot", &export::to_dot(&graph(APP, Detail::Pods)));
        snapshot(
            "broken.dot",
            &export::to_dot(&graph(BROKEN, Detail::Workloads)),
        );
        snapshot("policy.dot", &export::to_dot(&with_policies()));
    }

    #[test]
    fn exports_cover_every_node_and_edge() {
        let graph = graph(GATEWAY, Detail::Workloads);
        let mermaid = export::to_mermaid(&graph);
        let dot = export::to_dot(&graph);
        let links = mermaid
            .lines()
            .filter(|line| line.contains("-->") || line.contains("==>") || line.contains("-.->"))
            .count();
        assert_eq!(links, graph.edges().len());
        assert_eq!(dot.matches(" -> ").count(), graph.edges().len());
        for node in graph.nodes() {
            assert!(dot.contains(&format!("\"{}\" [", node.id)), "{}", node.id);
        }
    }

    #[test]
    fn hostile_names_stay_inside_their_labels() {
        let text = "apiVersion: v1\nkind: Pod\nmetadata: {name: p, namespace: x}\nspec:\n  \
                    containers:\n    - name: c\n      image: i\n      envFrom: \
                    [{configMapRef: {name: 'a\"]--> evil[\"x'}}]\n";
        let graph = graph(text, Detail::Pods);
        let mermaid = export::to_mermaid(&graph);
        assert!(!mermaid.contains("a\"]"), "{mermaid}");
        assert!(mermaid.contains("a#quot;]--#gt; evil[#quot;x"), "{mermaid}");
        let dot = export::to_dot(&graph);
        assert!(dot.contains("a\\\"]--> evil[\\\"x"), "{dot}");
    }

    #[test]
    fn neighborhood_within_steps() {
        let graph = graph(APP, Detail::Workloads);
        let web = id("Service/shop/web");
        let one = graph.neighborhood_within([&web], Some(1));
        assert!(one.contains(&id("Deployment.apps/shop/web")));
        assert!(one.contains(&id("Ingress.networking.k8s.io/shop/shop")));
        assert!(!one.contains(&id("ConfigMap/shop/web-config")));
        let two = graph.neighborhood_within([&web], Some(2));
        assert!(two.contains(&id("ConfigMap/shop/web-config")));
        assert_eq!(
            graph.neighborhood_within([&web], None),
            graph.neighborhood([&web])
        );
    }
}
