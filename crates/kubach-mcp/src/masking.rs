//! Masking of everything the agent or the audit log sees.
//!
//! Objects: Secret values are always replaced (keys and sizes stay), and
//! `last-applied-configuration` and `managedFields` are stripped. Outside
//! Secrets, values under a credential-like name are replaced the same way:
//! `env[].value` in pod templates, ConfigMap `data`/`binaryData` and
//! annotations. Text (logs, exec output, rendered YAML): best-effort masking
//! of JWTs, PEM blocks and `password=`-style assignments. Text masking is not
//! a guarantee.
//!
//! What a Secret is, where its values live and which names are sensitive
//! comes from [`kubach_core::secrets::mask`]; text masking is
//! [`kubach_core::redact`]'s, the same as for error texts.

use std::borrow::Cow;

use kubach_core::apply::yaml;
use kubach_core::secrets::mask::{LAST_APPLIED, VALUE_FIELDS, is_secret_value, mask_named_values};
use serde_json::Value;

pub use kubach_core::redact::mask_text;

/// Replacement for masked values.
pub const MASK: &str = "***";

/// Masks an object or a list of objects in place.
pub fn mask_object(value: &mut Value) {
    mask_named_values(value, &mut |entry| {
        let size = match entry {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        };
        placeholder(size)
    });
    mask_secrets(value);
}

/// `*** (N bytes)`.
fn placeholder(size: usize) -> Value {
    Value::String(format!("{MASK} ({size} bytes)"))
}

/// Strips `managedFields` and last-applied from an object or a list, and
/// replaces every Secret value.
fn mask_secrets(value: &mut Value) {
    let is_secret = is_secret_value(value);
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(items)) = object.get_mut("items") {
        for item in items {
            mask_secrets(item);
        }
    }
    if let Some(metadata) = object.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("managedFields");
        if let Some(annotations) = metadata
            .get_mut("annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove(LAST_APPLIED);
        }
    }
    if is_secret {
        for field in VALUE_FIELDS {
            if let Some(Value::Object(entries)) = object.get_mut(field) {
                for entry in entries.values_mut() {
                    let size = match entry {
                        Value::String(s) if field == "data" => decoded_len(s),
                        Value::String(s) => s.len(),
                        _ => 0,
                    };
                    *entry = placeholder(size);
                }
            }
        }
    }
}

/// Approximate decoded size of a base64 string.
fn decoded_len(encoded: &str) -> usize {
    let padding = encoded.bytes().rev().take_while(|b| *b == b'=').count();
    (encoded.len() / 4 * 3).saturating_sub(padding)
}

/// Masks every string in a JSON value with [`mask_text`] (for audit args).
pub fn mask_json_strings(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Cow::Owned(masked) = mask_text(s) {
                *s = masked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_json_strings),
        Value::Object(map) => map.values_mut().for_each(mask_json_strings),
        _ => {}
    }
}

/// Masks the arguments of a tool call for the audit log.
///
/// A manifest (`yaml`, of `apply_manifest`) is parsed, every document goes
/// through [`mask_object`] and is written back as YAML, so Secret values and
/// values under credential-like names never reach the log, plain or base64.
/// A manifest that does not parse is replaced by its kinds, names and size,
/// without its text. Every string then goes through [`mask_text`].
pub fn mask_audit_args(args: &mut Value) {
    if let Some(Value::String(text)) = args.get_mut("yaml") {
        *text = masked_manifest(text);
    }
    // Helm values (#418): parsed and masked by key name, like a manifest.
    if let Some(Value::String(text)) = args.get_mut("values") {
        *text = crate::helm_tools::masked_values_yaml(text);
    }
    mask_json_strings(args);
}

/// The audit form of a manifest; see [`mask_audit_args`].
pub(crate) fn masked_manifest(text: &str) -> String {
    match yaml::parse(text) {
        Ok(manifests) => {
            let mut values: Vec<Value> = manifests.into_iter().map(|m| m.value).collect();
            values.iter_mut().for_each(mask_object);
            yaml::to_yaml_multi(&values)
        }
        Err(_) => {
            let objects = manifest_outline(text);
            format!(
                "<unparsed YAML, {} bytes{}{}>",
                text.len(),
                if objects.is_empty() { "" } else { ": " },
                objects.join(", ")
            )
        }
    }
}

