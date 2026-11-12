//! Synthetic clusters and hooks for performance measurements (`docs/perf.md`).
//!
//! Compiled only with the `perf` feature, which the benches and the stands
//! of this workspace enable; never in the app itself.
//!
//! Objects look like the ones an API server returns: pods carry
//! `managedFields`, several containers with env, probes and resources, and
//! a full status. [`PodShape`] sizes them, so the same measurement runs on
//! objects with and without the parts the cache may drop.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube::runtime::watcher::Event;
use serde_json::{Map, Value, json};

use crate::cache::{EventStream, WatchKey};
use crate::discovery::ResourceKind;

/// Annotation `kubectl apply` keeps the applied manifest in.
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

/// What a synthetic pod carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PodShape {
    /// `metadata.managedFields` of the controller and the kubelet, as the
    /// API server returns them.
    pub managed_fields: bool,
    /// The last-applied annotation, as on a pod created by `kubectl apply`
    /// (pods of a Deployment have none).
    pub last_applied: bool,
    /// App containers; one init container comes on top.
    pub containers: usize,
    /// Env variables of each container.
    pub env: usize,
}

impl PodShape {
    /// A pod of a Deployment as the API server returns it.
    pub const MANAGED: Self = Self {
        managed_fields: true,
        last_applied: false,
        containers: 3,
        env: 12,
    };

    /// A pod applied by hand: `managedFields` and last-applied.
    pub const FULL: Self = Self {
        last_applied: true,
        ..Self::MANAGED
    };

    /// Without `managedFields` and last-applied: what the list needs.
    pub const SLIM: Self = Self {
        managed_fields: false,
        ..Self::MANAGED
    };
}

/// The `v1` Pod kind, as discovery would return it.
pub fn pod_kind() -> ResourceKind {
    kind("", "v1", "Pod", "pods")
}

/// A namespaced kind with list and watch.
pub fn kind(group: &str, version: &str, kind: &str, plural: &str) -> ResourceKind {
    let gvk = GroupVersionKind::gvk(group, version, kind);
    ResourceKind {
        api: ApiResource::from_gvk_with_plural(&gvk, plural),
        namespaced: true,
        verbs: vec!["list".into(), "watch".into()],
    }
}

/// Name of namespace `ix`.
pub fn namespace(ix: usize) -> String {
    format!("team-{ix:03}")
}

/// `count` pods spread evenly over `namespaces` namespaces, sorted by
/// namespace and name like a cache snapshot.
pub fn pods(count: usize, namespaces: usize, shape: PodShape) -> Vec<DynamicObject> {
    let namespaces = namespaces.max(1);
    let mut pods: Vec<DynamicObject> = (0..count)
        .map(|ix| pod(ix, &namespace(ix % namespaces), shape))
        .collect();
    pods.sort_by(|a, b| {
        (&a.metadata.namespace, &a.metadata.name).cmp(&(&b.metadata.namespace, &b.metadata.name))
    });
    pods
}

/// [`pods`] as the shared objects the cache hands out.
pub fn shared(objects: Vec<DynamicObject>) -> Vec<Arc<DynamicObject>> {
    objects.into_iter().map(Arc::new).collect()
}

/// Pod number `ix` in `namespace`. Most pods run; some are pending or
/// crash-looping, so statuses, restarts and container cubes differ.
pub fn pod(ix: usize, namespace: &str, shape: PodShape) -> DynamicObject {
    let app = format!("app-{:04}", ix / 4);
    pod_of(ix, namespace, &app, shape)
}

