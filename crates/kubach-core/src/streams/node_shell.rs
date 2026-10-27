//! A shell on a node, like node-shell in Lens and k9s: a temporary
//! privileged pod on that node shares the host's PID, network and IPC
//! namespaces, and an exec into it runs `nsenter` into PID 1's namespaces.
//!
//! The pod sleeps; the shell is an exec, so it can reconnect and exiting it
//! leaves the pod for the caller to delete ([`delete_shell_pod`], grace 0).
//! Every such pod carries [`MANAGED_BY_LABEL`] and [`NODE_SHELL_LABEL`]
//! and says who owns it ([`owner`], #351); [`sweep_orphans`] deletes the
//! ones left by a crash, and never a live pod of another Kubach.

pub mod owner;

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, SecurityContext, Toleration};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt as _};

use self::owner::{Owner, Verdict};
use super::StreamError;
use crate::deletion::{self, DeleteError};

/// Default image: the pinned busybox of debug containers (#202); its
/// `nsenter` is all the shell needs.
pub const DEFAULT_IMAGE: &str = super::debug::DEFAULT_IMAGE;
/// Default namespace of node shell pods.
pub const DEFAULT_NAMESPACE: &str = "default";
/// How long the pod may take to start (image pull included).
pub const START_TIMEOUT: Duration = Duration::from_secs(120);

pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
pub const MANAGED_BY: &str = "kubach";
/// Label with the node's name (cut to a valid label value).
pub const NODE_SHELL_LABEL: &str = "kubach.dev/node-shell";

/// Selects node shell pods of Kubach and nothing else.
pub fn label_selector() -> String {
    format!("{MANAGED_BY_LABEL}={MANAGED_BY},{NODE_SHELL_LABEL}")
}

/// What the exec runs: a login shell in the host's namespaces, bash when
/// the host has it.
pub fn shell_command() -> Vec<String> {
    [
        "nsenter",
        "-t",
        "1",
        "-m",
        "-u",
        "-i",
        "-n",
        "-p",
        "--",
        "sh",
        "-c",
        "command -v bash >/dev/null 2>&1 && exec bash -l || exec sh -l",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// `node` as a label value: at most 63 characters, starting and ending
/// with an alphanumeric one.
fn label_value(node: &str) -> String {
    let cut: String = node.chars().take(63).collect();
    cut.trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_owned()
}

/// The pod for a shell on `node` in `namespace` running `image`, owned by
/// this process.
pub fn shell_pod(node: &str, namespace: &str, image: &str) -> Pod {
    shell_pod_of(Owner::current(), Timestamp::now(), node, namespace, image)
}

/// The pod for a shell on `node`, owned by `owner` with a first heartbeat
/// at `heartbeat`.
pub fn shell_pod_of(
    owner: &Owner,
    heartbeat: Timestamp,
    node: &str,
    namespace: &str,
    image: &str,
) -> Pod {
    let mut labels = owner.labels();
    labels.extend([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY.to_owned()),
        (NODE_SHELL_LABEL.to_owned(), label_value(node)),
    ]);
    // generateName is cut to 58 characters by the API server.
    let prefix: String = format!("node-shell-{}", node.replace('.', "-"))
        .chars()
        .take(52)
        .collect();
    let container = Container {
        name: "shell".to_owned(),
        image: Some(image.to_owned()),
        // Sleeps until deleted; the shell is an exec.
        command: Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "trap 'exit 0' TERM INT; while true; do sleep 3600 & wait $!; done".to_owned(),
        ]),
        security_context: Some(SecurityContext {
            privileged: Some(true),
            ..SecurityContext::default()
        }),
        stdin: Some(true),
        tty: Some(true),
        ..Container::default()
    };
    Pod {
        metadata: ObjectMeta {
            generate_name: Some(format!("{}-", prefix.trim_end_matches('-'))),
            namespace: Some(namespace.to_owned()),
            labels: Some(labels),
            annotations: Some(owner.annotations(heartbeat)),
            ..ObjectMeta::default()
        },
        spec: Some(PodSpec {
            node_name: Some(node.to_owned()),
            host_pid: Some(true),
            host_network: Some(true),
            host_ipc: Some(true),
            containers: vec![container],
            // Runs on any taint, NoExecute included.
            tolerations: Some(vec![Toleration {
                operator: Some("Exists".to_owned()),
                ..Toleration::default()
            }]),
            restart_policy: Some("Never".to_owned()),
            termination_grace_period_seconds: Some(0),
            automount_service_account_token: Some(false),
            ..PodSpec::default()
        }),
        status: None,
    }
}

