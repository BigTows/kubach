//! What a port-forward points at: a pod, or a Service or workload whose
//! ready pod is picked by selector, the way `kubectl port-forward svc/web 80`
//! does. Covers the ports to offer and how a requested port maps to a pod
//! port (a Service `targetPort` may be a number or a container port name).

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::Client;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, ListParams, TypeMeta};
use serde_json::Value;

use super::StreamError;
use crate::plugin::builtin::selector_string;

/// Where a forwarded port lands in the pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetPort {
    Number(u16),
    /// A container port name, looked up in the chosen pod.
    Name(String),
}

/// A port the forward dialog offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardPort {
    /// The number to forward: the Service port, or the container port.
    pub port: u16,
    pub name: Option<String>,
    /// `TCP`, `UDP` or `SCTP`.
    pub protocol: String,
    /// The pod port behind [`port`](Self::port); for containers the same number.
    pub target: TargetPort,
    /// Container declaring the port (pods and workload templates).
    pub container: Option<String>,
}

impl ForwardPort {
    /// Port-forward tunnels TCP only.
    pub fn is_tcp(&self) -> bool {
        self.protocol == "TCP"
    }
}

/// A forward source: a pod, or a Service / workload resolved to a ready pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardSource {
    /// `Pod`, `Service`, `Deployment`, …
    pub kind: String,
    pub namespace: String,
    pub name: String,
    /// Label selector of the pods behind a Service or workload; `None` for a pod.
    pub selector: Option<String>,
    pub ports: Vec<ForwardPort>,
}

/// Kinds that can be forwarded through their pods.
const WORKLOADS: [&str; 5] = [
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "ReplicaSet",
    "Job",
];

impl ForwardSource {
    /// A source for a Pod, a Service with a selector or a workload
    /// (Deployment, StatefulSet, DaemonSet, ReplicaSet, Job).
    pub fn from_object(object: &DynamicObject) -> Option<Self> {
        let kind = object.types.as_ref()?.kind.clone();
        let namespace = object.metadata.namespace.clone()?;
        let name = object.metadata.name.clone()?;
        let spec = object.data.get("spec")?;
        let (selector, ports) = match kind.as_str() {
            "Pod" => (None, container_ports(spec)),
            "Service" => {
                let selector = spec
                    .get("selector")
                    .and_then(Value::as_object)
                    .filter(|labels| !labels.is_empty())?
                    .iter()
                    .filter_map(|(key, value)| Some(format!("{key}={}", value.as_str()?)))
                    .collect::<Vec<_>>()
                    .join(",");
                (Some(selector), service_ports(spec))
            }
            kind if WORKLOADS.contains(&kind) => {
                let selector: LabelSelector =
                    serde_json::from_value(spec.get("selector")?.clone()).ok()?;
                let ports = spec
                    .get("template")
                    .and_then(|template| template.get("spec"))
                    .map(container_ports)
                    .unwrap_or_default();
                (Some(selector_string(&selector)?), ports)
            }
            _ => return None,
        };
        Some(Self {
            kind,
            namespace,
            name,
            selector,
            ports,
        })
    }

    /// `svc/web`, `deploy/api`, `pod/web-1`, like kubectl.
    pub fn label(&self) -> String {
        format!("{}/{}", short_kind(&self.kind), self.name)
    }

    pub fn is_service(&self) -> bool {
        self.kind == "Service"
    }

    /// The pod port `requested` reaches on `pod`. For a Service `requested` is
    /// a Service port and its `targetPort` is resolved; otherwise it is the
    /// pod port itself.
    pub fn pod_port(&self, pod: &Pod, requested: u16) -> Result<u16, StreamError> {
        let offered: Vec<&ForwardPort> =
            self.ports.iter().filter(|p| p.port == requested).collect();
        if !offered.is_empty() && !offered.iter().any(|p| p.is_tcp()) {
            return Err(StreamError::Target(format!(
                "port {requested} is {}; port-forward supports TCP only",
                offered[0].protocol
            )));
        }
        if !self.is_service() {
            return Ok(requested);
        }
        let Some(port) = offered.into_iter().find(|p| p.is_tcp()) else {
            return Err(StreamError::Target(format!(
                "service {} has no port {requested}",
                self.name
            )));
        };
        match &port.target {
            TargetPort::Number(number) => Ok(*number),
            TargetPort::Name(name) => named_container_port(pod, name).ok_or_else(|| {
                StreamError::Target(format!(
                    "pod {} has no container port named {name}",
                    pod.metadata.name.as_deref().unwrap_or_default()
                ))
            }),
        }
    }
}

