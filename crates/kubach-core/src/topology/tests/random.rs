//! The builder against the join as it was first written
//! ([`reference`](super::super::reference)) on random clusters: small pools
//! of names, so references hit, miss and collide in every way.

use std::collections::BTreeSet;
use std::sync::Arc;

use kube::api::DynamicObject;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng as _};
use serde_json::{Value, json};

use super::super::reference;
use super::*;
use crate::cache::Objects;

struct Random(ChaCha8Rng);

impl Random {
    fn new(seed: u64) -> Self {
        Self(ChaCha8Rng::seed_from_u64(seed))
    }

    fn below(&mut self, n: usize) -> usize {
        self.0.next_u32() as usize % n.max(1)
    }

    fn chance(&mut self, of: usize) -> bool {
        self.below(of) == 0
    }

    fn of<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

const NAMESPACES: [&str; 3] = ["a", "b", "c"];
const APPS: [&str; 3] = ["x", "y", "z"];
const TIERS: [&str; 2] = ["web", "db"];
const CONFIGS: [&str; 4] = ["c0", "c1", "c2", "ghost"];
const SECRETS: [&str; 4] = ["s0", "s1", "s2", "gone"];
const ACCOUNTS: [&str; 3] = ["sa0", "sa1", "default"];
const SERVICES: [&str; 4] = ["svc0", "svc1", "svc2", "nowhere"];
const DEPLOYMENTS: [&str; 3] = ["d0", "d1", "d2"];
const SETS: [&str; 3] = ["db", "db-0", "data-db"];
const TEMPLATES: [&str; 3] = ["data", "data-db", "wal"];
const CLAIMS: [&str; 10] = [
    "data-db-0",
    "data-db-1",
    "data-db-0-1",
    "data-db-db-0",
    "data-data-db-2",
    "wal-db-0",
    "wal-db-",
    "wal-db-x",
    "data-db-0-0-7",
    "plain",
];
const GATEWAYS: [&str; 2] = ["gw0", "gw1"];

fn labels(rng: &mut Random) -> Value {
    let mut labels = serde_json::Map::new();
    if !rng.chance(5) {
        labels.insert("app".into(), json!(rng.of(&APPS)));
    }
    if rng.chance(2) {
        labels.insert("tier".into(), json!(rng.of(&TIERS)));
    }
    Value::Object(labels)
}

fn pod_spec(rng: &mut Random) -> Value {
    let mut env = Vec::new();
    for _ in 0..rng.below(3) {
        let optional = rng.chance(4);
        env.push(if rng.chance(2) {
            json!({"name": "A", "valueFrom": {"secretKeyRef": {
                "name": rng.of(&SECRETS), "key": "k", "optional": optional}}})
        } else {
            json!({"name": "B", "valueFrom": {"configMapKeyRef": {
                "name": rng.of(&CONFIGS), "key": "k", "optional": optional}}})
        });
    }
    let mut env_from = Vec::new();
    if rng.chance(3) {
        env_from.push(json!({"configMapRef": {"name": rng.of(&CONFIGS)}}));
    }
    if rng.chance(4) {
        env_from.push(json!({"secretRef": {"name": rng.of(&SECRETS), "optional": true}}));
    }
    let mut volumes = Vec::new();
    if rng.chance(3) {
        volumes.push(json!({"name": "config", "configMap": {"name": rng.of(&CONFIGS)}}));
    }
    if rng.chance(3) {
        volumes.push(json!({"name": "tls", "secret": {"secretName": rng.of(&SECRETS)}}));
    }
    if rng.chance(3) {
        volumes.push(json!({"name": "data",
            "persistentVolumeClaim": {"claimName": rng.of(&CLAIMS)}}));
    }
    if rng.chance(5) {
        volumes.push(json!({"name": "mix", "projected": {"sources": [
            {"configMap": {"name": rng.of(&CONFIGS)}},
            {"secret": {"name": rng.of(&SECRETS)}}
        ]}}));
    }
    let port = if rng.chance(2) { 8080 } else { 9090 };
    let mut ports = vec![json!({"name": "http", "containerPort": port})];
    if rng.chance(3) {
        ports.push(json!({"name": "dns", "containerPort": 53, "protocol": "UDP"}));
    }
    let mut spec = json!({
        "containers": [{"name": "main", "env": env, "envFrom": env_from, "ports": ports}],
        "volumes": volumes
    });
    if rng.chance(2) {
        spec["serviceAccountName"] = json!(rng.of(&ACCOUNTS));
    }
    if rng.chance(6) {
        spec["imagePullSecrets"] = json!([{"name": rng.of(&SECRETS)}]);
    }
    spec
}

fn owner(api_version: &str, kind: &str, name: &str) -> Value {
    json!([{"apiVersion": api_version, "kind": kind, "name": name, "uid": "u", "controller": true}])
}

fn make(api_version: &str, kind: &str, namespace: &str, name: &str, rest: Value) -> DynamicObject {
    let mut value = json!({
        "apiVersion": api_version,
        "kind": kind,
        "metadata": {"name": name, "namespace": namespace, "resourceVersion": "1"}
    });
    if let (Some(object), Value::Object(rest)) = (value.as_object_mut(), rest) {
        for (key, part) in rest {
            if key == "metadata" {
                if let (Some(meta), Value::Object(part)) =
                    (object["metadata"].as_object_mut(), part)
                {
                    meta.extend(part);
                }
            } else {
                object.insert(key, part);
            }
        }
    }
    serde_json::from_value(value).unwrap()
}

fn selector(rng: &mut Random) -> Value {
    match rng.below(6) {
        0 => json!({}),
        1 => json!({"matchLabels": {"app": rng.of(&APPS), "tier": rng.of(&TIERS)}}),
        2 => json!({"matchExpressions": [
            {"key": "app", "operator": "In", "values": [rng.of(&APPS), rng.of(&APPS)]}]}),
        3 => json!({"matchExpressions": [
            {"key": "tier", "operator": rng.of(&["Exists", "DoesNotExist", "NotIn", "Bogus"]),
             "values": [rng.of(&TIERS)]}]}),
        4 => json!({"matchLabels": {"tier": rng.of(&TIERS)}, "matchExpressions": [
            {"key": "app", "operator": "In", "values": [rng.of(&APPS)]}]}),
        _ => json!({"matchLabels": {"app": rng.of(&APPS)}}),
    }
}

fn policy_ports(rng: &mut Random) -> Value {
    let mut ports = Vec::new();
    for _ in 0..rng.below(3) {
        ports.push(match rng.below(5) {
            0 => json!({"port": "http"}),
            1 => json!({"port": 8080, "endPort": 9090}),
            2 => json!({"protocol": "UDP", "port": 53}),
            3 => json!({"protocol": "TCP"}),
            _ => json!({"port": 8080}),
        });
    }
    json!(ports)
}

fn peers(rng: &mut Random) -> Value {
    let mut peers = Vec::new();
    for _ in 0..rng.below(3) {
        peers.push(match rng.below(6) {
            0 => json!({"ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.1.0.0/16"]}}),
            1 => json!({"namespaceSelector": match rng.below(3) {
                0 => json!({}),
                1 => json!({"matchLabels": {"team": rng.of(&["red", "blue"])}}),
                _ => json!({"matchLabels": {"kubernetes.io/metadata.name": rng.of(&NAMESPACES)}}),
            }}),
            2 => json!({"podSelector": selector(rng), "namespaceSelector": match rng.below(3) {
                0 => json!({}),
                1 => json!({"matchLabels": {"team": rng.of(&["red", "blue"])}}),
                _ => json!({"matchLabels": {"kubernetes.io/metadata.name": rng.of(&NAMESPACES)}}),
            }}),
            _ => json!({"podSelector": selector(rng)}),
        });
    }
    json!(peers)
}

