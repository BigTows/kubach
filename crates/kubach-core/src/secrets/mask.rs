//! The one place that decides what is a Secret and how its values are hidden.
//!
//! Callers:
//! - the object cache: [`redact`], values are gone for good;
//! - diffs and reviews (Kustomize, the YAML editor): [`mask_for_diff`],
//!   values become a fingerprint so a change stays visible;
//! - the MCP server and its policy: [`is_core_secret`], [`is_secret_value`],
//!   [`VALUE_FIELDS`] and [`LAST_APPLIED`]; it has its own placeholder with
//!   sizes (`kubach_mcp::masking`).
//!
//! Everything that can carry values is covered, not only `data`: `stringData`
//! and the `last-applied-configuration` annotation, which holds a full copy of
//! the object as last applied.
//!
//! Outside Secrets, values are hidden by the name they are stored under
//! ([`is_sensitive_name`]): `env[].value` in any pod template, ConfigMap
//! `data`/`binaryData` and annotations ([`mask_named_values`]).

mod named;

pub use named::{is_sensitive_name, mask_named_values};

use std::hash::{BuildHasher, RandomState};
use std::sync::LazyLock;

use kube::api::DynamicObject;
use serde_json::Value;

/// Annotation `kubectl apply` writes with a copy of the whole object, Secret
/// values included.
pub const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

/// Fields of a Secret that hold values.
pub const VALUE_FIELDS: [&str; 2] = ["data", "stringData"];

/// Whether a resource type is the core `Secret` (empty group, kind `Secret`).
pub fn is_core_secret(group: &str, kind: &str) -> bool {
    group.is_empty() && kind == "Secret"
}

/// Whether a manifest or object looks like a Secret and must be masked.
///
/// Deliberately looser than [`is_core_secret`]: only `kind` is checked, so a
/// manifest with a missing or odd `apiVersion` is masked rather than leaked.
pub fn is_secret_value(value: &Value) -> bool {
    value.get("kind").and_then(Value::as_str) == Some("Secret")
}

/// Strips everything from a cached Secret that could reveal its values:
/// `data` values become `null` (keys are kept), `stringData` and the
/// last-applied annotation are dropped.
pub fn redact(object: &mut DynamicObject) {
    if let Some(Value::Object(data)) = object.data.get_mut("data") {
        for value in data.values_mut() {
            *value = Value::Null;
        }
    }
    if let Value::Object(fields) = &mut object.data {
        fields.remove("stringData");
    }
    if let Some(annotations) = &mut object.metadata.annotations {
        annotations.remove(LAST_APPLIED);
    }
}

/// Masks Secrets for a diff: every value under `data` and `stringData`
/// becomes `hidden-<fingerprint>`, so equal values look the same and changed
/// ones differ, while the values stay unreadable. The last-applied annotation
/// is removed. `before` is the previous state, if any; each side that is a
/// Secret ([`is_secret_value`]) is masked the same way, other objects are
/// left alone.
pub fn mask_for_diff(before: Option<&mut Value>, after: &mut Value) {
    for value in before.into_iter().chain(Some(after)) {
        if is_secret_value(value) {
            mask_one(value);
        }
    }
}

/// [`mask_for_diff`] of a single object, as a masked copy.
pub fn masked_copy(value: &Value) -> Value {
    let mut out = value.clone();
    mask_for_diff(None, &mut out);
    out
}

fn mask_one(value: &mut Value) {
    for field in VALUE_FIELDS {
        if let Some(Value::Object(entries)) = value.get_mut(field) {
            for entry in entries.values_mut() {
                *entry = Value::String(format!("hidden-{:08x}", fingerprint(entry)));
            }
        }
    }
    if let Some(annotations) = value
        .pointer_mut("/metadata/annotations")
        .and_then(Value::as_object_mut)
    {
        annotations.remove(LAST_APPLIED);
    }
}

/// Key of [`fingerprint`], random for each run of Kubach.
static FINGERPRINT_KEY: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// 32 bits of a keyed hash (SipHash with a per-run random key): equal for
/// equal values within a run, so a diff shows what changed. The key keeps a
/// short value (`true`, a PIN) from being looked up by its fingerprint in a
/// precomputed table or brute-forced from a screenshot.
fn fingerprint(value: &Value) -> u32 {
    let hash = match value {
        Value::String(text) => FINGERPRINT_KEY.hash_one(text.as_bytes()),
        Value::Null => FINGERPRINT_KEY.hash_one(b""),
        other => FINGERPRINT_KEY.hash_one(other.to_string().as_bytes()),
    };
    (hash >> 32) as u32 ^ hash as u32
}

#[cfg(test)]
mod tests {
    use kube::api::DynamicObject;
    use serde_json::{Value, json};

