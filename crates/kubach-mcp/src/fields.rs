//! `fields` on `get_resource` and `list_resources` (#110): only the named
//! parts of an object, so a question about one field of many objects does
//! not cost their whole YAML.
//!
//! A path is a key, then `.key`, `['any.key/at-all']`, `[N]` or `[*]`:
//! `spec.strategy.type`, `spec.template.spec.containers[*].image`,
//! `metadata.labels['app.kubernetes.io/name']`. No filters, no recursion:
//! selecting is a walk down an object, which needs no JSONPath engine.
//!
//! A selection never shows more than the whole answer would, in two steps:
//! - [`Fields::select`] only takes a [`Masked`] object, and masking is the
//!   only way to get one. Whatever masking replaced or removed is replaced or
//!   gone here too;
//! - the result is the object cut down, not a list of values. Text masking
//!   ([`crate::masking::mask_text`]) knows `password: x` by the key beside
//!   the value, so each value stays under its key and goes through the same
//!   YAML and the same final masking as the whole object.

use kubach_core::apply as core_apply;
use serde_json::{Map, Value, json};

use crate::masking::Masked;
use crate::policy::{Denial, DenialCode};

/// Most paths in one call.
const MAX_PATHS: usize = 32;
/// Longest path.
const MAX_PATH_BYTES: usize = 256;
/// The key under which an object lists the paths that matched nothing in
/// it: not an error, objects of one kind differ.
const NOT_FOUND: &str = "fieldsNotFound";
const SYNTAX: &str = "a path is keys joined by dots, with [N] or [*] for list items and ['key'] \
     for a key with dots or slashes, e.g. spec.strategy.type, \
     spec.template.spec.containers[*].image, metadata.labels['app.kubernetes.io/name']";

/// One step down an object.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    Key(String),
    Index(usize),
    Each,
}

/// The paths of a call, checked.
#[derive(Debug)]
pub(crate) struct Fields {
    paths: Vec<(String, Vec<Step>)>,
}

impl Fields {
    /// The `fields` argument; `None` when it was not given. Checked before
    /// anything is asked or read: a malformed path is bad input.
    pub(crate) fn parse(fields: Option<&[String]>) -> Result<Option<Self>, Denial> {
        let Some(fields) = fields else {
            return Ok(None);
        };
        if fields.is_empty() {
            return Err(invalid(format!(
                "fields is empty: leave it out for the whole object, or name a path; {SYNTAX}"
            )));
        }
        if fields.len() > MAX_PATHS {
            return Err(invalid(format!("fields takes at most {MAX_PATHS} paths")));
        }
        let paths = fields
            .iter()
            .map(|path| {
                let steps = parse_path(path).map_err(|why| {
                    invalid(format!("field path {path:?} is malformed: {why}; {SYNTAX}"))
                })?;
                Ok((path.clone(), steps))
            })
            .collect::<Result<_, Denial>>()?;
        Ok(Some(Self { paths }))
    }

    /// `object` cut down to the paths, with its name and namespace, and the
    /// paths that matched nothing under [`NOT_FOUND`].
    pub(crate) fn select(&self, object: &Masked) -> Value {
        let value = object.value();
        let name = [Step::Key("metadata".into()), Step::Key("name".into())];
        let namespace = [Step::Key("metadata".into()), Step::Key("namespace".into())];
        let mut wanted: Vec<&[Step]> = vec![&name, &namespace];
        wanted.extend(self.paths.iter().map(|(_, steps)| steps.as_slice()));
        let mut selected = keep(value, &wanted).unwrap_or_else(|| json!({}));
        let missing: Vec<&str> = self
            .paths
            .iter()
            .filter(|(_, steps)| keep(value, &[steps]).is_none())
            .map(|(path, _)| path.as_str())
            .collect();
        if !missing.is_empty() {
            selected[NOT_FOUND] = json!(missing);
        }
        selected
    }

    /// The selection as YAML, for one object.
    pub(crate) fn document(&self, object: &Masked) -> String {
        core_apply::to_yaml(&self.select(object))
    }

    /// The selection as a YAML document of a list, without the final newline.
    pub(crate) fn row(&self, object: &Masked) -> String {
        format!("---\n{}", self.document(object).trim_end())
    }
}

fn invalid(message: String) -> Denial {
    Denial::new(DenialCode::InvalidInput, message)
}