fn policy(rng: &mut Random, namespace: &str, name: &str) -> DynamicObject {
    let mut spec = json!({"podSelector": selector(rng)});
    match rng.below(4) {
        0 => {}
        1 => spec["policyTypes"] = json!(["Ingress"]),
        2 => spec["policyTypes"] = json!(["Egress"]),
        _ => spec["policyTypes"] = json!(["Ingress", "Egress"]),
    }
    if !rng.chance(4) {
        let rules: Vec<Value> = (0..1 + rng.below(2))
            .map(|_| json!({"from": peers(rng), "ports": policy_ports(rng)}))
            .collect();
        spec["ingress"] = json!(rules);
    }
    if rng.chance(2) {
        let rules: Vec<Value> = (0..1 + rng.below(2))
            .map(|_| json!({"to": peers(rng), "ports": policy_ports(rng)}))
            .collect();
        spec["egress"] = json!(rules);
    }
    make(
        "networking.k8s.io/v1",
        "NetworkPolicy",
        namespace,
        name,
        json!({"spec": spec}),
    )
}

/// One object of `kind`, numbered `n` among its kind.
fn object(rng: &mut Random, kind: SourceKind, n: usize) -> DynamicObject {
    let namespace = rng.of(&NAMESPACES);
    let template =
        |rng: &mut Random| json!({"metadata": {"labels": labels(rng)}, "spec": pod_spec(rng)});
    match kind {
        SourceKind::Pod => {
            let mut meta = json!({"labels": labels(rng)});
            match rng.below(8) {
                0 => {}
                1 => meta["ownerReferences"] = owner("v1", "Node", "node-1"),
                2 => meta["ownerReferences"] = owner("apps/v1", "StatefulSet", rng.of(&SETS)),
                3 => meta["ownerReferences"] = owner("batch/v1", "Job", rng.of(&["j0", "j1"])),
                4 => meta["ownerReferences"] = owner("apps/v1", "DaemonSet", "ds0"),
                5 => meta["ownerReferences"] = owner("argoproj.io/v1alpha1", "Rollout", "r0"),
                _ => {
                    let hash = rng.of(&["h1", "h2"]);
                    let deployment = rng.of(&DEPLOYMENTS);
                    meta["labels"]["pod-template-hash"] = json!(hash);
                    meta["ownerReferences"] =
                        owner("apps/v1", "ReplicaSet", &format!("{deployment}-{hash}"));
                }
            }
            let ready = if rng.chance(3) { "False" } else { "True" };
            make(
                "v1",
                "Pod",
                namespace,
                &format!("p{n}"),
                json!({"metadata": meta, "spec": pod_spec(rng), "status": {
                    "phase": rng.of(&["Running", "Pending", "Failed"]),
                    "conditions": [{"type": "Ready", "status": ready}]}}),
            )
        }
        SourceKind::Deployment => make(
            "apps/v1",
            "Deployment",
            namespace,
            rng.of(&DEPLOYMENTS),
            json!({"metadata": {"labels": labels(rng)},
                "spec": {"replicas": rng.below(4), "template": template(rng)},
                "status": {"replicas": 2, "readyReplicas": rng.below(3)}}),
        ),
        SourceKind::ReplicaSet => {
            let deployment = rng.of(&DEPLOYMENTS);
            let hash = rng.of(&["h1", "h2"]);
            let mut meta = json!({"labels": labels(rng)});
            if !rng.chance(4) {
                meta["ownerReferences"] = owner("apps/v1", "Deployment", deployment);
            }
            make(
                "apps/v1",
                "ReplicaSet",
                namespace,
                &format!("{deployment}-{hash}"),
                json!({"metadata": meta, "spec": {"replicas": 2, "template": template(rng)}}),
            )
        }
        SourceKind::StatefulSet => {
            let claims: Vec<Value> = (0..rng.below(3))
                .map(|_| json!({"metadata": {"name": rng.of(&TEMPLATES)}}))
                .collect();
            make(
                "apps/v1",
                "StatefulSet",
                namespace,
                rng.of(&SETS),
                json!({"spec": {"replicas": 2, "serviceName": rng.of(&SERVICES),
                    "template": template(rng), "volumeClaimTemplates": claims}}),
            )
        }
        SourceKind::DaemonSet => make(
            "apps/v1",
            "DaemonSet",
            namespace,
            "ds0",
            json!({"spec": {"template": template(rng)}}),
        ),
        SourceKind::Job => {
            let mut meta = json!({});
            if rng.chance(2) {
                meta["ownerReferences"] = owner("batch/v1", "CronJob", rng.of(&["cj0", "cj1"]));
            }
            make(
                "batch/v1",
                "Job",
                namespace,
                rng.of(&["j0", "j1"]),
                json!({"metadata": meta, "spec": {"template": template(rng)}}),
            )
        }
        SourceKind::CronJob => make(
            "batch/v1",
            "CronJob",
            namespace,
            "cj0",
            json!({"spec": {"jobTemplate": {"spec": {"template": template(rng)}}}}),
        ),
        SourceKind::Service => {
            let mut spec = json!({"ports": [
                {"name": "http", "port": 80, "targetPort": rng.of(&["http", "8080", "nope"])}]});
            match rng.below(7) {
                0 => {}
                1 => {
                    spec["type"] = json!("ExternalName");
                    spec["externalName"] = json!(rng.of(&["db.example.com", "10.9.9.9"]));
                }
                2 => spec["selector"] = json!({}),
                3 => {
                    spec["selector"] = json!({"app": rng.of(&APPS), "tier": rng.of(&TIERS)});
                }
                _ => spec["selector"] = json!({"app": rng.of(&APPS)}),
            }
            let mut rest = json!({"spec": spec});
            if rng.chance(5) {
                rest["spec"]["type"] = json!("LoadBalancer");
                rest["status"] = json!({"loadBalancer": {"ingress": [
                    {"ip": "203.0.113.7"}, {"hostname": "lb.example.com"}]}});
            }
            if rng.chance(6) {
                rest["metadata"] = json!({"ownerReferences":
                    owner("apps/v1", "Deployment", rng.of(&DEPLOYMENTS))});
            }
            make("v1", "Service", namespace, rng.of(&SERVICES), rest)
        }
        SourceKind::EndpointSlice => {
            let endpoints: Vec<Value> = (0..rng.below(4))
                .map(|_| {
                    let ready = !rng.chance(3);
                    if rng.chance(3) {
                        json!({"addresses": [rng.of(&["192.0.2.1", "192.0.2.2"])],
                            "conditions": {"ready": ready}})
                    } else {
                        json!({"addresses": ["10.0.0.1"], "conditions": {"ready": ready},
                            "targetRef": {"kind": "Pod", "name": format!("p{}", rng.below(8))}})
                    }
                })
                .collect();
            make(
                "discovery.k8s.io/v1",
                "EndpointSlice",
                namespace,
                &format!("es{n}"),
                json!({"metadata": {"labels": {"kubernetes.io/service-name": rng.of(&SERVICES)}},
                    "endpoints": endpoints}),
            )
        }
        SourceKind::Ingress => {
            let port = if rng.chance(2) {
                json!({"number": if rng.chance(3) { 81 } else { 80 }})
            } else {
                json!({"name": rng.of(&["http", "admin"])})
            };
            let mut spec = json!({"rules": [{"host": "app.example.com", "http": {"paths": [
                {"path": "/", "backend": {"service": {"name": rng.of(&SERVICES), "port": port}}},
                {"path": "/api", "backend": {"service": {"name": rng.of(&SERVICES),
                    "port": {"number": 80}}}}
            ]}}]});
            if rng.chance(2) {
                spec["tls"] =
                    json!([{"hosts": ["app.example.com"], "secretName": rng.of(&SECRETS)}]);
            }
            if rng.chance(4) {
                spec["defaultBackend"] =
                    json!({"service": {"name": rng.of(&SERVICES), "port": {"number": 80}}});
            }
            let mut rest = json!({"spec": spec});
            if rng.chance(4) {
                rest["status"] = json!({"loadBalancer": {"ingress": [{"ip": "203.0.113.7"}]}});
            }
            make(
                "networking.k8s.io/v1",
                "Ingress",
                namespace,
                &format!("ing{n}"),
                rest,
            )
        }
        SourceKind::Gateway => {
            let mut certificate = json!({"kind": "Secret", "name": rng.of(&SECRETS)});
            if rng.chance(2) {
                certificate["namespace"] = json!(rng.of(&NAMESPACES));
            }
            make(
                "gateway.networking.k8s.io/v1",
                "Gateway",
                namespace,
                rng.of(&GATEWAYS),
                json!({"spec": {"gatewayClassName": "istio",
                    "listeners": [{"name": "https", "hostname": "gw.example.com", "port": 443,
                        "tls": {"certificateRefs": [certificate]}}]},
                    "status": {"addresses": [{"value": "198.51.100.4"}]}}),
            )
        }
        SourceKind::HttpRoute | SourceKind::GrpcRoute | SourceKind::TlsRoute => {
            let mut parent = json!({"name": rng.of(&["gw0", "gw1", "gw-missing"])});
            if rng.chance(2) {
                parent["namespace"] = json!(rng.of(&NAMESPACES));
            }
            if rng.chance(3) {
                parent["sectionName"] = json!("https");
            }
            let mut backend = json!({"name": rng.of(&SERVICES), "port": 80});
            if rng.chance(3) {
                backend["namespace"] = json!(rng.of(&NAMESPACES));
            }
            let (kind_name, match_) = match kind {
                SourceKind::HttpRoute => ("HTTPRoute", json!({"path": {"value": "/shop"}})),
                SourceKind::GrpcRoute => (
                    "GRPCRoute",
                    json!({"method": {"service": "shop.Cart", "method": "Add"}}),
                ),
                _ => ("TLSRoute", json!({})),
            };
            make(
                "gateway.networking.k8s.io/v1",
                kind_name,
                namespace,
                &format!("route{n}"),
                json!({"spec": {"parentRefs": [parent], "hostnames": ["shop.example.com"],
                    "rules": [{"matches": [match_], "backendRefs": [backend]}]}}),
            )
        }
        SourceKind::ConfigMap | SourceKind::Secret | SourceKind::ServiceAccount => {
            let (kind_name, pool): (&str, &[&str]) = match kind {
                SourceKind::ConfigMap => ("ConfigMap", &CONFIGS[..3]),
                SourceKind::Secret => ("Secret", &SECRETS[..3]),
                _ => ("ServiceAccount", &ACCOUNTS),
            };
            let mut meta = json!({"labels": labels(rng)});
            if rng.chance(5) {
                meta["ownerReferences"] = owner("apps/v1", "Deployment", rng.of(&DEPLOYMENTS));
            }
            make(
                "v1",
                kind_name,
                namespace,
                rng.of(pool),
                json!({"metadata": meta}),
            )
        }
        SourceKind::PersistentVolumeClaim => {
            let mut meta = json!({});
            if rng.chance(6) {
                meta["ownerReferences"] = owner("apps/v1", "StatefulSet", rng.of(&SETS));
            }
            make(
                "v1",
                "PersistentVolumeClaim",
                namespace,
                rng.of(&CLAIMS),
                json!({"metadata": meta}),
            )
        }
        SourceKind::NetworkPolicy => policy(rng, namespace, &format!("np{n}")),
        SourceKind::Namespace => {
            let mut object = make("v1", "Namespace", "", NAMESPACES[n % 3], json!({}));
            object.metadata.namespace = None;
            if rng.chance(2) {
                object.metadata.labels =
                    Some([("team".to_owned(), rng.of(&["red", "blue"]).to_owned())].into());
            }
            object
        }
    }
}