fn pod_of(ix: usize, namespace: &str, app: &str, shape: PodShape) -> DynamicObject {
    let hash = format!(
        "{:08x}",
        (ix as u64 / 4).wrapping_mul(0x9e37_79b9) & 0xffff_ffff
    );
    let replica_set = format!("{app}-{hash}");
    let name = format!("{replica_set}-{:05x}", ix & 0xf_ffff);
    let crashing = ix % 37 == 5;
    let pending = ix % 53 == 7;

    let containers: Vec<Value> = (0..shape.containers.max(1))
        .map(|c| container(c, app, shape.env))
        .collect();
    let spec = json!({
        "serviceAccountName": app,
        "nodeName": format!("node-{:03}", ix % 120),
        "restartPolicy": "Always",
        "terminationGracePeriodSeconds": 30,
        "dnsPolicy": "ClusterFirst",
        "schedulerName": "default-scheduler",
        "priority": 0,
        "enableServiceLinks": true,
        "preemptionPolicy": "PreemptLowerPriority",
        "securityContext": {"runAsNonRoot": true, "runAsUser": 1000, "fsGroup": 2000},
        "initContainers": [{
            "name": "migrate",
            "image": format!("registry.example.com/{app}/migrate:1.4.{}", ix % 9),
            "command": ["/bin/migrate", "--wait"],
            "envFrom": [{"configMapRef": {"name": format!("{app}-config")}}],
            "resources": {"requests": {"cpu": "50m", "memory": "64Mi"}},
            "terminationMessagePath": "/dev/termination-log",
            "terminationMessagePolicy": "File",
            "imagePullPolicy": "IfNotPresent"
        }],
        "containers": containers,
        "volumes": [
            {"name": "config", "configMap": {"name": format!("{app}-config"), "defaultMode": 420}},
            {"name": "tls", "secret": {"secretName": format!("{app}-tls"), "defaultMode": 420}},
            {"name": "tmp", "emptyDir": {}},
            {"name": "kube-api-access-x7k2p", "projected": {"defaultMode": 420, "sources": [
                {"serviceAccountToken": {"expirationSeconds": 3607, "path": "token"}},
                {"configMap": {"name": "kube-root-ca.crt", "items": [{"key": "ca.crt", "path": "ca.crt"}]}},
                {"downwardAPI": {"items": [{"path": "namespace", "fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.namespace"}}]}}
            ]}}
        ],
        "tolerations": [
            {"key": "node.kubernetes.io/not-ready", "operator": "Exists", "effect": "NoExecute", "tolerationSeconds": 300},
            {"key": "node.kubernetes.io/unreachable", "operator": "Exists", "effect": "NoExecute", "tolerationSeconds": 300}
        ]
    });

    let phase = if pending { "Pending" } else { "Running" };
    let statuses: Vec<Value> = (0..shape.containers.max(1))
        .map(|c| container_status(c, app, ix, crashing && c == 0, pending))
        .collect();
    let condition = |kind: &str, ok: bool| {
        json!({
            "type": kind,
            "status": if ok { "True" } else { "False" },
            "lastProbeTime": null,
            "lastTransitionTime": "2026-10-01T08:00:12Z"
        })
    };
    let ready = !crashing && !pending;
    let status = json!({
        "phase": phase,
        "conditions": [
            condition("PodReadyToStartContainers", !pending),
            condition("Initialized", !pending),
            condition("Ready", ready),
            condition("ContainersReady", ready),
            condition("PodScheduled", true)
        ],
        "hostIP": format!("10.0.{}.{}", ix % 120 / 16, ix % 120 % 16 + 10),
        "hostIPs": [{"ip": format!("10.0.{}.{}", ix % 120 / 16, ix % 120 % 16 + 10)}],
        "podIP": format!("10.{}.{}.{}", 100 + ix / 65_536, ix / 256 % 256, ix % 256),
        "podIPs": [{"ip": format!("10.{}.{}.{}", 100 + ix / 65_536, ix / 256 % 256, ix % 256)}],
        "startTime": "2026-10-01T08:00:02Z",
        "initContainerStatuses": [{
            "name": "migrate",
            "state": {"terminated": {
                "exitCode": 0, "reason": "Completed",
                "startedAt": "2026-10-01T08:00:04Z", "finishedAt": "2026-10-01T08:00:09Z",
                "containerID": format!("containerd://{:064x}", ix * 7 + 1)
            }},
            "lastState": {},
            "ready": true,
            "restartCount": 0,
            "image": format!("registry.example.com/{app}/migrate:1.4.{}", ix % 9),
            "imageID": format!("registry.example.com/{app}/migrate@sha256:{:064x}", ix * 11 + 3),
            "containerID": format!("containerd://{:064x}", ix * 7 + 1),
            "started": false
        }],
        "containerStatuses": statuses,
        "qosClass": "Burstable"
    });

    let mut annotations = Map::new();
    annotations.insert("prometheus.io/scrape".into(), json!("true"));
    annotations.insert("prometheus.io/port".into(), json!("9090"));
    annotations.insert(
        "checksum/config".into(),
        json!(format!("{:064x}", ix as u64 * 31 + 17)),
    );
    if shape.last_applied {
        let applied = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace, "labels": {"app.kubernetes.io/name": app}},
            "spec": spec
        });
        annotations.insert(LAST_APPLIED.into(), json!(applied.to_string()));
    }

    let mut metadata = json!({
        "name": name,
        "generateName": format!("{replica_set}-"),
        "namespace": namespace,
        "uid": uid(ix as u64),
        "resourceVersion": format!("{}", 1_000_000 + ix),
        "creationTimestamp": format!("2026-10-{:02}T08:{:02}:00Z", ix % 28 + 1, ix % 60),
        "labels": {
            "app": app,
            "app.kubernetes.io/name": app,
            "app.kubernetes.io/instance": format!("{app}-prod"),
            "app.kubernetes.io/managed-by": "Helm",
            "pod-template-hash": hash
        },
        "annotations": annotations,
        "ownerReferences": [{
            "apiVersion": "apps/v1",
            "kind": "ReplicaSet",
            "name": replica_set,
            "uid": uid(ix as u64 / 4 + (1 << 40)),
            "controller": true,
            "blockOwnerDeletion": true
        }]
    });
    if shape.managed_fields {
        metadata["managedFields"] = managed_fields(&spec, &status);
    }

    object(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": metadata,
        "spec": spec,
        "status": status
    }))
}

