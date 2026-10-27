//! CustomResourceDefinition: group, kind, versions (served, storage) with a
//! short schema summary, scope, conditions and a link to the objects.
//!
//! The plugin reads the JSON: schemas of big CRDs are large, and copying
//! them into typed structs for every table row is wasted work.

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::gateway::{arr_at, str_at, terminating};
use super::{
    DELETE_ACTION, OVERVIEW, col, creation, object_ref, or_none, references, relations_with,
    run_delete, section, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation, ResourcePlugin,
    SectionBody, Table,
};

pub struct CustomResourceDefinitionPlugin;

/// Group of the CRD kind itself.
const GROUP: &str = "apiextensions.k8s.io";

/// CRD conditions that report a problem when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 1] = ["NonStructuralSchema"];
/// CRD conditions that are worth a look when `True`: the CRD is going away.
pub(crate) const WARNING_CONDITIONS: [&str; 1] = ["Terminating"];

fn versions(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/spec/versions")
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `v1 (storage), v1beta1, v1alpha1 (not served)`.
fn versions_text(obj: &DynamicObject) -> String {
    versions(obj)
        .iter()
        .map(|v| {
            let name = str_at(v, "/name").unwrap_or_default();
            let mut notes = Vec::new();
            if flag(v, "storage") {
                notes.push("storage");
            }
            if !flag(v, "served") {
                notes.push("not served");
            }
            if flag(v, "deprecated") {
                notes.push("deprecated");
            }
            if notes.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({})", notes.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The version objects are stored in, else the first served one: used for
/// the link to the objects (links resolve by group and kind anyway).
fn link_version(obj: &DynamicObject) -> Option<&str> {
    let list = versions(obj);
    list.iter()
        .find(|v| flag(v, "storage"))
        .or_else(|| list.iter().find(|v| flag(v, "served")))
        .or(list.first())
        .and_then(|v| str_at(v, "/name"))
}

/// The objects of the defined kind: a link without a name opens the list.
fn objects_ref(obj: &DynamicObject) -> Option<ObjectRef> {
    let group = str_at(&obj.data, "/spec/group")?;
    let kind = str_at(&obj.data, "/spec/names/kind")?;
    let version = link_version(obj).unwrap_or("v1");
    Some(object_ref(&format!("{group}/{version}"), kind, "", None))
}

/// `spec (12 fields), status (3 fields)`; `any fields` for a schema that
/// keeps unknown fields; `<none>` without a schema.
fn schema_text(version: &Value) -> String {
    let Some(schema) = version.pointer("/schema/openAPIV3Schema") else {
        return "<none>".into();
    };
    let preserve = |v: &Value| flag(v, "x-kubernetes-preserve-unknown-fields");
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return if preserve(schema) {
            "any fields".into()
        } else {
            "<none>".into()
        };
    };
    let parts: Vec<String> = props
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "apiVersion" | "kind" | "metadata"))
        .map(
            |(name, prop)| match prop.get("properties").and_then(Value::as_object) {
                Some(fields) => format!("{name} ({} fields)", fields.len()),
                None if preserve(prop) => format!("{name} (any fields)"),
                None => match str_at(prop, "/type") {
                    Some(t) if t != "object" => format!("{name} ({t})"),
                    _ => name.clone(),
                },
            },
        )
        .collect();
    or_none(parts.join(", "))
}

/// `status, scale · 3 printer columns · deprecated: <warning>`.
fn version_extras(version: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(subresources) = version.get("subresources").and_then(Value::as_object) {
        let names: Vec<&str> = subresources.keys().map(String::as_str).collect();
        if !names.is_empty() {
            parts.push(format!("subresources {}", names.join(", ")));
        }
    }
    let columns = arr_at(version, "/additionalPrinterColumns").len();
    if columns > 0 {
        parts.push(format!("{columns} printer columns"));
    }
    if flag(version, "deprecated") {
        parts.push(match str_at(version, "/deprecationWarning") {
            Some(w) => format!("deprecated: {w}"),
            None => "deprecated".into(),
        });
    }
    parts.join(" · ")
}

fn yes_no(b: bool) -> String {
    if b { "Yes" } else { "No" }.into()
}

/// The conversion webhook's Service, when the CRD converts through one.
fn conversion_service(obj: &DynamicObject) -> Option<(ObjectRef, String)> {
    let service = obj
        .data
        .pointer("/spec/conversion/webhook/clientConfig/service")?;
    let name = str_at(service, "/name")?;
    let ns = str_at(service, "/namespace").unwrap_or("default");
    let port = service.get("port").and_then(Value::as_i64).unwrap_or(443);
    let path = str_at(service, "/path").unwrap_or_default();
    Some((
        object_ref("v1", "Service", name, Some(ns)),
        format!("{ns}/{name}:{port}{path}"),
    ))
}

fn condition<'a>(obj: &'a DynamicObject, type_: &str) -> Option<&'a Value> {
    arr_at(&obj.data, "/status/conditions")
        .iter()
        .find(|c| str_at(c, "/type") == Some(type_))
}