/// What of `value` the paths reach; `None` when they reach nothing. List
/// items a path does not reach are left out.
fn keep(value: &Value, paths: &[&[Step]]) -> Option<Value> {
    if paths.iter().any(|path| path.is_empty()) {
        return Some(value.clone());
    }
    match value {
        Value::Object(entries) => {
            let kept: Map<String, Value> = entries
                .iter()
                .filter_map(|(key, entry)| {
                    let rest = after(paths, |step| matches!(step, Step::Key(k) if k == key));
                    Some((key.clone(), keep(entry, &rest)?))
                })
                .collect();
            (!kept.is_empty()).then_some(Value::Object(kept))
        }
        Value::Array(items) => {
            let kept: Vec<Value> = items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let rest = after(paths, |step| {
                        matches!(step, Step::Each) || *step == Step::Index(index)
                    });
                    keep(item, &rest)
                })
                .collect();
            (!kept.is_empty()).then_some(Value::Array(kept))
        }
        _ => None,
    }
}

/// The rest of the paths whose next step is taken.
fn after<'a>(paths: &[&'a [Step]], taken: impl Fn(&Step) -> bool) -> Vec<&'a [Step]> {
    paths
        .iter()
        .filter_map(|path| path.split_first())
        .filter(|(step, _)| taken(step))
        .map(|(_, rest)| rest)
        .collect()
}

fn parse_path(path: &str) -> Result<Vec<Step>, String> {
    if path.len() > MAX_PATH_BYTES {
        return Err(format!("longer than {MAX_PATH_BYTES} bytes"));
    }
    let (key, mut rest) = plain_key(path)?;
    let mut steps = vec![Step::Key(key.to_owned())];
    while let Some(first) = rest.chars().next() {
        match first {
            '.' => {
                let (key, tail) = plain_key(&rest[1..])?;
                steps.push(Step::Key(key.to_owned()));
                rest = tail;
            }
            '[' => {
                let (step, tail) = bracket(&rest[1..])?;
                steps.push(step);
                rest = tail;
            }
            other => return Err(format!("unexpected {other:?}")),
        }
    }
    Ok(steps)
}

/// A key without quotes at the start of `text`, and what follows it.
fn plain_key(text: &str) -> Result<(&str, &str), String> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(text.len());
    if end == 0 {
        return Err("a key is missing".into());
    }
    Ok(text.split_at(end))
}

/// What follows a `[`: `*]`, `N]`, `'key']` or `"key"]`.
fn bracket(text: &str) -> Result<(Step, &str), String> {
    if let Some(quote) = text.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        let inner = &text[1..];
        let end = inner
            .find(quote)
            .ok_or_else(|| format!("no closing {quote}"))?;
        if end == 0 {
            return Err("a key is empty".into());
        }
        let tail = inner[end + 1..]
            .strip_prefix(']')
            .ok_or_else(|| "no closing ]".to_owned())?;
        return Ok((Step::Key(inner[..end].to_owned()), tail));
    }
    let (inside, tail) = text.split_once(']').ok_or("no closing ]")?;
    let step = if inside == "*" {
        Step::Each
    } else if !inside.is_empty() && inside.bytes().all(|b| b.is_ascii_digit()) {
        Step::Index(
            inside
                .parse()
                .map_err(|_| format!("index {inside} is too large"))?,
        )
    } else {
        return Err(format!("[{inside}] is not an index, * or a quoted key"));
    };
    Ok((step, tail))
}

#[cfg(test)]
mod tests {
    use kube::api::{ApiResource, DynamicObject};
    use serde_json::{Value, json};

    use super::*;
    use crate::masking::mask_text;

    fn api(group: &str, kind: &str, plural: &str) -> ApiResource {
        crate::testutil::kind(group, "v1", kind, plural, true).0
    }

