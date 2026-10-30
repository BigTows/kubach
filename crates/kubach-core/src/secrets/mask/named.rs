//! Values hidden by the name they are stored under, outside Secrets.
//!
//! A structural walk, not a text regex: in YAML `name:` and `value:` of an
//! environment variable sit on different lines, so a pattern over the text
//! never sees them together.

use serde_json::{Map, Value};

/// Words that mark a name as holding a credential when they are a whole
/// part of it (`DB_PASS`, `tls.key`, `apiKey`), so `PASSAGE`, `MONKEY` or
/// `KEYCLOAK_URL` stay visible.
const SENSITIVE_WORDS: [&str; 24] = [
    "PASS",
    "PASSWD",
    "PASSWORD",
    "PASSWORDS",
    "PASSPHRASE",
    "PWD",
    "SECRET",
    "SECRETS",
    "TOKEN",
    "TOKENS",
    "KEY",
    "KEYS",
    "APIKEY",
    "AUTH",
    "CRED",
    "CREDS",
    "CREDENTIAL",
    "CREDENTIALS",
    "DSN",
    "PRIVATE",
    "SALT",
    "CONNSTR",
    "COOKIE",
    "SIGNATURE",
];

/// Long words that mark a name even glued to others (`DBPASSWORD`,
/// `CLIENTSECRET`).
const SENSITIVE_PARTS: [&str; 5] = ["PASSWORD", "PASSWD", "SECRET", "TOKEN", "CREDENTIAL"];

/// Neighbouring parts that together mark a connection string (`CONN_STR`,
/// `connectionString`).
const SENSITIVE_PAIRS: [(&str, &str); 3] = [
    ("CONN", "STR"),
    ("CONNECTION", "STRING"),
    ("CONNECTION", "STR"),
];