/// Explains why the API server refused the pod: Pod Security admission,
/// another admission policy, RBAC.
pub fn explain_create_error(error: &kube::Error, namespace: &str) -> String {
    let kube::Error::Api(status) = error else {
        return format!("Couldn't create the node shell pod: {error}");
    };
    let message = &status.message;
    if message.contains("violates PodSecurity") {
        format!(
            "Pod Security admission in namespace {namespace} forbids privileged pods: {message}. \
             Pick a namespace that allows them (pod-security.kubernetes.io/enforce=privileged) \
             for node shells in Settings → General → Terminal."
        )
    } else if message.contains("admission webhook") || message.contains("denied the request") {
        format!("An admission policy refused the node shell pod: {message}")
    } else if status.is_forbidden() {
        format!("No permission (RBAC) to create pods in {namespace}: {message}")
    } else if status.is_not_found() {
        format!("Namespace {namespace} not found: {message}")
    } else {
        format!("Couldn't create the node shell pod: {message}")
    }
}

/// A created shell pod: its generated name and its UID, which the delete
/// carries as a precondition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedPod {
    pub name: String,
    pub uid: Option<String>,
}

/// Creates the shell pod.
pub async fn create_shell_pod(
    client: &Client,
    node: &str,
    namespace: &str,
    image: &str,
) -> Result<CreatedPod, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pod = shell_pod(node, namespace, image);
    let created = api
        .create(&PostParams::default(), &pod)
        .await
        .map_err(|error| StreamError::Remote(explain_create_error(&error, namespace)))?;
    let name = created.name_any();
    register_live(namespace, &name);
    Ok(CreatedPod {
        name,
        uid: created.uid(),
    })
}

/// Why a pending pod is not running yet, from its container and
/// scheduling state; `None` while there is nothing to say.
fn pending_reason(pod: &Pod) -> Option<String> {
    let status = pod.status.as_ref()?;
    let waiting = status
        .container_statuses
        .iter()
        .flatten()
        .filter_map(|c| c.state.as_ref()?.waiting.as_ref())
        .next();
    if let Some(waiting) = waiting
        && let Some(reason) = &waiting.reason
        && reason != "ContainerCreating"
    {
        return Some(match &waiting.message {
            Some(message) => format!("{reason}: {message}"),
            None => reason.clone(),
        });
    }
    status
        .conditions
        .iter()
        .flatten()
        .find(|c| c.type_ == "PodScheduled" && c.status == "False")
        .map(|c| c.message.clone().or(c.reason.clone()).unwrap_or_default())
}

/// What a polled pod says: running, still waiting (and why), or failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PodProgress {
    Running,
    Waiting(Option<String>),
    Failed(String),
}

pub fn progress(pod: &Pod) -> PodProgress {
    let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
    match phase {
        Some("Running") => PodProgress::Running,
        Some("Succeeded" | "Failed") => {
            let status = pod.status.as_ref();
            let terminated = status
                .and_then(|s| s.container_statuses.as_ref())
                .and_then(|c| c.first())
                .and_then(|c| c.state.as_ref()?.terminated.as_ref());
            let reason = terminated
                .and_then(|t| t.message.clone().or(t.reason.clone()))
                .or_else(|| status.and_then(|s| s.message.clone().or(s.reason.clone())))
                .unwrap_or_else(|| "the pod stopped".to_owned());
            PodProgress::Failed(reason)
        }
        _ => {
            let reason = pending_reason(pod);
            // These do not get better by waiting.
            match &reason {
                Some(r)
                    if r.starts_with("CreateContainerConfigError")
                        || r.starts_with("InvalidImageName") =>
                {
                    PodProgress::Failed(r.clone())
                }
                _ => PodProgress::Waiting(reason),
            }
        }
    }
}

