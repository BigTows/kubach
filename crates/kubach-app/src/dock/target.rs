//! What a logs/terminal/port-forward request points at, taken from the
//! objects the browser shows.

use kubach_core::plugin::{Action, RelationKind, ResourcePlugin, StreamKind};
use kubach_core::streams::ForwardSource;
use kube::api::DynamicObject;
use serde_json::Value;

/// The parts of a pod the streaming views need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodRef {
    pub namespace: String,
    pub name: String,
    pub containers: Vec<String>,
    pub init_containers: Vec<String>,
    /// `kubectl.kubernetes.io/default-container` annotation.
    annotated_default: Option<String>,
    /// Container states from the pod status at the time it was read.
    states: Vec<(String, ContainerState)>,
}

/// Runtime state of a container, from `status.*containerStatuses`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    /// `waiting.reason`, e.g. `CrashLoopBackOff`.
    Waiting(String),
    /// `terminated.reason`, e.g. `Completed`.
    Terminated(String),
}

impl ContainerState {
    fn from_status(status: &Value) -> Option<Self> {
        let state = status.get("state")?;
        let reason = |key: &str, fallback: &str| {
            let reason = state.get(key)?.get("reason").and_then(Value::as_str);
            Some(reason.unwrap_or(fallback).to_owned())
        };
        if state.get("running").is_some() {
            Some(Self::Running)
        } else if let Some(reason) = reason("terminated", "Terminated") {
            Some(Self::Terminated(reason))
        } else {
            reason("waiting", "Waiting").map(Self::Waiting)
        }
    }

    /// Short text for menus; `None` for a running container.
    pub fn hint(&self) -> Option<&str> {
        match self {
            ContainerState::Running => None,
            ContainerState::Waiting(reason) | ContainerState::Terminated(reason) => Some(reason),
        }
    }
}

impl PodRef {
    pub fn from_object(object: &DynamicObject) -> Option<Self> {
        let spec = object.data.get("spec")?;
        let names = |key: &str| -> Vec<String> {
            containers(spec, key)
                .filter_map(|c| c.get("name")?.as_str().map(str::to_owned))
                .collect()
        };
        let status = object.data.get("status").unwrap_or(&Value::Null);
        let states = ["initContainerStatuses", "containerStatuses"]
            .into_iter()
            .flat_map(|key| containers(status, key))
            .filter_map(|status| {
                let name = status.get("name")?.as_str()?.to_owned();
                Some((name, ContainerState::from_status(status)?))
            })
            .collect();
        Some(Self {
            namespace: object.metadata.namespace.clone()?,
            name: object.metadata.name.clone()?,
            containers: names("containers"),
            init_containers: names("initContainers"),
            annotated_default: object
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kubectl.kubernetes.io/default-container"))
                .cloned(),
            states,
        })
    }

    /// Last known state of `container`, if the pod status had one.
    pub fn state(&self, container: &str) -> Option<&ContainerState> {
        self.states
            .iter()
            .find(|(name, _)| name == container)
            .map(|(_, state)| state)
    }

    fn choices(&self, names: &[String]) -> Vec<ContainerChoice> {
        names
            .iter()
            .map(|name| ContainerChoice {
                name: name.clone(),
                state: self.state(name).cloned(),
            })
            .collect()
    }

    /// The container exec and logs use when none is chosen: the
    /// `kubectl.kubernetes.io/default-container` annotation, else the first.
    pub fn default_container(&self) -> Option<&str> {
        self.annotated_default
            .as_deref()
            .filter(|name| self.containers.iter().any(|c| c == name))
            .or_else(|| self.containers.first().map(String::as_str))
    }
}

fn containers<'a>(spec: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    spec.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// One container in a [`ContainerMenu`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerChoice {
    pub name: String,
    pub state: Option<ContainerState>,
}