/// The most objects of a kind in a cluster.
fn most(kind: SourceKind) -> usize {
    match kind {
        SourceKind::Pod => 14,
        SourceKind::Service | SourceKind::PersistentVolumeClaim => 6,
        SourceKind::ConfigMap | SourceKind::Secret | SourceKind::NetworkPolicy => 5,
        SourceKind::Namespace => 3,
        SourceKind::Gateway | SourceKind::GrpcRoute | SourceKind::TlsRoute => 2,
        SourceKind::DaemonSet | SourceKind::CronJob => 2,
        _ => 4,
    }
}

/// Objects of `kind` with distinct namespace and name, sorted by them.
fn list(rng: &mut Random, kind: SourceKind) -> Vec<Arc<DynamicObject>> {
    let mut objects: Vec<DynamicObject> = (0..rng.below(most(kind) + 1))
        .map(|n| object(rng, kind, n))
        .collect();
    let key = |object: &DynamicObject| {
        (
            object.metadata.namespace.clone(),
            object.metadata.name.clone(),
        )
    };
    objects.sort_by_key(key);
    objects.dedup_by_key(|object| key(object));
    objects.into_iter().map(Arc::new).collect()
}

/// A cluster as lists by kind, with whether each list is whole (`None`:
/// the kind is not set at all).
struct Cluster {
    lists: Vec<(SourceKind, Objects, Option<bool>)>,
}

