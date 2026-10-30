//! `wait_for_rollout` (#67): waits until a Deployment, StatefulSet or
//! DaemonSet has rolled out, like `kubectl rollout status --timeout`, so
//! an agent does not poll list_resources after scale, rollout_restart or
//! Helm and read intermediate states. On a timeout or a failure it tells
//! which pods are not ready and why.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Timestamp;
use kubach_core::discovery;
use kubach_core::plugin::{Health, HealthStatus};
use kubach_core::workload_pods::{PodRow, Workload, pods_of};
use kube::ResourceExt as _;
use kube::api::{DynamicObject, ListParams};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::policy::{Denial, DenialCode, Request};
use crate::tools::{
    Ctx, ToolError, ToolOutput, authorize, fetch, finish, kind_of, kind_ref, parse,
};

/// Waited for when the agent names no timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest wait: a call holds the agent that long.
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the workload is read again.
const POLL: Duration = Duration::from_secs(2);
/// Pods described when the rollout is not done.
const MAX_PODS: usize = 10;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitArgs {
    kind: String,
    api_version: Option<String>,
    name: String,
    namespace: Option<String>,
    timeout_seconds: Option<u64>,
}

/// Where a rollout stands, from the workload's health.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Done,
    /// Will not finish by waiting: failed (progress deadline) or paused.
    Stuck(String),
    Waiting(String),
}

pub(crate) fn step(obj: &DynamicObject, health: &Health) -> Step {
    // A paused Deployment: nothing changes until it is resumed.
    if obj.data.pointer("/spec/paused") == Some(&Value::Bool(true)) {
        return Step::Stuck("the rollout is paused (spec.paused); it goes on once resumed".into());
    }
    let message = health.message.clone();
    match health.status {
        HealthStatus::Healthy => Step::Done,
        HealthStatus::Failed => Step::Stuck(message),
        _ => Step::Waiting(message),
    }
}

pub(crate) async fn wait_for_rollout(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: WaitArgs = parse(args)?;
    let timeout = match a.timeout_seconds {
        None => DEFAULT_TIMEOUT,
        Some(0) => {
            return Err(
                Denial::new(DenialCode::InvalidInput, "timeoutSeconds must be above 0").into(),
            );
        }
        Some(seconds) => Duration::from_secs(seconds).min(MAX_TIMEOUT),
    };
    let request = Request::Get {
        kind: kind_ref(a.kind, a.api_version),
        name: a.name,
        namespace: a.namespace,
    };
    let allowed = authorize(ctx, &request).await?;
    let kind = kind_of(&allowed);
    let workload = Workload::from_api(&kind.api.group, &kind.api.kind)
        .filter(|w| {
            matches!(
                w,
                Workload::Deployment | Workload::StatefulSet | Workload::DaemonSet
            )
        })
        .ok_or_else(|| {
            Denial::new(
                DenialCode::InvalidInput,
                format!(
                    "{} has no rollout; wait_for_rollout takes a Deployment, StatefulSet or \
                     DaemonSet",
                    kind.api.kind
                ),
            )
        })?;
    let client = ctx.session.client_in(&allowed.namespace)?;
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    let started = tokio::time::Instant::now();
    let deadline = started + timeout;
    let mut last = String::new();
    let (obj, outcome) = loop {
        let obj = fetch(&client, &allowed).await?;
        let step = step(&obj, &plugin.health(&obj));
        if let Step::Waiting(message) = &step {
            if let Some(progress) = &ctx.progress
                && *message != last
            {
                progress(message.clone());
            }
            last.clone_from(message);
            if tokio::time::Instant::now() + POLL > deadline {
                break (obj, step);
            }
            tokio::time::sleep(POLL).await;
            continue;
        }
        break (obj, step);
    };
    let waited = started.elapsed().as_secs();
    let name = obj.name_any();
    let mut out = match &outcome {
        Step::Done => format!(
            "{} {}/{name} rolled out ({waited}s).\n",
            kind.api.kind, allowed.namespace
        ),
        Step::Stuck(why) => format!(
            "{} {}/{name} will not finish by waiting: {why}\n",
            kind.api.kind, allowed.namespace
        ),
        Step::Waiting(why) => format!(
            "{} {}/{name} has not rolled out after {waited}s: {why}. Call again to wait longer, \
             or look at the pods below and list_events.\n",
            kind.api.kind, allowed.namespace
        ),
    };
    if outcome != Step::Done {
        match unready_pods(&client, ctx, workload, &obj, &allowed.namespace).await {
            Ok(text) => out.push_str(&text),
            Err(error) => {
                let _ = writeln!(out, "(the pods could not be read: {})", error.message());
            }
        }
    }
    if outcome == Step::Done {
        Ok(ToolOutput::read(finish(&out)))
    } else {
        Err(ToolError::Failed(finish(&out)))
    }
}