fn container(ix: usize, app: &str, env: usize) -> Value {
    let name = match ix {
        0 => "app".to_owned(),
        1 => "envoy".to_owned(),
        n => format!("sidecar-{n}"),
    };
    let env: Vec<Value> = (0..env)
        .map(|e| match e % 4 {
            0 => json!({"name": format!("POD_FIELD_{e}"), "valueFrom": {"fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.name"}}}),
            1 => json!({"name": format!("DB_PASSWORD_{e}"), "valueFrom": {"secretKeyRef": {"name": format!("{app}-db"), "key": "password"}}}),
            _ => json!({"name": format!("SERVICE_URL_{e}"), "value": format!("http://{app}-backend-{e}.svc.cluster.local:8080/api/v1")}),
        })
        .collect();
    json!({
        "name": name,
        "image": format!("registry.example.com/{app}/{name}:2.{}.{}", ix + 3, ix * 5 + 1),
        "args": ["--config=/etc/app/config.yaml", "--log-format=json"],
        "ports": [
            {"name": "http", "containerPort": 8080 + ix, "protocol": "TCP"},
            {"name": "metrics", "containerPort": 9090 + ix, "protocol": "TCP"}
        ],
        "env": env,
        "resources": {
            "requests": {"cpu": "100m", "memory": "128Mi"},
            "limits": {"cpu": "500m", "memory": "512Mi"}
        },
        "volumeMounts": [
            {"name": "config", "mountPath": "/etc/app", "readOnly": true},
            {"name": "tls", "mountPath": "/etc/tls", "readOnly": true},
            {"name": "tmp", "mountPath": "/tmp"},
            {"name": "kube-api-access-x7k2p", "mountPath": "/var/run/secrets/kubernetes.io/serviceaccount", "readOnly": true}
        ],
        "livenessProbe": {
            "httpGet": {"path": "/healthz", "port": "http", "scheme": "HTTP"},
            "initialDelaySeconds": 10, "timeoutSeconds": 1, "periodSeconds": 10,
            "successThreshold": 1, "failureThreshold": 3
        },
        "readinessProbe": {
            "httpGet": {"path": "/ready", "port": "http", "scheme": "HTTP"},
            "timeoutSeconds": 1, "periodSeconds": 5, "successThreshold": 1, "failureThreshold": 3
        },
        "securityContext": {
            "allowPrivilegeEscalation": false,
            "readOnlyRootFilesystem": true,
            "capabilities": {"drop": ["ALL"]}
        },
        "terminationMessagePath": "/dev/termination-log",
        "terminationMessagePolicy": "File",
        "imagePullPolicy": "IfNotPresent"
    })
}

