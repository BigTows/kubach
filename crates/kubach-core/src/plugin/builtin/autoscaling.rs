//! HorizontalPodAutoscaler plugin: target, replica range, metrics (current
//! against target), conditions and scaling behavior.

use k8s_openapi::api::autoscaling::v2::{
    CrossVersionObjectReference, HPAScalingRules, HorizontalPodAutoscaler, MetricSpec,
    MetricStatus, MetricTarget, MetricValueStatus,
};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, col, creation, decode, fallback_cells, kv, name_ns_cells,
    name_ns_columns, object_ref, pair, push_kv, references, relations_with, run_delete, section,
    text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation,
    ResourcePlugin, SectionBody,
};

pub struct HorizontalPodAutoscalerPlugin;

/// HPA conditions that are something to look at when `True`: the replica
/// count hit the min or max.
pub(crate) const WARNING_CONDITIONS: [&str; 1] = ["ScalingLimited"];

/// What a metric measures: `cpu`, `cpu (app)`, `requests_per_second`,
/// `hits on Ingress/main`. Also the key that pairs a spec with its status.
fn metric_label(
    type_: &str,
    resource: Option<&str>,
    container: Option<&str>,
    metric: Option<&str>,
    object: Option<&CrossVersionObjectReference>,
) -> String {
    match (type_, object) {
        ("Resource", _) => resource.unwrap_or_default().to_owned(),
        ("ContainerResource", _) => format!(
            "{} ({})",
            resource.unwrap_or_default(),
            container.unwrap_or_default()
        ),
        ("Object", Some(o)) => format!("{} on {}/{}", metric.unwrap_or_default(), o.kind, o.name),
        _ => metric.unwrap_or_default().to_owned(),
    }
}

fn spec_label(m: &MetricSpec) -> String {
    let (resource, container, metric, object) = match m.type_.as_str() {
        "Resource" => (
            m.resource.as_ref().map(|r| r.name.as_str()),
            None,
            None,
            None,
        ),
        "ContainerResource" => {
            m.container_resource
                .as_ref()
                .map_or((None, None, None, None), |r| {
                    (
                        Some(r.name.as_str()),
                        Some(r.container.as_str()),
                        None,
                        None,
                    )
                })
        }
        "Pods" => (
            None,
            None,
            m.pods.as_ref().map(|p| p.metric.name.as_str()),
            None,
        ),
        "Object" => m.object.as_ref().map_or((None, None, None, None), |o| {
            (
                None,
                None,
                Some(o.metric.name.as_str()),
                Some(&o.described_object),
            )
        }),
        "External" => (
            None,
            None,
            m.external.as_ref().map(|e| e.metric.name.as_str()),
            None,
        ),
        _ => (None, None, None, None),
    };
    metric_label(&m.type_, resource, container, metric, object)
}

fn status_label(m: &MetricStatus) -> String {
    let (resource, container, metric, object) = match m.type_.as_str() {
        "Resource" => (
            m.resource.as_ref().map(|r| r.name.as_str()),
            None,
            None,
            None,
        ),
        "ContainerResource" => {
            m.container_resource
                .as_ref()
                .map_or((None, None, None, None), |r| {
                    (
                        Some(r.name.as_str()),
                        Some(r.container.as_str()),
                        None,
                        None,
                    )
                })
        }
        "Pods" => (
            None,
            None,
            m.pods.as_ref().map(|p| p.metric.name.as_str()),
            None,
        ),
        "Object" => m.object.as_ref().map_or((None, None, None, None), |o| {
            (
                None,
                None,
                Some(o.metric.name.as_str()),
                Some(&o.described_object),
            )
        }),
        "External" => (
            None,
            None,
            m.external.as_ref().map(|e| e.metric.name.as_str()),
            None,
        ),
        _ => (None, None, None, None),
    };
    metric_label(&m.type_, resource, container, metric, object)
}

fn spec_target(m: &MetricSpec) -> Option<&MetricTarget> {
    match m.type_.as_str() {
        "Resource" => m.resource.as_ref().map(|r| &r.target),
        "ContainerResource" => m.container_resource.as_ref().map(|r| &r.target),
        "Pods" => m.pods.as_ref().map(|p| &p.target),
        "Object" => m.object.as_ref().map(|o| &o.target),
        "External" => m.external.as_ref().map(|e| &e.target),
        _ => None,
    }
}

fn status_current(m: &MetricStatus) -> Option<&MetricValueStatus> {
    match m.type_.as_str() {
        "Resource" => m.resource.as_ref().map(|r| &r.current),
        "ContainerResource" => m.container_resource.as_ref().map(|r| &r.current),
        "Pods" => m.pods.as_ref().map(|p| &p.current),
        "Object" => m.object.as_ref().map(|o| &o.current),
        "External" => m.external.as_ref().map(|e| &e.current),
        _ => None,
    }
}

