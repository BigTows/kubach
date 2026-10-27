//! ReferenceGrant (Gateway API): which objects in other namespaces may
//! refer to objects in this one. A CRD; delete goes through discovery.

use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::gateway::{GROUP, arr_at, delete_action, str_at};
use super::{
    DELETE_ACTION, OVERVIEW, col, creation, kv, name_ns_cells, name_ns_columns, or_none, pair,
    section, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection,
    GenericPlugin, Health, HealthStatus, PluginError, Relation, ResourcePlugin, SectionBody, Table,
};

pub struct ReferenceGrantPlugin;

/// `core` for the core group.
fn group_of(v: &Value) -> String {
    match str_at(v, "/group").unwrap_or_default() {
        "" => "core".into(),
        g => g.into(),
    }
}

fn kind_of(v: &Value) -> &str {
    str_at(v, "/kind").unwrap_or_default()
}

fn from(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/spec/from")
}

fn to(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/spec/to")
}

/// `HTTPRoute in web, Gateway in infra`.
fn from_text(obj: &DynamicObject) -> String {
    from(obj)
        .iter()
        .map(|f| {
            format!(
                "{} in {}",
                kind_of(f),
                str_at(f, "/namespace").unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Service, Secret/wildcard-tls`.
fn to_text(obj: &DynamicObject) -> String {
    to(obj)
        .iter()
        .map(|t| match str_at(t, "/name") {
            Some(name) => format!("{}/{name}", kind_of(t)),
            None => kind_of(t).to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl ResourcePlugin for ReferenceGrantPlugin {
    fn name(&self) -> &'static str {
        "referencegrant"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1beta1", "ReferenceGrant")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("from", "From", ColumnKind::Text),
            col("to", "To", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(from_text(obj)),
            text(to_text(obj)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let mut sections = vec![kv(
            OVERVIEW,
            vec![
                pair("From", or_none(from_text(obj))),
                pair("To", or_none(to_text(obj))),
            ],
        )];
        let from_rows: Vec<Vec<String>> = from(obj)
            .iter()
            .map(|f| {
                vec![
                    group_of(f),
                    kind_of(f).to_owned(),
                    str_at(f, "/namespace").unwrap_or_default().to_owned(),
                ]
            })
            .collect();
        if !from_rows.is_empty() {
            sections.push(section(
                "From",
                SectionBody::Table(Table::new(&["Group", "Kind", "Namespace"], from_rows)),
            ));
        }
        let ns = obj.metadata.namespace.clone().unwrap_or_default();
        let to_rows: Vec<Vec<String>> = to(obj)
            .iter()
            .map(|t| {
                vec![
                    group_of(t),
                    kind_of(t).to_owned(),
                    str_at(t, "/name").unwrap_or("<any>").to_owned(),
                    ns.clone(),
                ]
            })
            .collect();
        if !to_rows.is_empty() {
            sections.push(section(
                "To",
                SectionBody::Table(Table::new(&["Group", "Kind", "Name", "Namespace"], to_rows)),
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
        delete_action(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    /// A grant has no status: present means in effect.
    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::phrase(HealthStatus::Healthy, msg!("state-active"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    fn grant() -> DynamicObject {
        obj(json!({
            "apiVersion": "gateway.networking.k8s.io/v1beta1", "kind": "ReferenceGrant",
            "metadata": {"name": "routes-to-backends", "namespace": "backends"},
            "spec": {
                "from": [
                    {"group": "gateway.networking.k8s.io", "kind": "HTTPRoute", "namespace": "web"},
                    {"group": "gateway.networking.k8s.io", "kind": "Gateway", "namespace": "infra"}
                ],
                "to": [
                    {"group": "", "kind": "Service"},
                    {"group": "", "kind": "Secret", "name": "wildcard-tls"}
                ]
            }
        }))
    }

    #[test]
    fn reference_grant_cells_and_details() {
        let p = ReferenceGrantPlugin;
        let o = grant();
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[2..4],
            [
                text("HTTPRoute in web, Gateway in infra"),
                text("Service, Secret/wildcard-tls")
            ]
        );
        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, OVERVIEW),
            [
                pair("From", "HTTPRoute in web, Gateway in infra"),
                pair("To", "Service, Secret/wildcard-tls")
            ]
        );
        let SectionBody::Table(from) = body(&sections, "From") else {
            panic!()
        };
        assert_eq!(
            from.rows[0],
            ["gateway.networking.k8s.io", "HTTPRoute", "web"]
        );
        let SectionBody::Table(to) = body(&sections, "To") else {
            panic!()
        };
        assert_eq!(
            to.rows,
            [
                ["core", "Service", "<any>", "backends"],
                ["core", "Secret", "wildcard-tls", "backends"]
            ]
        );
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
    }
}
