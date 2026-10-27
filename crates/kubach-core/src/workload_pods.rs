//! The pods of a workload (#393), for the "Pods" block of its details.
//!
//! Pods are found by owner references, as the controllers create them:
//! a Deployment owns ReplicaSets that own the pods, a CronJob owns Jobs
//! that own the pods, the other workloads own their pods directly. When
//! the owner chain cannot be followed (the object has no UID, or the
//! ReplicaSets / Jobs are not listed), the workload's selector picks them.
//!
//! The view lists pods and ReplicaSets / Jobs from the cache; this module
//! only picks, sorts and describes them.

use std::collections::HashSet;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;

use crate::drain::selector_matches;
use crate::plugin::HealthStatus;
use crate::plugin::builtin::{pod_health_with_events, summarize_pod};

/// A kind whose details list its pods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    Deployment,
    StatefulSet,
    DaemonSet,
    ReplicaSet,
    Job,
    CronJob,
}

impl Workload {
    /// The workload kind of `group`/`kind`, if it is one.
    pub fn from_api(group: &str, kind: &str) -> Option<Self> {
        Some(match (group, kind) {
            ("apps", "Deployment") => Self::Deployment,
            ("apps", "StatefulSet") => Self::StatefulSet,
            ("apps", "DaemonSet") => Self::DaemonSet,
            ("apps", "ReplicaSet") => Self::ReplicaSet,
            ("batch", "Job") => Self::Job,
            ("batch", "CronJob") => Self::CronJob,
            _ => return None,
        })
    }

    /// The kind between the workload and its pods, as `(group, kind)`:
    /// ReplicaSets of a Deployment, Jobs of a CronJob.
    pub fn intermediate(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Deployment => Some(("apps", "ReplicaSet")),
            Self::CronJob => Some(("batch", "Job")),
            _ => None,
        }
    }
}

/// Whether `object` has an owner reference to one of `uids`.
fn owned_by(object: &DynamicObject, uids: &HashSet<&str>) -> bool {
    object
        .metadata
        .owner_references
        .iter()
        .flatten()
        .any(|owner| uids.contains(owner.uid.as_str()))
}

/// The pod selector of a workload: `spec.selector` (`None` for a CronJob,
/// whose Jobs get their selectors from the API server).
fn selector(object: &DynamicObject) -> Option<LabelSelector> {
    let value = object.data.pointer("/spec/selector")?;
    serde_json::from_value(value.clone()).ok()
}

/// Whether `name` is `<cronjob>-<scheduled time>`, the name the CronJob
/// controller gives its Jobs.
fn scheduled_job_of(cronjob: &str, name: &str) -> bool {
    name.strip_prefix(cronjob)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}

/// The pods of `object` (a `workload`) among `pods`.
///
/// `intermediates` are the ReplicaSets (Deployment) or Jobs (CronJob) of
/// the namespace, `None` when they are not listed (forbidden, not loaded);
/// other workloads ignore them. Pods and intermediates of other
/// namespaces never match. Sorted with [`sort_pods`].
pub fn pods_of(
    workload: Workload,
    object: &DynamicObject,
    intermediates: Option<&[Arc<DynamicObject>]>,
    pods: &[Arc<DynamicObject>],
) -> Vec<Arc<DynamicObject>> {
    let namespace = object.metadata.namespace.as_deref();
    let same_namespace = |o: &&Arc<DynamicObject>| o.metadata.namespace.as_deref() == namespace;
    let uid = object.metadata.uid.as_deref().filter(|uid| !uid.is_empty());
    let owners: Option<HashSet<&str>> = match (uid, workload.intermediate()) {
        (Some(uid), None) => Some(HashSet::from([uid])),
        (Some(uid), Some(_)) => intermediates.map(|list| {
            let uid = HashSet::from([uid]);
            list.iter()
                .filter(same_namespace)
                .filter(|o| owned_by(o, &uid))
                .filter_map(|o| o.metadata.uid.as_deref())
                .collect()
        }),
        (None, _) => None,
    };
    let mut found: Vec<Arc<DynamicObject>> = match owners {
        Some(owners) => pods
            .iter()
            .filter(same_namespace)
            .filter(|pod| owned_by(pod, &owners))
            .cloned()
            .collect(),
        None => fallback(workload, object, pods)
            .into_iter()
            .filter(|pod| pod.metadata.namespace.as_deref() == namespace)
            .collect(),
    };
    sort_pods(workload, &mut found);
    found
}