impl ContainerChoice {
    /// `name`, plus the state when the container is not running.
    pub fn label(&self) -> String {
        match self.state.as_ref().and_then(ContainerState::hint) {
            Some(hint) => format!("{} · {hint}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The containers a request can be pointed at, in menu groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerMenu {
    /// Offer "All containers" (logs only), meaning `container: None`.
    pub all: bool,
    pub containers: Vec<ContainerChoice>,
    pub init_containers: Vec<ContainerChoice>,
}

impl ContainerMenu {
    /// Every container and init container of `pod`, plus "All containers".
    pub fn for_logs(pod: &PodRef) -> Self {
        Self {
            all: true,
            containers: pod.choices(&pod.containers),
            init_containers: pod.choices(&pod.init_containers),
        }
    }

    /// App containers to exec into or target, with init containers apart
    /// when `with_init`.
    pub fn for_terminal(pod: &PodRef, with_init: bool) -> Self {
        Self {
            all: false,
            containers: pod.choices(&pod.containers),
            init_containers: if with_init {
                pod.choices(&pod.init_containers)
            } else {
                Vec::new()
            },
        }
    }

    pub fn len(&self) -> usize {
        self.containers.len() + self.init_containers.len()
    }
}

/// Whose logs to stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTarget {
    Pod(PodRef),
    /// All pods matching a selector, e.g. the pods of a Deployment.
    Selector {
        namespace: String,
        selector: String,
        /// `Deployment/web`, shown as the tab title.
        title: String,
    },
}

impl LogTarget {
    pub fn title(&self) -> String {
        match self {
            LogTarget::Pod(pod) => pod.name.clone(),
            LogTarget::Selector { title, .. } => title.clone(),
        }
    }
}

/// Something the dock can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamRequest {
    /// `container: None` streams every container of a pod (or the only one).
    Logs {
        target: LogTarget,
        container: Option<String>,
    },
    Shell {
        pod: PodRef,
        container: Option<String>,
    },
    /// An ephemeral busybox container targeting `container`, attached.
    Debug {
        pod: PodRef,
        container: Option<String>,
    },
    /// A pod, or a Service / workload forwarded through a ready pod.
    PortForward(ForwardSource),
}

impl StreamRequest {
    /// Kind of the request, for tests (the buttons have tooltips instead).
    #[cfg(test)]
    pub fn label(&self) -> &'static str {
        match self {
            StreamRequest::Logs { .. } => "Logs",
            StreamRequest::Shell { .. } => "Shell",
            StreamRequest::Debug { .. } => "Debug",
            StreamRequest::PortForward(_) => "Port forward",
        }
    }

    /// Request for a plugin's stream action on `object`; `None` for API
    /// actions (run through `run_action`) and objects the stream cannot use.
    pub fn for_action(action: &Action, object: &DynamicObject) -> Option<Self> {
        Some(match action.stream_kind()? {
            StreamKind::Logs => StreamRequest::Logs {
                target: LogTarget::Pod(PodRef::from_object(object)?),
                container: None,
            },
            StreamKind::Exec => StreamRequest::Shell {
                pod: PodRef::from_object(object)?,
                container: None,
            },
            StreamKind::Debug => StreamRequest::Debug {
                pod: PodRef::from_object(object)?,
                container: None,
            },
            StreamKind::PortForward => {
                StreamRequest::PortForward(ForwardSource::from_object(object)?)
            }
        })
    }

    /// Everything the details panel offers for `object`: the plugin's
    /// stream actions and logs of the selected pods for workloads.
    pub fn for_object(object: &DynamicObject, plugin: &dyn ResourcePlugin) -> Vec<Self> {
        let mut requests: Vec<Self> = plugin
            .actions_for(object)
            .iter()
            .filter_map(|action| Self::for_action(action, object))
            .collect();
        let has_logs = requests
            .iter()
            .any(|request| matches!(request, StreamRequest::Logs { .. }));
        if !has_logs
            && let Some(relation) = plugin.selector_relations(object).into_iter().find(|r| {
                r.kind == RelationKind::Selects && r.target_kind == "Pod" && r.api_version == "v1"
            })
            && let Some(namespace) = relation.namespace
        {
            let kind = object
                .types
                .as_ref()
                .map(|types| types.kind.as_str())
                .unwrap_or("Pods");
            let name = object.metadata.name.as_deref().unwrap_or_default();
            requests.insert(
                0,
                StreamRequest::Logs {
                    target: LogTarget::Selector {
                        namespace,
                        selector: relation.label_selector,
                        title: format!("{kind}/{name}"),
                    },
                    container: None,
                },
            );
        }
        requests
    }

    /// The pod a logs / shell / debug request points at.
    pub fn pod(&self) -> Option<&PodRef> {
        match self {
            StreamRequest::Logs {
                target: LogTarget::Pod(pod),
                ..
            }
            | StreamRequest::Shell { pod, .. }
            | StreamRequest::Debug { pod, .. } => Some(pod),
            _ => None,
        }
    }