    /// An object as a list answer gives it: without `kind` and `apiVersion`.
    fn list_item(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn fields(paths: &[&str]) -> Fields {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        Fields::parse(Some(&paths)).unwrap().unwrap()
    }

    fn deployment() -> Value {
        json!({
            "metadata": {"name": "web", "namespace": "dev", "uid": "1", "labels": {
                "app.kubernetes.io/name": "web", "tier": "front",
            }},
            "spec": {
                "replicas": 2,
                "strategy": {"type": "RollingUpdate", "rollingUpdate": {"maxSurge": 1}},
                "template": {"spec": {"containers": [
                    {"name": "app", "image": "nginx:1", "ports": [{"containerPort": 80}]},
                    {"name": "sidecar", "image": "envoy:2"},
                ]}},
            },
        })
    }

    fn selected(paths: &[&str]) -> Value {
        let object = Masked::object(
            &list_item(deployment()),
            &api("apps", "Deployment", "deployments"),
        );
        fields(paths).select(&object)
    }

    #[test]
    fn a_path_takes_only_its_field_and_the_identity_of_the_object() {
        assert_eq!(
            selected(&["spec.strategy.type"]),
            json!({
                "metadata": {"name": "web", "namespace": "dev"},
                "spec": {"strategy": {"type": "RollingUpdate"}},
            })
        );
    }

    #[test]
    fn list_items_keep_their_fields_together() {
        let value = selected(&[
            "spec.template.spec.containers[*].image",
            "spec.template.spec.containers[*].name",
            "spec.template.spec.containers[0].ports[0].containerPort",
        ]);
        assert_eq!(
            value["spec"]["template"]["spec"]["containers"],
            json!([
                {"name": "app", "image": "nginx:1", "ports": [{"containerPort": 80}]},
                {"name": "sidecar", "image": "envoy:2"},
            ])
        );
    }

    #[test]
    fn an_index_takes_one_item_and_a_quoted_key_may_have_dots() {
        let value = selected(&[
            "spec.template.spec.containers[1].image",
            "metadata.labels['app.kubernetes.io/name']",
            "metadata.labels[\"tier\"]",
        ]);
        assert_eq!(
            value["spec"]["template"]["spec"]["containers"],
            json!([{"image": "envoy:2"}])
        );
        assert_eq!(
            value["metadata"]["labels"],
            json!({"app.kubernetes.io/name": "web", "tier": "front"})
        );
    }

    #[test]
    fn a_path_to_a_subtree_takes_all_of_it() {
        assert_eq!(
            selected(&["spec.strategy"])["spec"]["strategy"],
            deployment()["spec"]["strategy"]
        );
    }

    #[test]
    fn a_path_that_matches_nothing_is_marked_on_the_object() {
        let value = selected(&[
            "spec.replicas",
            "spec.nothing.here",
            "spec.template.spec.containers.image",
            "spec.template.spec.containers[7].image",
            "spec.template.spec.containers[*].nothing",
            "spec.replicas.deeper",
        ]);
        assert_eq!(value["spec"]["replicas"], 2);
        assert_eq!(
            value[NOT_FOUND],
            json!([
                "spec.nothing.here",
                "spec.template.spec.containers.image",
                "spec.template.spec.containers[7].image",
                "spec.template.spec.containers[*].nothing",
                "spec.replicas.deeper",
            ])
        );
        assert_eq!(value["metadata"]["name"], "web");
        assert!(selected(&["spec.replicas"]).get(NOT_FOUND).is_none());
    }

    #[test]
    fn a_field_that_is_null_or_empty_is_found() {
        let object = Masked::object(
            &list_item(json!({
                "metadata": {"name": "a", "namespace": "dev"},
                "spec": {"gone": null, "none": {}, "list": []},
            })),
            &api("example.com", "Widget", "widgets"),
        );
        let value = fields(&["spec.gone", "spec.none", "spec.list"]).select(&object);
        assert_eq!(value["spec"], json!({"gone": null, "none": {}, "list": []}));
        assert!(value.get(NOT_FOUND).is_none());
    }

    #[test]
    fn malformed_paths_are_input_errors() {
        for path in [
            "",
            ".spec",
            "spec.",
            "spec..type",
            "{.spec.type}",
            "$.spec",
            "spec.containers[",
            "spec.containers[]",
            "spec.containers[x]",
            "spec.containers[-1]",
            "spec.containers[99999999999999999999999]",
            "spec.containers[*",
            "spec.containers[*]x",
            "spec.containers[?(@.name=='a')]",
            "metadata.labels['a'",
            "metadata.labels['a]",
            "metadata.labels['']",
            "spec type",
            "spec.тип",
            "spec,type",
        ] {
            let error = Fields::parse(Some(&[path.to_owned()])).unwrap_err();
            assert_eq!(error.code, DenialCode::InvalidInput, "{path}");
            assert!(error.message.contains("malformed"), "{path}");
            assert!(error.message.contains("spec.strategy.type"), "{path}");
        }
        let long = "a".repeat(MAX_PATH_BYTES + 1);
        assert!(Fields::parse(Some(&[long])).is_err());
        assert!(Fields::parse(Some(&[])).is_err());
        assert!(Fields::parse(Some(&vec!["a".to_owned(); MAX_PATHS + 1])).is_err());
        assert!(Fields::parse(None).unwrap().is_none());
    }

    /// The whole masked object and the selection, as the agent gets them.
    fn both(object: &Masked, paths: &[&str]) -> (String, String) {
        let whole = mask_text(&core_apply::to_yaml(object.value())).into_owned();
        let part = mask_text(&fields(paths).document(object)).into_owned();
        (whole, part)
    }

    #[test]
    fn a_selection_of_a_list_item_is_masked_though_it_has_no_kind() {
        // Items of a list answer carry no `kind`, and masking of ConfigMaps
        // and Secrets goes by it.
        let item = list_item(json!({
            "metadata": {"name": "app", "namespace": "dev"},
            "data": {"DB_PASS": "hunter2-map", "DB_HOST": "db.local"},
            "binaryData": {"tls.key": "aHVudGVyMg=="},
        }));
        let object = Masked::object(&item, &api("", "ConfigMap", "configmaps"));
        let (whole, part) = both(
            &object,
            &[
                "data.DB_PASS",
                "data",
                "binaryData['tls.key']",
                "data.DB_HOST",
            ],
        );
        for text in [&whole, &part] {
            assert!(!text.contains("hunter2-map"), "{text}");
            assert!(!text.contains("aHVudGVyMg"), "{text}");
            assert!(text.contains("*** (11 bytes)"), "{text}");
            assert!(text.contains("db.local"), "{text}");
        }
    }

    #[test]
    fn a_kind_named_secret_is_masked_in_a_list_too() {
        let item = list_item(json!({
            "metadata": {"name": "s", "namespace": "dev"},
            "data": {"plain": "aHVudGVyMg=="},
            "stringData": {"other": "hunter2-string"},
        }));
        let object = Masked::object(&item, &api("vault.example.com", "Secret", "secrets"));
        let (_, part) = both(&object, &["data.plain", "stringData", "data"]);
        assert!(!part.contains("aHVudGVyMg"), "{part}");
        assert!(!part.contains("hunter2-string"), "{part}");
        assert!(part.contains("*** (7 bytes)"), "{part}");
    }

    #[test]
    fn a_selected_env_value_or_annotation_stays_masked() {
        let item = list_item(json!({
            "metadata": {"name": "web", "namespace": "dev", "annotations": {
                "example.com/api-token": "ghp_annotation", "owner": "team",
            }},
            "spec": {"template": {
                "metadata": {"annotations": {"vault/secret": "in-template"}},
                "spec": {"containers": [{"name": "app", "env": [
                    {"name": "DB_PASSWORD", "value": "s3cr3t-env"},
                    {"name": "LOG_LEVEL", "value": "debug"},
                ]}]},
            }},
        }));
        let object = Masked::object(&item, &api("apps", "Deployment", "deployments"));
        let (_, part) = both(
            &object,
            &[
                "spec.template.spec.containers[*].env[*].value",
                "spec.template.spec.containers[0].env[0]",
                "spec.template.metadata.annotations",
                "metadata.annotations['example.com/api-token']",
                "metadata.annotations.owner",
            ],
        );
        for leaked in ["s3cr3t-env", "ghp_annotation", "in-template"] {
            assert!(!part.contains(leaked), "{leaked} in {part}");
        }
        for kept in ["debug", "team", "DB_PASSWORD"] {
            assert!(part.contains(kept), "{kept} not in {part}");
        }
    }

    #[test]
    fn a_sensitive_key_keeps_the_text_masking_of_the_whole_answer() {
        // Nothing structural masks `spec.password` of a custom resource: the
        // final text masking does, by the key beside the value. A selection
        // that printed the bare value would show it.
        let item = list_item(json!({
            "metadata": {"name": "w", "namespace": "dev"},
            "spec": {
                "password": "hunter2-crd",
                "clientSecret": "hunter2-client",
                "auth": {"apiKey": "hunter2-nested"},
                "backends": [{"url": "postgres://app:hunter2-url@db/x", "token": "hunter2-item"}],
                "size": 3,
            },
        }));
        let object = Masked::object(&item, &api("example.com", "Widget", "widgets"));
        let (whole, part) = both(
            &object,
            &[
                "spec.password",
                "spec.clientSecret",
                "spec.auth.apiKey",
                "spec.backends[*].url",
                "spec.backends[0].token",
                "spec.size",
            ],
        );
        for text in [&whole, &part] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(text.contains("size: 3"), "{text}");
        }
    }

