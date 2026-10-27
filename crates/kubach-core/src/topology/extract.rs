//! What the graph needs from each object, read straight from the JSON of a
//! [`DynamicObject`] (no typed deserialization: cheap on thousands of pods,
//! and the same code reads cached objects and manifests).

use std::collections::BTreeMap;

use kube::api::DynamicObject;
use serde_json::Value;

use super::model::{
    ContainerPort, EdgeKind, PortRef, Protocol, RefUse, RefVia, RouteRule, ServicePort, ServiceType,
};
use super::source::{GATEWAY_GROUP, SourceKind};

/// Metadata every object contributes.
#[derive(Clone, Debug, Default)]
pub(super) struct Meta {
    pub namespace: Option<String>,
    pub name: String,
    pub labels: BTreeMap<String, String>,
    /// The controller owner, or the first owner if none is the controller.
    pub owner: Option<OwnerRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OwnerRef {
    pub group: String,
    pub kind: String,
    pub name: String,
}

/// Facts of one object.
#[derive(Debug)]
pub(super) struct Facts {
    pub meta: Meta,
    pub body: Body,
}

#[derive(Debug)]
pub(super) enum Body {
    Pod(PodFacts),
    Workload(WorkloadFacts),
    Service(ServiceFacts),
    EndpointSlice(SliceFacts),
    Ingress(IngressFacts),
    Gateway(GatewayFacts),
    Route(RouteFacts),
    /// ConfigMap, Secret, PVC, ServiceAccount: metadata only.
    Plain,
}

/// The parts of a pod spec that point at other objects.
#[derive(Debug, Default)]
pub(super) struct SpecFacts {
    pub refs: Vec<SpecRef>,
    /// Set explicitly and not `default`.
    pub service_account: Option<String>,
    /// `containers[].ports` (and of init containers: sidecars).
    pub ports: Vec<ContainerPort>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum RefTarget {
    ConfigMap,
    Secret,
    Pvc,
}

impl RefTarget {
    pub fn source(self) -> SourceKind {
        match self {
            RefTarget::ConfigMap => SourceKind::ConfigMap,
            RefTarget::Secret => SourceKind::Secret,
            RefTarget::Pvc => SourceKind::PersistentVolumeClaim,
        }
    }
}

#[derive(Debug)]
pub(super) struct SpecRef {
    pub target: RefTarget,
    pub name: String,
    pub edge: EdgeKind,
    pub use_: RefUse,
}

#[derive(Debug)]
pub(super) struct PodFacts {
    pub spec: SpecFacts,
    pub phase: Option<String>,
    pub ready: bool,
}

#[derive(Debug)]
pub(super) struct WorkloadFacts {
    pub source: SourceKind,
    pub template_labels: BTreeMap<String, String>,
    pub spec: SpecFacts,
    pub replicas: Option<i64>,
    pub ready_replicas: Option<i64>,
    /// StatefulSet `volumeClaimTemplates` names.
    pub claim_templates: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ServiceFacts {
    pub ty: ServiceType,
    /// `None`: no selector (endpoints managed by hand or ExternalName).
    pub selector: Option<BTreeMap<String, String>>,
    pub cluster_ip: Option<String>,
    pub ports: Vec<ServicePort>,
    pub external_name: Option<String>,
    pub load_balancer: Vec<String>,
}

#[derive(Debug)]
pub(super) struct SliceFacts {
    pub service: Option<String>,
    pub endpoints: Vec<SliceEndpoint>,
}

#[derive(Debug)]
pub(super) struct SliceEndpoint {
    /// `targetRef` name when it is a Pod.
    pub pod: Option<String>,
    pub addresses: Vec<String>,
    pub ready: bool,
}

/// An Ingress or route backend.
#[derive(Debug)]
pub(super) struct Backend {
    pub namespace: Option<String>,
    pub service: String,
    pub rule: RouteRule,
}

#[derive(Debug)]
pub(super) struct IngressFacts {
    pub class: Option<String>,
    pub hosts: Vec<String>,
    pub backends: Vec<Backend>,
    pub tls_secrets: Vec<String>,
    pub load_balancer: Vec<String>,
}

#[derive(Debug)]
pub(super) struct GatewayFacts {
    pub class: Option<String>,
    pub hosts: Vec<String>,
    /// `(namespace, name)` of listener certificate Secrets.
    pub certificates: Vec<(Option<String>, String)>,
    pub addresses: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ParentRef {
    pub namespace: Option<String>,
    pub name: String,
    pub section: Option<String>,
    pub port: Option<i32>,
}

#[derive(Debug)]
pub(super) struct RouteFacts {
    pub hosts: Vec<String>,
    pub parents: Vec<ParentRef>,
    pub backends: Vec<Backend>,
}

fn s(value: &Value) -> Option<&str> {
    value.as_str()
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(s)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
        .collect()
}

fn port_ref(value: Option<&Value>) -> Option<PortRef> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(PortRef::Number),
        Value::String(name) if !name.is_empty() => Some(match name.parse() {
            Ok(number) => PortRef::Number(number),
            Err(_) => PortRef::Name(name.clone()),
        }),
        _ => None,
    }
}

fn int(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

/// Group of an `apiVersion` (`apps/v1` → `apps`, `v1` → ``).
pub(super) fn api_group(api_version: &str) -> &str {
    api_version.split_once('/').map_or("", |(group, _)| group)
}

pub(super) fn meta(object: &DynamicObject) -> Meta {
    let metadata = &object.metadata;
    let owners = metadata.owner_references.as_deref().unwrap_or_default();
    let owner = owners
        .iter()
        .find(|owner| owner.controller == Some(true))
        .or_else(|| owners.first())
        .map(|owner| OwnerRef {
            group: api_group(&owner.api_version).to_owned(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
        });
    Meta {
        namespace: metadata.namespace.clone().filter(|ns| !ns.is_empty()),
        name: metadata.name.clone().unwrap_or_default(),
        labels: metadata
            .labels
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        owner,
    }
}

/// Reads one object of `kind`.
pub(super) fn facts(kind: SourceKind, object: &DynamicObject) -> Facts {
    let data = &object.data;
    let body = match kind {
        SourceKind::Pod => Body::Pod(pod(data)),
        SourceKind::Deployment
        | SourceKind::ReplicaSet
        | SourceKind::StatefulSet
        | SourceKind::DaemonSet
        | SourceKind::Job
        | SourceKind::CronJob => Body::Workload(workload(kind, data)),
        SourceKind::Service => Body::Service(service(data)),
        SourceKind::EndpointSlice => Body::EndpointSlice(slice(object)),
        SourceKind::Ingress => Body::Ingress(ingress(data)),
        SourceKind::Gateway => Body::Gateway(gateway(data)),
        SourceKind::HttpRoute | SourceKind::GrpcRoute | SourceKind::TlsRoute => {
            Body::Route(route(kind, data))
        }
        SourceKind::ConfigMap
        | SourceKind::Secret
        | SourceKind::PersistentVolumeClaim
        | SourceKind::ServiceAccount
        | SourceKind::NetworkPolicy
        | SourceKind::Namespace => Body::Plain,
    };
    Facts {
        meta: meta(object),
        body,
    }
}

/// ConfigMap every pod gets through its service account token volume;
/// linking it would tie every workload to it.
const ROOT_CA: &str = "kube-root-ca.crt";

pub(super) fn spec(spec: &Value) -> SpecFacts {
    let ports = items(spec, "initContainers")
        .chain(items(spec, "containers"))
        .flat_map(|container| items(container, "ports"))
        .filter_map(|port| {
            Some(ContainerPort {
                name: string_at(port, &["name"]),
                port: i32::try_from(int(port.get("containerPort"))?).ok()?,
                protocol: port
                    .get("protocol")
                    .and_then(s)
                    .and_then(Protocol::parse)
                    .unwrap_or_default(),
            })
        })
        .collect();
    let mut refs = Vec::new();
    let mut push = |target, name: Option<String>, edge, use_| {
        if let Some(name) = name {
            refs.push(SpecRef {
                target,
                name,
                edge,
                use_,
            });
        }
    };
    let containers = items(spec, "initContainers").chain(items(spec, "containers"));
    for container in containers {
        let at = format!(
            "container {}",
            container.get("name").and_then(s).unwrap_or_default()
        );
        for env in items(container, "env") {
            let Some(from) = env.get("valueFrom") else {
                continue;
            };
            for (key, target) in [
                ("configMapKeyRef", RefTarget::ConfigMap),
                ("secretKeyRef", RefTarget::Secret),
            ] {
                if let Some(selector) = from.get(key) {
                    push(
                        target,
                        string_at(selector, &["name"]),
                        EdgeKind::EnvRef,
                        RefUse {
                            via: RefVia::Env,
                            at: at.clone(),
                            key: string_at(selector, &["key"]),
                            optional: flag(selector, "optional"),
                        },
                    );
                }
            }
        }
        for from in items(container, "envFrom") {
            for (key, target) in [
                ("configMapRef", RefTarget::ConfigMap),
                ("secretRef", RefTarget::Secret),
            ] {
                if let Some(source) = from.get(key) {
                    push(
                        target,
                        string_at(source, &["name"]),
                        EdgeKind::EnvRef,
                        RefUse {
                            via: RefVia::EnvFrom,
                            at: at.clone(),
                            key: None,
                            optional: flag(source, "optional"),
                        },
                    );
                }
            }
        }
    }
    for volume in items(spec, "volumes") {
        let at = format!(
            "volume {}",
            volume.get("name").and_then(s).unwrap_or_default()
        );
        let mut volume_ref = |target, source: &Value, name_key: &str, via| {
            let optional = flag(source, "optional");
            let keys: Vec<_> = items(source, "items")
                .filter_map(|item| string_at(item, &["key"]))
                .collect();
            let name = string_at(source, &[name_key]);
            if via == RefVia::Projected && name.as_deref() == Some(ROOT_CA) {
                return;
            }
            if keys.is_empty() {
                push(
                    target,
                    name.clone(),
                    EdgeKind::Mounts,
                    RefUse {
                        via,
                        at: at.clone(),
                        key: None,
                        optional,
                    },
                );
            }
            for key in keys {
                push(
                    target,
                    name.clone(),
                    EdgeKind::Mounts,
                    RefUse {
                        via,
                        at: at.clone(),
                        key: Some(key),
                        optional,
                    },
                );
            }
        };
        if let Some(source) = volume.get("configMap") {
            volume_ref(RefTarget::ConfigMap, source, "name", RefVia::Volume);
        }
        if let Some(source) = volume.get("secret") {
            volume_ref(RefTarget::Secret, source, "secretName", RefVia::Volume);
        }
        if let Some(source) = volume.get("persistentVolumeClaim") {
            volume_ref(RefTarget::Pvc, source, "claimName", RefVia::Volume);
        }
        if let Some(projected) = volume.get("projected") {
            for source in items(projected, "sources") {
                if let Some(source) = source.get("configMap") {
                    volume_ref(RefTarget::ConfigMap, source, "name", RefVia::Projected);
                }
                if let Some(source) = source.get("secret") {
                    volume_ref(RefTarget::Secret, source, "name", RefVia::Projected);
                }
            }
        }
    }
    for secret in items(spec, "imagePullSecrets") {
        push(
            RefTarget::Secret,
            string_at(secret, &["name"]),
            EdgeKind::Mounts,
            RefUse {
                via: RefVia::ImagePullSecret,
                at: "imagePullSecrets".into(),
                key: None,
                optional: false,
            },
        );
    }
    let service_account = string_at(spec, &["serviceAccountName"])
        .or_else(|| string_at(spec, &["serviceAccount"]))
        .filter(|name| name != "default");
    SpecFacts {
        refs,
        service_account,
        ports,
    }
}

fn pod(data: &Value) -> PodFacts {
    let status = data.get("status").unwrap_or(&Value::Null);
    let ready = items(status, "conditions").any(|condition| {
        condition.get("type").and_then(s) == Some("Ready")
            && condition.get("status").and_then(s) == Some("True")
    });
    PodFacts {
        spec: spec(data.get("spec").unwrap_or(&Value::Null)),
        phase: string_at(status, &["phase"]),
        ready,
    }
}

fn workload(source: SourceKind, data: &Value) -> WorkloadFacts {
    let spec_value = data.get("spec").unwrap_or(&Value::Null);
    let template = if source == SourceKind::CronJob {
        ["jobTemplate", "spec", "template"]
            .iter()
            .try_fold(spec_value, |value, key| value.get(key))
    } else {
        spec_value.get("template")
    }
    .unwrap_or(&Value::Null);
    let status = data.get("status").unwrap_or(&Value::Null);
    let (replicas, ready_replicas) = match source {
        SourceKind::DaemonSet => (
            int(status.get("desiredNumberScheduled")),
            int(status.get("numberReady")),
        ),
        SourceKind::Job | SourceKind::CronJob => (None, None),
        _ => (
            int(spec_value.get("replicas")).or(Some(1)),
            int(status.get("readyReplicas")).or(Some(0)),
        ),
    };
    WorkloadFacts {
        source,
        template_labels: string_map(template.get("metadata").and_then(|m| m.get("labels"))),
        spec: spec(template.get("spec").unwrap_or(&Value::Null)),
        replicas,
        ready_replicas,
        claim_templates: items(spec_value, "volumeClaimTemplates")
            .filter_map(|claim| string_at(claim, &["metadata", "name"]))
            .collect(),
    }
}

fn load_balancer(status: &Value) -> Vec<String> {
    let lb = status.get("loadBalancer").unwrap_or(&Value::Null);
    items(lb, "ingress")
        .filter_map(|entry| string_at(entry, &["ip"]).or_else(|| string_at(entry, &["hostname"])))
        .collect()
}

fn service(data: &Value) -> ServiceFacts {
    let spec = data.get("spec").unwrap_or(&Value::Null);
    let cluster_ip = string_at(spec, &["clusterIP"]);
    let ty = match spec.get("type").and_then(s) {
        Some("NodePort") => ServiceType::NodePort,
        Some("LoadBalancer") => ServiceType::LoadBalancer,
        Some("ExternalName") => ServiceType::ExternalName,
        _ if cluster_ip.as_deref() == Some("None") => ServiceType::Headless,
        _ => ServiceType::ClusterIp,
    };
    let selector = Some(string_map(spec.get("selector")))
        .filter(|selector| !selector.is_empty() && ty != ServiceType::ExternalName);
    let ports = items(spec, "ports")
        .map(|port| ServicePort {
            name: string_at(port, &["name"]),
            port: int(port.get("port"))
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or_default(),
            protocol: string_at(port, &["protocol"]),
            target_port: port_ref(port.get("targetPort")),
            node_port: int(port.get("nodePort")).and_then(|n| i32::try_from(n).ok()),
        })
        .collect();
    ServiceFacts {
        ty,
        selector,
        cluster_ip,
        ports,
        external_name: string_at(spec, &["externalName"]),
        load_balancer: load_balancer(data.get("status").unwrap_or(&Value::Null)),
    }
}

/// Label EndpointSlices carry with their Service's name.
const SERVICE_NAME_LABEL: &str = "kubernetes.io/service-name";

fn slice(object: &DynamicObject) -> SliceFacts {
    let endpoints = items(&object.data, "endpoints")
        .map(|endpoint| {
            let target = endpoint.get("targetRef").unwrap_or(&Value::Null);
            SliceEndpoint {
                pod: (target.get("kind").and_then(s) == Some("Pod"))
                    .then(|| string_at(target, &["name"]))
                    .flatten(),
                addresses: items(endpoint, "addresses")
                    .filter_map(|address| address.as_str().map(str::to_owned))
                    .collect(),
                // Unset means unknown, which consumers treat as ready.
                ready: endpoint
                    .get("conditions")
                    .and_then(|conditions| conditions.get("ready"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            }
        })
        .collect();
    SliceFacts {
        service: object
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SERVICE_NAME_LABEL))
            .cloned(),
        endpoints,
    }
}

fn ingress_backend(backend: &Value, hosts: Vec<String>, path: Option<String>) -> Option<Backend> {
    let service = backend.get("service")?;
    let port = service.get("port").unwrap_or(&Value::Null);
    let port = port_ref(port.get("number")).or_else(|| port_ref(port.get("name")));
    Some(Backend {
        namespace: None,
        service: string_at(service, &["name"])?,
        rule: RouteRule {
            hosts,
            path,
            port,
            section: None,
        },
    })
}

fn ingress(data: &Value) -> IngressFacts {
    let spec = data.get("spec").unwrap_or(&Value::Null);
    let mut hosts = Vec::new();
    let mut backends = Vec::new();
    if let Some(backend) = spec
        .get("defaultBackend")
        .and_then(|backend| ingress_backend(backend, Vec::new(), None))
    {
        backends.push(backend);
    }
    for rule in items(spec, "rules") {
        let host = string_at(rule, &["host"]);
        hosts.extend(host.clone());
        let http = rule.get("http").unwrap_or(&Value::Null);
        for path in items(http, "paths") {
            if let Some(backend) = path.get("backend").and_then(|backend| {
                ingress_backend(
                    backend,
                    host.clone().into_iter().collect(),
                    string_at(path, &["path"]),
                )
            }) {
                backends.push(backend);
            }
        }
    }
    let mut tls_secrets = Vec::new();
    for tls in items(spec, "tls") {
        hosts.extend(items(tls, "hosts").filter_map(|host| host.as_str().map(str::to_owned)));
        tls_secrets.extend(string_at(tls, &["secretName"]));
    }
    hosts.sort();
    hosts.dedup();
    IngressFacts {
        class: string_at(spec, &["ingressClassName"]),
        hosts,
        backends,
        tls_secrets,
        load_balancer: load_balancer(data.get("status").unwrap_or(&Value::Null)),
    }
}

fn gateway(data: &Value) -> GatewayFacts {
    let spec = data.get("spec").unwrap_or(&Value::Null);
    let mut hosts = Vec::new();
    let mut certificates = Vec::new();
    for listener in items(spec, "listeners") {
        hosts.extend(string_at(listener, &["hostname"]));
        let tls = listener.get("tls").unwrap_or(&Value::Null);
        for certificate in items(tls, "certificateRefs") {
            let kind = certificate.get("kind").and_then(s).unwrap_or("Secret");
            let group = certificate.get("group").and_then(s).unwrap_or("");
            if kind == "Secret"
                && group.is_empty()
                && let Some(name) = string_at(certificate, &["name"])
            {
                certificates.push((string_at(certificate, &["namespace"]), name));
            }
        }
    }
    hosts.sort();
    hosts.dedup();
    let status = data.get("status").unwrap_or(&Value::Null);
    GatewayFacts {
        class: string_at(spec, &["gatewayClassName"]),
        hosts,
        certificates,
        addresses: items(status, "addresses")
            .filter_map(|address| string_at(address, &["value"]))
            .collect(),
    }
}

fn route(kind: SourceKind, data: &Value) -> RouteFacts {
    let spec = data.get("spec").unwrap_or(&Value::Null);
    let hosts: Vec<String> = items(spec, "hostnames")
        .filter_map(|host| host.as_str().map(str::to_owned))
        .collect();
    let parents = items(spec, "parentRefs")
        .filter(|parent| {
            parent.get("kind").and_then(s).unwrap_or("Gateway") == "Gateway"
                && parent.get("group").and_then(s).unwrap_or(GATEWAY_GROUP) == GATEWAY_GROUP
        })
        .filter_map(|parent| {
            Some(ParentRef {
                namespace: string_at(parent, &["namespace"]),
                name: string_at(parent, &["name"])?,
                section: string_at(parent, &["sectionName"]),
                port: int(parent.get("port")).and_then(|n| i32::try_from(n).ok()),
            })
        })
        .collect();
    let mut backends = Vec::new();
    for rule in items(spec, "rules") {
        let mut paths: Vec<Option<String>> = items(rule, "matches")
            .map(|matched| match kind {
                SourceKind::GrpcRoute => {
                    let method = matched.get("method").unwrap_or(&Value::Null);
                    match (
                        string_at(method, &["service"]),
                        string_at(method, &["method"]),
                    ) {
                        (Some(service), Some(method)) => Some(format!("{service}/{method}")),
                        (Some(service), None) => Some(service),
                        (None, Some(method)) => Some(format!("*/{method}")),
                        (None, None) => None,
                    }
                }
                _ => string_at(matched, &["path", "value"]),
            })
            .collect();
        if paths.is_empty() {
            paths.push(None);
        }
        paths.sort();
        paths.dedup();
        for backend in items(rule, "backendRefs") {
            let kind = backend.get("kind").and_then(s).unwrap_or("Service");
            let group = backend.get("group").and_then(s).unwrap_or("");
            if kind != "Service" || !group.is_empty() {
                continue;
            }
            let Some(name) = string_at(backend, &["name"]) else {
                continue;
            };
            for path in &paths {
                backends.push(Backend {
                    namespace: string_at(backend, &["namespace"]),
                    service: name.clone(),
                    rule: RouteRule {
                        hosts: hosts.clone(),
                        path: path.clone(),
                        port: port_ref(backend.get("port")),
                        section: None,
                    },
                });
            }
        }
    }
    RouteFacts {
        hosts,
        parents,
        backends,
    }
}