impl Cluster {
    fn random(rng: &mut Random) -> Self {
        let lists = SourceKind::every()
            .map(|kind| {
                let complete = match rng.below(12) {
                    0 => None,
                    1 => Some(false),
                    _ => Some(true),
                };
                (kind, list(rng, kind).into(), complete)
            })
            .collect();
        Self { lists }
    }

    fn input(&self) -> TopologyInput {
        let mut input = TopologyInput::new();
        for (kind, objects, complete) in &self.lists {
            match complete {
                Some(true) => input.set(*kind, Arc::clone(objects)),
                Some(false) => input.set_partial(*kind, Arc::clone(objects)),
                None => {}
            }
        }
        input
    }

    /// Changes the cluster the ways a watch does: an object changes (a
    /// new `resourceVersion`), goes or comes; a list becomes whole or not;
    /// or a list is handed out anew with the same objects.
    fn change(&mut self, rng: &mut Random, revision: &mut u64) {
        let slot = rng.below(self.lists.len());
        let (kind, objects, complete) = &mut self.lists[slot];
        let mut list: Vec<Arc<DynamicObject>> = objects.to_vec();
        match rng.below(8) {
            0 => {
                *complete = match complete {
                    Some(true) => Some(false),
                    Some(false) => None,
                    None => Some(true),
                };
                return;
            }
            1 => {}
            2 | 3 if !list.is_empty() => {
                list.remove(rng.below(list.len()));
            }
            4 | 5 if !list.is_empty() => {
                // The same object changed: other content under its name.
                let ix = rng.below(list.len());
                let mut changed = object(rng, *kind, ix);
                changed.metadata.namespace = list[ix].metadata.namespace.clone();
                changed.metadata.name = list[ix].metadata.name.clone();
                *revision += 1;
                changed.metadata.resource_version = Some(revision.to_string());
                list[ix] = Arc::new(changed);
            }
            _ => {
                let n = 20 + rng.below(4);
                let mut added = object(rng, *kind, n);
                *revision += 1;
                added.metadata.resource_version = Some(revision.to_string());
                let key = |object: &DynamicObject| {
                    (
                        object.metadata.namespace.clone(),
                        object.metadata.name.clone(),
                    )
                };
                if !list.iter().any(|old| key(old) == key(&added)) {
                    list.push(Arc::new(added));
                    list.sort_by_key(|object| key(object));
                }
            }
        }
        *objects = list.into();
    }
}