    /// The containers to choose from before opening, or `None` when there is
    /// nothing to choose: logs of a pod with several containers (init ones
    /// included), a shell or debug container in a pod with several app
    /// containers. A debug container can only target app containers.
    pub fn container_menu(&self) -> Option<ContainerMenu> {
        let menu = match self {
            StreamRequest::Logs {
                target: LogTarget::Pod(pod),
                ..
            } => ContainerMenu::for_logs(pod),
            StreamRequest::Shell { pod, .. } if pod.containers.len() > 1 => {
                ContainerMenu::for_terminal(pod, true)
            }
            StreamRequest::Debug { pod, .. } if pod.containers.len() > 1 => {
                ContainerMenu::for_terminal(pod, false)
            }
            _ => return None,
        };
        (menu.len() > 1).then_some(menu)
    }

    /// The same request pointed at `container` (`None`: the default).
    pub fn with_container(&self, container: Option<String>) -> Self {
        let mut request = self.clone();
        match &mut request {
            StreamRequest::Logs { container: c, .. }
            | StreamRequest::Shell { container: c, .. }
            | StreamRequest::Debug { container: c, .. } => *c = container,
            StreamRequest::PortForward(_) => {}
        }
        request
    }
}

/// Whether a forwarded port probably speaks HTTP, for "Open in browser".
/// Returns the URL scheme.
pub fn http_scheme(port: u16, name: Option<&str>) -> Option<&'static str> {
    let name = name.unwrap_or_default().to_ascii_lowercase();
    if name.contains("https") || matches!(port, 443 | 8443) {
        return Some("https");
    }
    if name.contains("http")
        || name.contains("web")
        || name.contains("ui")
        || matches!(
            port,
            80 | 3000 | 4000 | 5000 | 8000 | 8008 | 8080 | 8081 | 8088 | 8888 | 9000 | 9090
        )
    {
        return Some("http");
    }
    None
}