/// API group of a kind [`ForwardSource::from_object`] accepts; `None` for
/// any other kind.
fn source_group(kind: &str) -> Option<&'static str> {
    match kind {
        "Pod" | "Service" => Some(""),
        "Job" => Some("batch"),
        kind if WORKLOADS.contains(&kind) => Some("apps"),
        _ => None,
    }
}

/// Reads the source `kind`/`name` in `namespace` again, e.g. to restart a
/// forward remembered from the last run (#474). A source that is gone is
/// [`StreamError::SourceGone`]. Must run on the [`runtime`](crate::runtime).
pub async fn fetch_source(
    client: &Client,
    kind: &str,
    namespace: &str,
    name: &str,
) -> Result<ForwardSource, StreamError> {
    let label = format!("{namespace}/{}/{name}", short_kind(kind));
    let group = source_group(kind)
        .ok_or_else(|| StreamError::Target(format!("{kind} cannot be forwarded")))?;
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", kind));
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, &resource);
    let object = match api.get(name).await {
        Ok(object) => object,
        Err(kube::Error::Api(status)) if status.is_not_found() => {
            return Err(StreamError::SourceGone(label));
        }
        Err(error) => return Err(error.into()),
    };
    source_from_fetched(object, &resource).ok_or(StreamError::SourceGone(label))
}

/// [`ForwardSource::from_object`] for an object read through `resource`,
/// whose type the API may leave out.
fn source_from_fetched(mut object: DynamicObject, resource: &ApiResource) -> Option<ForwardSource> {
    if object.types.is_none() {
        object.types = Some(TypeMeta {
            api_version: resource.api_version.clone(),
            kind: resource.kind.clone(),
        });
    }
    ForwardSource::from_object(&object)
}

/// Short kind used by kubectl (`svc`, `deploy`, …).
pub fn short_kind(kind: &str) -> String {
    match kind {
        "Service" => "svc".into(),
        "Deployment" => "deploy".into(),
        "StatefulSet" => "sts".into(),
        "DaemonSet" => "ds".into(),
        "ReplicaSet" => "rs".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn container_ports(pod_spec: &Value) -> Vec<ForwardPort> {
    pod_spec
        .get("containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|container| {
            let name = container.get("name").and_then(Value::as_str);
            container
                .get("ports")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |port| {
                    let number = u16::try_from(port.get("containerPort")?.as_u64()?).ok()?;
                    Some(ForwardPort {
                        port: number,
                        name: str_field(port, "name"),
                        protocol: str_field(port, "protocol").unwrap_or_else(|| "TCP".into()),
                        target: TargetPort::Number(number),
                        container: name.map(str::to_owned),
                    })
                })
        })
        .collect()
}

fn service_ports(spec: &Value) -> Vec<ForwardPort> {
    spec.get("ports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|port| {
            let number = u16::try_from(port.get("port")?.as_u64()?).ok()?;
            let target = match port.get("targetPort") {
                Some(Value::String(name)) => match name.parse::<u16>() {
                    Ok(number) => TargetPort::Number(number),
                    Err(_) => TargetPort::Name(name.clone()),
                },
                Some(Value::Number(n)) => {
                    TargetPort::Number(n.as_u64().and_then(|n| u16::try_from(n).ok())?)
                }
                _ => TargetPort::Number(number),
            };
            Some(ForwardPort {
                port: number,
                name: str_field(port, "name"),
                protocol: str_field(port, "protocol").unwrap_or_else(|| "TCP".into()),
                target,
                container: None,
            })
        })
        .collect()
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The TCP `containerPort` named `name` in any container of `pod`.
fn named_container_port(pod: &Pod, name: &str) -> Option<u16> {
    pod.spec
        .as_ref()?
        .containers
        .iter()
        .flat_map(|container| container.ports.iter().flatten())
        .find(|port| {
            port.name.as_deref() == Some(name)
                && port.protocol.as_deref().is_none_or(|p| p == "TCP")
        })
        .and_then(|port| u16::try_from(port.container_port).ok())
}

/// Running, not terminating, and its `Ready` condition is true.
pub fn is_pod_ready(pod: &Pod) -> bool {
    let Some(status) = pod.status.as_ref() else {
        return false;
    };
    pod.metadata.deletion_timestamp.is_none()
        && status.phase.as_deref() == Some("Running")
        && status
            .conditions
            .iter()
            .flatten()
            .any(|c| c.type_ == "Ready" && c.status == "True")
}

/// The ready pod a forward goes to: the oldest one, then by name, so the
/// choice is stable while pods come and go.
pub fn pick_ready_pod(pods: &[Pod]) -> Option<&Pod> {
    pods.iter().filter(|pod| is_pod_ready(pod)).min_by(|a, b| {
        let created = |pod: &Pod| pod.metadata.creation_timestamp.clone().map(|t| t.0);
        created(a)
            .cmp(&created(b))
            .then_with(|| a.metadata.name.cmp(&b.metadata.name))
    })
}