fn container_status(ix: usize, app: &str, pod: usize, crashing: bool, pending: bool) -> Value {
    let name = match ix {
        0 => "app".to_owned(),
        1 => "envoy".to_owned(),
        n => format!("sidecar-{n}"),
    };
    let image = format!(
        "registry.example.com/{app}/{name}:2.{}.{}",
        ix + 3,
        ix * 5 + 1
    );
    let state = if pending {
        json!({"waiting": {"reason": "ContainerCreating"}})
    } else if crashing {
        json!({"waiting": {
            "reason": "CrashLoopBackOff",
            "message": format!("back-off 5m0s restarting failed container={name} pod={app}")
        }})
    } else {
        json!({"running": {"startedAt": "2026-10-01T08:00:11Z"}})
    };
    let last = if crashing {
        json!({"terminated": {
            "exitCode": 1, "reason": "Error",
            "startedAt": "2026-10-02T10:00:00Z", "finishedAt": "2026-10-02T10:00:03Z",
            "containerID": format!("containerd://{:064x}", pod * 13 + ix)
        }})
    } else {
        json!({})
    };
    json!({
        "name": name,
        "state": state,
        "lastState": last,
        "ready": !crashing && !pending,
        "restartCount": if crashing { 42 } else { pod % 3 },
        "image": image,
        "imageID": format!("registry.example.com/{app}/{name}@sha256:{:064x}", pod * 17 + ix),
        "containerID": format!("containerd://{:064x}", pod * 19 + ix),
        "started": !crashing && !pending
    })
}

/// `managedFields` of the two managers of a running pod: the controller
/// owns the spec, the kubelet the status.
fn managed_fields(spec: &Value, status: &Value) -> Value {
    json!([
        {
            "manager": "kube-controller-manager",
            "operation": "Update",
            "apiVersion": "v1",
            "time": "2026-10-01T08:00:00Z",
            "fieldsType": "FieldsV1",
            "fieldsV1": {
                "f:metadata": {
                    "f:annotations": {".": {}, "f:checksum/config": {}, "f:prometheus.io/port": {}, "f:prometheus.io/scrape": {}},
                    "f:generateName": {},
                    "f:labels": {".": {}, "f:app": {}, "f:app.kubernetes.io/instance": {}, "f:app.kubernetes.io/managed-by": {}, "f:app.kubernetes.io/name": {}, "f:pod-template-hash": {}},
                    "f:ownerReferences": {".": {}, "k:{\"uid\":\"00000000-0000-4000-8000-000000000000\"}": {}}
                },
                "f:spec": fields(spec)
            }
        },
        {
            "manager": "kubelet",
            "operation": "Update",
            "apiVersion": "v1",
            "time": "2026-10-01T08:00:12Z",
            "fieldsType": "FieldsV1",
            "subresource": "status",
            "fieldsV1": {"f:status": fields(status)}
        }
    ])
}