#[cfg(test)]
mod tests {
    use kubach_core::plugin::PluginRegistry;
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn pod() -> DynamicObject {
        object(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "web-1", "namespace": "shop"},
            "spec": {
                "initContainers": [{"name": "migrate", "image": "x"}],
                "containers": [
                    {"name": "app", "image": "x", "ports": [
                        {"containerPort": 8080, "name": "http"},
                        {"containerPort": 53, "protocol": "UDP"}
                    ]},
                    {"name": "sidecar", "image": "y", "ports": [{"containerPort": 9090}]}
                ]
            }
        }))
    }

    #[test]
    fn reads_pod_containers() {
        let pod = PodRef::from_object(&pod()).unwrap();
        assert_eq!(pod.namespace, "shop");
        assert_eq!(pod.containers, vec!["app", "sidecar"]);
        assert_eq!(pod.init_containers, vec!["migrate"]);
        assert_eq!(pod.default_container(), Some("app"));
    }

    #[test]
    fn pod_offers_logs_shell_debug_and_forward() {
        let registry = PluginRegistry::with_builtins();
        let object = pod();
        let plugin = registry.plugin_for("", "Pod");
        let labels: Vec<_> = StreamRequest::for_object(&object, plugin.as_ref())
            .iter()
            .map(StreamRequest::label)
            .collect();
        assert_eq!(labels, vec!["Logs", "Shell", "Debug", "Port forward"]);
    }

    #[test]
    fn deployment_offers_logs_by_selector() {
        let registry = PluginRegistry::with_builtins();
        let object = object(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {"selector": {"matchLabels": {"app": "web"}}, "template": {}}
        }));
        let plugin = registry.plugin_for("apps", "Deployment");
        let requests = StreamRequest::for_object(&object, plugin.as_ref());
        assert_eq!(
            requests,
            vec![
                StreamRequest::Logs {
                    target: LogTarget::Selector {
                        namespace: "shop".into(),
                        selector: "app=web".into(),
                        title: "Deployment/web".into(),
                    },
                    container: None,
                },
                StreamRequest::PortForward(ForwardSource::from_object(&object).unwrap()),
            ]
        );
    }

    #[test]
    fn service_offers_logs_and_forward() {
        let registry = PluginRegistry::with_builtins();
        let object = object(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {"selector": {"app": "web"}, "ports": [{"port": 80}]}
        }));
        let plugin = registry.plugin_for("", "Service");
        let labels: Vec<_> = StreamRequest::for_object(&object, plugin.as_ref())
            .iter()
            .map(StreamRequest::label)
            .collect();
        assert_eq!(labels, vec!["Logs", "Port forward"]);
    }

    fn pod_with_status() -> DynamicObject {
        let mut object = pod();
        object.data["status"] = json!({
            "initContainerStatuses": [
                {"name": "migrate", "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}
            ],
            "containerStatuses": [
                {"name": "app", "state": {"running": {}}},
                {"name": "sidecar", "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
            ]
        });
        object
    }

    fn pod_requests(object: &DynamicObject) -> Vec<StreamRequest> {
        let registry = PluginRegistry::with_builtins();
        let plugin = registry.plugin_for("", "Pod");
        StreamRequest::for_object(object, plugin.as_ref())
    }

    fn names(choices: &[ContainerChoice]) -> Vec<&str> {
        choices.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn reads_container_states() {
        let with_status = PodRef::from_object(&pod_with_status()).unwrap();
        assert_eq!(with_status.state("app"), Some(&ContainerState::Running));
        assert_eq!(
            with_status.state("migrate"),
            Some(&ContainerState::Terminated("Completed".into()))
        );
        assert_eq!(
            with_status.state("sidecar"),
            Some(&ContainerState::Waiting("CrashLoopBackOff".into()))
        );
        assert_eq!(PodRef::from_object(&pod()).unwrap().state("app"), None);
    }

    #[test]
    fn multi_container_pod_offers_container_menus() {
        let requests = pod_requests(&pod_with_status());
        let menus: Vec<_> = requests.iter().map(StreamRequest::container_menu).collect();

        // Logs: "All containers", app containers, init containers apart.
        let logs = menus[0].as_ref().unwrap();
        assert!(logs.all);
        assert_eq!(names(&logs.containers), vec!["app", "sidecar"]);
        assert_eq!(names(&logs.init_containers), vec!["migrate"]);
        assert_eq!(logs.init_containers[0].label(), "migrate · Completed");
        assert_eq!(logs.containers[0].label(), "app");
        assert_eq!(logs.containers[1].label(), "sidecar · CrashLoopBackOff");

        // Shell: no "All", init containers in their own group.
        let shell = menus[1].as_ref().unwrap();
        assert!(!shell.all);
        assert_eq!(names(&shell.containers), vec!["app", "sidecar"]);
        assert_eq!(names(&shell.init_containers), vec!["migrate"]);

        // Debug: ephemeral containers can only target app containers.
        let debug = menus[2].as_ref().unwrap();
        assert_eq!(names(&debug.containers), vec!["app", "sidecar"]);
        assert!(debug.init_containers.is_empty());

        // Port forward has nothing to choose.
        assert_eq!(menus[3], None);
    }

    #[test]
    fn chosen_container_goes_into_the_request() {
        let object = pod();
        let pod_ref = PodRef::from_object(&object).unwrap();
        let requests = pod_requests(&object);

        // Opened without a choice: the default container / all containers.
        assert!(requests.iter().all(|r| match r {
            StreamRequest::Logs { container, .. }
            | StreamRequest::Shell { container, .. }
            | StreamRequest::Debug { container, .. } => container.is_none(),
            StreamRequest::PortForward(_) => true,
        }));

        assert_eq!(
            requests[0].with_container(Some("migrate".into())),
            StreamRequest::Logs {
                target: LogTarget::Pod(pod_ref.clone()),
                container: Some("migrate".into()),
            }
        );
        assert_eq!(
            requests[1].with_container(Some("sidecar".into())),
            StreamRequest::Shell {
                pod: pod_ref.clone(),
                container: Some("sidecar".into()),
            }
        );
        assert_eq!(
            requests[2].with_container(Some("sidecar".into())),
            StreamRequest::Debug {
                pod: pod_ref.clone(),
                container: Some("sidecar".into()),
            }
        );
        assert_eq!(requests[3].with_container(Some("x".into())), requests[3]);
        assert_eq!(requests[1].pod(), Some(&pod_ref));
    }

    #[test]
    fn single_container_pod_opens_directly() {
        let requests = pod_requests(&object(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "solo", "namespace": "shop"},
            "spec": {"containers": [{"name": "app", "image": "x"}]}
        })));

        assert!(requests.iter().all(|r| r.container_menu().is_none()));
    }

    #[test]
    fn init_container_alone_only_widens_logs() {
        let requests = pod_requests(&object(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "solo", "namespace": "shop"},
            "spec": {
                "initContainers": [{"name": "migrate", "image": "x"}],
                "containers": [{"name": "app", "image": "x"}]
            }
        })));
        let logs = requests[0].container_menu().unwrap();
        assert_eq!(names(&logs.containers), vec!["app"]);
        assert_eq!(names(&logs.init_containers), vec!["migrate"]);
        assert_eq!(requests[1].container_menu(), None);
        assert_eq!(requests[2].container_menu(), None);
    }

    /// Objects as the API server lists them: list items have no
    /// `apiVersion` / `kind`. The cache types them from the listed kind
    /// (#372); without that the Port forward button was missing.
    fn listed(mut value: Value, group: &str, version: &str, kind: &str) -> DynamicObject {
        let map = value.as_object_mut().unwrap();
        map.remove("apiVersion");
        map.remove("kind");
        let untyped = object(value);
        assert!(untyped.types.is_none());
        let api = kube::api::ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk(
            group, version, kind,
        ));
        let typed = kubach_core::discovery::typed(std::sync::Arc::new(untyped), &api);
        (*typed).clone()
    }

    fn labels_of(object: &DynamicObject, group: &str, kind: &str) -> Vec<&'static str> {
        let registry = PluginRegistry::with_builtins();
        let plugin = registry.plugin_for(group, kind);
        StreamRequest::for_object(object, plugin.as_ref())
            .iter()
            .map(StreamRequest::label)
            .collect()
    }

    #[test]
    fn listed_operator_pod_offers_port_forward() {
        // kube-prometheus-stack-operator: Running, owned by a ReplicaSet,
        // one container with `https 10250/TCP`.
        let pod = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "kube-prometheus-stack-operator-7d9c5b8f4-x2x7q",
                "namespace": "monitoring",
                "ownerReferences": [{
                    "apiVersion": "apps/v1", "kind": "ReplicaSet",
                    "name": "kube-prometheus-stack-operator-7d9c5b8f4",
                    "uid": "1", "controller": true, "blockOwnerDeletion": true
                }]
            },
            "spec": {"containers": [{
                "name": "kube-prometheus-stack",
                "image": "quay.io/prometheus-operator/prometheus-operator:v0.86.0",
                "ports": [{"name": "https", "containerPort": 10250, "protocol": "TCP"}]
            }]},
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [{
                    "name": "kube-prometheus-stack", "ready": true,
                    "state": {"running": {"startedAt": "2026-10-25T10:00:00Z"}}
                }]
            }
        });
        let mut untyped = object(pod.clone());
        untyped.types = None;
        // What the details got before the fix: no kind, no forward.
        assert_eq!(
            labels_of(&untyped, "", "Pod"),
            vec!["Logs", "Shell", "Debug"]
        );

        let listed = listed(pod, "", "v1", "Pod");
        assert_eq!(
            labels_of(&listed, "", "Pod"),
            vec!["Logs", "Shell", "Debug", "Port forward"]
        );
        let requests = pod_requests(&listed);
        let StreamRequest::PortForward(source) = &requests[3] else {
            panic!("no port forward");
        };
        assert_eq!(
            source.label(),
            "pod/kube-prometheus-stack-operator-7d9c5b8f4-x2x7q"
        );
        assert_eq!(source.ports.len(), 1);
        assert_eq!(source.ports[0].port, 10250);
        assert_eq!(source.ports[0].name.as_deref(), Some("https"));
        assert!(source.ports[0].is_tcp());
    }

    #[test]
    fn listed_service_and_deployment_offer_port_forward() {
        let service = listed(
            json!({
                "metadata": {"name": "operator", "namespace": "monitoring"},
                "spec": {"selector": {"app": "operator"}, "ports": [
                    {"name": "https", "port": 443, "targetPort": "https"}
                ]}
            }),
            "",
            "v1",
            "Service",
        );
        assert_eq!(
            labels_of(&service, "", "Service"),
            vec!["Logs", "Port forward"]
        );
        let deployment = listed(
            json!({
                "metadata": {"name": "operator", "namespace": "monitoring"},
                "spec": {
                    "selector": {"matchLabels": {"app": "operator"}},
                    "template": {"spec": {"containers": [{
                        "name": "operator", "image": "x",
                        "ports": [{"name": "https", "containerPort": 10250}]
                    }]}}
                }
            }),
            "apps",
            "v1",
            "Deployment",
        );
        assert_eq!(
            labels_of(&deployment, "apps", "Deployment"),
            vec!["Logs", "Port forward"]
        );
    }

    #[test]
    fn guesses_http_ports() {
        assert_eq!(http_scheme(8080, None), Some("http"));
        assert_eq!(http_scheme(443, None), Some("https"));
        assert_eq!(http_scheme(5432, None), None);
        assert_eq!(http_scheme(12345, Some("http-metrics")), Some("http"));
    }
}
