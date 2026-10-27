//! VerticalPodAutoscaler (`autoscaling.k8s.io`): a CRD served only when
//! the VPA is installed. The target, the update mode and the
//! recommendations per container (target, lower and upper bound).

use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::gateway::{arr_at, delete_action, str_at, terminating};
use super::{
    DELETE_ACTION, OVERVIEW, col, creation, name_ns_cells, name_ns_columns, object_ref, or_none,
    pair, push_kv, references, relations_with, section, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation, ResourcePlugin,
    SectionBody, Table,
};

pub struct VerticalPodAutoscalerPlugin;

pub const GROUP: &str = "autoscaling.k8s.io";

/// VPA conditions that report a problem when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 2] = ["ConfigUnsupported", "NoPodsMatched"];
/// VPA conditions worth a look when `True`.
pub(crate) const WARNING_CONDITIONS: [&str; 3] =
    ["LowConfidence", "ConfigDeprecated", "FetchingHistory"];

fn target(obj: &DynamicObject) -> Option<ObjectRef> {
    let t = obj.data.pointer("/spec/targetRef")?;
    let name = str_at(t, "/name")?;
    let kind = str_at(t, "/kind").unwrap_or("Deployment");
    let api_version = str_at(t, "/apiVersion").unwrap_or("apps/v1");
    Some(object_ref(
        api_version,
        kind,
        name,
        obj.metadata.namespace.as_deref(),
    ))
}

/// `updatePolicy.updateMode`; the VPA defaults it to `Auto`.
fn update_mode(obj: &DynamicObject) -> &str {
    str_at(&obj.data, "/spec/updatePolicy/updateMode").unwrap_or("Auto")
}

fn recommendations(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/status/recommendation/containerRecommendations")
}