fn options(rng: &mut Random) -> BuildOptions {
    let mut filter = Filter::default();
    if rng.chance(6) {
        filter.namespaces = Some(
            (0..1 + rng.below(2))
                .map(|_| rng.of(&NAMESPACES).to_owned())
                .collect(),
        );
    }
    if rng.chance(6) {
        let classes = [
            NodeClass::Ingress,
            NodeClass::Service,
            NodeClass::Workload,
            NodeClass::Pod,
            NodeClass::Secret,
            NodeClass::External,
            NodeClass::ConfigMap,
        ];
        filter.kinds = Some(
            classes
                .into_iter()
                .filter(|_| !rng.chance(3))
                .collect::<BTreeSet<_>>(),
        );
    }
    if rng.chance(6) {
        let text = format!("app={}", rng.of(&APPS));
        filter.labels = Some(LabelSelector::parse(&text).unwrap());
    }
    BuildOptions {
        detail: if rng.chance(2) {
            Detail::Pods
        } else {
            Detail::Workloads
        },
        filter,
        include_unreferenced: rng.chance(4),
        network_policies: rng.chance(2),
    }
}

fn assert_same(built: &TopologyGraph, expected: &TopologyGraph, case: &str) {
    assert_eq!(built.nodes(), expected.nodes(), "{case}: nodes");
    assert_eq!(built.edges(), expected.edges(), "{case}: edges");
    assert_eq!(
        built.diagnostics(),
        expected.diagnostics(),
        "{case}: diagnostics"
    );
}