fn condition_text(c: &Value, fallback: &str) -> String {
    str_at(c, "/message")
        .filter(|m| !m.is_empty())
        .or_else(|| str_at(c, "/reason"))
        .unwrap_or(fallback)
        .to_owned()
}

/// Established is healthy; names that clash with another CRD or a schema
/// that is not structural are degraded.
pub(crate) fn crd_health(obj: &DynamicObject) -> Health {
    if let Some(h) = terminating(obj) {
        return h;
    }
    let status = |c: &Value| str_at(c, "/status").unwrap_or_default().to_owned();
    if let Some(c) = condition(obj, "NamesAccepted").filter(|c| status(c) == "False") {
        return Health::new(
            HealthStatus::Degraded,
            condition_text(c, "Names not accepted"),
        );
    }
    if let Some(c) = condition(obj, "NonStructuralSchema").filter(|c| status(c) == "True") {
        return Health::new(
            HealthStatus::Degraded,
            condition_text(c, "Non-structural schema"),
        );
    }
    match condition(obj, "Established") {
        Some(c) if status(c) == "True" => {
            Health::phrase(HealthStatus::Healthy, msg!("state-crd-established"))
        }
        Some(c) if status(c) == "False" => Health::new(
            HealthStatus::Progressing,
            condition_text(c, "Not established"),
        ),
        _ => Health::phrase(HealthStatus::Progressing, msg!("state-crd-not-established")),
    }
}

impl ResourcePlugin for CustomResourceDefinitionPlugin {
    fn name(&self) -> &'static str {
        "customresourcedefinition"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            GROUP,
            "v1",
            "CustomResourceDefinition",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("group", "Group", ColumnKind::Text),
            col("kind", "Kind", ColumnKind::Text),
            col("versions", "Versions", ColumnKind::Text),
            col("scope", "Scope", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let spec = |p: &str| text(str_at(&obj.data, p).unwrap_or_default());
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            spec("/spec/group"),
            spec("/spec/names/kind"),
            text(versions_text(obj)),
            spec("/spec/scope"),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let data = &obj.data;
        let s = |p: &str| str_at(data, p).unwrap_or_default().to_owned();
        let join = |p: &str| {
            arr_at(data, p)
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut fields = vec![
            Field::text("Group", or_none(s("/spec/group"))),
            Field::text("Kind", or_none(s("/spec/names/kind"))),
            Field::text(
                "Names",
                format!(
                    "{} (plural) · {} (singular)",
                    s("/spec/names/plural"),
                    s("/spec/names/singular")
                ),
            ),
            Field::text("Scope", or_none(s("/spec/scope"))),
        ];
        let short = join("/spec/names/shortNames");
        if !short.is_empty() {
            fields.push(Field::text("Short names", short));
        }
        let categories = join("/spec/names/categories");
        if !categories.is_empty() {
            fields.push(Field::text("Categories", categories));
        }
        fields.push(Field::text(
            "Stored versions",
            or_none(join("/status/storedVersions")),
        ));
        let strategy = str_at(data, "/spec/conversion/strategy").unwrap_or("None");
        match conversion_service(obj) {
            Some((service, label)) => {
                fields.push(Field::object_link("Conversion webhook", label, service));
            }
            None => match str_at(data, "/spec/conversion/webhook/clientConfig/url") {
                Some(url) => fields.push(Field::text("Conversion webhook", url)),
                None => fields.push(Field::text("Conversion", strategy)),
            },
        }
        if let Some(objects) = objects_ref(obj) {
            let plural = s("/spec/names/plural");
            fields.push(Field::phrase_link(
                "Objects",
                msg!("crd-show-objects", plural = or_none(plural)),
                objects,
            ));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        let rows: Vec<Vec<String>> = versions(obj)
            .iter()
            .map(|v| {
                vec![
                    str_at(v, "/name").unwrap_or_default().to_owned(),
                    yes_no(flag(v, "served")),
                    yes_no(flag(v, "storage")),
                    schema_text(v),
                    version_extras(v),
                ]
            })
            .collect();
        if !rows.is_empty() {
            sections.push(section(
                "Versions",
                SectionBody::Table(Table::new(
                    &["Version", "Served", "Storage", "Schema", "Extras"],
                    rows,
                )),
            ));
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
        run_delete::<CustomResourceDefinition>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let extra = conversion_service(obj)
            .map(|(service, _)| references(service))
            .into_iter()
            .collect();
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        crd_health(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, section as body};

    fn crd(conditions: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "widgets.example.com"},
            "spec": {
                "group": "example.com",
                "names": {"kind": "Widget", "plural": "widgets", "singular": "widget",
                          "shortNames": ["wd"], "categories": ["all"]},
                "scope": "Namespaced",
                "conversion": {"strategy": "Webhook", "webhook": {"clientConfig": {
                    "service": {"namespace": "infra", "name": "widget-webhook", "path": "/convert"}}}},
                "versions": [
                    {"name": "v1", "served": true, "storage": true,
                     "schema": {"openAPIV3Schema": {"type": "object", "properties": {
                        "apiVersion": {"type": "string"},
                        "spec": {"type": "object", "properties": {"size": {"type": "integer"}, "color": {"type": "string"}}},
                        "status": {"type": "object", "x-kubernetes-preserve-unknown-fields": true}
                     }}},
                     "subresources": {"status": {}},
                     "additionalPrinterColumns": [{"name": "Size", "type": "integer", "jsonPath": ".spec.size"}]},
                    {"name": "v1beta1", "served": true, "storage": false, "deprecated": true,
                     "deprecationWarning": "use v1",
                     "schema": {"openAPIV3Schema": {"type": "object", "x-kubernetes-preserve-unknown-fields": true}}},
                    {"name": "v1alpha1", "served": false, "storage": false}
                ]
            },
            "status": {"storedVersions": ["v1beta1", "v1"], "conditions": conditions}
        }))
    }