/// Splits a name into upper-cased parts at `_`, `-`, `.`, `/` and other
/// non-alphanumerics, and at camelCase humps (`apiKey` -> `API`, `KEY`).
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower {
            parts.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        current.extend(c.to_uppercase());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Whether a key or variable name looks like it holds a credential:
/// `PASS`, `PASSWORD`, `SECRET`, `TOKEN`, `KEY`, `AUTH`, `CRED`, `DSN`,
/// `PRIVATE`, `CONN_STR`, ... as a whole part of the name, ignoring case
/// (`DB_PASS`, `tls.key`, `apiKey`, `x-auth-token`). The long words
/// (`PASSWORD`, `SECRET`, `TOKEN`, ...) count inside a part too
/// (`DBPASSWORD`).
pub fn is_sensitive_name(name: &str) -> bool {
    let parts = name_parts(name);
    parts.iter().any(|part| {
        SENSITIVE_WORDS.contains(&part.as_str())
            || SENSITIVE_PARTS.iter().any(|word| part.contains(word))
    }) || parts.windows(2).any(|pair| {
        SENSITIVE_PAIRS
            .iter()
            .any(|(a, b)| pair[0] == *a && pair[1] == *b)
    })
}

/// Whether the last word of a sensitive name ([`is_sensitive_name`]) is the
/// sensitive one: `dbPassword`, `apiToken`, `tls.key`, but not `tokenTTL`
/// or `passwordMinLength`, which name a setting of a credential.
pub fn ends_with_sensitive_word(name: &str) -> bool {
    let parts = name_parts(name);
    parts.last().is_some_and(|last| is_sensitive_name(last))
        || parts.len() >= 2 && {
            let (a, b) = (&parts[parts.len() - 2], &parts[parts.len() - 1]);
            SENSITIVE_PAIRS.iter().any(|(x, y)| a == x && b == y)
        }
}

/// Masks values stored under a sensitive name ([`is_sensitive_name`]) in an
/// object or a `List` of objects, whatever its kind; `replace` gives the
/// placeholder for a value:
/// - `env[].value` anywhere in the object, by the variable's `name`: every
///   pod template (Pod, Deployment, StatefulSet, DaemonSet, ReplicaSet, Job,
///   CronJob, ControllerRevision `data`) and custom resources alike;
/// - annotations anywhere in the object (its own and its templates'), by key;
/// - ConfigMap `data` and `binaryData`, by key.
///
/// Secret values are not touched here: they are hidden whatever their name
/// (see the module docs of [`super`]).
pub fn mask_named_values(value: &mut Value, replace: &mut impl FnMut(&Value) -> Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let is_list = matches!(object.get("items"), Some(Value::Array(_)));
    if let Some(Value::Array(items)) = object.get_mut("items") {
        for item in items {
            mask_named_values(item, replace);
        }
    }
    if object.get("kind").and_then(Value::as_str) == Some("ConfigMap") {
        for field in ["data", "binaryData"] {
            if let Some(Value::Object(entries)) = object.get_mut(field) {
                mask_entries(entries, replace);
            }
        }
    }
    for (key, child) in object.iter_mut() {
        if !(is_list && key == "items") {
            walk(key, child, replace);
        }
    }
}

/// Masks the entries of a map whose key is sensitive.
fn mask_entries(entries: &mut Map<String, Value>, replace: &mut impl FnMut(&Value) -> Value) {
    for (key, entry) in entries.iter_mut() {
        if is_sensitive_name(key) && !entry.is_null() {
            *entry = replace(entry);
        }
    }
}

/// Finds `env` lists and `annotations` maps in `value`, stored under `key`.
fn walk(key: &str, value: &mut Value, replace: &mut impl FnMut(&Value) -> Value) {
    match value {
        Value::Array(items) if key == "env" => {
            for item in items {
                let sensitive = item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_sensitive_name);
                if let Some(entry) = item.get_mut("value")
                    && sensitive
                    && !entry.is_null()
                {
                    *entry = replace(entry);
                }
            }
        }
        Value::Object(entries) if key == "annotations" => mask_entries(entries, replace),
        Value::Object(entries) => {
            for (key, child) in entries.iter_mut() {
                walk(key, child, replace);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk("", item, replace);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ends_with_sensitive_word, is_sensitive_name, mask_named_values};

    #[test]
    fn sensitive_names() {
        for name in [
            "DB_PASS",
            "PASSWORD",
            "db_password",
            "DBPASSWORD",
            "PASS",
            "AUTH",
            "auth_header",
            "CONN_STR",
            "SQL_CONN_STR",
            "connectionString",
            "SENTRY_DSN",
            "tls.key",
            "apiKey",
            "AWS_SECRET_ACCESS_KEY",
            "x-auth-token",
            "GITHUB_TOKEN",
            "example.com/oauth-client-secret",
            "PRIVATE_KEY",
            "DB_CRED",
            "credentials.json",
        ] {
            assert!(is_sensitive_name(name), "{name}");
        }
        for name in [
            "PASSAGE",
            "MONKEY",
            "KEYCLOAK_URL",
            "LOG_LEVEL",
            "DB_HOST",
            "deployment.kubernetes.io/revision",
            "app.kubernetes.io/name",
            "AUTHOR",
            "",
        ] {
            assert!(!is_sensitive_name(name), "{name}");
        }
    }

    #[test]
    fn names_ending_in_a_sensitive_word() {
        for name in [
            "password",
            "dbPassword",
            "apiToken",
            "tls.key",
            "CONN_STR",
            "SECRET",
        ] {
            assert!(ends_with_sensitive_word(name), "{name}");
        }
        for name in [
            "tokenTTL",
            "passwordMinLength",
            "secretName2x",
            "replicas",
            "",
        ] {
            assert!(!ends_with_sensitive_word(name), "{name}");
        }
    }

    fn mask(value: &mut Value) {
        mask_named_values(value, &mut |_| json!("***"));
    }

    #[test]
    fn env_values_in_every_pod_template() {
        let env = || {
            json!([
                {"name": "DB_PASSWORD", "value": "s3cr3t"},
                {"name": "LOG_LEVEL", "value": "debug"},
                {"name": "API_TOKEN", "valueFrom": {"secretKeyRef": {"name": "t", "key": "k"}}},
            ])
        };
        let pod = || {
            json!({
                "containers": [{"name": "app", "env": env()}],
                "initContainers": [{"name": "i", "env": [{"name": "AUTH", "value": "x1y2"}]}],
            })
        };
        let template = || json!({"metadata": {}, "spec": pod()});
        let mut objects = vec![
            json!({"kind": "Pod", "spec": pod()}),
            json!({"kind": "Deployment", "spec": {"template": template()}}),
            json!({"kind": "StatefulSet", "spec": {"template": template()}}),
            json!({"kind": "DaemonSet", "spec": {"template": template()}}),
            json!({"kind": "ReplicaSet", "spec": {"template": template()}}),
            json!({"kind": "Job", "spec": {"template": template()}}),
            json!({"kind": "CronJob", "spec": {"jobTemplate": {"spec": {"template": template()}}}}),
            json!({"kind": "ControllerRevision", "data": {"spec": {"template": template()}}}),
            json!({"kind": "List", "items": [{"kind": "Deployment", "spec": {"template": template()}}]}),
        ];
        for object in &mut objects {
            mask(object);
            let text = object.to_string();
            assert!(!text.contains("s3cr3t") && !text.contains("x1y2"), "{text}");
            assert!(
                text.contains("debug") && text.contains("secretKeyRef"),
                "{text}"
            );
        }
    }

    #[test]
    fn config_map_values_by_key() {
        let mut config = json!({
            "kind": "ConfigMap",
            "data": {"DB_PASS": "hunter2", "DB_HOST": "db", "conn_str": "Server=x;Password=y"},
            "binaryData": {"tls.key": "LS0tLS1CRUdJTg=="},
        });
        mask(&mut config);
        assert_eq!(config["data"]["DB_PASS"], "***");
        assert_eq!(config["data"]["conn_str"], "***");
        assert_eq!(config["data"]["DB_HOST"], "db");
        assert_eq!(config["binaryData"]["tls.key"], "***");
        // `data` of other kinds is not a ConfigMap's.
        let mut other = json!({"kind": "Widget", "data": {"DB_PASS": "hunter2"}});
        mask(&mut other);
        assert_eq!(other["data"]["DB_PASS"], "hunter2");
    }

    #[test]
    fn annotations_by_key() {
        let mut deployment = json!({
            "kind": "Deployment",
            "metadata": {"annotations": {"example.com/api-token": "ghp_abc", "note": "hi"}},
            "spec": {"template": {"metadata": {"annotations": {"vault.io/secret": "v"}}}},
        });
        mask(&mut deployment);
        let annotations = &deployment["metadata"]["annotations"];
        assert_eq!(annotations["example.com/api-token"], "***");
        assert_eq!(annotations["note"], "hi");
        assert_eq!(
            deployment["spec"]["template"]["metadata"]["annotations"]["vault.io/secret"],
            "***"
        );
    }

    #[test]
    fn each_value_is_replaced_once() {
        let mut list = json!({"kind": "List", "items": [
            {"kind": "ConfigMap", "metadata": {"annotations": {"token": "a"}}, "data": {"PASS": "b"}},
        ]});
        let mut calls = 0;
        mask_named_values(&mut list, &mut |_| {
            calls += 1;
            json!("***")
        });
        assert_eq!(calls, 2);
    }
}
