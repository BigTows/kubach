//! Templates for new resources (#320): built-in YAML for common kinds,
//! skeletons from the OpenAPI schema for the others, and helpers to turn an
//! edited manifest into a user preset (stored in
//! [`crate::settings::Presets`]).
//!
//! Templates and presets hold the placeholders [`NAME`] and [`NAMESPACE`];
//! [`render`] fills them in.

use std::ops::Range;

use serde_json::{Map, Value};

use crate::apply::{self, SchemaDocument};

/// Placeholder of the object's name.
pub const NAME: &str = "${name}";
/// Placeholder of the object's namespace.
pub const NAMESPACE: &str = "${namespace}";

/// A built-in template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Template {
    /// Shown in menus.
    pub label: &'static str,
    /// API group of the main kind (`""` for the core group).
    pub group: &'static str,
    /// Version of the main kind.
    pub version: &'static str,
    /// The main kind; a multi-document template may hold more.
    pub kind: &'static str,
    /// Whether its objects live in a namespace.
    pub namespaced: bool,
    /// YAML with placeholders.
    pub text: &'static str,
}

impl Template {
    /// `apps/v1`, `v1`.
    pub fn api_version(&self) -> String {
        api_version(self.group, self.version)
    }
}

const fn template(
    label: &'static str,
    group: &'static str,
    version: &'static str,
    kind: &'static str,
    namespaced: bool,
    text: &'static str,
) -> Template {
    Template {
        label,
        group,
        version,
        kind,
        namespaced,
        text,
    }
}

const BUILTIN: &[Template] = &[
    template(
        "Deployment",
        "apps",
        "v1",
        "Deployment",
        true,
        include_str!("builtin/deployment.yaml"),
    ),
    template(
        "StatefulSet",
        "apps",
        "v1",
        "StatefulSet",
        true,
        include_str!("builtin/statefulset.yaml"),
    ),
    template(
        "DaemonSet",
        "apps",
        "v1",
        "DaemonSet",
        true,
        include_str!("builtin/daemonset.yaml"),
    ),
    template(
        "Job",
        "batch",
        "v1",
        "Job",
        true,
        include_str!("builtin/job.yaml"),
    ),
    template(
        "CronJob",
        "batch",
        "v1",
        "CronJob",
        true,
        include_str!("builtin/cronjob.yaml"),
    ),
    template(
        "Service",
        "",
        "v1",
        "Service",
        true,
        include_str!("builtin/service.yaml"),
    ),
    template(
        "Ingress",
        "networking.k8s.io",
        "v1",
        "Ingress",
        true,
        include_str!("builtin/ingress.yaml"),
    ),
    template(
        "NetworkPolicy",
        "networking.k8s.io",
        "v1",
        "NetworkPolicy",
        true,
        include_str!("builtin/networkpolicy.yaml"),
    ),
    template(
        "ConfigMap",
        "",
        "v1",
        "ConfigMap",
        true,
        include_str!("builtin/configmap.yaml"),
    ),
    template(
        "PersistentVolumeClaim",
        "",
        "v1",
        "PersistentVolumeClaim",
        true,
        include_str!("builtin/persistentvolumeclaim.yaml"),
    ),
    template(
        "HorizontalPodAutoscaler",
        "autoscaling",
        "v2",
        "HorizontalPodAutoscaler",
        true,
        include_str!("builtin/horizontalpodautoscaler.yaml"),
    ),
    template(
        "PodDisruptionBudget",
        "policy",
        "v1",
        "PodDisruptionBudget",
        true,
        include_str!("builtin/poddisruptionbudget.yaml"),
    ),
    template(
        "ServiceAccount + Role + RoleBinding",
        "",
        "v1",
        "ServiceAccount",
        true,
        include_str!("builtin/serviceaccount-rbac.yaml"),
    ),
    template(
        "Namespace",
        "",
        "v1",
        "Namespace",
        false,
        include_str!("builtin/namespace.yaml"),
    ),
];

/// Every built-in template.
pub fn builtin() -> &'static [Template] {
    BUILTIN
}

/// The built-in template of a kind, if any.
pub fn builtin_for(group: &str, kind: &str) -> Option<&'static Template> {
    BUILTIN.iter().find(|t| t.group == group && t.kind == kind)
}

