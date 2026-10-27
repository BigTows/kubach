use std::collections::BTreeMap;

use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::builtin::{DELETE_ACTION, action, condition_polarity, delete_discovered, unknown};
use super::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, ConditionInfo,
    DetailSection, Health, HealthStatus, ObjectRef, Placement, PluginError, Relation, RelationKind,
    ResourcePlugin, SectionBody,
};

/// Works for any kind (table + metadata details), including CRDs.
pub struct GenericPlugin;

pub(crate) fn conditions(obj: &DynamicObject) -> Vec<ConditionInfo> {
    let types = obj.types.as_ref();
    let str_of = |c: &Value, key: &str| c.get(key).and_then(Value::as_str).map(str::to_owned);
    obj.data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let type_ = str_of(c, "type")?;
            let status = str_of(c, "status")?;
            Some(condition_polarity(types, &type_).condition(
                type_,
                status,
                str_of(c, "reason"),
                str_of(c, "message"),
                str_of(c, "lastTransitionTime").and_then(|t| t.parse().ok()),
            ))
        })
        .collect()
}

fn condition_text(c: &ConditionInfo) -> String {
    c.message
        .as_deref()
        .or(c.reason.as_deref())
        .unwrap_or(&c.type_)
        .to_owned()
}

fn pairs(map: &BTreeMap<String, String>) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

