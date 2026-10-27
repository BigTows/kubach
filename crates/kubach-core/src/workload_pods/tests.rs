use std::sync::Arc;

use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;

use super::*;
use crate::apply::yaml;

const WORKLOADS: &str = include_str!("fixtures/workloads.yaml");

fn objects() -> Vec<Arc<DynamicObject>> {
    yaml::parse(WORKLOADS)
        .unwrap()
        .into_iter()
        .map(|manifest| Arc::new(serde_json::from_value(manifest.value).unwrap()))
        .collect()
}

fn of_kind(all: &[Arc<DynamicObject>], kind: &str) -> Vec<Arc<DynamicObject>> {
    all.iter()
        .filter(|o| o.types.as_ref().is_some_and(|t| t.kind == kind))
        .cloned()
        .collect()
}

fn find(all: &[Arc<DynamicObject>], kind: &str, name: &str) -> Arc<DynamicObject> {
    of_kind(all, kind)
        .into_iter()
        .find(|o| o.metadata.name.as_deref() == Some(name))
        .unwrap()
}

fn names(pods: &[Arc<DynamicObject>]) -> Vec<&str> {
    pods.iter()
        .map(|p| p.metadata.name.as_deref().unwrap())
        .collect()
}

/// Pods of the fixture's `kind` `name`, with the ReplicaSets or Jobs
/// listed (`listed`) or not.
fn pods(kind: &str, name: &str, listed: bool) -> Vec<String> {
    let all = objects();
    let object = find(&all, kind, name);
    let group = object
        .types
        .as_ref()
        .unwrap()
        .api_version
        .rsplit_once('/')
        .map_or("", |(g, _)| g)
        .to_owned();
    let workload = Workload::from_api(&group, kind).unwrap();
    let intermediates = workload
        .intermediate()
        .map(|(_, kind)| of_kind(&all, kind))
        .unwrap_or_default();
    let found = pods_of(
        workload,
        &object,
        listed.then_some(intermediates.as_slice()),
        &of_kind(&all, "Pod"),
    );
    names(&found).into_iter().map(str::to_owned).collect()
}

#[test]
fn workload_kinds() {
    assert_eq!(
        Workload::from_api("apps", "Deployment"),
        Some(Workload::Deployment)
    );
    assert_eq!(
        Workload::from_api("batch", "CronJob"),
        Some(Workload::CronJob)
    );
    assert_eq!(Workload::from_api("", "Pod"), None);
    assert_eq!(Workload::from_api("", "Service"), None);
    assert_eq!(
        Workload::Deployment.intermediate(),
        Some(("apps", "ReplicaSet"))
    );
    assert_eq!(Workload::CronJob.intermediate(), Some(("batch", "Job")));
    assert_eq!(Workload::StatefulSet.intermediate(), None);
}

#[test]
fn deployment_pods_through_its_replicasets() {
    // Newest first; not api's pod (same labels, another owner) nor the pod
    // of another namespace.
    assert_eq!(
        pods("Deployment", "web", true),
        ["web-2-ccc", "web-2-bbb", "web-1-aaa"]
    );
    assert_eq!(pods("Deployment", "api", true), ["api-1-xxx"]);
}

#[test]
fn deployment_without_replicasets_falls_back_to_its_selector() {
    // The selector is all there is: overlapping labels match too.
    assert_eq!(
        pods("Deployment", "web", false),
        ["web-2-ccc", "web-2-bbb", "web-1-aaa", "api-1-xxx"]
    );
}

#[test]
fn replicaset_daemonset_and_job_own_their_pods() {
    assert_eq!(
        pods("ReplicaSet", "web-2", true),
        ["web-2-ccc", "web-2-bbb"]
    );
    // Same age: by name.
    assert_eq!(pods("DaemonSet", "agent", true), ["agent-q1", "agent-q2"]);
    assert_eq!(pods("Job", "report", true), ["report-abc"]);
}

#[test]
fn statefulset_pods_by_ordinal() {
    assert_eq!(pods("StatefulSet", "db", true), ["db-0", "db-2", "db-10"]);
}

#[test]
fn cronjob_pods_through_its_jobs() {
    assert_eq!(
        pods("CronJob", "nightly", true),
        ["nightly-29000001-k2", "nightly-29000000-k1"]
    );
    // Without the Jobs: by the names the controller gives them.
    assert_eq!(
        pods("CronJob", "nightly", false),
        ["nightly-29000001-k2", "nightly-29000000-k1"]
    );
}

#[test]
fn without_a_uid_the_selector_picks() {
    let all = objects();
    let mut object = (*find(&all, "StatefulSet", "db")).clone();
    object.metadata.uid = None;
    let found = pods_of(Workload::StatefulSet, &object, None, &of_kind(&all, "Pod"));
    assert_eq!(names(&found), ["db-0", "db-2", "db-10"]);
    // An empty selector selects nothing rather than the whole namespace.
    object.data["spec"]["selector"] = serde_json::json!({});
    assert!(pods_of(Workload::StatefulSet, &object, None, &of_kind(&all, "Pod")).is_empty());
}

#[test]
fn scheduled_job_names() {
    assert!(scheduled_job_of("nightly", "nightly-29000000"));
    assert!(!scheduled_job_of("nightly", "nightly-backup"));
    assert!(!scheduled_job_of("nightly", "nightly-"));
    assert!(!scheduled_job_of("nightly", "nightly29000000"));
}

#[test]
fn rows_read_like_the_pod_list() {
    let all = objects();
    let now: Timestamp = "2026-10-25T11:00:00Z".parse().unwrap();
    let row = |name: &str| PodRow::of(&find(&all, "Pod", name), now);

    let running = row("web-1-aaa");
    assert_eq!(running.status, "Running");
    assert_eq!(running.health, HealthStatus::Healthy);
    assert_eq!(running.ready_text(), "1/1");
    assert_eq!(running.restarts, 0);
    assert_eq!(running.node_text(), "node-a");
    assert_eq!(
        running.created,
        Some(
            "2026-10-25T10:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .as_second()
        )
    );
    assert_eq!(running.namespace.as_deref(), Some("shop"));

    let crashing = row("web-2-bbb");
    assert_eq!(crashing.status, "CrashLoopBackOff");
    assert_ne!(crashing.health, HealthStatus::Healthy);
    assert_eq!(crashing.ready_text(), "0/1");
    assert_eq!(crashing.restarts, 3);

    let pending = row("web-2-ccc");
    assert_eq!(pending.status, "Pending");
    assert_eq!(pending.node, None);
    assert_eq!(pending.node_text(), "Not scheduled");

    assert_eq!(row("report-abc").status, "Completed");
}

#[test]
fn keys_change_with_the_pods_shown() {
    let all = objects();
    let pods = vec![find(&all, "Pod", "web-1-aaa")];
    let key = pods_key(&pods);
    assert_eq!(key, [("p-web-1-aaa".to_owned(), "10".to_owned())]);
    let mut updated = (*pods[0]).clone();
    updated.metadata.resource_version = Some("13".into());
    assert_ne!(pods_key(&[Arc::new(updated)]), key);
    assert_eq!(pod_rows(&pods, Timestamp::now()).len(), 1);
}