/// `group/version`, or `version` for the core group.
pub fn api_version(group: &str, version: &str) -> String {
    if group.is_empty() {
        version.to_owned()
    } else {
        format!("{group}/{version}")
    }
}

/// The group of an `apiVersion` (`""` for `v1`).
pub fn group_of(api_version: &str) -> &str {
    api_version.rsplit_once('/').map_or("", |(group, _)| group)
}

/// Fills in the placeholders. Without a namespace (cluster-scoped kinds)
/// `namespace: ${namespace}` lines are dropped.
pub fn render(text: &str, name: &str, namespace: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if namespace.is_none() && line.trim() == format!("namespace: {NAMESPACE}") {
            continue;
        }
        out.push_str(line);
    }
    let out = out.replace(NAME, name);
    match namespace {
        Some(namespace) => out.replace(NAMESPACE, namespace),
        None => out,
    }
}

/// A name for a new object of `kind`: `my-deployment`,
/// `my-horizontal-pod-autoscaler`.
pub fn default_name(kind: &str) -> String {
    let mut name = String::from("my");
    let mut previous_lower = false;
    for c in kind.chars().filter(char::is_ascii_alphanumeric) {
        if name == "my" || (c.is_ascii_uppercase() && previous_lower) {
            name.push('-');
        }
        previous_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        name.push(c.to_ascii_lowercase());
    }
    if name == "my" {
        name.push_str("-resource");
    }
    name
}

/// Byte range of the value of the first top-level `metadata.name` that
/// equals `name`, to select it in the editor.
pub fn name_range(text: &str, name: &str) -> Option<Range<usize>> {
    let mut offset = 0;
    let mut in_metadata = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if !content.starts_with(' ') && !content.is_empty() {
            in_metadata = content == "metadata:";
        } else if in_metadata
            && let Some(rest) = content.strip_prefix("  name: ")
            && !rest.starts_with(' ')
            && rest.trim_end() == name
        {
            let start = offset + content.len() - rest.len();
            return Some(start..start + name.len());
        }
        offset += line.len();
    }
    None
}

/// A new object of a kind without a template: `apiVersion`, `kind`,
/// `metadata` and, when the schema is known, the required fields of `spec`
/// set to placeholders of their types.
pub fn skeleton(
    api_version: &str,
    kind: &str,
    schema: Option<&SchemaDocument>,
    name: &str,
    namespace: Option<&str>,
) -> String {
    let mut metadata = Map::new();
    metadata.insert("name".into(), Value::String(name.to_owned()));
    if let Some(namespace) = namespace {
        metadata.insert("namespace".into(), Value::String(namespace.to_owned()));
    }
    let mut object = Map::new();
    object.insert("apiVersion".into(), Value::String(api_version.to_owned()));
    object.insert("kind".into(), Value::String(kind.to_owned()));
    object.insert("metadata".into(), Value::Object(metadata));
    let (group, version) = match api_version.rsplit_once('/') {
        Some((group, version)) => (group, version),
        None => ("", api_version),
    };
    let gvk = kube::api::GroupVersionKind::gvk(group, version, kind);
    if let Some(spec) = schema.and_then(|schema| schema.required_spec(&gvk)) {
        object.insert("spec".into(), spec);
    }
    apply::to_yaml(&Value::Object(object))
}

/// `apiVersion` and `kind` of the first document, if it parses.
pub fn kind_of(text: &str) -> Option<(String, String)> {
    let manifests = apply::parse(text).ok()?;
    let value = &manifests.first()?.value;
    let field = |key| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some((field("apiVersion")?, field("kind")?))
}

/// Whether any document is a core `Secret`: its values must not end up in
/// a preset, which is stored as plain text.
pub fn has_secret(text: &str) -> bool {
    apply::parse(text).is_ok_and(|manifests| {
        manifests.iter().any(|m| {
            m.value.get("kind").and_then(Value::as_str) == Some("Secret")
                && m.value.get("apiVersion").and_then(Value::as_str) == Some("v1")
        })
    })
}