/// Pods picked without the owner chain: by the selector, or for a
/// CronJob by the names of its Jobs.
fn fallback(
    workload: Workload,
    object: &DynamicObject,
    pods: &[Arc<DynamicObject>],
) -> Vec<Arc<DynamicObject>> {
    if workload == Workload::CronJob {
        let name = object.metadata.name.as_deref().unwrap_or_default();
        return pods
            .iter()
            .filter(|pod| {
                pod.metadata.owner_references.iter().flatten().any(|owner| {
                    owner.kind == "Job" && !name.is_empty() && scheduled_job_of(name, &owner.name)
                })
            })
            .cloned()
            .collect();
    }
    // A selector without terms would select every pod of the namespace.
    let Some(selector) = selector(object).filter(|s| {
        s.match_labels.as_ref().is_some_and(|m| !m.is_empty())
            || s.match_expressions.as_ref().is_some_and(|e| !e.is_empty())
    }) else {
        return Vec::new();
    };
    let empty = Default::default();
    pods.iter()
        .filter(|pod| selector_matches(&selector, pod.metadata.labels.as_ref().unwrap_or(&empty)))
        .cloned()
        .collect()
}

/// Trailing `-<n>` of a name.
fn ordinal(name: &str) -> Option<u64> {
    name.rsplit_once('-')?.1.parse().ok()
}

/// StatefulSet pods by ordinal (`web-2` before `web-10`); the others
/// newest first, so a rollout or the latest run is on top; ties by name.
pub fn sort_pods<O: AsRef<DynamicObject>>(workload: Workload, pods: &mut [O]) {
    let name = |o: &O| o.as_ref().metadata.name.clone().unwrap_or_default();
    match workload {
        Workload::StatefulSet => {
            pods.sort_by_key(|o| (ordinal(&name(o)).unwrap_or(u64::MAX), name(o)))
        }
        _ => pods.sort_by_key(|o| {
            let created = o.as_ref().metadata.creation_timestamp.as_ref().map(|t| t.0);
            (std::cmp::Reverse(created), name(o))
        }),
    }
}

/// One pod of the block, as the pod list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodRow {
    pub name: String,
    pub namespace: Option<String>,
    /// Phase or reason, like the pod list's status: `Running`,
    /// `CrashLoopBackOff`, `Init:0/1`, `Completed`, `Terminating`.
    pub status: String,
    pub health: HealthStatus,
    pub ready: usize,
    pub total: usize,
    pub restarts: i32,
    /// Unix seconds.
    pub created: Option<i64>,
    /// `None` while the pod is not scheduled.
    pub node: Option<String>,
}

impl PodRow {
    /// The row of `object` (a pod) at `now`.
    pub fn of(object: &DynamicObject, now: Timestamp) -> Self {
        let name = object.metadata.name.clone().unwrap_or_default();
        let namespace = object.metadata.namespace.clone();
        let created = object
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|t| t.0.as_second());
        let pod: Option<Pod> = serde_json::to_value(object)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok());
        let Some(pod) = pod else {
            return Self {
                name,
                namespace,
                status: "Unknown".into(),
                health: HealthStatus::Unknown,
                ready: 0,
                total: 0,
                restarts: 0,
                created,
                node: None,
            };
        };
        let summary = summarize_pod(&pod);
        Self {
            name,
            namespace,
            health: pod_health_with_events(&pod, &[], now).status,
            status: summary.status,
            ready: summary.ready,
            total: summary.total,
            restarts: summary.restarts,
            created,
            node: pod
                .spec
                .as_ref()
                .and_then(|s| s.node_name.clone())
                .filter(|n| !n.is_empty()),
        }
    }

    /// `1/2`.
    pub fn ready_text(&self) -> String {
        format!("{}/{}", self.ready, self.total)
    }

    /// The node, or `Not scheduled`.
    pub fn node_text(&self) -> &str {
        self.node.as_deref().unwrap_or("Not scheduled")
    }
}

/// The rows of `pods`, in their order.
pub fn pod_rows(pods: &[Arc<DynamicObject>], now: Timestamp) -> Vec<PodRow> {
    pods.iter().map(|pod| PodRow::of(pod, now)).collect()
}

/// What the rows were built from: the pods and their versions. Equal keys
/// mean nothing shown changed (a watch event of another pod).
pub fn pods_key(pods: &[Arc<DynamicObject>]) -> Vec<(String, String)> {
    pods.iter()
        .map(|pod| {
            (
                pod.metadata.uid.clone().unwrap_or_default(),
                pod.metadata.resource_version.clone().unwrap_or_default(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