/// The `fieldsV1` shape of `value`: a key per field, list items by name.
fn fields(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                out.insert(format!("f:{key}"), fields(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let mut out = Map::new();
            out.insert(".".into(), json!({}));
            for (ix, item) in items.iter().enumerate() {
                let key = match item.get("name").or_else(|| item.get("type")) {
                    Some(Value::String(name)) => format!("k:{{\"name\":\"{name}\"}}"),
                    _ => format!("v:{ix}"),
                };
                let mut entry = fields(item);
                if let Value::Object(entry) = &mut entry {
                    entry.insert(".".into(), json!({}));
                }
                out.insert(key, entry);
            }
            Value::Object(out)
        }
        _ => json!({}),
    }
}

fn uid(n: u64) -> String {
    format!(
        "{:08x}-{:04x}-4000-8000-{:012x}",
        n >> 16 & 0xffff_ffff,
        n & 0xffff,
        n
    )
}

fn object(value: Value) -> DynamicObject {
    serde_json::from_value(value).expect("a synthetic object is a valid DynamicObject")
}

/// Size of a synthetic cluster for the topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClusterSize {
    pub namespaces: usize,
    /// Deployments per namespace; each has a ReplicaSet, a Service, an
    /// Ingress, a ConfigMap, a Secret and a ServiceAccount.
    pub apps: usize,
    /// Pods per Deployment.
    pub replicas: usize,
}

impl ClusterSize {
    pub fn pods(&self) -> usize {
        self.namespaces * self.apps * self.replicas
    }
}

/// Objects of a cluster of web applications, each a Deployment with its
/// ReplicaSet and pods behind a Service and an Ingress, reading a
/// ConfigMap and a Secret under its own ServiceAccount.
pub fn cluster(size: ClusterSize, shape: PodShape) -> Vec<DynamicObject> {
    let mut objects = Vec::new();
    let mut pod_ix = 0;
    for ns in 0..size.namespaces {
        let namespace = namespace(ns);
        for a in 0..size.apps {
            let app = format!("app-{:04}", ns * size.apps + a);
            let first = pod_of(pod_ix, &namespace, &app, shape);
            let owner = first.metadata.owner_references.clone().unwrap_or_default();
            let replica_set = owner.first().map(|o| o.name.clone()).unwrap_or_default();
            let rs_uid = owner.first().map(|o| o.uid.clone()).unwrap_or_default();
            let deployment_uid = uid((ns * size.apps + a) as u64 + (1 << 41));
            let labels = json!({"app": app});
            let template = json!({
                "metadata": {"labels": labels},
                "spec": first.data.get("spec").cloned().unwrap_or(Value::Null)
            });
            let meta = |name: &str| {
                json!({
                    "name": name,
                    "namespace": namespace,
                    "uid": uid(objects_seed(name, ns)),
                    "resourceVersion": "1",
                    "creationTimestamp": "2026-09-01T00:00:00Z",
                    "labels": labels
                })
            };
            let mut deployment_meta = meta(&app);
            deployment_meta["uid"] = json!(deployment_uid);
            objects.push(object(json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": deployment_meta,
                "spec": {
                    "replicas": size.replicas,
                    "selector": {"matchLabels": labels},
                    "template": template
                },
                "status": {
                    "replicas": size.replicas,
                    "readyReplicas": size.replicas,
                    "availableReplicas": size.replicas,
                    "updatedReplicas": size.replicas
                }
            })));
            let mut rs_meta = meta(&replica_set);
            rs_meta["uid"] = json!(rs_uid);
            rs_meta["ownerReferences"] = json!([{
                "apiVersion": "apps/v1", "kind": "Deployment", "name": app,
                "uid": deployment_uid, "controller": true, "blockOwnerDeletion": true
            }]);
            objects.push(object(json!({
                "apiVersion": "apps/v1",
                "kind": "ReplicaSet",
                "metadata": rs_meta,
                "spec": {
                    "replicas": size.replicas,
                    "selector": {"matchLabels": labels},
                    "template": template
                },
                "status": {"replicas": size.replicas, "readyReplicas": size.replicas}
            })));
            objects.push(first);
            for _ in 1..size.replicas {
                pod_ix += 1;
                // Replicas share the ReplicaSet: the hash comes from `ix / 4`.
                let mut pod = pod_of(pod_ix, &namespace, &app, shape);
                pod.metadata.owner_references = Some(owner.clone());
                objects.push(pod);
            }
            pod_ix += 1;
            objects.push(object(json!({
                "apiVersion": "v1",
                "kind": "Service",
                "metadata": meta(&app),
                "spec": {
                    "type": "ClusterIP",
                    "clusterIP": format!("10.96.{}.{}", ns % 256, a % 256),
                    "selector": labels,
                    "ports": [{"name": "http", "port": 80, "targetPort": "http", "protocol": "TCP"}]
                }
            })));
            objects.push(object(json!({
                "apiVersion": "networking.k8s.io/v1",
                "kind": "Ingress",
                "metadata": meta(&app),
                "spec": {
                    "ingressClassName": "nginx",
                    "tls": [{"hosts": [format!("{app}.example.com")], "secretName": format!("{app}-tls")}],
                    "rules": [{
                        "host": format!("{app}.example.com"),
                        "http": {"paths": [{
                            "path": "/", "pathType": "Prefix",
                            "backend": {"service": {"name": app, "port": {"name": "http"}}}
                        }]}
                    }]
                }
            })));
            objects.push(object(json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": meta(&format!("{app}-config")),
                "data": {"config.yaml": "listen: :8080\nlog: json\n"}
            })));
            for secret in ["tls", "db"] {
                objects.push(object(json!({
                    "apiVersion": "v1",
                    "kind": "Secret",
                    "metadata": meta(&format!("{app}-{secret}")),
                    "type": "Opaque"
                })));
            }
            objects.push(object(json!({
                "apiVersion": "v1",
                "kind": "ServiceAccount",
                "metadata": meta(&app)
            })));
        }
    }
    objects
}