/// `80%`, `200Mi`, `10 (avg)` (an average of an object or external metric,
/// like kubectl).
fn target_text(type_: &str, t: &MetricTarget) -> String {
    if let Some(u) = t.average_utilization {
        return format!("{u}%");
    }
    if let Some(v) = &t.average_value {
        return match type_ {
            "Object" | "External" => format!("{} (avg)", v.0),
            _ => v.0.clone(),
        };
    }
    t.value.as_ref().map(|v| v.0.clone()).unwrap_or_default()
}

/// The current value in the target's terms: a percentage for a
/// utilization target, a quantity otherwise; `<unknown>` until reported.
fn current_text(target: Option<&MetricTarget>, current: Option<&MetricValueStatus>) -> String {
    let Some(c) = current else {
        return "<unknown>".into();
    };
    let utilization = target.is_some_and(|t| t.average_utilization.is_some());
    let value = if utilization {
        c.average_utilization.map(|u| format!("{u}%"))
    } else {
        None
    };
    value
        .or_else(|| c.average_value.as_ref().map(|v| v.0.clone()))
        .or_else(|| c.value.as_ref().map(|v| v.0.clone()))
        .or_else(|| c.average_utilization.map(|u| format!("{u}%")))
        .unwrap_or_else(|| "<unknown>".into())
}

/// Each metric of the spec as `(label, current/target)`.
fn metrics(hpa: &HorizontalPodAutoscaler) -> Vec<(String, String)> {
    let current: Vec<&MetricStatus> = hpa
        .status
        .as_ref()
        .and_then(|s| s.current_metrics.as_ref())
        .into_iter()
        .flatten()
        .collect();
    hpa.spec
        .metrics
        .as_ref()
        .into_iter()
        .flatten()
        .map(|m| {
            let label = spec_label(m);
            let target = spec_target(m);
            let status = current
                .iter()
                .find(|c| c.type_ == m.type_ && status_label(c) == label)
                .and_then(|c| status_current(c));
            let value = format!(
                "{}/{}",
                current_text(target, status),
                target.map(|t| target_text(&m.type_, t)).unwrap_or_default()
            );
            (label, value)
        })
        .collect()
}

/// `cpu 45%/80%, memory 120Mi/200Mi`.
fn targets_text(hpa: &HorizontalPodAutoscaler) -> String {
    let all = metrics(hpa);
    if all.is_empty() {
        return "<none>".into();
    }
    all.iter()
        .map(|(label, value)| format!("{label} {value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn target_ref(hpa: &HorizontalPodAutoscaler) -> Option<ObjectRef> {
    let r = &hpa.spec.scale_target_ref;
    if r.name.is_empty() {
        return None;
    }
    Some(object_ref(
        r.api_version.as_deref().unwrap_or("apps/v1"),
        &r.kind,
        &r.name,
        hpa.metadata.namespace.as_deref(),
    ))
}

fn min_replicas(hpa: &HorizontalPodAutoscaler) -> i32 {
    hpa.spec.min_replicas.unwrap_or(1)
}

fn max_replicas(hpa: &HorizontalPodAutoscaler) -> i32 {
    hpa.spec.max_replicas
}

/// Current and desired replicas, as text. The controller reports 0
/// desired while it cannot compute a count (no metrics): that is shown as
/// `?`, not as a scale down to zero.
fn replicas(hpa: &HorizontalPodAutoscaler) -> (String, String) {
    let status = hpa.status.as_ref();
    let current = status.and_then(|s| s.current_replicas).unwrap_or(0);
    let desired = status.map_or(0, |s| s.desired_replicas);
    let desired = if desired == 0 && current > 0 {
        "?".to_owned()
    } else {
        desired.to_string()
    };
    (current.to_string(), desired)
}

/// Health from the conditions: unable to scale or to read metrics is
/// degraded (except a target scaled to zero, which turns scaling off),
/// no conditions yet is progressing.
pub(crate) fn hpa_health(obj: &DynamicObject, hpa: &HorizontalPodAutoscaler) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        return Health::new(HealthStatus::Progressing, "Terminating");
    }
    let conditions = hpa
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .cloned()
        .unwrap_or_default();
    let find = |type_: &str| conditions.iter().find(|c| c.type_ == type_);
    let text = |c: &k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscalerCondition| {
        c.message
            .clone()
            .or_else(|| c.reason.clone())
            .unwrap_or_else(|| c.type_.clone())
    };
    if let Some(c) = find("AbleToScale").filter(|c| c.status == "False") {
        return Health::new(HealthStatus::Degraded, text(c));
    }
    if let Some(c) = find("ScalingActive").filter(|c| c.status == "False") {
        if c.reason.as_deref() == Some("ScalingDisabled") {
            return Health::phrase(HealthStatus::Healthy, msg!("state-hpa-disabled"));
        }
        return Health::new(HealthStatus::Degraded, text(c));
    }
    if conditions.is_empty() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-hpa-waiting-metrics"));
    }
    if let Some(c) = find("ScalingLimited").filter(|c| c.status == "True") {
        let reason = c.reason.clone().unwrap_or_else(|| "ScalingLimited".into());
        return Health::phrase(
            HealthStatus::Healthy,
            msg!("state-hpa-limited", reason = reason),
        );
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-hpa-scaling"))
}