    use super::{
        LAST_APPLIED, is_core_secret, is_secret_value, mask_for_diff, masked_copy, redact,
    };

    const VALUES: [&str; 4] = ["aHVudGVyMg==", "YWRtaW4=", "s3cr3t-tok", "last-applied"];

    fn secret() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "db", "annotations": {
                LAST_APPLIED: "{\"data\":{\"password\":\"aHVudGVyMg==\"}}",
                "keep": "me",
            }},
            "data": {"password": "aHVudGVyMg==", "user": "YWRtaW4="},
            "stringData": {"token": "s3cr3t-tok"},
        })
    }

    fn assert_no_values(text: &str) {
        for leaked in VALUES {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
    }

    #[test]
    fn core_secret_predicate() {
        assert!(is_core_secret("", "Secret"));
        assert!(!is_core_secret("example.com", "Secret"));
        assert!(!is_core_secret("", "ConfigMap"));
        assert!(!is_core_secret("", "secret"));
    }

    #[test]
    fn secret_value_predicate_ignores_api_version() {
        assert!(is_secret_value(&secret()));
        assert!(is_secret_value(&json!({"kind": "Secret"})));
        assert!(is_secret_value(
            &json!({"apiVersion": "x/v1", "kind": "Secret"})
        ));
        assert!(!is_secret_value(&json!({"kind": "ConfigMap"})));
        assert!(!is_secret_value(&json!("Secret")));
    }

    #[test]
    fn redact_drops_values() {
        let mut object: DynamicObject = serde_json::from_value(secret()).unwrap();
        redact(&mut object);
        assert_no_values(&serde_json::to_string(&object).unwrap());
        assert_eq!(object.data["data"], json!({"password": null, "user": null}));
        assert!(object.data.get("stringData").is_none());
        assert_eq!(object.metadata.annotations.unwrap()["keep"], "me");
    }

    #[test]
    fn redact_tolerates_missing_fields() {
        let mut object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Secret", "metadata": {"name": "empty"},
        }))
        .unwrap();
        redact(&mut object);
        assert!(object.data.get("data").is_none());
    }

    #[test]
    fn diff_masks_with_fingerprints() {
        let before = secret();
        let masked = masked_copy(&before);
        assert_no_values(&masked.to_string());
        for (field, key) in [
            ("data", "password"),
            ("data", "user"),
            ("stringData", "token"),
        ] {
            let shown = masked[field][key].as_str().unwrap();
            assert!(shown.starts_with("hidden-"), "{shown}");
        }
        assert_eq!(masked["metadata"]["annotations"], json!({"keep": "me"}));
        assert_eq!(masked["metadata"]["name"], "db");
        // Stable within a run.
        assert_eq!(masked_copy(&before), masked);
    }

    #[test]
    fn diff_shows_which_values_changed() {
        let mut before = secret();
        let mut after = secret();
        after["data"]["password"] = json!("b3RoZXI=");
        after["data"]["added"] = json!("bmV3");
        mask_for_diff(Some(&mut before), &mut after);
        assert_ne!(after["data"]["password"], before["data"]["password"]);
        assert_eq!(after["data"]["user"], before["data"]["user"]);
        assert_eq!(after["stringData"]["token"], before["stringData"]["token"]);
        assert!(before["data"].get("added").is_none());
        let text = format!("{before}{after}");
        assert_no_values(&text);
        assert!(
            !text.contains("b3RoZXI=") && !text.contains("bmV3"),
            "{text}"
        );
    }

    #[test]
    fn diff_masks_each_side_on_its_own() {
        // A new Secret: nothing before.
        let mut after = secret();
        mask_for_diff(None, &mut after);
        assert_no_values(&after.to_string());
        // Only the previous state is a Secret: it is still masked.
        let mut before = secret();
        let mut config = json!({"kind": "ConfigMap", "data": {"a": "b"}});
        mask_for_diff(Some(&mut before), &mut config);
        assert_no_values(&before.to_string());
        assert_eq!(config["data"]["a"], "b");
    }

    #[test]
    fn non_string_values_are_masked() {
        let masked = masked_copy(&json!({
            "kind": "Secret", "data": {"n": 42, "empty": null, "nested": {"x": "s3cr3t-tok"}},
        }));
        assert_no_values(&masked.to_string());
        for key in ["n", "empty", "nested"] {
            assert!(masked["data"][key].as_str().unwrap().starts_with("hidden-"));
        }
    }

    #[test]
    fn other_kinds_are_untouched() {
        let config = json!({
            "kind": "ConfigMap",
            "metadata": {"annotations": {LAST_APPLIED: "{}"}},
            "data": {"a": "b"},
        });
        assert_eq!(masked_copy(&config), config);
    }
}