/// Pod and pod port a forward of `requested` goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub pod: String,
    pub port: u16,
}

/// The pod `source` forwards to now: the pod itself (must be running), or a
/// ready pod matching its selector. Must run on the [`runtime`](crate::runtime).
pub async fn target_pod(client: &Client, source: &ForwardSource) -> Result<TargetPod, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &source.namespace);
    target_pod_in(&api, source)
        .await
        .map(|pod| TargetPod(Box::new(pod)))
}

/// The pod a forward would go to, for previews.
#[derive(Clone, Debug)]
pub struct TargetPod(Box<Pod>);

impl TargetPod {
    pub fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }

    /// See [`ForwardSource::pod_port`].
    pub fn port_for(&self, source: &ForwardSource, requested: u16) -> Result<u16, StreamError> {
        source.pod_port(&self.0, requested)
    }
}

pub(crate) async fn target_pod_in(
    api: &Api<Pod>,
    source: &ForwardSource,
) -> Result<Pod, StreamError> {
    let Some(selector) = &source.selector else {
        let pod = api.get(&source.name).await?;
        let running = pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running");
        return if running {
            Ok(pod)
        } else {
            Err(StreamError::PodNotRunning(source.name.clone()))
        };
    };
    let pods = api.list(&ListParams::default().labels(selector)).await?;
    pick_ready_pod(&pods.items).cloned().ok_or_else(|| {
        StreamError::Target(if pods.items.is_empty() {
            format!("no pods match {} (selector {selector})", source.label())
        } else {
            format!(
                "none of the {} pods of {} is ready",
                pods.items.len(),
                source.label()
            )
        })
    })
}