    fn established() -> Value {
        json!([
            {"type": "NamesAccepted", "status": "True", "reason": "NoConflicts"},
            {"type": "Established", "status": "True", "reason": "InitialNamesAccepted"}
        ])
    }

    #[test]
    fn crd_cells() {
        let p = CustomResourceDefinitionPlugin;
        let o = crd(established());
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[..5],
            [
                text("widgets.example.com"),
                text("example.com"),
                text("Widget"),
                text("v1 (storage), v1beta1 (deprecated), v1alpha1 (not served)"),
                text("Namespaced"),
            ]
        );
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Healthy, "Established")
        );
    }

    #[test]
    fn crd_details_versions_and_objects_link() {
        let p = CustomResourceDefinitionPlugin;
        let o = crd(established());
        let sections = p.details(&o);
        let SectionBody::Fields(fields) = body(&sections, OVERVIEW) else {
            panic!("overview is not fields");
        };
        let field = |key: &str| {
            fields
                .iter()
                .find(|f| f.key == key)
                .unwrap_or_else(|| panic!("{key} missing"))
        };
        assert_eq!(
            field("Names").value.text(),
            "widgets (plural) · widget (singular)"
        );
        assert_eq!(field("Short names").value.text(), "wd");
        assert_eq!(field("Stored versions").value.text(), "v1beta1, v1");
        assert_eq!(
            field("Conversion webhook").value,
            crate::plugin::FieldValue::Link {
                label: "infra/widget-webhook:443/convert".into(),
                target: object_ref("v1", "Service", "widget-webhook", Some("infra")),
                phrase: None,
            }
        );
        // The objects link has no name: it opens the list of widgets.
        let crate::plugin::FieldValue::Link { label, target, .. } = &field("Objects").value else {
            panic!("objects is not a link");
        };
        assert_eq!(label, "Show widgets");
        assert_eq!(target, &object_ref("example.com/v1", "Widget", "", None));
        let SectionBody::Table(table) = body(&sections, "Versions") else {
            panic!("versions is not a table");
        };
        assert_eq!(table.columns[0], "Version");
        assert_eq!(
            table.rows[0],
            [
                "v1",
                "Yes",
                "Yes",
                "spec (2 fields), status (any fields)",
                "subresources status · 1 printer columns"
            ]
        );
        assert_eq!(
            table.rows[1],
            ["v1beta1", "Yes", "No", "any fields", "deprecated: use v1"]
        );
        assert_eq!(table.rows[2], ["v1alpha1", "No", "No", "<none>", ""]);
        let SectionBody::Conditions(conditions) = body(&sections, "Conditions") else {
            panic!("no conditions");
        };
        assert!(conditions.iter().all(|c| c.is_good() == Some(true)));
        let targets: Vec<_> = p.relations(&o).into_iter().map(|r| r.target.name).collect();
        assert_eq!(targets, ["widget-webhook"]);
    }

    #[test]
    fn crd_health_states() {
        let p = CustomResourceDefinitionPlugin;
        let clash = crd(json!([
            {"type": "NamesAccepted", "status": "False", "reason": "MultipleNamesConflict",
             "message": "\"wd\" is already in use"},
            {"type": "Established", "status": "False"}
        ]));
        assert_eq!(
            p.health(&clash),
            Health::new(HealthStatus::Degraded, "\"wd\" is already in use")
        );
        let non_structural = crd(json!([
            {"type": "Established", "status": "True"},
            {"type": "NonStructuralSchema", "status": "True", "reason": "Violations"}
        ]));
        assert_eq!(
            p.health(&non_structural),
            Health::new(HealthStatus::Degraded, "Violations")
        );
        let SectionBody::Conditions(c) = body(&p.details(&non_structural), "Conditions").clone()
        else {
            panic!()
        };
        assert_eq!(c[1].is_good(), Some(false));
        assert_eq!(p.health(&crd(json!([]))).status, HealthStatus::Progressing);
    }
}