    #[test]
    fn what_masking_removed_is_not_found() {
        let last_applied = "kubectl.kubernetes.io/last-applied-configuration";
        let item = list_item(json!({
            "metadata": {
                "name": "web", "namespace": "dev",
                "annotations": {last_applied: "{\"password\":\"hunter2\"}"},
                "managedFields": [{"manager": "kubectl"}],
            },
        }));
        let object = Masked::object(&item, &api("apps", "Deployment", "deployments"));
        let path = format!("metadata.annotations['{last_applied}']");
        let value = fields(&[&path, "metadata.managedFields"]).select(&object);
        assert_eq!(value[NOT_FOUND], json!([path, "metadata.managedFields"]));
        assert!(!value.to_string().contains("hunter2"));
    }

    #[test]
    fn rows_are_yaml_documents_that_name_their_object() {
        let object = Masked::object(
            &list_item(deployment()),
            &api("apps", "Deployment", "deployments"),
        );
        let row = fields(&["spec.replicas", "kind"]).row(&object);
        assert!(row.starts_with("---\n"), "{row}");
        assert!(!row.ends_with('\n'), "{row}");
        for line in [
            "name: web",
            "namespace: dev",
            "replicas: 2",
            "kind: Deployment",
        ] {
            assert!(row.contains(line), "{line} not in {row}");
        }
    }
}
