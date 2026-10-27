//! Cluster-wide classes: IngressClass, StorageClass and PriorityClass.

use k8s_openapi::api::networking::v1::IngressClass;
use k8s_openapi::api::scheduling::v1::PriorityClass;
use k8s_openapi::api::storage::v1::StorageClass;
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, col, creation, decode, fallback_cells, kv, map_pairs, or_none, pair,
    push_kv, run_delete, section, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    GenericPlugin, Health, HealthStatus, PluginError, Relation, ResourcePlugin, SectionBody,
};

pub struct IngressClassPlugin;
pub struct StorageClassPlugin;
pub struct PriorityClassPlugin;

const INGRESS_DEFAULT: &str = "ingressclass.kubernetes.io/is-default-class";
const STORAGE_DEFAULT: &str = "storageclass.kubernetes.io/is-default-class";
/// Set by old clusters instead of [`STORAGE_DEFAULT`].
const STORAGE_DEFAULT_BETA: &str = "storageclass.beta.kubernetes.io/is-default-class";

fn annotated_default(obj: &DynamicObject, keys: &[&str]) -> bool {
    obj.metadata
        .annotations
        .as_ref()
        .is_some_and(|a| keys.iter().any(|k| a.get(*k).is_some_and(|v| v == "true")))
}

fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

fn name_column() -> Column {
    col("name", "Name", ColumnKind::Text)
}

fn name_cell(obj: &DynamicObject) -> Cell {
    text(obj.metadata.name.clone().unwrap_or_default())
}

/// A class is configuration: present means in effect.
fn active() -> Health {
    Health::phrase(HealthStatus::Healthy, msg!("state-active"))
}

// ---------------------------------------------------------------- IngressClass

fn ingress_parameters(class: &IngressClass) -> String {
    class
        .spec
        .as_ref()
        .and_then(|s| s.parameters.as_ref())
        .map(|p| {
            let group = p
                .api_group
                .as_deref()
                .map(|g| format!(".{g}"))
                .unwrap_or_default();
            match &p.namespace {
                Some(ns) => format!("{}{group} {ns}/{}", p.kind, p.name),
                None => format!("{}{group} {}", p.kind, p.name),
            }
        })
        .unwrap_or_default()
}