/// `Kind/name` of each document of a manifest that does not parse, from
/// top-level `kind:` lines and `name:` lines under a top-level `metadata:`.
/// Only words that can be a kind or a name are kept, nothing else.
fn manifest_outline(text: &str) -> Vec<String> {
    fn word(value: &str) -> Option<&str> {
        let value = value.trim().trim_matches(['"', '\'']);
        (!value.is_empty()
            && value.len() <= 253
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.')))
        .then_some(value)
    }
    let mut objects = Vec::new();
    let (mut kind, mut name, mut in_metadata) = (None, None, false);
    let mut flush = |kind: &mut Option<&str>, name: &mut Option<&str>| {
        if kind.is_some() || name.is_some() {
            objects.push(format!("{}/{}", kind.unwrap_or("?"), name.unwrap_or("?")));
        }
        (*kind, *name) = (None, None);
    };
    for line in text.lines() {
        if line.starts_with("---") {
            flush(&mut kind, &mut name);
            in_metadata = false;
        } else if let Some(rest) = line.strip_prefix("kind:") {
            kind = word(rest);
        } else if line.starts_with("metadata:") {
            in_metadata = true;
        } else if !line.starts_with([' ', '\t']) && !line.trim().is_empty() {
            in_metadata = false;
        } else if in_metadata
            && name.is_none()
            && let Some(rest) = line.trim_start().strip_prefix("name:")
        {
            name = word(rest);
        }
    }
    flush(&mut kind, &mut name);
    objects
}

/// Cuts `text` to at most `max` bytes on a char boundary, noting the cut.
pub fn truncate(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return Cow::Borrowed(text);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!(
        "{}\n… (truncated, {} of {} bytes shown)",
        &text[..end],
        end,
        text.len()
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn secret_values_are_masked_keys_kept() {
        let mut secret = json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "db", "annotations": {LAST_APPLIED: "{\"data\":{\"pw\":\"aHVudGVyMg==\"}}", "keep": "me"}},
            "data": {"pw": "aHVudGVyMg=="},
            "stringData": {"token": "plain"},
        });
        mask_object(&mut secret);
        assert_eq!(secret["data"]["pw"], "*** (7 bytes)");
        assert_eq!(secret["stringData"]["token"], "*** (5 bytes)");
        assert!(
            secret["metadata"]["annotations"]
                .get(LAST_APPLIED)
                .is_none()
        );
        assert_eq!(secret["metadata"]["annotations"]["keep"], "me");
        assert!(!secret.to_string().contains("aHVudGVyMg"));
    }

    #[test]
    fn lists_are_masked() {
        let mut list = json!({"kind": "List", "items": [
            {"kind": "Secret", "metadata": {"name": "a"}, "data": {"k": "dmFsdWU="}},
            {"kind": "ConfigMap", "metadata": {"name": "b", "managedFields": [{}]}, "data": {"k": "v"}},
        ]});
        mask_object(&mut list);
        assert_eq!(list["items"][0]["data"]["k"], "*** (5 bytes)");
        assert_eq!(list["items"][1]["data"]["k"], "v");
        assert!(list["items"][1]["metadata"].get("managedFields").is_none());
    }

    #[test]
    fn last_applied_is_stripped_from_any_kind() {
        let mut deployment =
            json!({"kind": "Deployment", "metadata": {"annotations": {LAST_APPLIED: "{}"}}});
        mask_object(&mut deployment);
        assert_eq!(deployment["metadata"]["annotations"], json!({}));
    }

    #[test]
    fn text_masks_jwt_pem_and_assignments() {
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJzeXN0ZW0ifQ.c2lnbmF0dXJl";
        let text = format!(
            "token {jwt}\npassword=hunter2 user=bob\nDB_PASSWORD: s3cr3t\n\"apiKey\": \"abc123\"\n\
             Authorization: Bearer abcdefghijkl\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\n\
             postgres://app:topsecret@db:5432/x"
        );
        let masked = mask_text(&text);
        for leaked in [
            "hunter2",
            "s3cr3t",
            "abc123",
            "abcdefghijkl",
            "MIIEow",
            "topsecret",
            "c2lnbmF0dXJl",
        ] {
            assert!(!masked.contains(leaked), "{leaked} leaked in {masked}");
        }
        assert!(masked.contains("user=bob"));
        assert!(masked.contains("-----BEGIN RSA PRIVATE KEY----- *** (masked)"));
    }

    #[test]
    fn unterminated_pem_is_masked_to_the_end() {
        let masked = mask_text("x\n-----BEGIN PRIVATE KEY-----\nMIIabc\nmore");
        assert!(!masked.contains("MIIabc"));
    }

    #[test]
    fn plain_text_is_borrowed() {
        assert!(matches!(mask_text("all good here"), Cow::Borrowed(_)));
    }

    #[test]
    fn json_strings_are_masked() {
        let mut args = json!({"command": ["sh", "-c", "echo password=x1y2"], "n": 1});
        mask_json_strings(&mut args);
        assert_eq!(args["command"][2], "echo password=***");
    }

    #[test]
    fn named_values_get_sizes() {
        let mut config = json!({
            "kind": "ConfigMap",
            "metadata": {"annotations": {"example.com/token": "ghp_abc", LAST_APPLIED: "{}"}},
            "data": {"DB_PASS": "hunter2", "DB_HOST": "db"},
        });
        mask_object(&mut config);
        assert_eq!(config["data"]["DB_PASS"], "*** (7 bytes)");
        assert_eq!(config["data"]["DB_HOST"], "db");
        assert_eq!(
            config["metadata"]["annotations"],
            json!({"example.com/token": "*** (7 bytes)"})
        );
    }

    /// A Deployment as an agent writes it: `name` and `value` of a variable
    /// on different lines, so text masking alone misses it.
    const DEPLOYMENT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  annotations:
    example.com/api-token: ghp_annotation