/// Picks the pod and pod port for forwarding `requested` of `source`.
/// Must run on the [`runtime`](crate::runtime).
pub(crate) async fn resolve_in(
    api: &Api<Pod>,
    source: &ForwardSource,
    requested: u16,
) -> Result<ResolvedTarget, StreamError> {
    let pod = target_pod_in(api, source).await?;
    let port = source.pod_port(&pod, requested)?;
    Ok(ResolvedTarget {
        pod: pod.metadata.name.clone().unwrap_or_default(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn service() -> ForwardSource {
        ForwardSource::from_object(&object(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {
                "selector": {"app": "web", "tier": "front"},
                "ports": [
                    {"name": "http", "port": 80, "targetPort": "http"},
                    {"name": "admin", "port": 9000, "targetPort": 9001},
                    {"name": "plain", "port": 8000},
                    {"name": "dns", "port": 53, "protocol": "UDP", "targetPort": 5353}
                ]
            }
        })))
        .unwrap()
    }

    fn pod(name: &str, ready: bool, created: &str) -> Pod {
        serde_json::from_value(json!({
            "metadata": {"name": name, "namespace": "shop", "creationTimestamp": created},
            "spec": {"containers": [
                {"name": "app", "image": "x", "ports": [
                    {"name": "http", "containerPort": 8080},
                    {"name": "dns", "containerPort": 5353, "protocol": "UDP"}
                ]}
            ]},
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}]
            }
        }))
        .unwrap()
    }

    #[test]
    fn reads_service_ports_and_selector() {
        let svc = service();
        assert_eq!(svc.label(), "svc/web");
        assert_eq!(svc.selector.as_deref(), Some("app=web,tier=front"));
        let targets: Vec<_> = svc.ports.iter().map(|p| p.target.clone()).collect();
        assert_eq!(
            targets,
            vec![
                TargetPort::Name("http".into()),
                TargetPort::Number(9001),
                TargetPort::Number(8000),
                TargetPort::Number(5353),
            ]
        );
        assert!(!svc.ports[3].is_tcp());
    }

    #[test]
    fn maps_service_ports_to_pod_ports() {
        let svc = service();
        let pod = pod("web-1", true, "2026-01-01T00:00:00Z");
        assert_eq!(svc.pod_port(&pod, 80).unwrap(), 8080);
        assert_eq!(svc.pod_port(&pod, 9000).unwrap(), 9001);
        assert_eq!(svc.pod_port(&pod, 8000).unwrap(), 8000);
        assert!(
            svc.pod_port(&pod, 53)
                .unwrap_err()
                .to_string()
                .contains("TCP only")
        );
        assert!(
            svc.pod_port(&pod, 8080)
                .unwrap_err()
                .to_string()
                .contains("no port 8080")
        );
    }

    #[test]
    fn named_target_must_exist_in_pod() {
        let svc = service();
        let mut pod = pod("web-1", true, "2026-01-01T00:00:00Z");
        pod.spec.as_mut().unwrap().containers[0].ports = None;
        let error = svc.pod_port(&pod, 80).unwrap_err().to_string();
        assert!(error.contains("no container port named http"), "{error}");
    }

    #[test]
    fn reads_pod_ports_per_container() {
        let source = ForwardSource::from_object(&object(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "web-1", "namespace": "shop"},
            "spec": {"containers": [
                {"name": "app", "image": "x", "ports": [
                    {"name": "http", "containerPort": 8080},
                    {"name": "metrics", "containerPort": 9090, "protocol": "TCP"}
                ]},
                {"name": "dns", "image": "y", "ports": [
                    {"containerPort": 53, "protocol": "UDP"}
                ]},
                {"name": "sidecar", "image": "z"}
            ]}
        })))
        .unwrap();
        assert_eq!(source.label(), "pod/web-1");
        assert!(source.selector.is_none());
        let ports: Vec<_> = source
            .ports
            .iter()
            .map(|p| {
                (
                    p.port,
                    p.name.as_deref(),
                    p.container.as_deref(),
                    p.is_tcp(),
                )
            })
            .collect();
        assert_eq!(
            ports,
            vec![
                (8080, Some("http"), Some("app"), true),
                (9090, Some("metrics"), Some("app"), true),
                (53, None, Some("dns"), false),
            ]
        );
        let pod = pod("web-1", true, "2026-01-01T00:00:00Z");
        assert_eq!(source.pod_port(&pod, 9090).unwrap(), 9090);
        assert!(
            source
                .pod_port(&pod, 53)
                .unwrap_err()
                .to_string()
                .contains("TCP only")
        );
    }

    #[test]
    fn reads_workload_template_ports() {
        let deploy = ForwardSource::from_object(&object(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "api", "namespace": "shop"},
            "spec": {
                "selector": {"matchLabels": {"app": "api"}},
                "template": {"spec": {"containers": [
                    {"name": "app", "image": "x", "ports": [
                        {"name": "http", "containerPort": 8080},
                        {"containerPort": 53, "protocol": "UDP"}
                    ]}
                ]}}
            }
        })))
        .unwrap();
        assert_eq!(deploy.label(), "deploy/api");
        assert_eq!(deploy.selector.as_deref(), Some("app=api"));
        assert_eq!(deploy.ports.len(), 2);
        assert_eq!(deploy.ports[0].container.as_deref(), Some("app"));
        let pod = pod("api-1", true, "2026-01-01T00:00:00Z");
        assert_eq!(deploy.pod_port(&pod, 8080).unwrap(), 8080);
        // Ports not in the template are forwarded as typed.
        assert_eq!(deploy.pod_port(&pod, 9999).unwrap(), 9999);
        assert!(deploy.pod_port(&pod, 53).is_err());
    }

    #[test]
    fn skips_objects_without_pods() {
        let headless = object(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "external", "namespace": "shop"},
            "spec": {"type": "ExternalName", "externalName": "example.com"}
        }));
        assert!(ForwardSource::from_object(&headless).is_none());
        let config = object(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "c", "namespace": "shop"},
            "spec": {}
        }));
        assert!(ForwardSource::from_object(&config).is_none());
    }

    #[test]
    fn picks_oldest_ready_pod() {
        let pods = vec![
            pod("web-c", false, "2026-01-01T00:00:00Z"),
            pod("web-b", true, "2026-01-03T00:00:00Z"),
            pod("web-a", true, "2026-01-02T00:00:00Z"),
        ];
        let picked = pick_ready_pod(&pods).unwrap();
        assert_eq!(picked.metadata.name.as_deref(), Some("web-a"));
        assert!(pick_ready_pod(&pods[..1]).is_none());

        let mut terminating = pod("web-d", true, "2026-01-01T00:00:00Z");
        terminating.metadata.deletion_timestamp = terminating.metadata.creation_timestamp.clone();
        assert!(!is_pod_ready(&terminating));
    }

    #[test]
    fn fetched_sources_are_read_through_their_api_group() {
        assert_eq!(source_group("Pod"), Some(""));
        assert_eq!(source_group("Service"), Some(""));
        assert_eq!(source_group("Deployment"), Some("apps"));
        assert_eq!(source_group("StatefulSet"), Some("apps"));
        assert_eq!(source_group("Job"), Some("batch"));
        assert_eq!(source_group("ConfigMap"), None);
        // An object read without its type still makes a source.
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Service"));
        assert_eq!(resource.plural, "services");
        let fetched = object(json!({
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {"selector": {"app": "web"}, "ports": [{"port": 80}]}
        }));
        let source = source_from_fetched(fetched, &resource).unwrap();
        assert_eq!(source.label(), "svc/web");
        assert_eq!(source.ports.len(), 1);
    }
}
