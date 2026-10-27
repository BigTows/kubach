//! ConfigMap and Secret plugins. Values are never returned: only key names,
//! types and byte sizes.

use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::{
    DELETE_ACTION, action, col, creation, decode, delete, fallback_cells, kv, name_ns_cells,
    name_ns_columns, pair, push_kv, relations_with, text, unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Brief, Cell, Column, ColumnKind, DetailSection,
    GenericPlugin, Health, ObjectRef, PluginError, Relation, RelationKind, ResourcePlugin,
    SectionBody,
};

pub struct ConfigMapPlugin;
pub struct SecretPlugin;

/// Annotation that holds the full applied manifest, including Secret data.
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";
const HIDDEN: &str = "(hidden)";

fn size(bytes: usize) -> String {
    if bytes == 1 {
        "1 byte".into()
    } else {
        format!("{bytes} bytes")
    }
}

/// Decoded length of a base64 string, computed without decoding it.
fn base64_len(s: &str) -> usize {
    let s = s.trim();
    let padding = s.chars().rev().take_while(|c| *c == '=').count();
    (s.len() / 4 * 3).saturating_sub(padding)
}

/// Key names and byte sizes of `data`/`binaryData`-like maps in raw JSON.
fn raw_sizes(obj: &DynamicObject, field: &str, base64: bool) -> Vec<(String, usize)> {
    obj.data
        .get(field)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            let s = v.as_str().unwrap_or_default();
            (k.clone(), if base64 { base64_len(s) } else { s.len() })
        })
        .collect()
}

/// A brief without a health: only the number of keys, `3 keys`, the state
/// line of objects that have no status (#275).
fn keys_brief(count: usize) -> Brief {
    Brief::phrased(
        Health::unknown(),
        msg!("brief-keys", count = count),
        Vec::new(),
    )
}

fn configmap_keys(obj: &DynamicObject) -> usize {
    ["data", "binaryData"]
        .iter()
        .filter_map(|field| obj.data.get(*field).and_then(Value::as_object))
        .map(|map| map.len())
        .sum()
}

fn size_pairs(sizes: Vec<(String, usize)>) -> Vec<(String, String)> {
    sizes.into_iter().map(|(k, n)| (k, size(n))).collect()
}

/// Generic metadata sections with annotation values that may embed secret
/// data replaced by a placeholder.
fn masked_metadata(obj: &DynamicObject) -> Vec<DetailSection> {
    let mut sections = GenericPlugin.details(obj);
    for s in &mut sections {
        if s.title == "Annotations"
            && let SectionBody::KeyValues(pairs) = &mut s.body
        {
            for (k, v) in pairs.iter_mut() {
                if k == LAST_APPLIED {
                    *v = HIDDEN.into();
                }
            }
        }
    }
    sections
}

fn delete_action<'a, K>(
    client: &'a Client,
    obj: &'a DynamicObject,
    action_id: &'a ActionId,
) -> BoxFuture<'a, Result<(), PluginError>>
where
    K: kube::Resource<DynamicType = ()> + 'a,
{
    Box::pin(async move {
        match *action_id {
            action::DELETE => delete::<K>(client, obj).await,
            _ => Err(unknown(action_id)),
        }
    })
}

// ----------------------------------------------------------------- ConfigMap

impl ResourcePlugin for ConfigMapPlugin {
    fn name(&self) -> &'static str {
        "configmap"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "ConfigMap")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("data", "Data", ColumnKind::Number),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(cm) = decode::<ConfigMap>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let count = cm.data.as_ref().map_or(0, |d| d.len())
            + cm.binary_data.as_ref().map_or(0, |d| d.len());
        let mut cells = name_ns_cells(obj);
        cells.extend([Cell::Number(count as i64), Cell::Age(creation(obj))]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(cm) = decode::<ConfigMap>(obj) else {
            return GenericPlugin.details(obj);
        };
        let mut sections = vec![kv(
            "Overview",
            vec![pair("Immutable", (cm.immutable == Some(true)).to_string())],
        )];
        push_kv(
            &mut sections,
            "Data",
            size_pairs(
                cm.data
                    .iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), v.len()))
                    .collect(),
            ),
        );
        push_kv(
            &mut sections,
            "Binary data",
            size_pairs(
                cm.binary_data
                    .iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), v.0.len()))
                    .collect(),
            ),
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
        delete_action::<ConfigMap>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::unknown()
    }

    /// ConfigMaps have no status: the number of keys instead of "Unknown".
    fn brief(&self, obj: &DynamicObject) -> Brief {
        keys_brief(configmap_keys(obj))
    }
}

// -------------------------------------------------------------------- Secret

/// Key names and decoded byte sizes; values are never read beyond length.
fn secret_sizes(obj: &DynamicObject) -> Vec<(String, usize)> {
    match decode::<Secret>(obj) {
        Some(s) => {
            let mut sizes: Vec<(String, usize)> = s
                .data
                .iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.0.len()))
                .collect();
            for (k, v) in s.string_data.iter().flatten() {
                if !sizes.iter().any(|(key, _)| key == k) {
                    sizes.push((k.clone(), v.len()));
                }
            }
            sizes
        }
        None => raw_sizes(obj, "data", true),
    }
}

fn secret_type(obj: &DynamicObject) -> String {
    obj.data
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque")
        .to_owned()
}

