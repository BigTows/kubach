use super::*;
use serde_json::{Value, json};

const NOW: &str = "2026-10-22T12:00:00Z";

fn now() -> Timestamp {
    NOW.parse().unwrap()
}

fn pod(value: Value) -> Pod {
    serde_json::from_value(value).unwrap()
}

fn event(value: Value) -> Event {
    serde_json::from_value(value).unwrap()
}

fn run(pod: &Pod) -> Vec<Finding> {
    diagnose_at(pod, &[], now())
}

/// A running pod with one ready `api` container; tests override parts of it.
fn base(status: Value) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": "web", "namespace": "default", "uid": "pod-uid" },
        "spec": {
            "containers": [{
                "name": "api",
                "image": "ghcr.io/acme/api:1.0",
                "resources": { "limits": { "memory": "256Mi", "cpu": "500m" } }
            }]
        },
        "status": status
    })
}

fn running_status() -> Value {
    json!({
        "phase": "Running",
        "conditions": [
            { "type": "PodScheduled", "status": "True" },
            { "type": "Initialized", "status": "True" },
            { "type": "ContainersReady", "status": "True" },
            { "type": "Ready", "status": "True" }
        ],
        "containerStatuses": [{
            "name": "api",
            "image": "ghcr.io/acme/api:1.0",
            "imageID": "",
            "ready": true,
            "started": true,
            "restartCount": 0,
            "state": { "running": { "startedAt": "2026-10-22T11:00:00Z" } }
        }]
    })
}

fn with_container_status(cs: Value) -> Pod {
    let mut status = running_status();
    status["containerStatuses"] = json!([cs]);
    pod(base(status))
}

fn only(findings: &[Finding]) -> &Finding {
    assert_eq!(findings.len(), 1, "{findings:#?}");
    &findings[0]
}

#[test]
fn healthy_pod_has_no_findings() {
    assert!(run(&pod(base(running_status()))).is_empty());
}

#[test]
fn completed_job_pod_has_no_findings() {
    let pod = pod(base(json!({
        "phase": "Succeeded",
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "terminated": { "exitCode": 0, "reason": "Completed" } }
        }]
    })));
    assert!(run(&pod).is_empty());
}

#[test]
fn crash_loop_back_off() {
    let pod = with_container_status(json!({
        "name": "api", "image": "ghcr.io/acme/api:1.0", "imageID": "",
        "ready": false, "restartCount": 12,
        "state": { "waiting": { "reason": "CrashLoopBackOff", "message": "back-off 5m0s restarting failed container" } },
        "lastState": { "terminated": { "exitCode": 1, "reason": "Error", "message": "panic: db unreachable" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::CrashLoopBackOff);
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(f.container.as_deref(), Some("api"));
    assert_eq!(
        f.title.english(),
        "Container `api` is crash looping (exit code 1, 12 restarts)"
    );
    assert!(f.detail().contains("exit code 1"), "{}", f.detail());
    assert!(
        f.detail().contains("panic: db unreachable"),
        "{}",
        f.detail()
    );
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Check logs of the previous run")
    );
}

#[test]
fn crash_loop_names_signal() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 1,
        "state": { "waiting": { "reason": "CrashLoopBackOff" } },
        "lastState": { "terminated": { "exitCode": 139, "reason": "Error" } }
    }));
    let f = only(&run(&pod)).clone();
    assert!(
        f.title.english().ends_with("(exit code 139, 1 restart)"),
        "{}",
        f.title.english()
    );
    assert!(f.detail().contains("SIGSEGV"));
}

#[test]
fn image_pull_back_off() {
    let pod = with_container_status(json!({
        "name": "api", "image": "ghcr.io/acme/api:9.9", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": {
            "reason": "ImagePullBackOff",
            "message": "Back-off pulling image \"ghcr.io/acme/api:9.9\": manifest unknown"
        } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ImagePull);
    assert_eq!(
        f.title.english(),
        "Container `api` cannot pull image `ghcr.io/acme/api:9.9` (ImagePullBackOff)"
    );
    assert!(f.detail().contains("manifest unknown"));
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Check that the image name and tag exist in the registry")
    );
}

