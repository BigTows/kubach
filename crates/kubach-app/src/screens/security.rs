//! Security findings of pods: rows of the "Security checks" list and the
//! badge in pod details.

use std::sync::Arc;

use gpui_kit::px;
use kubach_core::ResourceKind;
use kubach_core::security::{self, Counts, Finding, Severity};
use kubach_i18n::t;
use kubach_ui::{ResourceCell, ResourceColumn, ResourceRow, StatusBadge, Tone};
use kube::api::DynamicObject;

pub fn tone(severity: Severity) -> Tone {
    match severity {
        Severity::High => Tone::Danger,
        Severity::Medium => Tone::Warning,
        Severity::Low => Tone::Neutral,
    }
}

/// The name of a severity level.
pub fn severity_title(severity: Severity) -> String {
    match severity {
        Severity::High => t!("security-severity-high"),
        Severity::Medium => t!("security-severity-medium"),
        Severity::Low => t!("security-severity-low"),
    }
}

/// `1 high · 4 medium`, levels without findings left out.
pub fn summary(counts: Counts) -> String {
    let parts: Vec<String> = [
        (counts.high, Severity::High),
        (counts.medium, Severity::Medium),
        (counts.low, Severity::Low),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, severity)| match severity {
        Severity::High => t!("security-count-high", count = count),
        Severity::Medium => t!("security-count-medium", count = count),
        Severity::Low => t!("security-count-low", count = count),
    })
    .collect();
    if parts.is_empty() {
        t!("security-no-issues")
    } else {
        parts.join(" · ")
    }
}

fn is_pod(kind: &ResourceKind) -> bool {
    kind.api.group.is_empty() && kind.api.kind == "Pod"
}

/// Badge with the pod's findings by severity; `None` for other kinds.
pub fn pod_badge(kind: &ResourceKind, object: &DynamicObject) -> Option<StatusBadge> {
    if !is_pod(kind) {
        return None;
    }
    let counts = Counts::of(&security::check_pod_object(object));
    let tone = counts.worst().map(tone).unwrap_or(Tone::Success);
    Some(StatusBadge::new(summary(counts), tone))
}

/// A finding with the pod it was found in.
#[derive(Clone, Debug, PartialEq)]
pub struct PodFinding {
    pub namespace: Option<String>,
    pub pod: String,
    pub finding: Finding,
}

/// Findings of `pods` at `min` severity or above, most severe first, then
/// in pod order.
pub fn findings(pods: &[Arc<DynamicObject>], min: Severity) -> Vec<PodFinding> {
    let mut all: Vec<PodFinding> = pods
        .iter()
        .flat_map(|pod| {
            let namespace = pod.metadata.namespace.clone();
            let name = pod.metadata.name.clone().unwrap_or_default();
            security::check_pod_object(pod)
                .into_iter()
                .filter(move |finding| finding.severity >= min)
                .map(move |finding| PodFinding {
                    namespace: namespace.clone(),
                    pod: name.clone(),
                    finding,
                })
        })
        .collect();
    all.sort_by_key(|item| std::cmp::Reverse(item.finding.severity));
    all
}

pub fn columns(show_namespace: bool) -> Vec<ResourceColumn> {
    let mut columns = vec![
        ResourceColumn::new("severity", t!("security-column-severity"))
            .sortable()
            .width(px(100.)),
        ResourceColumn::new("pod", t!("security-column-pod"))
            .sortable()
            .width(px(280.)),
    ];
    if show_namespace {
        columns.push(
            ResourceColumn::new("namespace", t!("column-namespace"))
                .sortable()
                .width(px(160.)),
        );
    }
    columns.extend([
        ResourceColumn::new("container", t!("security-column-container"))
            .sortable()
            .width(px(140.)),
        ResourceColumn::new("finding", t!("security-column-finding"))
            .sortable()
            .width(px(260.)),
    ]);
    columns
}

/// One row per finding, keyed by its index in `findings`.
pub fn rows(findings: &[PodFinding], show_namespace: bool) -> Vec<ResourceRow> {
    findings
        .iter()
        .enumerate()
        .map(|(ix, item)| {
            let severity = item.finding.severity;
            let mut cells = vec![
                ResourceCell::status(severity_title(severity), tone(severity))
                    .sort_by(severity as i64),
                ResourceCell::text(item.pod.clone()),
            ];
            if show_namespace {
                cells.push(ResourceCell::text(
                    item.namespace.clone().unwrap_or_default(),
                ));
            }
            cells.push(match &item.finding.container {
                Some(container) => ResourceCell::text(container.clone()),
                None => ResourceCell::text(t!("security-pod-level")).muted(),
            });
            cells.push(ResourceCell::text(item.finding.title.to_string()));
            ResourceRow::new(ix.to_string(), cells)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::security::{Counts, Severity};
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{columns, findings, rows, summary};

    fn pod(namespace: &str, name: &str, container: serde_json::Value) -> Arc<DynamicObject> {
        Arc::new(
            serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": name, "namespace": namespace },
                "spec": { "containers": [container] },
            }))
            .unwrap(),
        )
    }

    #[test]
    fn summary_skips_empty_levels() {
        assert_eq!(summary(Counts::default()), "No issues");
        let counts = Counts {
            high: 1,
            medium: 0,
            low: 2,
        };
        assert_eq!(summary(counts), "1 high · 2 low");
    }

    #[test]
    fn findings_are_filtered_and_sorted_by_severity() {
        let pods = [
            pod(
                "web",
                "api",
                json!({ "name": "app", "image": "nginx:latest" }),
            ),
            pod(
                "web",
                "debug",
                json!({ "name": "sh", "image": "busybox:1.36",
                        "securityContext": { "privileged": true } }),
            ),
        ];
        let medium = findings(&pods, Severity::Medium);
        assert!(
            medium
                .iter()
                .all(|f| f.finding.severity >= Severity::Medium)
        );
        assert_eq!(medium[0].pod, "debug");
        assert_eq!(medium[0].finding.title.english(), "Privileged container");
        assert!(
            medium
                .iter()
                .any(|f| f.pod == "api" && f.finding.title.english() == "Image uses :latest")
        );
        let all = findings(&pods, Severity::Low);
        assert!(all.len() > medium.len());
        assert_eq!(all[0].namespace.as_deref(), Some("web"));

        let table = rows(&medium, true);
        assert_eq!(table.len(), medium.len());
        assert_eq!(table[0].cells.len(), columns(true).len());
        assert_eq!(table[0].cells[0].label().as_ref(), "High");
        assert_eq!(rows(&medium, false)[0].cells.len(), columns(false).len());
    }
}