impl ResourcePlugin for SecretPlugin {
    fn name(&self) -> &'static str {
        "secret"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "Secret")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("type", "Type", ColumnKind::Text),
            col("data", "Data", ColumnKind::Number),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        if decode::<Secret>(obj).is_none() {
            return fallback_cells(obj, &self.columns());
        }
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(secret_type(obj)),
            Cell::Number(secret_sizes(obj).len() as i64),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    /// Never includes values: keys with byte sizes, and the
    /// last-applied-configuration annotation (which embeds the data) is hidden.
    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let immutable = obj.data.get("immutable").and_then(Value::as_bool) == Some(true);
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Type", secret_type(obj)),
                pair("Immutable", immutable.to_string()),
            ],
        )];
        push_kv(&mut sections, "Data", size_pairs(secret_sizes(obj)));
        sections.extend(masked_metadata(obj));
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
        delete_action::<Secret>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let sa = obj
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("kubernetes.io/service-account.name"))
            .filter(|_| secret_type(obj) == "kubernetes.io/service-account-token")
            .map(|name| Relation {
                kind: RelationKind::References,
                target: ObjectRef {
                    api_version: "v1".into(),
                    kind: "ServiceAccount".into(),
                    name: name.clone(),
                    namespace: obj.metadata.namespace.clone(),
                },
            });
        relations_with(obj, sa.into_iter().collect())
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::unknown()
    }

    /// Secrets have no status: the number of keys instead of "Unknown".
    fn brief(&self, obj: &DynamicObject) -> Brief {
        keys_brief(secret_sizes(obj).len())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs};

    // "c3VwZXItc2VjcmV0LXZhbHVl" is base64 of "super-secret-value".
    const SECRET_B64: &str = "c3VwZXItc2VjcmV0LXZhbHVl";
    const SECRET_PLAIN: &str = "super-secret-value";

    fn secret() -> DynamicObject {
        let last_applied = json!({
            "apiVersion": "v1", "kind": "Secret",
            "data": {"password": SECRET_B64}
        })
        .to_string();
        obj(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {
                "name": "db", "namespace": "default",
                "annotations": {
                    LAST_APPLIED: last_applied,
                    "kubernetes.io/service-account.name": "builder",
                    "team": "core"
                }
            },
            "type": "kubernetes.io/service-account-token",
            "data": {"password": SECRET_B64, "empty": ""}
        }))
    }

    /// Everything the plugin returns, rendered for leak checks.
    fn everything(p: &dyn ResourcePlugin, o: &DynamicObject) -> String {
        format!(
            "{:?}{:?}{:?}{:?}{:?}{:?}",
            p.cells(o),
            p.details(o),
            p.relations(o),
            p.selector_relations(o),
            p.health(o),
            p.actions_for(o)
        )
    }

    #[test]
    fn secret_values_are_masked() {
        let p = SecretPlugin;
        let o = secret();
        let all = everything(&p, &o);
        assert!(!all.contains(SECRET_B64), "base64 value leaked: {all}");
        assert!(!all.contains(SECRET_PLAIN), "decoded value leaked: {all}");

        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Data"),
            [pair("empty", "0 bytes"), pair("password", "18 bytes")]
        );
        let annotations = pairs(&sections, "Annotations");
        assert!(annotations.contains(&pair(LAST_APPLIED, HIDDEN)));
        assert!(annotations.contains(&pair("team", "core")));
    }

    #[test]
    fn undecodable_secret_is_masked_too() {
        let mut o = secret();
        o.data["data"] = json!({"password": SECRET_B64, "bad": 42});
        let p = SecretPlugin;
        let all = everything(&p, &o);
        assert!(!all.contains(SECRET_B64));
        assert!(!all.contains(SECRET_PLAIN));
        assert!(pairs(&p.details(&o), "Data").contains(&pair("password", "18 bytes")));
        assert_eq!(p.cells(&o).len(), p.columns().len());
    }

    #[test]
    fn secret_cells_and_relations() {
        let p = SecretPlugin;
        let o = secret();
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("kubernetes.io/service-account-token"));
        assert_eq!(cells[3], Cell::Number(2));
        assert_eq!(p.relations(&o)[0].target.name, "builder");
    }

    #[test]
    fn base64_length() {
        assert_eq!(base64_len(SECRET_B64), SECRET_PLAIN.len());
        assert_eq!(base64_len("YQ=="), 1);
        assert_eq!(base64_len("YWI="), 2);
        assert_eq!(base64_len(""), 0);
    }

    #[test]
    fn configmap_keys_and_sizes() {
        let o = obj(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "app", "namespace": "default"},
            "data": {"app.conf": "listen 80;", "x": "y"},
            "binaryData": {"logo.png": "iVBORw0KGgo="}
        }));
        let p = ConfigMapPlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], Cell::Number(3));
        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Data"),
            [pair("app.conf", "10 bytes"), pair("x", "1 byte")]
        );
        assert_eq!(
            pairs(&sections, "Binary data"),
            [pair("logo.png", "8 bytes")]
        );
        assert!(!format!("{sections:?}").contains("listen 80"));
        let brief = p.brief(&o);
        assert_eq!(brief.note, "3 keys");
        assert_eq!(brief.health, Health::unknown());
        let empty = obj(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "e", "namespace": "default"}
        }));
        assert_eq!(p.brief(&empty).note, "0 keys");
        assert_eq!(SecretPlugin.brief(&secret()).note, "2 keys");
    }
}