/// Waits until the shell pod runs; on timeout says what it waited for
/// (`ImagePullBackOff`, unschedulable).
pub async fn wait_running(
    client: &Client,
    namespace: &str,
    name: &str,
    timeout: Duration,
) -> Result<(), StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        let pod = api.get(name).await?;
        match progress(&pod) {
            PodProgress::Running => return Ok(()),
            PodProgress::Failed(reason) => {
                return Err(StreamError::Remote(format!(
                    "Node shell pod {namespace}/{name} failed: {reason}"
                )));
            }
            PodProgress::Waiting(reason) => last = reason.or(last),
        }
        if Instant::now() >= deadline {
            let why = last.unwrap_or_else(|| "still pending".to_owned());
            return Err(StreamError::Timeout(format!(
                "node shell pod {namespace}/{name} did not start in {}s: {why}",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Deletes the shell pod at once (grace 0), only if it still has `uid`
/// when given; a missing pod is fine. The pod stops counting as live
/// first, so one whose deletion fails is swept on the next connect.
pub async fn delete_shell_pod(
    client: &Client,
    namespace: &str,
    name: &str,
    uid: Option<&str>,
) -> Result<(), DeleteError> {
    unregister_live(namespace, name);
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = deletion::with_uid(DeleteParams::default().grace_period(0), uid);
    deletion::finish(api.delete(name, &params).await)
}

/// Sets the pod's heartbeat to now: a merge patch of one annotation, no
/// resourceVersion to conflict on. With `uid` the patch applies only to
/// that pod, not to one recreated under its name.
pub async fn heartbeat(
    client: &Client,
    namespace: &str,
    name: &str,
    uid: Option<&str>,
) -> Result<(), kube::Error> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let mut metadata = serde_json::json!({"annotations": {
        owner::HEARTBEAT_ANNOTATION: Timestamp::now().to_string()
    }});
    if let Some(uid) = uid.filter(|uid| !uid.is_empty()) {
        metadata["uid"] = uid.into();
    }
    let patch = Patch::Merge(serde_json::json!({ "metadata": metadata }));
    api.patch(name, &PatchParams::default(), &patch).await?;
    Ok(())
}

/// Refreshes the pod's heartbeat every [`owner::HEARTBEAT_INTERVAL`] until
/// dropped; a failed refresh is retried on the next tick.
pub async fn keep_alive(client: Client, namespace: String, name: String, uid: Option<String>) {
    // The pod is created with a fresh heartbeat: the first refresh is due
    // one interval later.
    let start = tokio::time::Instant::now() + owner::HEARTBEAT_INTERVAL;
    let mut ticks = tokio::time::interval_at(start, owner::HEARTBEAT_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        heartbeat(&client, &namespace, &name, uid.as_deref())
            .await
            .ok();
    }
}

/// Node shell pods of this process still in use: [`sweep_orphans`] keeps
/// them.
static LIVE: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

fn register_live(namespace: &str, name: &str) {
    LIVE.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_default()
        .insert((namespace.to_owned(), name.to_owned()));
}

fn unregister_live(namespace: &str, name: &str) {
    if let Some(live) = LIVE.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        live.remove(&(namespace.to_owned(), name.to_owned()));
    }
}

fn is_live(namespace: &str, name: &str) -> bool {
    LIVE.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|live| live.contains(&(namespace.to_owned(), name.to_owned())))
}

/// Whether `pod` is a node shell pod of Kubach (both labels), so a sweep
/// never deletes anything else even if the selector were wrong.
pub fn is_shell_pod(pod: &Pod) -> bool {
    let labels = pod.labels();
    labels.get(MANAGED_BY_LABEL).map(String::as_str) == Some(MANAGED_BY)
        && labels.contains_key(NODE_SHELL_LABEL)
}

/// Deletes node shell pods left behind (by a crash) in every namespace, or
/// in `fallback` when listing all namespaces is not allowed, as
/// [`owner::decide`] says: orphans of this installation, and pods of
/// anyone else whose heartbeat stopped long ago. Returns how many were
/// deleted.
pub async fn sweep_orphans(client: &Client, fallback: &str) -> Result<usize, kube::Error> {
    let params = ListParams::default().labels(&label_selector());
    let all: Api<Pod> = Api::all(client.clone());
    let pods = match all.list(&params).await {
        Ok(list) => list.items,
        Err(kube::Error::Api(status)) if status.is_forbidden() => {
            let api: Api<Pod> = Api::namespaced(client.clone(), fallback);
            api.list(&params).await?.items
        }
        Err(error) => return Err(error),
    };
    let me = Owner::current();
    let now = Timestamp::now();
    let mut deleted = 0;
    for pod in pods.iter().filter(|pod| is_shell_pod(pod)) {
        let namespace = pod.namespace().unwrap_or_default();
        let name = pod.name_any();
        let live = is_live(&namespace, &name);
        let verdict = owner::decide(pod, me, now, live, crate::kubeconfig::temp::is_alive);
        if matches!(verdict, Verdict::Keep(_)) {
            continue;
        }
        let uid = pod.uid();
        if delete_shell_pod(client, &namespace, &name, uid.as_deref())
            .await
            .is_ok()
        {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A fixed owner: `shell_pod` would create the installation id in the
    /// real config directory.
    fn test_pod(node: &str, namespace: &str, image: &str) -> Pod {
        let owner = Owner {
            install_id: "11111111-1111-4111-8111-111111111111".into(),
            pid: 42,
            host: "laptop".into(),
        };
        let at = "2026-10-25T12:00:00Z".parse().unwrap();
        shell_pod_of(&owner, at, node, namespace, image)
    }

    #[test]
    fn pod_is_privileged_on_the_node_and_labelled() {
        let pod = test_pod("minikube", "tools", "busybox:1.36");
        let value = serde_json::to_value(&pod).unwrap();
        assert_eq!(value["metadata"]["generateName"], "node-shell-minikube-");
        assert_eq!(value["metadata"]["namespace"], "tools");
        assert_eq!(
            value["metadata"]["labels"],
            json!({
                MANAGED_BY_LABEL: "kubach",
                NODE_SHELL_LABEL: "minikube",
                owner::OWNER_LABEL: owner::install_hash("11111111-1111-4111-8111-111111111111"),
            })
        );
        assert_eq!(
            value["metadata"]["annotations"],
            json!({
                owner::INSTALL_ANNOTATION: "11111111-1111-4111-8111-111111111111",
                owner::PID_ANNOTATION: "42",
                owner::HOST_ANNOTATION: "laptop",
                owner::HEARTBEAT_ANNOTATION: "2026-10-25T12:00:00Z",
            })
        );
        let spec = &value["spec"];
        assert_eq!(spec["nodeName"], "minikube");
        assert_eq!(spec["hostPID"], true);
        assert_eq!(spec["hostNetwork"], true);
        assert_eq!(spec["hostIPC"], true);
        assert_eq!(spec["restartPolicy"], "Never");
        assert_eq!(spec["terminationGracePeriodSeconds"], 0);
        assert_eq!(spec["tolerations"], json!([{"operator": "Exists"}]));
        let container = &spec["containers"][0];
        assert_eq!(container["image"], "busybox:1.36");
        assert_eq!(container["securityContext"]["privileged"], true);
        assert!(is_shell_pod(&pod));
    }

    #[test]
    fn long_node_names_make_valid_labels() {
        let node = format!("{}.compute.internal", "ip-10-0-0-1".repeat(8));
        let pod = test_pod(&node, "default", DEFAULT_IMAGE);
        let label = &pod.labels()[NODE_SHELL_LABEL];
        assert!(label.len() <= 63);
        assert!(label.ends_with(|c: char| c.is_ascii_alphanumeric()));
        let prefix = pod.metadata.generate_name.unwrap();
        assert!(prefix.len() <= 54 && !prefix.contains('.'));
        assert_eq!(label_value("-a.b-"), "a.b");
    }

    #[test]
    fn shell_enters_the_host() {
        let command = shell_command();
        assert_eq!(
            command[..9],
            ["nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--"]
        );
        assert!(command.last().unwrap().contains("bash -l"));
        assert_eq!(
            label_selector(),
            "app.kubernetes.io/managed-by=kubach,kubach.dev/node-shell"
        );
    }

    #[test]
    fn foreign_pods_are_never_shell_pods() {
        let other: Pod = serde_json::from_value(json!({
            "metadata": {"name": "x", "labels": {MANAGED_BY_LABEL: "helm", NODE_SHELL_LABEL: "n"}}
        }))
        .unwrap();
        assert!(!is_shell_pod(&other));
        let half: Pod = serde_json::from_value(json!({
            "metadata": {"name": "x", "labels": {MANAGED_BY_LABEL: "kubach"}}
        }))
        .unwrap();
        assert!(!is_shell_pod(&half));
    }

    #[test]
    fn live_pods_are_kept() {
        register_live("ns", "node-shell-a");
        assert!(is_live("ns", "node-shell-a"));
        assert!(!is_live("ns", "node-shell-b"));
        unregister_live("ns", "node-shell-a");
        assert!(!is_live("ns", "node-shell-a"));
    }

    fn pod_with(status: serde_json::Value) -> Pod {
        serde_json::from_value(json!({"metadata": {"name": "p"}, "status": status})).unwrap()
    }

    #[test]
    fn progress_explains_waiting_and_failures() {
        assert_eq!(
            progress(&pod_with(json!({"phase": "Running"}))),
            PodProgress::Running
        );
        assert_eq!(
            progress(&pod_with(json!({"phase": "Pending"}))),
            PodProgress::Waiting(None)
        );
        let pull = pod_with(json!({"phase": "Pending", "containerStatuses": [{
            "name": "shell", "ready": false, "restartCount": 0, "image": "x", "imageID": "",
            "state": {"waiting": {"reason": "ImagePullBackOff", "message": "Back-off pulling"}}
        }]}));
        assert_eq!(
            progress(&pull),
            PodProgress::Waiting(Some("ImagePullBackOff: Back-off pulling".into()))
        );
        let unschedulable = pod_with(json!({"phase": "Pending", "conditions": [{
            "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
            "message": "node(s) didn't match"
        }]}));
        assert_eq!(
            progress(&unschedulable),
            PodProgress::Waiting(Some("node(s) didn't match".into()))
        );
        let config = pod_with(json!({"phase": "Pending", "containerStatuses": [{
            "name": "shell", "ready": false, "restartCount": 0, "image": "x", "imageID": "",
            "state": {"waiting": {"reason": "CreateContainerConfigError"}}
        }]}));
        assert!(matches!(progress(&config), PodProgress::Failed(_)));
        let failed =
            pod_with(json!({"phase": "Failed", "reason": "Evicted", "message": "low disk"}));
        assert_eq!(progress(&failed), PodProgress::Failed("low disk".into()));
    }

    #[test]
    fn explains_admission_refusals() {
        let error = |code: u16, reason: &str, message: &str| {
            kube::Error::Api(Box::new(
                serde_json::from_value(json!({
                    "status": "Failure", "code": code, "reason": reason, "message": message
                }))
                .unwrap(),
            ))
        };
        let psa = error(
            403,
            "Forbidden",
            "pods \"x\" is forbidden: violates PodSecurity \"baseline:latest\": privileged",
        );
        let text = explain_create_error(&psa, "apps");
        assert!(text.starts_with("Pod Security admission in namespace apps"));
        assert!(text.contains("Settings"));
        let webhook = error(
            400,
            "BadRequest",
            "admission webhook \"validate.kyverno.svc\" denied the request: no privileged",
        );
        assert!(explain_create_error(&webhook, "apps").starts_with("An admission policy"));
        let rbac = error(
            403,
            "Forbidden",
            "pods is forbidden: User \"dev\" cannot create",
        );
        assert!(explain_create_error(&rbac, "apps").starts_with("No permission (RBAC)"));
    }
}