fn objects_seed(name: &str, ns: usize) -> u64 {
    name.bytes().fold(ns as u64 + 7, |seed, byte| {
        seed.wrapping_mul(31) + u64::from(byte)
    }) & 0xffff_ffff_ffff
}

/// A change as a watch would deliver it: a new `resourceVersion` and one
/// more restart of the first container.
pub fn touch(object: &mut DynamicObject, revision: u64) {
    object.metadata.resource_version = Some(revision.to_string());
    if let Some(count) = object
        .data
        .pointer_mut("/status/containerStatuses/0/restartCount")
    {
        *count = json!(count.as_u64().unwrap_or(0) + 1);
    }
}

/// Changes a [`source`] keeps delivering after the initial list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Churn {
    /// Time between bursts.
    pub period: Duration,
    /// Objects changed per burst.
    pub changed: usize,
}

/// An event source for [`ResourceCache::with_source`](crate::cache::ResourceCache::with_source):
/// lists the `objects` of the watched kind and namespace, then, with
/// `churn`, [touches](touch) a few of them every period, going round.
pub fn source(
    objects: Vec<DynamicObject>,
    churn: Option<Churn>,
) -> impl Fn(&ResourceKind, &WatchKey) -> EventStream + Send + Sync + 'static {
    let objects = Arc::new(objects);
    move |kind, key| {
        let mut watched: Vec<DynamicObject> = objects
            .iter()
            .filter(|object| {
                object
                    .types
                    .as_ref()
                    .is_some_and(|t| t.kind == kind.api.kind)
                    && key
                        .namespace
                        .as_ref()
                        .is_none_or(|ns| object.metadata.namespace.as_ref() == Some(ns))
            })
            .cloned()
            .collect();
        let mut list = vec![Ok(Event::Init)];
        list.extend(watched.iter().cloned().map(|o| Ok(Event::InitApply(o))));
        list.push(Ok(Event::InitDone));
        let list = futures::stream::iter(list);
        let Some(churn) = churn.filter(|churn| churn.changed > 0 && !watched.is_empty()) else {
            return list.chain(futures::stream::pending()).boxed();
        };
        let mut cursor = 0;
        let mut revision = 10_000_000;
        let changes = futures::stream::unfold((), move |()| {
            let burst: Vec<_> = (0..churn.changed)
                .map(|_| {
                    cursor = (cursor + 1) % watched.len();
                    revision += 1;
                    touch(&mut watched[cursor], revision);
                    Ok(Event::Apply(watched[cursor].clone()))
                })
                .collect();
            async move {
                tokio::time::sleep(churn.period).await;
                Some((futures::stream::iter(burst), ()))
            }
        })
        .flatten();
        list.chain(changes).boxed()
    }
}