#[test]
fn err_image_pull_unauthorized() {
    let pod = with_container_status(json!({
        "name": "api", "image": "private/api:1", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "ErrImagePull", "message": "pull access denied, authorization failed" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ImagePull);
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Check imagePullSecrets and registry credentials")
    );
}

#[test]
fn invalid_image_name() {
    let pod = with_container_status(json!({
        "name": "api", "image": "", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "InvalidImageName", "message": "couldn't parse image name" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ImagePull);
    // Falls back to the image from the spec.
    assert!(
        f.title.english().contains("`ghcr.io/acme/api:1.0`"),
        "{}",
        f.title.english()
    );
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Fix the image reference in the pod spec")
    );
}

#[test]
fn oom_killed_in_crash_loop_mentions_limit() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 4,
        "state": { "waiting": { "reason": "CrashLoopBackOff" } },
        "lastState": { "terminated": { "exitCode": 137, "reason": "OOMKilled" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::OomKilled);
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(
        f.title.english(),
        "Container `api` was OOMKilled (limit 256Mi, 4 restarts, crash looping)"
    );
    assert!(f.hint.as_ref().unwrap().english().contains("256Mi"));
}

#[test]
fn oom_killed_current_state_without_limit() {
    let mut value = base(json!({
        "phase": "Failed",
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "terminated": { "exitCode": 137, "reason": "OOMKilled" } }
        }]
    }));
    value["spec"]["containers"][0]["resources"] = json!({});
    let f = only(&run(&pod(value))).clone();
    assert_eq!(f.kind, FindingKind::OomKilled);
    assert_eq!(
        f.title.english(),
        "Container `api` was OOMKilled (0 restarts)"
    );
    assert!(f.detail().contains("no memory limit"));
}

#[test]
fn oom_killed_previously_but_running_is_warning() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": true, "started": true, "restartCount": 2,
        "state": { "running": {} },
        "lastState": { "terminated": { "exitCode": 137, "reason": "OOMKilled" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::OomKilled);
    assert_eq!(f.severity, Severity::Warning);
}

#[test]
fn restarted_running_container_is_info() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": true, "started": true, "restartCount": 3,
        "state": { "running": {} },
        "lastState": { "terminated": { "exitCode": 2, "reason": "Error" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::Restarted);
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(
        f.title.english(),
        "Container `api` restarted (exit code 2, 3 restarts)"
    );
}

fn unschedulable(message: &str) -> Pod {
    pod(base(json!({
        "phase": "Pending",
        "conditions": [{
            "type": "PodScheduled", "status": "False", "reason": "Unschedulable", "message": message
        }]
    })))
}

#[test]
fn pending_insufficient_cpu_and_memory() {
    let pod = unschedulable(
        "0/3 nodes are available: 1 Insufficient cpu, 2 Insufficient memory. \
         preemption: 0/3 nodes are available: 3 No preemption victims found for incoming pod.",
    );
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![
            SchedulingCause::InsufficientCpu,
            SchedulingCause::InsufficientMemory
        ])
    );
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(
        f.title.english(),
        "Pod cannot be scheduled: insufficient CPU, insufficient memory"
    );
    assert!(f.detail().starts_with("0/3 nodes are available"));
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Lower the container requests or add capacity to the cluster")
    );
}

#[test]
fn pending_insufficient_extended_resource() {
    let pod = unschedulable("0/2 nodes are available: 2 Insufficient nvidia.com/gpu.");
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::InsufficientResource(
            "nvidia.com/gpu".into()
        )])
    );
}

#[test]
fn pending_untolerated_taint() {
    let pod = unschedulable(
        "0/1 nodes are available: 1 node(s) had untolerated taint {node-role.kubernetes.io/control-plane: }.",
    );
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::UntoleratedTaint])
    );
    assert_eq!(
        f.title.english(),
        "Pod cannot be scheduled: untolerated taints"
    );
}

#[test]
fn pending_legacy_taint_message() {
    let pod = unschedulable(
        "0/1 nodes are available: 1 node(s) had taint {dedicated: gpu}, that the pod didn't tolerate.",
    );
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::UntoleratedTaint])
    );
}

