//! The check before a forward is put to the user (#60): the target must
//! have a ready pod and the port must reach it. A forward that cannot
//! work is refused with the reason (phase, unready containers and why,
//! the ports there are), and the user is not asked.
//!
//! A pod that is Running but not Ready is refused too: a Service would
//! not send it traffic either, and a forward picked from a Service or
//! workload goes to a ready pod only.

use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use kubach_core::plugin::builtin::summarize_pod;
use kubach_core::streams::{ForwardSource, is_pod_ready, pick_ready_pod};
use kube::Client;
use kube::api::{Api, ListParams};

use crate::tools::ToolError;

/// Pods described in the error about a Service or workload, at most.
const DESCRIBED_PODS: usize = 3;

/// Said after every refusal, so the agent knows nobody was asked.
const NOT_ASKED: &str = "The forward cannot work now, so the user was not asked; \
     try again when the pod is ready.";

/// Reads the pod `source` would forward to and checks it is ready and
/// reaches `requested` (a pod port, or a Service port).
pub(super) async fn check_target(
    client: &Client,
    source: &ForwardSource,
    requested: u16,
) -> Result<(), ToolError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &source.namespace);
    let pod = match &source.selector {
        None => {
            let pod = api.get(&source.name).await?;
            if let Some(problem) = pod_problem(&pod) {
                return Err(ToolError::Failed(format!("{problem}. {NOT_ASKED}")));
            }
            pod
        }
        Some(selector) => {
            let pods = api
                .list(&ListParams::default().labels(selector))
                .await?
                .items;
            match pick_ready_pod(&pods) {
                Some(pod) => pod.clone(),
                None => {
                    return Err(ToolError::Failed(format!(
                        "{}. {NOT_ASKED}",
                        no_ready_pod(source, selector, pods)
                    )));
                }
            }
        }
    };
    source.pod_port(&pod, requested).map_err(|error| {
        ToolError::Failed(format!(
            "{error}; {}. The user was not asked.",
            ports_text(source)
        ))
    })?;
    Ok(())
}

/// The TCP ports `source` declares, for an error naming a wrong one.
pub(super) fn ports_text(source: &ForwardSource) -> String {
    let ports: Vec<String> = source
        .ports
        .iter()
        .filter(|port| port.is_tcp())
        .map(|port| match &port.name {
            Some(name) => format!("{} ({name})", port.port),
            None => port.port.to_string(),
        })
        .collect();
    if ports.is_empty() {
        format!("{} declares no TCP ports", source.label())
    } else {
        format!("{} has TCP ports {}", source.label(), ports.join(", "))
    }
}

/// Why no pod of a Service or workload can take the forward.
fn no_ready_pod(source: &ForwardSource, selector: &str, mut pods: Vec<Pod>) -> String {
    let label = source.label();
    if pods.is_empty() {
        let endpoints = if source.is_service() {
            ", so the Service has no endpoints"
        } else {
            ""
        };
        return format!("no pods match {label} (selector {selector}){endpoints}");
    }
    pods.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
    let endpoints = if source.is_service() {
        " (the Service has no ready endpoints)"
    } else {
        ""
    };
    let mut text = format!(
        "none of the {} pods of {label} is ready{endpoints}: ",
        pods.len()
    );
    let described: Vec<String> = pods
        .iter()
        .take(DESCRIBED_PODS)
        .filter_map(pod_problem)
        .collect();
    text.push_str(&described.join("; "));
    if pods.len() > DESCRIBED_PODS {
        text.push_str(&format!("; and {} more", pods.len() - DESCRIBED_PODS));
    }
    text
}

/// Why `pod` cannot take a forward now; `None` when it is ready.
pub(super) fn pod_problem(pod: &Pod) -> Option<String> {
    if is_pod_ready(pod) {
        return None;
    }
    let name = pod.metadata.name.as_deref().unwrap_or_default();
    if pod.metadata.deletion_timestamp.is_some() {
        return Some(format!("pod {name} is terminating"));
    }
    let status = pod.status.as_ref();
    let phase = status
        .and_then(|s| s.phase.as_deref())
        .filter(|p| !p.is_empty())
        .unwrap_or("Unknown");
    let summary = summarize_pod(pod);
    let mut text = format!("pod {name} is not ready: phase {phase}");
    if !summary.status.is_empty() && summary.status != phase {
        text.push_str(&format!(", status {}", summary.status));
    }
    text.push_str(&format!(
        ", {}/{} containers ready",
        summary.ready, summary.total
    ));
    let init = status
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|cs| !cs.ready && !completed(cs))
        .map(|cs| container_text("init container", cs));
    let main = status
        .and_then(|s| s.container_statuses.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|cs| !cs.ready)
        .map(|cs| container_text("container", cs));
    // Ready and ContainersReady only repeat the containers.
    let conditions = status
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|c| c.status != "True" && c.type_ != "Ready" && c.type_ != "ContainersReady")
        .filter_map(|c| {
            let why = [c.reason.as_deref(), c.message.as_deref()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(": ");
            (!why.is_empty()).then(|| format!("{} is {} ({why})", c.type_, c.status))
        });
    let details: Vec<String> = init.chain(main).chain(conditions).collect();
    if !details.is_empty() {
        text.push_str(" (");
        text.push_str(&details.join("; "));
        text.push(')');
    }
    Some(text)
}

/// An init container that finished successfully.
fn completed(cs: &ContainerStatus) -> bool {
    cs.state
        .as_ref()
        .and_then(|s| s.terminated.as_ref())
        .is_some_and(|t| t.exit_code == 0)
}

/// `container app: waiting, CrashLoopBackOff: back-off 5m0s …`.
fn container_text(what: &str, cs: &ContainerStatus) -> String {
    let reason = |reason: Option<&str>, message: Option<&str>| {
        [reason, message]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(": ")
    };
    let state = cs.state.as_ref();
    let text = if let Some(waiting) = state.and_then(|s| s.waiting.as_ref()) {
        let why = reason(waiting.reason.as_deref(), waiting.message.as_deref());
        if why.is_empty() {
            "waiting".to_owned()
        } else {
            format!("waiting, {why}")
        }
    } else if let Some(terminated) = state.and_then(|s| s.terminated.as_ref()) {
        let why = reason(terminated.reason.as_deref(), terminated.message.as_deref());
        let code = format!("exit code {}", terminated.exit_code);
        if why.is_empty() {
            format!("terminated, {code}")
        } else {
            format!("terminated, {why}, {code}")
        }
    } else if state.and_then(|s| s.running.as_ref()).is_some() {
        "running, but not ready (its readiness probe does not pass yet)".to_owned()
    } else {
        "not started".to_owned()
    };
    let restarts = if cs.restart_count > 0 {
        format!(", {} restarts", cs.restart_count)
    } else {
        String::new()
    };
    format!("{what} {}: {text}{restarts}", cs.name)
}