/// `Pods 4 per 15s, Percent 100 per 15s · Max · window 0s · tolerance 0.1`.
fn rules_text(rules: &HPAScalingRules) -> String {
    let mut parts = Vec::new();
    let policies: Vec<String> = rules
        .policies
        .iter()
        .flatten()
        .map(|p| format!("{} {} per {}s", p.type_, p.value, p.period_seconds))
        .collect();
    if !policies.is_empty() {
        parts.push(policies.join(", "));
    }
    if let Some(select) = &rules.select_policy {
        parts.push(format!("select {select}"));
    }
    if let Some(window) = rules.stabilization_window_seconds {
        parts.push(format!("stabilization {window}s"));
    }
    if let Some(tolerance) = &rules.tolerance {
        parts.push(format!("tolerance {}", tolerance.0));
    }
    parts.join(" · ")
}

impl ResourcePlugin for HorizontalPodAutoscalerPlugin {
    fn name(&self) -> &'static str {
        "horizontalpodautoscaler"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            "autoscaling",
            "v2",
            "HorizontalPodAutoscaler",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("reference", "Reference", ColumnKind::Text),
            col("targets", "Targets", ColumnKind::Text),
            col("min", "Min", ColumnKind::Number),
            col("max", "Max", ColumnKind::Number),
            col("replicas", "Replicas", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(hpa) = decode::<HorizontalPodAutoscaler>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let (current, desired) = replicas(&hpa);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(
                target_ref(&hpa)
                    .map(|r| format!("{}/{}", r.kind, r.name))
                    .unwrap_or_default(),
            ),
            text(targets_text(&hpa)),
            Cell::Number(min_replicas(&hpa).into()),
            Cell::Number(max_replicas(&hpa).into()),
            text(format!("{current}/{desired}")),
            Cell::Status(hpa_health(obj, &hpa)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(hpa) = decode::<HorizontalPodAutoscaler>(obj) else {
            return GenericPlugin.details(obj);
        };
        let (current, desired) = replicas(&hpa);
        let mut fields = Vec::new();
        if let Some(target) = target_ref(&hpa) {
            let label = format!("{}/{}", target.kind, target.name);
            fields.push(Field::object_link("Reference", label, target));
        }
        fields.extend([
            Field::text("Min replicas", min_replicas(&hpa).to_string()),
            Field::text("Max replicas", max_replicas(&hpa).to_string()),
            Field::text("Replicas", format!("{current} current · {desired} desired")),
        ]);
        if let Some(t) = hpa.status.as_ref().and_then(|s| s.last_scale_time.as_ref()) {
            fields.push(Field::new("Last scaled", FieldValue::Time(t.0)));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        push_kv(&mut sections, "Metrics", metrics(&hpa));
        if let Some(behavior) = hpa.spec.behavior.as_ref() {
            let mut rules = Vec::new();
            if let Some(up) = &behavior.scale_up {
                rules.push(pair("Scale up", rules_text(up)));
            }
            if let Some(down) = &behavior.scale_down {
                rules.push(pair("Scale down", rules_text(down)));
            }
            if !rules.is_empty() {
                sections.push(kv("Behavior", rules));
            }
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_delete::<HorizontalPodAutoscaler>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let extra = decode::<HorizontalPodAutoscaler>(obj)
            .and_then(|hpa| target_ref(&hpa))
            .map(references)
            .into_iter()
            .collect();
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<HorizontalPodAutoscaler>(obj) {
            Some(hpa) => hpa_health(obj, &hpa),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    fn hpa(status: serde_json::Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": {
                "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
                "minReplicas": 2, "maxReplicas": 10,
                "metrics": [
                    {"type": "Resource", "resource": {"name": "cpu",
                        "target": {"type": "Utilization", "averageUtilization": 80}}},
                    {"type": "Resource", "resource": {"name": "memory",
                        "target": {"type": "AverageValue", "averageValue": "200Mi"}}},
                    {"type": "Pods", "pods": {"metric": {"name": "rps"},
                        "target": {"type": "AverageValue", "averageValue": "1k"}}},
                    {"type": "Object", "object": {
                        "describedObject": {"apiVersion": "networking.k8s.io/v1", "kind": "Ingress", "name": "main"},
                        "metric": {"name": "hits"},
                        "target": {"type": "Value", "value": "10k"}}}
                ],
                "behavior": {
                    "scaleDown": {"stabilizationWindowSeconds": 300,
                        "policies": [{"type": "Percent", "value": 10, "periodSeconds": 60}]},
                    "scaleUp": {"selectPolicy": "Max",
                        "policies": [{"type": "Pods", "value": 4, "periodSeconds": 15}]}
                }
            },
            "status": status
        }))
    }

    fn active() -> serde_json::Value {
        json!({
            "currentReplicas": 3, "desiredReplicas": 4,
            "lastScaleTime": "2026-10-01T10:00:00Z",
            "currentMetrics": [
                {"type": "Resource", "resource": {"name": "cpu",
                    "current": {"averageUtilization": 45, "averageValue": "90m"}}},
                {"type": "Object", "object": {
                    "describedObject": {"apiVersion": "networking.k8s.io/v1", "kind": "Ingress", "name": "main"},
                    "metric": {"name": "hits"}, "current": {"value": "7k"}}}
            ],
            "conditions": [
                {"type": "AbleToScale", "status": "True", "reason": "ReadyForNewScale"},
                {"type": "ScalingActive", "status": "True", "reason": "ValidMetricFound"},
                {"type": "ScalingLimited", "status": "True", "reason": "TooFewReplicas"}
            ]
        })
    }

    #[test]
    fn cells_show_reference_targets_and_replicas() {
        let p = HorizontalPodAutoscalerPlugin;
        let o = hpa(active());
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("Deployment/web"));
        assert_eq!(
            cells[3],
            text(
                "cpu 45%/80%, memory <unknown>/200Mi, rps <unknown>/1k, hits on Ingress/main 7k/10k"
            )
        );
        assert_eq!(cells[4], Cell::Number(2));
        assert_eq!(cells[5], Cell::Number(10));
        assert_eq!(cells[6], text("3/4"));
        // No metrics yet: the controller reports 0 desired.
        let unknown = hpa(json!({"currentReplicas": 2, "desiredReplicas": 0}));
        assert_eq!(p.cells(&unknown)[6], text("2/?"));
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Healthy, "Limited: TooFewReplicas")
        );
    }

    #[test]
    fn details_link_target_and_list_metrics_behavior_conditions() {
        let p = HorizontalPodAutoscalerPlugin;
        let sections = p.details(&hpa(active()));
        let SectionBody::Fields(fields) = body(&sections, OVERVIEW) else {
            panic!("overview is not fields");
        };
        let FieldValue::Link { label, target, .. } = &fields[0].value else {
            panic!("reference is not a link");
        };
        assert_eq!(label, "Deployment/web");
        assert_eq!(
            (target.api_version.as_str(), target.namespace.as_deref()),
            ("apps/v1", Some("default"))
        );
        assert!(fields.iter().any(|f| f.key == "Last scaled"));
        assert_eq!(pairs(&sections, "Metrics")[0], pair("cpu", "45%/80%"));
        assert_eq!(
            pairs(&sections, "Behavior"),
            [
                pair("Scale up", "Pods 4 per 15s · select Max"),
                pair("Scale down", "Percent 10 per 60s · stabilization 300s"),
            ]
        );
        let SectionBody::Conditions(conditions) = body(&sections, "Conditions") else {
            panic!("no conditions");
        };
        let limited = conditions
            .iter()
            .find(|c| c.type_ == "ScalingLimited")
            .unwrap();
        assert!(limited.warning && !limited.negative);
        assert_eq!(limited.is_good(), Some(false));
        let able = conditions
            .iter()
            .find(|c| c.type_ == "AbleToScale")
            .unwrap();
        assert_eq!(able.is_good(), Some(true));

        let rel = p.relations(&hpa(active()));
        assert_eq!(rel[0].target.kind, "Deployment");
    }

    #[test]
    fn health_follows_conditions() {
        let p = HorizontalPodAutoscalerPlugin;
        assert_eq!(
            p.health(&hpa(json!({"desiredReplicas": 0}))).status,
            HealthStatus::Progressing
        );
        let failing = hpa(json!({
            "desiredReplicas": 2,
            "conditions": [
                {"type": "AbleToScale", "status": "True"},
                {"type": "ScalingActive", "status": "False", "reason": "FailedGetResourceMetric",
                 "message": "unable to get metrics for resource cpu"}
            ]
        }));
        assert_eq!(
            p.health(&failing),
            Health::new(
                HealthStatus::Degraded,
                "unable to get metrics for resource cpu"
            )
        );
        let disabled = hpa(json!({
            "desiredReplicas": 0,
            "conditions": [{"type": "ScalingActive", "status": "False", "reason": "ScalingDisabled"}]
        }));
        assert_eq!(p.health(&disabled).status, HealthStatus::Healthy);
    }
}