impl ResourcePlugin for IngressClassPlugin {
    fn name(&self) -> &'static str {
        "ingressclass"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            "networking.k8s.io",
            "v1",
            "IngressClass",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            name_column(),
            col("controller", "Controller", ColumnKind::Text),
            col("default", "Default", ColumnKind::Text),
            col("parameters", "Parameters", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(class) = decode::<IngressClass>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        vec![
            name_cell(obj),
            text(
                class
                    .spec
                    .as_ref()
                    .and_then(|s| s.controller.clone())
                    .unwrap_or_default(),
            ),
            text(yes_no(annotated_default(obj, &[INGRESS_DEFAULT]))),
            text(or_none(ingress_parameters(&class))),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(class) = decode::<IngressClass>(obj) else {
            return GenericPlugin.details(obj);
        };
        let mut sections = vec![kv(
            OVERVIEW,
            vec![
                pair(
                    "Controller",
                    or_none(
                        class
                            .spec
                            .as_ref()
                            .and_then(|s| s.controller.clone())
                            .unwrap_or_default(),
                    ),
                ),
                pair(
                    "Default",
                    yes_no(annotated_default(obj, &[INGRESS_DEFAULT])),
                ),
                pair("Parameters", or_none(ingress_parameters(&class))),
            ],
        )];
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
        run_delete::<IngressClass>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        active()
    }
}

// ---------------------------------------------------------------- StorageClass

fn reclaim_policy(sc: &StorageClass) -> String {
    sc.reclaim_policy.clone().unwrap_or_else(|| "Delete".into())
}

fn binding_mode(sc: &StorageClass) -> String {
    sc.volume_binding_mode
        .clone()
        .unwrap_or_else(|| "Immediate".into())
}

fn storage_default(obj: &DynamicObject) -> bool {
    annotated_default(obj, &[STORAGE_DEFAULT, STORAGE_DEFAULT_BETA])
}

impl ResourcePlugin for StorageClassPlugin {
    fn name(&self) -> &'static str {
        "storageclass"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            "storage.k8s.io",
            "v1",
            "StorageClass",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            name_column(),
            col("provisioner", "Provisioner", ColumnKind::Text),
            col("reclaim_policy", "Reclaim policy", ColumnKind::Text),
            col("binding_mode", "Binding mode", ColumnKind::Text),
            col("expansion", "Expansion", ColumnKind::Text),
            col("default", "Default", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(sc) = decode::<StorageClass>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        vec![
            name_cell(obj),
            text(sc.provisioner.clone()),
            text(reclaim_policy(&sc)),
            text(binding_mode(&sc)),
            text(yes_no(sc.allow_volume_expansion.unwrap_or(false))),
            text(yes_no(storage_default(obj))),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(sc) = decode::<StorageClass>(obj) else {
            return GenericPlugin.details(obj);
        };
        let mut overview = vec![
            pair("Provisioner", sc.provisioner.clone()),
            pair("Reclaim policy", reclaim_policy(&sc)),
            pair("Binding mode", binding_mode(&sc)),
            pair(
                "Volume expansion",
                yes_no(sc.allow_volume_expansion.unwrap_or(false)),
            ),
            pair("Default", yes_no(storage_default(obj))),
        ];
        let options = sc.mount_options.clone().unwrap_or_default();
        if !options.is_empty() {
            overview.push(pair("Mount options", options.join(", ")));
        }
        let mut sections = vec![kv(OVERVIEW, overview)];
        push_kv(
            &mut sections,
            "Parameters",
            map_pairs(sc.parameters.as_ref()),
        );
        push_kv(
            &mut sections,
            "Allowed topologies",
            sc.allowed_topologies
                .iter()
                .flatten()
                .flat_map(|t| t.match_label_expressions.iter().flatten())
                .map(|e| pair(e.key.clone(), e.values.join(", ")))
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
        run_delete::<StorageClass>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        active()
    }
}

// --------------------------------------------------------------- PriorityClass

impl ResourcePlugin for PriorityClassPlugin {
    fn name(&self) -> &'static str {
        "priorityclass"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            "scheduling.k8s.io",
            "v1",
            "PriorityClass",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            name_column(),
            col("value", "Value", ColumnKind::Number),
            col("global_default", "Global default", ColumnKind::Text),
            col("preemption", "Preemption", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(pc) = decode::<PriorityClass>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        vec![
            name_cell(obj),
            Cell::Number(pc.value.unwrap_or(0).into()),
            text(yes_no(pc.global_default.unwrap_or(false))),
            text(
                pc.preemption_policy
                    .clone()
                    .unwrap_or_else(|| "PreemptLowerPriority".into()),
            ),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(pc) = decode::<PriorityClass>(obj) else {
            return GenericPlugin.details(obj);
        };
        let mut fields = vec![
            Field::text("Value", pc.value.unwrap_or(0).to_string()),
            Field::text("Global default", yes_no(pc.global_default.unwrap_or(false))),
            Field::text(
                "Preemption",
                pc.preemption_policy
                    .clone()
                    .unwrap_or_else(|| "PreemptLowerPriority".into()),
            ),
        ];
        if let Some(description) = pc.description.clone().filter(|d| !d.is_empty()) {
            fields.push(Field::text("Description", description));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
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
        run_delete::<PriorityClass>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        active()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs};

    #[test]
    fn ingress_class() {
        let p = IngressClassPlugin;
        let o = obj(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "IngressClass",
            "metadata": {"name": "nginx", "annotations": {INGRESS_DEFAULT: "true"}},
            "spec": {"controller": "k8s.io/ingress-nginx",
                     "parameters": {"apiGroup": "k8s.example.com", "kind": "IngressParameters", "name": "external"}}
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[1], text("k8s.io/ingress-nginx"));
        assert_eq!(cells[2], text("Yes"));
        assert_eq!(cells[3], text("IngressParameters.k8s.example.com external"));
        assert_eq!(
            pairs(&p.details(&o), OVERVIEW)[0],
            pair("Controller", "k8s.io/ingress-nginx")
        );
    }

    #[test]
    fn storage_class() {
        let p = StorageClassPlugin;
        let o = obj(json!({
            "apiVersion": "storage.k8s.io/v1", "kind": "StorageClass",
            "metadata": {"name": "standard", "annotations": {STORAGE_DEFAULT: "true"}},
            "provisioner": "k8s.io/minikube-hostpath",
            "volumeBindingMode": "WaitForFirstConsumer",
            "allowVolumeExpansion": true,
            "parameters": {"type": "pd-ssd"},
            "mountOptions": ["debug"]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[1..6],
            [
                text("k8s.io/minikube-hostpath"),
                text("Delete"),
                text("WaitForFirstConsumer"),
                text("Yes"),
                text("Yes"),
            ]
        );
        let sections = p.details(&o);
        assert_eq!(pairs(&sections, "Parameters"), [pair("type", "pd-ssd")]);
        assert!(pairs(&sections, OVERVIEW).contains(&pair("Mount options", "debug")));
    }

    #[test]
    fn priority_class() {
        let p = PriorityClassPlugin;
        let o = obj(json!({
            "apiVersion": "scheduling.k8s.io/v1", "kind": "PriorityClass",
            "metadata": {"name": "system-cluster-critical"},
            "value": 2000000000, "description": "Used for system critical pods"
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[1], Cell::Number(2_000_000_000));
        assert_eq!(cells[2], text("No"));
        assert_eq!(cells[3], text("PreemptLowerPriority"));
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
    }
}