/// The workload's pods that are not ready, with why.
async fn unready_pods(
    client: &kube::Client,
    ctx: &Ctx<'_>,
    workload: Workload,
    obj: &DynamicObject,
    namespace: &str,
) -> Result<String, ToolError> {
    let catalog = ctx.session.catalog();
    let list = |group: &str, kind: &str| {
        catalog
            .find(group, kind)
            .map(|kind| discovery::api(client, kind, Some(namespace)))
    };
    let selector = match_labels(obj);
    let params = ListParams::default().labels(&selector);
    let Some(pods_api) = list("", "Pod") else {
        return Ok(String::new());
    };
    let pods: Vec<Arc<DynamicObject>> = pods_api
        .list(&params)
        .await?
        .items
        .into_iter()
        .map(Arc::new)
        .collect();
    let intermediates: Option<Vec<Arc<DynamicObject>>> = match workload.intermediate() {
        Some((group, kind)) => match list(group, kind) {
            Some(api) => api
                .list(&params)
                .await
                .ok()
                .map(|list| list.items.into_iter().map(Arc::new).collect()),
            None => None,
        },
        None => None,
    };
    let mine = pods_of(workload, obj, intermediates.as_deref(), &pods);
    let now = Timestamp::now();
    let unready: Vec<&Arc<DynamicObject>> = mine
        .iter()
        .filter(|pod| {
            let row = PodRow::of(pod, now);
            row.total == 0 || row.ready < row.total || row.health != HealthStatus::Healthy
        })
        .collect();
    let mut out = format!(
        "\nPods: {} of {} not ready{}\n",
        unready.len(),
        mine.len(),
        if unready.is_empty() { "" } else { ":" }
    );
    for pod in unready.iter().take(MAX_PODS) {
        let _ = writeln!(out, "  {}", pod_line(pod, now));
    }
    if unready.len() > MAX_PODS {
        let _ = writeln!(out, "  (and {} more)", unready.len() - MAX_PODS);
    }
    Ok(out)
}

/// `spec.selector.matchLabels` as a label selector; empty when none, and
/// [`pods_of`] then picks the workload's pods by their owners.
fn match_labels(obj: &DynamicObject) -> String {
    obj.data
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| format!("{key}={value}")))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// One pod: status, ready, restarts, node, and why it is not ready: a
/// container waiting or last terminated, or the scheduler's message.
pub(crate) fn pod_line(object: &DynamicObject, now: Timestamp) -> String {
    let row = PodRow::of(object, now);
    let mut line = format!(
        "{} {} ready {} restarts {}",
        row.name,
        row.status,
        row.ready_text(),
        row.restarts
    );
    if let Some(node) = &row.node {
        let _ = write!(line, " on {node}");
    }
    let pod: Option<Pod> = serde_json::to_value(object)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok());
    let Some(status) = pod.and_then(|pod| pod.status) else {
        return line;
    };
    let mut why = Vec::new();
    for condition in status.conditions.iter().flatten() {
        if condition.type_ == "PodScheduled"
            && condition.status == "False"
            && let Some(message) = &condition.message
        {
            why.push(format!("not scheduled: {message}"));
        }
    }
    let containers = status
        .init_container_statuses
        .iter()
        .flatten()
        .chain(status.container_statuses.iter().flatten());
    for container in containers {
        if container.ready {
            continue;
        }
        let state = container.state.as_ref();
        if let Some(waiting) = state.and_then(|s| s.waiting.as_ref()) {
            let reason = waiting.reason.as_deref().unwrap_or("Waiting");
            let message = waiting
                .message
                .as_deref()
                .map(|m| format!(": {}", first_line(m)))
                .unwrap_or_default();
            why.push(format!("{} {reason}{message}", container.name));
        } else if state.and_then(|s| s.running.as_ref()).is_some() {
            why.push(format!(
                "{} running, not ready (readiness probe)",
                container.name
            ));
        }
        if let Some(last) = container
            .last_state
            .as_ref()
            .and_then(|s| s.terminated.as_ref())
        {
            why.push(format!(
                "{} last exit: {} (code {})",
                container.name,
                last.reason.as_deref().unwrap_or("Terminated"),
                last.exit_code
            ));
        }
    }
    if !why.is_empty() {
        let _ = write!(line, " — {}", why.join("; "));
    }
    line
}