#[test]
fn builds_like_the_reference_on_random_clusters() {
    let mut rng = Random::new(171);
    let mut kinds: BTreeSet<EdgeKind> = BTreeSet::new();
    let mut seen = (0, 0, 0, 0);
    for case in 0..1500 {
        let cluster = Cluster::random(&mut rng);
        let input = cluster.input();
        let options = options(&mut rng);
        let expected = reference::build(&input, &options);
        let built = build(&input, &options);
        assert_same(&built, &expected, &format!("case {case} {options:?}"));
        // The diagnostics of a node are found by a search in the sorted list.
        for node in built.nodes() {
            let found: Vec<_> = built.diagnostics_for(&node.id).collect();
            let all: Vec<_> = built
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.node == node.id)
                .collect();
            assert_eq!(found, all, "case {case}: diagnostics of {}", node.id);
        }
        // A shared graph is narrowed like one of its own.
        let in_a = |node: &Node| node.namespace.as_deref() == Some("a");
        let shared = built.clone();
        assert_same(
            &shared.retain(in_a),
            &expected.clone().retain(in_a),
            &format!("case {case}: retained"),
        );
        kinds.extend(built.edges().iter().map(|edge| edge.kind));
        seen.0 += built.diagnostics().len();
        seen.1 += built
            .edges()
            .iter()
            .flat_map(|edge| &edge.attrs.refs)
            .filter(|use_| use_.via == RefVia::ClaimTemplate)
            .count();
        seen.2 += built
            .nodes()
            .iter()
            .filter(|node| node.presence != Presence::Present)
            .count();
        seen.3 += built
            .nodes()
            .iter()
            .filter(|node| {
                node.attrs
                    .network_policy
                    .as_ref()
                    .is_some_and(|policy| policy.isolated_ingress() || policy.isolated_egress())
            })
            .count();
    }
    // The clusters exercise every edge kind the builder makes and the
    // cases the shortcuts are about.
    for kind in [
        EdgeKind::RoutesTo,
        EdgeKind::Selects,
        EdgeKind::Owns,
        EdgeKind::Mounts,
        EdgeKind::EnvRef,
        EdgeKind::UsesServiceAccount,
        EdgeKind::Exposes,
        EdgeKind::Tls,
        EdgeKind::PolicyAllows,
    ] {
        assert!(kinds.contains(&kind), "no {kind:?} edge in any case");
    }
    assert!(seen.0 > 1000, "{} diagnostics", seen.0);
    assert!(seen.1 > 50, "{} claim template uses", seen.1);
    assert!(seen.2 > 1000, "{} missing or unknown nodes", seen.2);
    assert!(seen.3 > 500, "{} isolated nodes", seen.3);
}