#[test]
fn pending_node_selector_mismatch() {
    let pod = unschedulable(
        "0/3 nodes are available: 3 node(s) didn't match Pod's node affinity/selector.",
    );
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::NodeSelectorMismatch])
    );
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Check nodeSelector/affinity against node labels")
    );
}

#[test]
fn pending_unbound_pvc() {
    let pod =
        unschedulable("0/3 nodes are available: pod has unbound immediate PersistentVolumeClaims.");
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::UnboundPvc])
    );
    assert_eq!(
        f.title.english(),
        "Pod cannot be scheduled: unbound PersistentVolumeClaim"
    );
}

#[test]
fn pending_missing_pvc() {
    let pod = unschedulable("persistentvolumeclaim \"data\" not found");
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::Unschedulable(vec![SchedulingCause::UnboundPvc])
    );
}

#[test]
fn pending_unknown_message_falls_back_to_raw() {
    let pod = unschedulable("something the scheduler has never said before");
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::Unschedulable(vec![]));
    assert_eq!(f.title.english(), "Pod cannot be scheduled");
    assert_eq!(f.detail(), "something the scheduler has never said before");
    assert!(f.hint.is_some());
}

#[test]
fn pending_scheduling_gated_is_info() {
    let pod = pod(base(json!({
        "phase": "Pending",
        "conditions": [{
            "type": "PodScheduled", "status": "False", "reason": "SchedulingGated",
            "message": "Scheduling is blocked due to non-empty scheduling gates"
        }]
    })));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::SchedulingGated);
    assert_eq!(f.severity, Severity::Info);
}

#[test]
fn create_container_config_error_missing_configmap() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "CreateContainerConfigError", "message": "configmap \"app-config\" not found" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::ContainerConfig(Some(MissingRef::ConfigMap("app-config".into())))
    );
    assert_eq!(
        f.title.english(),
        "Container `api` references missing ConfigMap `app-config`"
    );
}

#[test]
fn create_container_config_error_missing_secret() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "CreateContainerConfigError", "message": "secret \"db-credentials\" not found" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::ContainerConfig(Some(MissingRef::Secret("db-credentials".into())))
    );
}

#[test]
fn create_container_config_error_missing_key() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": {
            "reason": "CreateContainerConfigError",
            "message": "couldn't find key DB_URL in Secret default/db"
        } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(
        f.kind,
        FindingKind::ContainerConfig(Some(MissingRef::Secret("db".into())))
    );
}

#[test]
fn create_container_config_error_unknown() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "CreateContainerConfigError", "message": "runAsNonRoot and image will run as root" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ContainerConfig(None));
    assert_eq!(
        f.title.english(),
        "Container `api` has an invalid configuration"
    );
}

fn init_pod(init_status: Value) -> Pod {
    let mut value = base(json!({
        "phase": "Pending",
        "conditions": [
            { "type": "PodScheduled", "status": "True" },
            { "type": "Initialized", "status": "False", "reason": "ContainersNotInitialized" }
        ],
        "initContainerStatuses": [init_status],
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "waiting": { "reason": "PodInitializing" } }
        }]
    }));
    value["spec"]["initContainers"] = json!([{ "name": "migrate", "image": "acme/migrate:1" }]);
    pod(value)
}