fn first_line(text: &str) -> &str {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(200) {
        Some((at, _)) => &line[..at],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::jiff::Timestamp;
    use kubach_core::plugin::PluginRegistry;
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{Step, pod_line, step};

    fn health(object: serde_json::Value) -> Step {
        let obj: DynamicObject = serde_json::from_value(object).unwrap();
        let registry = PluginRegistry::with_builtins();
        let group = "apps";
        let kind = obj.types.as_ref().unwrap().kind.clone();
        step(&obj, &registry.plugin_for(group, &kind).health(&obj))
    }

    fn deployment(spec: serde_json::Value, status: serde_json::Value) -> serde_json::Value {
        json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "namespace": "ns", "generation": 3},
            "spec": spec, "status": status,
        })
    }

    #[test]
    fn a_rollout_is_done_waiting_or_stuck() {
        let done = deployment(
            json!({"replicas": 2}),
            json!({"observedGeneration": 3, "replicas": 2, "updatedReplicas": 2,
                   "readyReplicas": 2, "availableReplicas": 2}),
        );
        assert_eq!(health(done), Step::Done);
        let rolling = deployment(
            json!({"replicas": 2}),
            json!({"observedGeneration": 3, "replicas": 3, "updatedReplicas": 1,
                   "readyReplicas": 2, "availableReplicas": 2}),
        );
        assert!(matches!(health(rolling), Step::Waiting(_)));
        let not_seen = deployment(
            json!({"replicas": 2}),
            json!({"observedGeneration": 2, "replicas": 2, "updatedReplicas": 2,
                   "readyReplicas": 2, "availableReplicas": 2}),
        );
        assert!(matches!(health(not_seen), Step::Waiting(_)));
        let failed = deployment(
            json!({"replicas": 2}),
            json!({"observedGeneration": 3, "replicas": 2, "updatedReplicas": 1,
                   "conditions": [{"type": "Progressing", "status": "False",
                     "reason": "ProgressDeadlineExceeded",
                     "message": "ReplicaSet web-1 has timed out progressing."}]}),
        );
        assert_eq!(
            health(failed),
            Step::Stuck("ReplicaSet web-1 has timed out progressing.".into())
        );
        let paused = deployment(
            json!({"replicas": 2, "paused": true}),
            json!({"observedGeneration": 3, "replicas": 2, "updatedReplicas": 1}),
        );
        assert!(matches!(health(paused), Step::Stuck(_)), "a paused rollout");
    }

    #[test]
    fn an_unready_pod_says_why() {
        let pending: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "web-1", "namespace": "ns"},
            "spec": {"containers": [{"name": "app", "image": "x"}]},
            "status": {"phase": "Pending", "conditions": [{"type": "PodScheduled",
                "status": "False", "reason": "Unschedulable",
                "message": "0/1 nodes are available: 1 Insufficient memory."}]},
        }))
        .unwrap();
        let line = pod_line(&pending, Timestamp::now());
        assert!(line.starts_with("web-1 Pending ready 0/1"), "{line}");
        assert!(
            line.contains("not scheduled: 0/1 nodes are available: 1 Insufficient memory."),
            "{line}"
        );
        let crashing: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "web-2", "namespace": "ns"},
            "spec": {"nodeName": "n1", "containers": [{"name": "app", "image": "x"}]},
            "status": {"phase": "Running", "containerStatuses": [{
                "name": "app", "image": "x", "imageID": "", "ready": false,
                "restartCount": 4,
                "state": {"waiting": {"reason": "CrashLoopBackOff",
                    "message": "back-off 1m20s restarting failed container"}},
                "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137}},
            }]},
        }))
        .unwrap();
        let line = pod_line(&crashing, Timestamp::now());
        assert!(line.contains("restarts 4 on n1"), "{line}");
        assert!(
            line.contains("app CrashLoopBackOff: back-off 1m20s restarting failed container"),
            "{line}"
        );
        assert!(
            line.contains("app last exit: OOMKilled (code 137)"),
            "{line}"
        );
    }
}
