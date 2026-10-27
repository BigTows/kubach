//! The editable view of a live object.
//!
//! Like `kubectl edit`, minus what the user cannot or should not change:
//! - `status` is dropped: apply to the main resource ignores it.
//! - `metadata.managedFields` and the `last-applied-configuration`
//!   annotation are dropped: bookkeeping, large and noisy.
//! - `uid`, `selfLink`, `creationTimestamp`, `generation`,
//!   `deletionTimestamp` and `deletionGracePeriodSeconds` are dropped:
//!   read-only, set by the server.
//! - `resourceVersion` is dropped too. Sent with an apply it is an
//!   optimistic lock, but any status update bumps it (a fresh Deployment
//!   fails that way within a second), so edits would be rejected for
//!   changes the user never touched. Server-side apply already catches
//!   concurrent edits at field level: if another manager changed a field
//!   the edit carries with an older value, the apply is a conflict. A
//!   manifest that sets `resourceVersion` explicitly still gets the lock.
//!
//! Keys are ordered `apiVersion`, `kind`, `metadata`, then the rest as the
//! server sent them; metadata keys in a fixed, readable order.

use kube::api::DynamicObject;
use serde_json::{Map, Value};

use crate::secrets::mask::LAST_APPLIED;

const READ_ONLY_METADATA: &[&str] = &[
    "managedFields",
    "uid",
    "selfLink",
    "creationTimestamp",
    "generation",
    "deletionTimestamp",
    "deletionGracePeriodSeconds",
    "resourceVersion",
];

const METADATA_ORDER: &[&str] = &[
    "name",
    "generateName",
    "namespace",
    "labels",
    "annotations",
    "ownerReferences",
    "finalizers",
];

/// Editable view of a live object (see module docs).
pub fn edit_view(object: &DynamicObject) -> Value {
    let mut value = serde_json::to_value(object).unwrap_or(Value::Null);
    strip_for_edit(&mut value);
    value
}

/// Editable view as YAML.
pub fn edit_yaml(object: &DynamicObject) -> String {
    super::yaml::to_yaml(&edit_view(object))
}

/// Applies the edit-view rules to a document in place.
pub fn strip_for_edit(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("status");
    if let Some(Value::Object(metadata)) = object.get_mut("metadata") {
        for key in READ_ONLY_METADATA {
            metadata.remove(*key);
        }
        if let Some(Value::Object(annotations)) = metadata.get_mut("annotations") {
            annotations.remove(LAST_APPLIED);
        }
        drop_empty(metadata, "annotations");
        drop_empty(metadata, "labels");
        reorder(metadata, METADATA_ORDER);
    }
    reorder(object, &["apiVersion", "kind", "metadata"]);
}

fn drop_empty(map: &mut Map<String, Value>, key: &str) {
    if map
        .get(key)
        .is_some_and(|value| value.as_object().is_some_and(Map::is_empty) || value.is_null())
    {
        map.remove(key);
    }
}

/// Moves `first` keys to the front in that order, keeping the rest as is.
fn reorder(map: &mut Map<String, Value>, first: &[&str]) {
    let mut ordered = Map::with_capacity(map.len());
    for key in first {
        if let Some(value) = map.remove(*key) {
            ordered.insert((*key).to_string(), value);
        }
    }
    ordered.append(map);
    *map = ordered;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live() -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "annotations": {
                    "deployment.kubernetes.io/revision": "1",
                    LAST_APPLIED: "{}"
                },
                "creationTimestamp": "2026-10-01T00:00:00Z",
                "generation": 3,
                "labels": {"app": "web"},
                "managedFields": [{"manager": "kubectl", "operation": "Apply"}],
                "name": "web",
                "namespace": "default",
                "resourceVersion": "123",
                "uid": "abc"
            },
            "spec": {"replicas": 2},
            "status": {"replicas": 2}
        }))
        .unwrap()
    }

    #[test]
    fn strips_noise_and_orders_keys() {
        let view = edit_view(&live());
        assert_eq!(
            view,
            json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {
                    "name": "web",
                    "namespace": "default",
                    "labels": {"app": "web"},
                    "annotations": {"deployment.kubernetes.io/revision": "1"}
                },
                "spec": {"replicas": 2}
            })
        );
        let keys: Vec<_> = view["metadata"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["name", "namespace", "labels", "annotations"]);
        assert!(edit_yaml(&live()).starts_with("apiVersion: apps/v1\nkind: Deployment\n"));
    }

    #[test]
    fn drops_emptied_annotations() {
        let mut value = json!({
            "metadata": {"name": "x", "resourceVersion": "1",
                         "annotations": {LAST_APPLIED: "{}"}}
        });
        strip_for_edit(&mut value);
        assert_eq!(value, json!({"metadata": {"name": "x"}}));
    }
}