/// The typed decoding every built-in plugin starts its cells with.
pub fn decode_pod(object: &DynamicObject) -> Option<Pod> {
    crate::plugin::builtin::decode(object)
}

/// Size of `object` as JSON, the way it travels and is stored.
pub fn json_len(object: &DynamicObject) -> usize {
    serde_json::to_vec(object).map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Cell, PluginRegistry};
    use crate::topology::{BuildOptions, SourceKind, TopologyInput};

    #[test]
    fn pods_decode_and_differ_by_shape() {
        let full = pod(0, "team-000", PodShape::FULL);
        let slim = pod(0, "team-000", PodShape::SLIM);
        assert!(full.metadata.managed_fields.is_some());
        assert!(slim.metadata.managed_fields.is_none());
        assert!(json_len(&full) > json_len(&slim) * 2);
        let decoded = decode_pod(&full).expect("decodes");
        assert_eq!(decoded.spec.unwrap().containers.len(), 3);

        let plugin = PluginRegistry::with_builtins().plugin_for("", "Pod");
        let cells = plugin.cells(&slim);
        assert_eq!(cells.len(), plugin.columns().len());
        assert!(matches!(&cells[2], Cell::Containers(c) if c.len() == 4));
        // A crash-looping pod is told apart.
        let crashing = plugin.cells(&pod(5, "team-000", PodShape::SLIM));
        assert!(matches!(&crashing[3], Cell::Status(h) if h.message == "CrashLoopBackOff"));
    }

    #[test]
    fn pods_are_sorted_and_unique() {
        let pods = pods(500, 7, PodShape::SLIM);
        let keys: Vec<_> = pods
            .iter()
            .map(|p| (p.metadata.namespace.clone(), p.metadata.name.clone()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn cluster_builds_a_connected_topology() {
        let size = ClusterSize {
            namespaces: 2,
            apps: 3,
            replicas: 2,
        };
        let input = TopologyInput::from_objects(cluster(size, PodShape::MANAGED));
        assert_eq!(input.objects(SourceKind::Pod).len(), size.pods());
        assert_eq!(input.objects(SourceKind::Deployment).len(), 6);
        let graph = crate::topology::build(&input, &BuildOptions::default());
        // Each app: Ingress → Service → Deployment, with its config around.
        assert!(
            graph.nodes().len() >= 6 * 3,
            "{} nodes",
            graph.nodes().len()
        );
        assert!(
            graph.edges().len() >= 6 * 2,
            "{} edges",
            graph.edges().len()
        );
        assert!(graph.diagnostics().is_empty(), "{:?}", graph.diagnostics());
    }

    #[test]
    fn touch_changes_the_version_and_restarts() {
        let mut pod = pod(1, "team-000", PodShape::SLIM);
        let before = pod
            .data
            .pointer("/status/containerStatuses/0/restartCount")
            .cloned();
        touch(&mut pod, 77);
        assert_eq!(pod.metadata.resource_version.as_deref(), Some("77"));
        assert_ne!(
            pod.data
                .pointer("/status/containerStatuses/0/restartCount")
                .cloned(),
            before
        );
    }
}