#[test]
fn init_container_crash_loop() {
    let pod = init_pod(json!({
        "name": "migrate", "image": "acme/migrate:1", "imageID": "", "ready": false, "restartCount": 3,
        "state": { "waiting": { "reason": "CrashLoopBackOff" } },
        "lastState": { "terminated": { "exitCode": 1, "reason": "Error" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::InitContainerFailed);
    assert_eq!(f.container.as_deref(), Some("migrate"));
    assert_eq!(
        f.title.english(),
        "Init container `migrate` is crash looping (exit code 1, 3 restarts)"
    );
}

#[test]
fn init_container_failed_terminated() {
    let pod = init_pod(json!({
        "name": "migrate", "image": "acme/migrate:1", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "terminated": { "exitCode": 2, "reason": "Error", "message": "relation already exists" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::InitContainerFailed);
    assert_eq!(
        f.title.english(),
        "Init container `migrate` failed (exit code 2)"
    );
    assert!(f.detail().contains("relation already exists"));
}

#[test]
fn init_container_image_pull() {
    let pod = init_pod(json!({
        "name": "migrate", "image": "acme/migrate:1", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "ErrImagePull", "message": "not found" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ImagePull);
    assert!(f.title.english().starts_with("Init container `migrate`"));
}

#[test]
fn readiness_failing_while_running() {
    let mut status = running_status();
    status["conditions"][3] = json!({
        "type": "Ready", "status": "False", "reason": "ContainersNotReady",
        "message": "containers with unready status: [api]"
    });
    status["containerStatuses"][0]["ready"] = json!(false);
    let f = only(&run(&pod(base(status)))).clone();
    assert_eq!(f.kind, FindingKind::NotReady);
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(
        f.title.english(),
        "Container `api` is running but not ready"
    );
    assert_eq!(f.detail(), "containers with unready status: [api]");
}

#[test]
fn startup_probe_not_passed() {
    let mut status = running_status();
    status["conditions"][3] = json!({ "type": "Ready", "status": "False" });
    status["containerStatuses"][0]["ready"] = json!(false);
    status["containerStatuses"][0]["started"] = json!(false);
    let f = only(&run(&pod(base(status)))).clone();
    assert_eq!(
        f.title.english(),
        "Container `api` has not passed its startup probe"
    );
}

#[test]
fn readiness_gate_pending() {
    let mut status = running_status();
    status["conditions"][3] = json!({
        "type": "Ready", "status": "False", "reason": "ReadinessGatesNotReady",
        "message": "corresponding condition of pod readiness gate \"lb\" does not exist."
    });
    let f = only(&run(&pod(base(status)))).clone();
    assert_eq!(f.kind, FindingKind::NotReady);
    assert_eq!(f.container, None);
}

#[test]
fn crash_loop_does_not_duplicate_as_not_ready() {
    let mut status = running_status();
    status["conditions"][3] = json!({ "type": "Ready", "status": "False" });
    status["containerStatuses"] = json!([{
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 5,
        "state": { "waiting": { "reason": "CrashLoopBackOff" } },
        "lastState": { "terminated": { "exitCode": 1, "reason": "Error" } }
    }]);
    let f = only(&run(&pod(base(status)))).clone();
    assert_eq!(f.kind, FindingKind::CrashLoopBackOff);
}

fn terminating(deletion: &str, finalizers: Value) -> Pod {
    let mut value = base(running_status());
    value["metadata"]["deletionTimestamp"] = json!(deletion);
    value["metadata"]["deletionGracePeriodSeconds"] = json!(30);
    value["metadata"]["finalizers"] = finalizers;
    pod(value)
}

#[test]
fn terminating_within_grace_period_is_info() {
    let f = only(&run(&terminating("2026-10-22T12:00:20Z", json!(null)))).clone();
    assert_eq!(f.kind, FindingKind::Terminating);
    assert_eq!(f.severity, Severity::Info);
    assert!(f.detail().contains("20s"));
}

#[test]
fn stuck_terminating_with_finalizers() {
    let pod = terminating("2026-10-22T11:48:00Z", json!(["example.com/cleanup"]));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::StuckTerminating);
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.title.english(), "Pod is stuck terminating for 12m");
    assert!(f.detail().contains("example.com/cleanup"));
    assert!(f.hint.as_ref().unwrap().english().contains("finalizers"));
}

#[test]
fn stuck_terminating_without_finalizers() {
    let f = only(&run(&terminating("2026-10-22T09:00:00Z", json!([])))).clone();
    assert_eq!(f.title.english(), "Pod is stuck terminating for 3h");
    assert!(f.hint.as_ref().unwrap().english().contains("kubelet"));
}

#[test]
fn evicted_pod() {
    let pod = pod(base(json!({
        "phase": "Failed",
        "reason": "Evicted",
        "message": "The node was low on resource: memory."
    })));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::Evicted);
    assert_eq!(f.detail(), "The node was low on resource: memory.");
}

#[test]
fn failed_container_exit_code() {
    let pod = pod(base(json!({
        "phase": "Failed",
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "terminated": { "exitCode": 3, "reason": "Error" } }
        }]
    })));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ContainerFailed);
    assert_eq!(f.title.english(), "Container `api` failed (exit code 3)");
}

#[test]
fn unknown_waiting_reason_is_reported() {
    let pod = with_container_status(json!({
        "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
        "state": { "waiting": { "reason": "CreateContainerError", "message": "name already in use" } }
    }));
    let f = only(&run(&pod)).clone();
    assert_eq!(f.kind, FindingKind::ContainerWaiting);
    assert_eq!(
        f.title.english(),
        "Container `api` is waiting: CreateContainerError"
    );
}

#[test]
fn container_creating_is_not_a_problem() {
    let pod = pod(base(json!({
        "phase": "Pending",
        "conditions": [{ "type": "PodScheduled", "status": "True" }],
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "waiting": { "reason": "ContainerCreating" } }
        }]
    })));
    assert!(run(&pod).is_empty());
}