spec:
  template:
    spec:
      containers:
      - name: app
        env:
        - name: DB_PASSWORD
          value: s3cr3t-env
        - name: LOG_LEVEL
          value: debug
";

    #[test]
    fn yaml_env_values_are_masked() {
        assert!(mask_text(DEPLOYMENT).contains("s3cr3t-env"));
        let mut args = json!({"yaml": DEPLOYMENT, "namespace": "dev"});
        mask_audit_args(&mut args);
        let text = args.to_string();
        assert!(!text.contains("s3cr3t-env"), "{text}");
        assert!(!text.contains("ghp_annotation"), "{text}");
        assert!(
            text.contains("debug") && text.contains("DB_PASSWORD"),
            "{text}"
        );
        assert_eq!(args["namespace"], "dev");
    }

    #[test]
    fn audit_never_holds_secret_values_in_any_document() {
        let yaml = format!(
            "\
apiVersion: v1
kind: Secret
metadata:
  name: tls
data:
  tls.key: aHVudGVyMg==
  ca: YWRtaW4=
stringData:
  plain: Zx9-plain
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: app
data:
  DB_PASS: cm-hunter
  DB_HOST: db.local
---
{DEPLOYMENT}"
        );
        let mut args = json!({"yaml": yaml});
        mask_audit_args(&mut args);
        let text = args.to_string();
        for leaked in [
            "aHVudGVyMg",
            "hunter2",
            "YWRtaW4",
            "admin",
            "Zx9-plain",
            "cm-hunter",
            "s3cr3t-env",
        ] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
        for kept in ["tls.key", "db.local", "kind: Deployment", "*** (7 bytes)"] {
            assert!(text.contains(kept), "{kept} not in {text}");
        }
    }

    #[test]
    fn unparsable_manifest_keeps_only_kind_name_and_size() {
        let yaml = "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\ndata:\n  pw: aHVudGVyMg==\n  bad: [\n";
        let mut args = json!({"yaml": yaml});
        mask_audit_args(&mut args);
        assert_eq!(
            args["yaml"],
            format!("<unparsed YAML, {} bytes: Secret/db>", yaml.len())
        );
        let mut args = json!({"yaml": "{{{"});
        mask_audit_args(&mut args);
        assert_eq!(args["yaml"], "<unparsed YAML, 3 bytes>");
    }

    #[test]
    fn truncation_is_char_safe() {
        let text = "ééééé";
        let cut = truncate(text, 3);
        assert!(cut.starts_with('é'));
        assert!(cut.contains("truncated"));
        assert_eq!(truncate("abc", 10), "abc");
    }
}