#[test]
fn rebuilds_like_the_reference_as_the_cluster_changes() {
    let mut rng = Random::new(1710);
    let mut reused = 0;
    for case in 0..60 {
        let mut cluster = Cluster::random(&mut rng);
        let mut builder = TopologyBuilder::new();
        let mut options = options(&mut rng);
        let mut revision = 1;
        let mut last: Option<TopologyGraph> = None;
        for step in 0..40 {
            let unchanged = rng.chance(5);
            if !unchanged {
                if rng.chance(6) {
                    options = self::options(&mut rng);
                } else {
                    cluster.change(&mut rng, &mut revision);
                }
            }
            let input = cluster.input();
            let built = builder.build(&input, &options);
            let expected = reference::build(&input, &options);
            assert_same(&built, &expected, &format!("case {case} step {step}"));
            if unchanged && let Some(last) = &last {
                // Nothing changed: the very graph of the last build.
                assert!(built.same(last), "case {case} step {step}: built again");
                reused += 1;
            }
            last = Some(built);
        }
    }
    assert!(reused > 200, "{reused} builds reused");
}

/// More sources than are drawn per target (the cut at 24 must fall on the
/// same ones), across namespaces, some with their egress restricted.
#[cfg(feature = "perf")]
#[test]
fn policy_layer_matches_the_reference_with_many_sources() {
    use crate::perf::{self, ClusterSize, PodShape};
    use crate::topology::netpol::{self, PolicySet};

    let size = ClusterSize {
        namespaces: 3,
        apps: 40,
        replicas: 2,
    };
    let mut objects = perf::cluster(size, PodShape::SLIM);
    objects.extend(perf::namespaces(size.namespaces));
    objects.extend(perf::network_policies(size, 36));
    for ns in 0..size.namespaces {
        let namespace = perf::namespace(ns);
        let apps: Vec<String> = (0..size.namespaces * size.apps)
            .filter(|app| app % 3 != 1)
            .map(|app| format!("app-{app:04}"))
            .collect();
        objects.push(make(
            "networking.k8s.io/v1",
            "NetworkPolicy",
            &namespace,
            "from-any-app-here",
            json!({"spec": {"podSelector": {"matchExpressions": [
                    {"key": "app", "operator": "In", "values": apps[..apps.len() / 2].to_vec()}]},
                "ingress": [{
                "from": [{"podSelector": {"matchExpressions": [
                    {"key": "app", "operator": "Exists"}]}}],
                "ports": [{"port": "http"}, {"port": 9090}]}]}}),
        ));
        objects.push(make(
            "networking.k8s.io/v1",
            "NetworkPolicy",
            &namespace,
            "from-listed-apps-anywhere",
            json!({"spec": {"podSelector": {"matchLabels": {"app": format!("app-{:04}", ns * 40 + 5)}},
                "ingress": [{"from": [{"namespaceSelector": {}, "podSelector": {"matchExpressions": [
                    {"key": "app", "operator": "In", "values": apps}]}}]}]}}),
        ));
    }
    let input = TopologyInput::from_objects(objects);
    let policies = PolicySet::from_input(&input);
    for detail in [Detail::Workloads, Detail::Pods] {
        let graph = build(
            &input,
            &BuildOptions {
                detail,
                ..BuildOptions::default()
            },
        );
        let expected = netpol::reference_apply(graph.clone(), &policies);
        let layered = netpol::apply(graph, &policies);
        assert_same(&layered, &expected, &format!("{detail:?}"));
        let allows = |graph: &TopologyGraph, node: &Node| {
            graph
                .edges_to(&node.id)
                .filter(|edge| edge.kind == EdgeKind::PolicyAllows)
                .count()
        };
        let most = layered
            .nodes()
            .iter()
            .map(|node| allows(&layered, node))
            .max();
        assert_eq!(most, Some(24), "{detail:?}: the cut is reached");
        assert!(
            layered
                .nodes()
                .iter()
                .any(|node| (1..24).contains(&allows(&layered, node))),
            "{detail:?}: some targets have fewer sources"
        );
    }
}