fn creating_pod() -> Pod {
    pod(base(json!({
        "phase": "Pending",
        "conditions": [{ "type": "PodScheduled", "status": "True" }],
        "containerStatuses": [{
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
            "state": { "waiting": { "reason": "ContainerCreating" } }
        }]
    })))
}

fn warning(uid: &str, reason: &str, message: &str, at: &str) -> Event {
    event(json!({
        "metadata": { "name": format!("web.{reason}.{at}"), "namespace": "default" },
        "involvedObject": { "kind": "Pod", "name": "web", "namespace": "default", "uid": uid },
        "type": "Warning",
        "reason": reason,
        "message": message,
        "lastTimestamp": at
    }))
}

#[test]
fn failed_mount_event_reports_latest() {
    let events = [
        warning(
            "pod-uid",
            "FailedMount",
            "old message",
            "2026-10-22T11:50:00Z",
        ),
        warning(
            "pod-uid",
            "FailedMount",
            "MountVolume.SetUp failed for volume \"tls\" : secret \"web-tls\" not found",
            "2026-10-22T11:59:00Z",
        ),
        warning(
            "other-uid",
            "FailedMount",
            "not ours",
            "2026-10-22T11:59:30Z",
        ),
    ];
    let findings = diagnose_at(&creating_pod(), &events, now());
    let f = only(&findings).clone();
    assert_eq!(f.kind, FindingKind::VolumeMountFailed);
    assert!(f.detail().contains("web-tls"));
    assert_eq!(
        f.hint.as_ref().map(Message::english).as_deref(),
        Some("Create Secret `web-tls` or fix the volume reference")
    );
}

#[test]
fn sandbox_failure_event() {
    let events = [warning(
        "pod-uid",
        "FailedCreatePodSandBox",
        "failed to setup network for sandbox",
        "2026-10-22T11:59:00Z",
    )];
    let f = only(&diagnose_at(&creating_pod(), &events, now())).clone();
    assert_eq!(f.kind, FindingKind::SandboxFailed);
}

#[test]
fn events_ignored_once_pod_runs() {
    let events = [warning(
        "pod-uid",
        "FailedMount",
        "transient",
        "2026-10-22T10:00:00Z",
    )];
    assert!(diagnose_at(&pod(base(running_status())), &events, now()).is_empty());
}

#[test]
fn findings_sorted_by_severity() {
    let mut status = running_status();
    status["containerStatuses"] = json!([
        {
            "name": "sidecar", "image": "x", "imageID": "", "ready": true, "started": true,
            "restartCount": 1, "state": { "running": {} },
            "lastState": { "terminated": { "exitCode": 1, "reason": "Error" } }
        },
        {
            "name": "api", "image": "x", "imageID": "", "ready": false, "restartCount": 7,
            "state": { "waiting": { "reason": "CrashLoopBackOff" } },
            "lastState": { "terminated": { "exitCode": 1, "reason": "Error" } }
        }
    ]);
    let findings = run(&pod(base(status)));
    let severities: Vec<Severity> = findings.iter().map(|f| f.severity).collect();
    assert_eq!(severities, [Severity::Error, Severity::Info]);
}

#[test]
fn pod_without_status_has_no_findings() {
    let pod = pod(json!({
        "metadata": { "name": "web" },
        "spec": { "containers": [{ "name": "api" }] }
    }));
    assert!(run(&pod).is_empty());
}