/// The text of a preset made from an edited manifest: the first
/// document's name and namespace become [`NAME`] and [`NAMESPACE`] wherever
/// they stand as a whole value (the namespace only under `namespace:`
/// keys), so the preset fits any new object.
pub fn generalize(text: &str) -> String {
    let Ok(manifests) = apply::parse(text) else {
        return text.to_owned();
    };
    let Some(first) = manifests.first() else {
        return text.to_owned();
    };
    let metadata = |key| {
        first
            .value
            .pointer(&format!("/metadata/{key}"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let name = metadata("name");
    let namespace = metadata("namespace");
    text.split_inclusive('\n')
        .map(|line| generalize_line(line, name.as_deref(), namespace.as_deref()))
        .collect()
}

fn generalize_line(line: &str, name: Option<&str>, namespace: Option<&str>) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return line.to_owned();
    }
    // Only values change: the part after `key: ` or a `- ` item marker.
    let indent = line.len() - trimmed.len();
    let mut rest = trimmed;
    while let Some(item) = rest.strip_prefix("- ") {
        rest = item;
    }
    let item_start = line.len() - rest.len();
    let (key, value_start) = match rest.find(": ") {
        Some(colon) => (&rest[..colon], item_start + colon + 2),
        None if rest.trim_end().ends_with(':') => return line.to_owned(),
        None => ("", item_start.max(indent)),
    };
    let (head, value) = line.split_at(value_start);
    let value = match namespace {
        Some(namespace) if key == "namespace" => replace_token(value, namespace, NAMESPACE),
        _ => value.to_owned(),
    };
    let value = match name {
        Some(name) => replace_token(&value, name, NAME),
        None => value,
    };
    format!("{head}{value}")
}

/// Replaces `from` in `text` where it is not part of a longer name.
fn replace_token(text: &str, from: &str, to: &str) -> String {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + from.len()..].chars().next();
        let whole = !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char);
        out.push_str(&rest[..at]);
        out.push_str(if whole { to } else { from });
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_builtin_renders_to_valid_yaml_of_its_kind() {
        assert_eq!(BUILTIN.len(), 14);
        for template in BUILTIN {
            let namespace = template.namespaced.then_some("team-a");
            let text = render(template.text, "web", namespace);
            assert!(!text.contains("${"), "{}: placeholder left", template.label);
            let manifests =
                apply::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", template.label));
            let first = &manifests[0].value;
            assert_eq!(first["apiVersion"], json!(template.api_version()));
            assert_eq!(first["kind"], json!(template.kind));
            for manifest in &manifests {
                assert_eq!(manifest.value["metadata"]["name"], json!("web"));
                let ns = manifest.value["metadata"].get("namespace");
                if template.namespaced {
                    assert_eq!(ns, Some(&json!("team-a")), "{}", template.label);
                } else {
                    assert_eq!(ns, None, "{}", template.label);
                }
            }
            assert!(name_range(&text, "web").is_some(), "{}", template.label);
        }
    }

    #[test]
    fn rbac_template_has_three_documents() {
        let template = builtin_for("", "ServiceAccount").unwrap();
        let manifests = apply::parse(&render(template.text, "ci", Some("tools"))).unwrap();
        let kinds: Vec<_> = manifests.iter().map(|m| m.value["kind"].clone()).collect();
        assert_eq!(
            kinds,
            [json!("ServiceAccount"), json!("Role"), json!("RoleBinding")]
        );
        assert_eq!(
            manifests[2].value["subjects"][0]["namespace"],
            json!("tools")
        );
    }

    #[test]
    fn cluster_scoped_render_drops_namespace_lines() {
        let text = "metadata:\n  name: ${name}\n  namespace: ${namespace}\n";
        assert_eq!(render(text, "x", None), "metadata:\n  name: x\n");
        assert_eq!(
            render(text, "x", Some("y")),
            "metadata:\n  name: x\n  namespace: y\n"
        );
    }

    #[test]
    fn default_names() {
        assert_eq!(default_name("Deployment"), "my-deployment");
        assert_eq!(
            default_name("HorizontalPodAutoscaler"),
            "my-horizontal-pod-autoscaler"
        );
        assert_eq!(default_name("ConfigMap"), "my-config-map");
        assert_eq!(default_name("CRD"), "my-crd");
        assert_eq!(default_name(""), "my-resource");
    }

    #[test]
    fn name_range_points_at_metadata_name() {
        let text =
            "kind: X\nspec:\n  name: web\nmetadata:\n  labels:\n    name: web\n  name: web\n";
        let range = name_range(text, "web").unwrap();
        assert_eq!(&text[range.clone()], "web");
        assert_eq!(text[..range.start].lines().count(), 7);
        assert_eq!(name_range(text, "api"), None);
    }

    #[test]
    fn generalize_replaces_name_and_namespace_values() {
        let text = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: prod
  labels:
    app.kubernetes.io/name: web
    tier: web-frontend
spec:
  template:
    spec:
      containers:
        - name: web
          image: registry/web:1
          env:
            - name: NS
              value: prod
";
        let preset = generalize(text);
        assert!(preset.contains("  name: ${name}\n  namespace: ${namespace}\n"));
        assert!(preset.contains("app.kubernetes.io/name: ${name}\n"));
        assert!(preset.contains("tier: web-frontend\n"));
        assert!(preset.contains("- name: ${name}\n"));
        assert!(preset.contains("image: registry/web:1\n"));
        // The namespace changes only under `namespace:` keys.
        assert!(preset.contains("value: prod\n"));
        assert_eq!(render(&preset, "web", Some("prod")), text);
        assert_eq!(generalize("not: [yaml"), "not: [yaml");
    }

    #[test]
    fn kind_of_reads_the_first_document() {
        assert_eq!(
            kind_of("apiVersion: v1\nkind: ConfigMap\n---\nkind: Secret\n"),
            Some(("v1".into(), "ConfigMap".into()))
        );
        assert_eq!(kind_of("foo: bar\n"), None);
    }

    #[test]
    fn finds_secrets_in_any_document() {
        assert!(has_secret(
            "apiVersion: v1\nkind: ConfigMap\n---\napiVersion: v1\nkind: Secret\n"
        ));
        assert!(!has_secret("apiVersion: v1\nkind: ConfigMap\n"));
        assert!(!has_secret("apiVersion: example.com/v1\nkind: Secret\n"));
    }

    #[test]
    fn group_and_api_version() {
        assert_eq!(group_of("v1"), "");
        assert_eq!(group_of("apps/v1"), "apps");
        assert_eq!(api_version("", "v1"), "v1");
        assert_eq!(
            api_version("example.com", "v1alpha1"),
            "example.com/v1alpha1"
        );
    }

    fn crd_schema() -> SchemaDocument {
        SchemaDocument::from_value(json!({
            "components": {"schemas": {
                "com.example.v1.Widget": {
                    "type": "object",
                    "x-kubernetes-group-version-kind": [
                        {"group": "example.com", "version": "v1", "kind": "Widget"}
                    ],
                    "properties": {
                        "apiVersion": {"type": "string"},
                        "kind": {"type": "string"},
                        "metadata": {"type": "object"},
                        "spec": {
                            "type": "object",
                            "required": ["size", "mode", "ports", "enabled", "target"],
                            "properties": {
                                "size": {"type": "integer"},
                                "mode": {"type": "string", "enum": ["fast", "slow"]},
                                "enabled": {"type": "boolean"},
                                "optional": {"type": "string"},
                                "ports": {"type": "array", "items": {
                                    "type": "object",
                                    "required": ["port"],
                                    "properties": {"port": {"type": "integer"}}
                                }},
                                "target": {"$ref": "#/components/schemas/com.example.v1.Target"}
                            }
                        }
                    }
                },
                "com.example.v1.Target": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {"name": {"type": "string"}, "extra": {"type": "string"}}
                }
            }}
        }))
    }

    #[test]
    fn skeleton_from_schema_has_required_spec_fields() {
        let schema = crd_schema();
        let text = skeleton(
            "example.com/v1",
            "Widget",
            Some(&schema),
            "my-widget",
            Some("team"),
        );
        let value = &apply::parse(&text).unwrap()[0].value;
        assert_eq!(
            value,
            &json!({
                "apiVersion": "example.com/v1",
                "kind": "Widget",
                "metadata": {"name": "my-widget", "namespace": "team"},
                "spec": {
                    "size": 0,
                    "mode": "fast",
                    "ports": [{"port": 0}],
                    "enabled": false,
                    "target": {"name": ""}
                }
            })
        );
        assert!(name_range(&text, "my-widget").is_some());
    }

    #[test]
    fn skeleton_without_schema_is_minimal() {
        let text = skeleton("example.com/v1", "Cluster", None, "my-cluster", None);
        let value = &apply::parse(&text).unwrap()[0].value;
        assert_eq!(
            value,
            &json!({
                "apiVersion": "example.com/v1",
                "kind": "Cluster",
                "metadata": {"name": "my-cluster"}
            })
        );
    }
}
