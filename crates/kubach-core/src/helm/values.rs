//! Computed values: chart defaults coalesced with user values, following
//! Helm 3 `chartutil.CoalesceValues` (what `helm get values --all` prints).
//!
//! Rules: user values win; maps are merged recursively; a user `null`
//! deletes the key (and its default); on a map/non-map conflict the user value
//! is kept and a warning is reported.
//!
//! Limits: the stored release has no subcharts (Helm does not serialize
//! chart dependencies), so subchart defaults and `global` propagation into
//! subcharts are not reproduced — exactly like `helm get values --all`. Values
//! computed at render time (`lookup`, `tpl`) are not visible here.

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Map, Value};

use super::Sensitive;
use crate::validation::{Field, ValidationError};

/// Result of [`coalesce_values`].
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedValues {
    pub values: Sensitive<Value>,
    /// Type conflicts, e.g. `image: map in defaults, user value is not a map`.
    pub warnings: Vec<String>,
}

/// Merges `user` over `defaults` with Helm's coalesce semantics.
pub fn coalesce_values(defaults: &Value, user: &Value) -> ComputedValues {
    let mut warnings = Vec::new();
    let mut result = match user {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    if let Value::Object(defaults) = defaults {
        coalesce_tables(&mut result, defaults, "", &mut warnings);
    }
    // Map order depends on serde_json's `preserve_order` feature.
    warnings.sort();
    ComputedValues {
        values: Sensitive::new(Value::Object(result)),
        warnings,
    }
}

fn coalesce_tables(
    dst: &mut Map<String, Value>,
    src: &Map<String, Value>,
    prefix: &str,
    warnings: &mut Vec<String>,
) {
    for (key, default) in src {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match dst.get_mut(key) {
            None => {
                dst.insert(key.clone(), default.clone());
            }
            Some(Value::Null) => {
                dst.remove(key);
            }
            Some(Value::Object(user)) => match default {
                Value::Object(default) => coalesce_tables(user, default, &path, warnings),
                Value::Null => {}
                _ => warnings.push(format!("{path}: default is not a map, user map is kept")),
            },
            Some(_) => {
                if default.is_object() {
                    warnings.push(format!("{path}: default is a map, user value is kept"));
                }
            }
        }
    }
}

/// Values as YAML, the way `helm get values` shows them (keys sorted).
pub fn values_to_yaml(values: &Value) -> String {
    match values {
        Value::Object(map) if map.is_empty() => "{}\n".to_owned(),
        _ => serde_saphyr::to_string(&Sorted(values)).unwrap_or_else(|_| {
            serde_json::to_string_pretty(values).expect("JSON value serializes")
        }),
    }
}

/// Checks values typed for `helm upgrade --values -`: one YAML mapping, or
/// nothing at all (no user values). The text itself is what helm gets.
pub fn check_values_yaml(text: &str) -> Result<(), ValidationError> {
    match crate::apply::parse(text) {
        Ok(documents) if documents.len() <= 1 => Ok(()),
        Ok(_) => Err(ValidationError::MultipleDocuments(Field::Values)),
        Err(error) if error.message == "document is not a mapping" => {
            Err(ValidationError::NotMapping {
                field: Field::Values,
                line: error.line,
            })
        }
        Err(error) => Err(ValidationError::Yaml {
            field: Some(Field::Values),
            line: Some(error.line),
            message: error.message,
        }),
    }
}

/// Serializes a JSON value with map keys sorted, whatever the map order.
struct Sorted<'a>(&'a Value);

impl Serialize for Sorted<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by_key(|(key, _)| *key);
                let mut out = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    out.serialize_entry(key, &Sorted(value))?;
                }
                out.end()
            }
            Value::Array(items) => serializer.collect_seq(items.iter().map(Sorted)),
            other => other.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn user_values_win_and_maps_merge() {
        let defaults = json!({
            "replicaCount": 1,
            "image": {"repository": "nginx", "tag": "stable", "pullPolicy": "IfNotPresent"},
            "service": {"port": 80},
            "list": [1, 2, 3]
        });
        let user = json!({
            "replicaCount": 3,
            "image": {"tag": "1.27"},
            "list": [9],
            "extra": {"a": true}
        });
        let computed = coalesce_values(&defaults, &user);
        assert_eq!(
            computed.values.expose(),
            &json!({
                "replicaCount": 3,
                "image": {"repository": "nginx", "tag": "1.27", "pullPolicy": "IfNotPresent"},
                "service": {"port": 80},
                "list": [9],
                "extra": {"a": true}
            })
        );
        assert!(computed.warnings.is_empty());
    }

    #[test]
    fn null_deletes_default_key() {
        let defaults =
            json!({"resources": {"limits": {"cpu": "1"}}, "image": {"tag": "x", "digest": "sha"}});
        let user = json!({"resources": null, "image": {"digest": null}});
        let computed = coalesce_values(&defaults, &user);
        assert_eq!(computed.values.expose(), &json!({"image": {"tag": "x"}}));
    }

    #[test]
    fn null_without_default_is_kept() {
        let computed = coalesce_values(&json!({}), &json!({"foo": null}));
        assert_eq!(computed.values.expose(), &json!({"foo": null}));
    }

    #[test]
    fn type_conflicts_keep_user_value_with_warning() {
        let defaults = json!({"image": {"tag": "x"}, "port": 80, "nested": {"a": {"b": 1}}});
        let user = json!({"image": "nginx:1", "port": {"http": 8080}, "nested": {"a": 5}});
        let computed = coalesce_values(&defaults, &user);
        assert_eq!(computed.values.expose(), &user);
        assert_eq!(
            computed.warnings,
            [
                "image: default is a map, user value is kept",
                "nested.a: default is a map, user value is kept",
                "port: default is not a map, user map is kept",
            ]
        );
    }

    #[test]
    fn empty_user_values_give_defaults() {
        let defaults = json!({"a": {"b": 1}});
        let computed = coalesce_values(&defaults, &json!({}));
        assert_eq!(computed.values.expose(), &defaults);
    }

    #[test]
    fn checks_typed_values() {
        assert!(check_values_yaml("").is_ok());
        assert!(check_values_yaml("# none\n").is_ok());
        assert!(check_values_yaml("replicaCount: 2\nimage:\n  tag: x\n").is_ok());
        assert_eq!(
            check_values_yaml("a: 1\n---\nb: 2\n"),
            Err(ValidationError::MultipleDocuments(Field::Values))
        );
        assert_eq!(
            check_values_yaml("- a\n"),
            Err(ValidationError::NotMapping {
                field: Field::Values,
                line: 1
            })
        );
        let error = check_values_yaml("a: [\n").unwrap_err();
        assert!(
            matches!(error, ValidationError::Yaml { line: Some(_), .. }),
            "{error:?}"
        );
        assert!(error.to_string().starts_with("line "));
    }

    #[test]
    fn renders_yaml() {
        assert_eq!(values_to_yaml(&json!({})), "{}\n");
        let yaml = values_to_yaml(&json!({"b": 1, "a": {"c": "x"}}));
        assert_eq!(yaml, "a:\n  c: x\nb: 1\n");
    }
}