impl ResourcePlugin for GenericPlugin {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        Vec::new()
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            Column {
                id: "name",
                title: "Name",
                kind: ColumnKind::Text,
            },
            Column {
                id: "namespace",
                title: "Namespace",
                kind: ColumnKind::Text,
            },
            Column {
                id: "age",
                title: "Age",
                kind: ColumnKind::Age,
            },
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        vec![
            Cell::Text(obj.metadata.name.clone().unwrap_or_default()),
            Cell::Text(obj.metadata.namespace.clone().unwrap_or_default()),
            Cell::Age(obj.metadata.creation_timestamp.as_ref().map(|t| t.0)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let meta = &obj.metadata;
        let mut sections = Vec::new();
        if let Some(labels) = &meta.labels {
            sections.push(
                DetailSection::new("Labels", SectionBody::KeyValues(pairs(labels)))
                    .placement(Placement::Metadata),
            );
        }
        if let Some(annotations) = &meta.annotations {
            sections.push(
                DetailSection::new("Annotations", SectionBody::KeyValues(pairs(annotations)))
                    .placement(Placement::Metadata),
            );
        }
        if let Some(owners) = meta.owner_references.as_ref().filter(|o| !o.is_empty()) {
            sections.push(
                DetailSection::new(
                    "Owner references",
                    SectionBody::KeyValues(
                        owners
                            .iter()
                            .map(|o| (o.kind.clone(), o.name.clone()))
                            .collect(),
                    ),
                )
                .placement(Placement::Metadata),
            );
        }
        let conditions = conditions(obj);
        if !conditions.is_empty() {
            sections.push(DetailSection::new(
                "Conditions",
                SectionBody::Conditions(conditions),
            ));
        }
        sections
    }

    /// Any kind, CRDs included, can be deleted.
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
        Box::pin(async move {
            match *action_id {
                action::DELETE => delete_discovered(client, obj).await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        obj.metadata
            .owner_references
            .iter()
            .flatten()
            .map(|o| Relation {
                kind: RelationKind::OwnedBy,
                target: ObjectRef {
                    api_version: o.api_version.clone(),
                    kind: o.kind.clone(),
                    name: o.name.clone(),
                    // Owners of namespaced objects live in the same namespace.
                    namespace: obj.metadata.namespace.clone(),
                },
            })
            .collect()
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        if obj.metadata.deletion_timestamp.is_some() {
            return Health::new(HealthStatus::Progressing, "Terminating");
        }
        let conditions = conditions(obj);
        let find = |types: &[&str], status: &str| {
            conditions
                .iter()
                .find(|c| types.contains(&c.type_.as_str()) && c.status == status)
        };
        if let Some(c) = find(&["Failed"], "True") {
            return Health::new(HealthStatus::Failed, condition_text(c));
        }
        if let Some(c) = find(&["Degraded"], "True") {
            return Health::new(HealthStatus::Degraded, condition_text(c));
        }
        let positive = ["Ready", "Available", "Healthy"];
        if let Some(c) = find(&positive, "False") {
            return Health::new(HealthStatus::Degraded, condition_text(c));
        }
        if let Some(c) = find(&positive, "True") {
            return Health::new(HealthStatus::Healthy, condition_text(c));
        }
        Health::unknown()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::plugin::{PluginRegistry, format_age};

    fn obj(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn widget() -> DynamicObject {
        obj(json!({
            "apiVersion": "example.com/v1",
            "kind": "Widget",
            "metadata": {
                "name": "w1",
                "namespace": "default",
                "creationTimestamp": "2026-10-01T00:00:00Z",
                "labels": {"b": "2", "a": "1"},
                "annotations": {"note": "hi"},
                "ownerReferences": [{
                    "apiVersion": "apps/v1", "kind": "Deployment",
                    "name": "owner", "uid": "u1"
                }]
            },
            "status": {"conditions": [
                {"type": "Ready", "status": "True", "reason": "AllGood"}
            ]}
        }))
    }

    fn with_conditions(c: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "X", "metadata": {"name": "x"},
            "status": {"conditions": c}
        }))
    }

    #[test]
    fn cells_match_columns() {
        let p = GenericPlugin;
        let cells = p.cells(&widget());
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[0], Cell::Text("w1".into()));
        assert_eq!(cells[1], Cell::Text("default".into()));
        let Cell::Age(Some(ts)) = &cells[2] else {
            panic!("age expected");
        };
        assert_eq!(ts.to_string(), "2026-10-01T00:00:00Z");
    }

    #[test]
    fn cluster_scoped_object_has_empty_namespace_and_no_age() {
        let o = obj(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "n"}}));
        let cells = GenericPlugin.cells(&o);
        assert_eq!(cells[1], Cell::Text(String::new()));
        assert_eq!(cells[2], Cell::Age(None));
    }

    #[test]
    fn details_from_metadata() {
        let sections = GenericPlugin.details(&widget());
        let titles: Vec<_> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Labels", "Annotations", "Owner references", "Conditions"]
        );
        assert_eq!(
            sections[0].body,
            SectionBody::KeyValues(vec![("a".into(), "1".into()), ("b".into(), "2".into())])
        );
    }

    #[test]
    fn owner_relations() {
        let rels = GenericPlugin.relations(&widget());
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, RelationKind::OwnedBy);
        assert_eq!(rels[0].target.kind, "Deployment");
        assert_eq!(rels[0].target.namespace.as_deref(), Some("default"));
    }

    #[test]
    fn health_from_conditions() {
        let p = GenericPlugin;
        assert_eq!(p.health(&widget()).status, HealthStatus::Healthy);

        let h = p.health(&with_conditions(json!([
            {"type": "Ready", "status": "False", "message": "no endpoints"}
        ])));
        assert_eq!(h, Health::new(HealthStatus::Degraded, "no endpoints"));

        let h = p.health(&with_conditions(json!([
            {"type": "Failed", "status": "True", "reason": "BackoffLimitExceeded"}
        ])));
        assert_eq!(h.status, HealthStatus::Failed);
        assert_eq!(h.message, "BackoffLimitExceeded");

        let h = p.health(&with_conditions(json!([
            {"type": "Degraded", "status": "True"}
        ])));
        assert_eq!(h.status, HealthStatus::Degraded);
    }

    #[test]
    fn health_unknown_without_conditions() {
        let o = obj(json!({"apiVersion": "v1", "kind": "X", "metadata": {"name": "x"}}));
        assert_eq!(GenericPlugin.health(&o), Health::unknown());
    }

    #[test]
    fn terminating_is_progressing() {
        let o = obj(json!({
            "apiVersion": "v1", "kind": "X",
            "metadata": {"name": "x", "deletionTimestamp": "2026-10-02T00:00:00Z"}
        }));
        assert_eq!(GenericPlugin.health(&o).status, HealthStatus::Progressing);
    }

    struct Special;

    impl ResourcePlugin for Special {
        fn name(&self) -> &'static str {
            "special"
        }
        fn supported(&self) -> Vec<GroupVersionKind> {
            vec![GroupVersionKind::gvk("apps", "v1", "Deployment")]
        }
        fn columns(&self) -> Vec<Column> {
            Vec::new()
        }
        fn cells(&self, _: &DynamicObject) -> Vec<Cell> {
            Vec::new()
        }
        fn details(&self, _: &DynamicObject) -> Vec<DetailSection> {
            Vec::new()
        }
        fn relations(&self, _: &DynamicObject) -> Vec<Relation> {
            Vec::new()
        }
        fn health(&self, _: &DynamicObject) -> Health {
            Health::unknown()
        }
    }

    #[test]
    fn registry_selects_by_gvk_with_generic_fallback() {
        let mut registry = PluginRegistry::new();
        registry.register(Arc::new(Special));

        assert_eq!(registry.plugin_for("apps", "Deployment").name(), "special");
        // The version is not part of the lookup.
        let gvk = GroupVersionKind::gvk("apps", "v1beta1", "Deployment");
        assert_eq!(registry.plugin_for_gvk(&gvk).name(), "special");
        // CRDs and unregistered kinds use the generic plugin.
        assert_eq!(
            registry.plugin_for("example.com", "Widget").name(),
            "generic"
        );
        assert_eq!(registry.plugin_for("", "Pod").name(), "generic");
    }

    #[test]
    fn age_formatting() {
        assert_eq!(format_age(10), "10s");
        assert_eq!(format_age(300), "5m");
        assert_eq!(format_age(3 * 3600), "3h");
        assert_eq!(format_age(5 * 86_400), "5d");
        assert_eq!(format_age(-1), "0s");
        // kubectl's HumanDuration, like the GUI.
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(192), "3m12s");
        assert_eq!(format_age(25 * 60), "25m");
        assert_eq!(format_age(3 * 3600 + 300), "3h5m");
        assert_eq!(format_age(10 * 3600), "10h");
        assert_eq!(format_age(3 * 86_400 + 4 * 3600), "3d4h");
        assert_eq!(format_age(3 * 365 * 86_400 + 10 * 86_400), "3y10d");
        assert_eq!(format_age(9 * 365 * 86_400), "9y");
    }

    #[test]
    fn generic_plugin_only_deletes() {
        let ids: Vec<_> = GenericPlugin
            .actions()
            .iter()
            .map(|a| (a.id.as_str(), a.dangerous))
            .collect();
        assert_eq!(ids, [("delete", true)]);
    }

    fn polarity(api_version: &str, kind: &str, types: &[&str]) -> Vec<(String, bool)> {
        let conditions: Vec<Value> = types
            .iter()
            .map(|t| json!({"type": t, "status": "True"}))
            .collect();
        conditions_of(&obj(json!({
            "apiVersion": api_version,
            "kind": kind,
            "metadata": {"name": "x"},
            "status": {"conditions": conditions}
        })))
    }

    fn conditions_of(o: &DynamicObject) -> Vec<(String, bool)> {
        conditions(o)
            .into_iter()
            .map(|c| (c.type_, c.negative))
            .collect()
    }

    fn pairs_of(list: &[(&str, bool)]) -> Vec<(String, bool)> {
        list.iter().map(|&(t, n)| (t.to_owned(), n)).collect()
    }

    #[test]
    fn plugins_fill_condition_polarity() {
        assert_eq!(
            polarity(
                "v1",
                "Node",
                &[
                    "Ready",
                    "MemoryPressure",
                    "DiskPressure",
                    "PIDPressure",
                    "NetworkUnavailable"
                ]
            ),
            pairs_of(&[
                ("Ready", false),
                ("MemoryPressure", true),
                ("DiskPressure", true),
                ("PIDPressure", true),
                ("NetworkUnavailable", true),
            ])
        );
        assert_eq!(
            polarity(
                "apps/v1",
                "Deployment",
                &["Available", "Progressing", "ReplicaFailure"]
            ),
            pairs_of(&[
                ("Available", false),
                ("Progressing", false),
                ("ReplicaFailure", true),
            ])
        );
        assert_eq!(
            polarity(
                "batch/v1",
                "Job",
                &["Complete", "SuccessCriteriaMet", "Failed", "FailureTarget"]
            ),
            pairs_of(&[
                ("Complete", false),
                ("SuccessCriteriaMet", false),
                ("Failed", true),
                ("FailureTarget", true),
            ])
        );
    }

    #[test]
    fn unknown_kinds_get_default_polarity() {
        assert_eq!(
            polarity(
                "example.com/v1",
                "Widget",
                &["Ready", "Synced", "Degraded", "Stalled", "SyncFailed"]
            ),
            pairs_of(&[
                ("Ready", false),
                ("Synced", false),
                ("Degraded", true),
                ("Stalled", true),
                ("SyncFailed", true),
            ])
        );
        // No kind at all: still the default rule.
        let mut untyped = obj(json!({
            "apiVersion": "v1", "kind": "Node", "metadata": {"name": "x"},
            "status": {"conditions": [{"type": "DiskPressure", "status": "True"}]}
        }));
        untyped.types = None;
        assert_eq!(conditions_of(&untyped), pairs_of(&[("DiskPressure", true)]));
    }

    /// `(type, negative, warning, is_good)` of each condition.
    fn tones(o: &DynamicObject) -> Vec<(String, bool, bool, Option<bool>)> {
        conditions(o)
            .into_iter()
            .map(|c| {
                let good = c.is_good();
                (c.type_, c.negative, c.warning, good)
            })
            .collect()
    }

    #[test]
    fn pod_disruption_target_is_a_warning() {
        let pod = |status: &str| {
            obj(json!({
                "apiVersion": "v1", "kind": "Pod", "metadata": {"name": "p"},
                "status": {"conditions": [
                    {"type": "Ready", "status": "True"},
                    {"type": "DisruptionTarget", "status": status}
                ]}
            }))
        };
        assert_eq!(
            tones(&pod("True")),
            [
                ("Ready".into(), false, false, Some(true)),
                ("DisruptionTarget".into(), false, true, Some(false)),
            ]
        );
        assert_eq!(
            tones(&pod("False"))[1],
            ("DisruptionTarget".into(), false, true, Some(true))
        );
        // Another kind with the same type keeps the default polarity.
        assert_eq!(
            polarity("example.com/v1", "Widget", &["DisruptionTarget"]),
            pairs_of(&[("DisruptionTarget", false)])
        );
    }

    #[test]
    fn flux_conditions_follow_kstatus() {
        let flux = |conditions: Value| {
            obj(json!({
                "apiVersion": "kustomize.toolkit.fluxcd.io/v1", "kind": "Kustomization",
                "metadata": {"name": "k"}, "status": {"conditions": conditions}
            }))
        };
        assert_eq!(
            tones(&flux(json!([
                {"type": "Ready", "status": "False"},
                {"type": "Stalled", "status": "True"},
                {"type": "Reconciling", "status": "True"},
                {"type": "Healthy", "status": "True"},
                {"type": "HealthCheckFailed", "status": "False"}
            ]))),
            [
                ("Ready".into(), false, false, Some(false)),
                ("Stalled".into(), true, false, Some(false)),
                ("Reconciling".into(), false, true, Some(false)),
                ("Healthy".into(), false, false, Some(true)),
                ("HealthCheckFailed".into(), true, false, Some(true)),
            ]
        );
        let source = obj(json!({
            "apiVersion": "source.toolkit.fluxcd.io/v1", "kind": "GitRepository",
            "metadata": {"name": "g"}, "status": {"conditions": [
                {"type": "ArtifactOutdated", "status": "True"},
                {"type": "Reconciling", "status": "False"}
            ]}
        }));
        assert_eq!(
            tones(&source),
            [
                ("ArtifactOutdated".into(), false, true, Some(false)),
                ("Reconciling".into(), false, true, Some(true)),
            ]
        );
    }
}