/// A resource of the first container's target recommendation, as
/// `kubectl get vpa` shows it.
fn first_target(obj: &DynamicObject, resource: &str) -> String {
    recommendations(obj)
        .first()
        .and_then(|r| r.pointer(&format!("/target/{resource}")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn condition<'a>(obj: &'a DynamicObject, type_: &str) -> Option<&'a Value> {
    arr_at(&obj.data, "/status/conditions")
        .iter()
        .find(|c| str_at(c, "/type") == Some(type_) && str_at(c, "/status") == Some("True"))
}

fn condition_text(c: &Value, fallback: &str) -> String {
    str_at(c, "/message")
        .filter(|m| !m.is_empty())
        .or_else(|| str_at(c, "/reason"))
        .unwrap_or(fallback)
        .to_owned()
}

pub(crate) fn vpa_health(obj: &DynamicObject) -> Health {
    if let Some(h) = terminating(obj) {
        return h;
    }
    for type_ in NEGATIVE_CONDITIONS {
        if let Some(c) = condition(obj, type_) {
            return Health::new(HealthStatus::Degraded, condition_text(c, type_));
        }
    }
    if condition(obj, "RecommendationProvided").is_some() || !recommendations(obj).is_empty() {
        let message = match condition(obj, "LowConfidence") {
            Some(_) => msg!("state-vpa-provided-low"),
            None => msg!("state-vpa-provided"),
        };
        return Health::phrase(HealthStatus::Healthy, message);
    }
    Health::phrase(
        HealthStatus::Progressing,
        msg!("state-vpa-no-recommendation"),
    )
}

/// Resource names in any of the recommendation's bounds, sorted.
fn resources_of(rec: &Value) -> Vec<String> {
    let mut names: Vec<String> = ["target", "lowerBound", "upperBound", "uncappedTarget"]
        .iter()
        .filter_map(|k| rec.get(*k).and_then(Value::as_object))
        .flat_map(|m| m.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn recommendation_rows(obj: &DynamicObject) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for rec in recommendations(obj) {
        let container = str_at(rec, "/containerName").unwrap_or_default();
        for resource in resources_of(rec) {
            let at = |bound: &str| {
                rec.pointer(&format!("/{bound}/{resource}"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            rows.push(vec![
                container.to_owned(),
                resource.clone(),
                at("target"),
                at("lowerBound"),
                at("upperBound"),
                at("uncappedTarget"),
            ]);
        }
    }
    rows
}

/// `mode Auto · min cpu 100m, memory 50Mi · max cpu 1 · controls RequestsOnly`.
fn container_policy(policy: &Value) -> String {
    let quantities = |key: &str| {
        policy
            .get(key)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("{k} {}", v.as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty())
    };
    let mut parts = Vec::new();
    if let Some(mode) = str_at(policy, "/mode") {
        parts.push(format!("mode {mode}"));
    }
    if let Some(min) = quantities("minAllowed") {
        parts.push(format!("min {min}"));
    }
    if let Some(max) = quantities("maxAllowed") {
        parts.push(format!("max {max}"));
    }
    let controlled: Vec<&str> = arr_at(policy, "/controlledResources")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !controlled.is_empty() {
        parts.push(format!("resources {}", controlled.join(", ")));
    }
    if let Some(values) = str_at(policy, "/controlledValues") {
        parts.push(format!("controls {values}"));
    }
    or_none(parts.join(" · "))
}

impl ResourcePlugin for VerticalPodAutoscalerPlugin {
    fn name(&self) -> &'static str {
        "verticalpodautoscaler"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1", "VerticalPodAutoscaler")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("target", "Target", ColumnKind::Text),
            col("mode", "Mode", ColumnKind::Text),
            col("cpu", "CPU", ColumnKind::Text),
            col("memory", "Memory", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(
                target(obj)
                    .map(|t| format!("{}/{}", t.kind, t.name))
                    .unwrap_or_default(),
            ),
            text(update_mode(obj)),
            text(first_target(obj, "cpu")),
            text(first_target(obj, "memory")),
            Cell::Status(vpa_health(obj)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let mut fields = Vec::new();
        match target(obj) {
            Some(t) => {
                let label = format!("{}/{}", t.kind, t.name);
                fields.push(Field::object_link("Target", label, t));
            }
            None => fields.push(Field::text("Target", "<none>")),
        }
        fields.push(Field::text("Update mode", update_mode(obj)));
        if let Some(min) = obj
            .data
            .pointer("/spec/updatePolicy/minReplicas")
            .and_then(Value::as_i64)
        {
            fields.push(Field::text("Min replicas", min.to_string()));
        }
        let recommenders: Vec<&str> = arr_at(&obj.data, "/spec/recommenders")
            .iter()
            .filter_map(|r| str_at(r, "/name"))
            .collect();
        if !recommenders.is_empty() {
            fields.push(Field::text("Recommenders", recommenders.join(", ")));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        let rows = recommendation_rows(obj);
        if !rows.is_empty() {
            sections.push(section(
                "Recommendations",
                SectionBody::Table(Table::new(
                    &[
                        "Container",
                        "Resource",
                        "Target",
                        "Lower",
                        "Upper",
                        "Uncapped",
                    ],
                    rows,
                )),
            ));
        }
        push_kv(
            &mut sections,
            "Container policies",
            arr_at(&obj.data, "/spec/resourcePolicy/containerPolicies")
                .iter()
                .map(|p| {
                    pair(
                        str_at(p, "/containerName").unwrap_or("*"),
                        container_policy(p),
                    )
                })
                .collect(),
        );
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
        delete_action(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        relations_with(obj, target(obj).map(references).into_iter().collect())
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        vpa_health(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::FieldValue;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    fn vpa(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "autoscaling.k8s.io/v1", "kind": "VerticalPodAutoscaler",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {
                "targetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
                "updatePolicy": {"updateMode": "Initial", "minReplicas": 2},
                "resourcePolicy": {"containerPolicies": [
                    {"containerName": "app", "minAllowed": {"cpu": "100m"}, "maxAllowed": {"memory": "1Gi"},
                     "controlledResources": ["cpu", "memory"], "controlledValues": "RequestsOnly"},
                    {"containerName": "*", "mode": "Off"}
                ]}
            },
            "status": status
        }))
    }

    fn recommended() -> Value {
        json!({
            "recommendation": {"containerRecommendations": [
                {"containerName": "app",
                 "target": {"cpu": "250m", "memory": "262144k"},
                 "lowerBound": {"cpu": "100m", "memory": "131072k"},
                 "upperBound": {"cpu": "1", "memory": "1Gi"},
                 "uncappedTarget": {"cpu": "250m", "memory": "262144k"}},
                {"containerName": "sidecar", "target": {"cpu": "25m"}}
            ]},
            "conditions": [{"type": "RecommendationProvided", "status": "True"}]
        })
    }

    #[test]
    fn vpa_cells() {
        let p = VerticalPodAutoscalerPlugin;
        let o = vpa(recommended());
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[2..6],
            [
                text("Deployment/web"),
                text("Initial"),
                text("250m"),
                text("262144k")
            ]
        );
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Healthy, "Recommendation provided")
        );
    }

    #[test]
    fn vpa_details_recommendations() {
        let p = VerticalPodAutoscalerPlugin;
        let o = vpa(recommended());
        let sections = p.details(&o);
        let SectionBody::Fields(fields) = body(&sections, OVERVIEW) else {
            panic!()
        };
        assert_eq!(
            fields[0].value,
            FieldValue::Link {
                phrase: None,
                label: "Deployment/web".into(),
                target: object_ref("apps/v1", "Deployment", "web", Some("shop")),
            }
        );
        assert_eq!(fields[1], Field::text("Update mode", "Initial"));
        assert_eq!(fields[2], Field::text("Min replicas", "2"));
        let SectionBody::Table(table) = body(&sections, "Recommendations") else {
            panic!()
        };
        assert_eq!(
            table.rows,
            [
                ["app", "cpu", "250m", "100m", "1", "250m"],
                ["app", "memory", "262144k", "131072k", "1Gi", "262144k"],
                ["sidecar", "cpu", "25m", "", "", ""],
            ]
        );
        assert_eq!(
            pairs(&sections, "Container policies"),
            [
                pair(
                    "app",
                    "min cpu 100m · max memory 1Gi · resources cpu, memory · controls RequestsOnly"
                ),
                pair("*", "mode Off"),
            ]
        );
        let targets: Vec<_> = p.relations(&o).into_iter().map(|r| r.target.kind).collect();
        assert_eq!(targets, ["Deployment"]);
    }

    #[test]
    fn vpa_health_states() {
        let p = VerticalPodAutoscalerPlugin;
        assert_eq!(
            p.health(&vpa(json!(null))),
            Health::new(HealthStatus::Progressing, "No recommendation yet")
        );
        let unmatched = vpa(json!({"conditions": [
            {"type": "NoPodsMatched", "status": "True", "message": "No pods match this VPA object"}
        ]}));
        assert_eq!(
            p.health(&unmatched),
            Health::new(HealthStatus::Degraded, "No pods match this VPA object")
        );
        let SectionBody::Conditions(c) = body(&p.details(&unmatched), "Conditions").clone() else {
            panic!()
        };
        assert_eq!(c[0].is_good(), Some(false));
        let low = vpa(json!({"conditions": [
            {"type": "RecommendationProvided", "status": "True"},
            {"type": "LowConfidence", "status": "True"}
        ]}));
        assert!(p.health(&low).message.contains("low confidence"));
        let bare = obj(json!({
            "apiVersion": "autoscaling.k8s.io/v1", "kind": "VerticalPodAutoscaler",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {"targetRef": {"apiVersion": "apps/v1", "kind": "StatefulSet", "name": "db"}}
        }));
        let cells = p.cells(&bare);
        assert_eq!(
            cells[2..6],
            [text("StatefulSet/db"), text("Auto"), text(""), text("")]
        );
    }
}
