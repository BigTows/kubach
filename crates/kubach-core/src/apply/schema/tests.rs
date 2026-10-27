use super::*;
use serde_json::json;

/// A small document shaped like the cluster's `apis/apps/v1`.
fn document() -> SchemaDocument {
    SchemaDocument::from_value(json!({
        "components": {"schemas": {
            "io.k8s.api.apps.v1.Deployment": {
                "description": "Deployment enables declarative updates.",
                "type": "object",
                "properties": {
                    "apiVersion": {"type": "string"},
                    "kind": {"type": "string"},
                    "metadata": {"allOf": [{"$ref": "#/components/schemas/io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta"}],
                                 "default": {}, "description": "Standard object metadata."},
                    "spec": {"allOf": [{"$ref": "#/components/schemas/io.k8s.api.apps.v1.DeploymentSpec"}]},
                    "status": {"type": "object"}
                },
                "x-kubernetes-group-version-kind": [{"group": "apps", "kind": "Deployment", "version": "v1"}]
            },
            "io.k8s.api.apps.v1.DeploymentSpec": {
                "type": "object",
                "required": ["selector", "template"],
                "properties": {
                    "replicas": {"type": "integer", "format": "int32", "description": "Number of desired pods."},
                    "selector": {"type": "object", "additionalProperties": {"type": "string"}},
                    "strategy": {"type": "object", "properties": {
                        "type": {"type": "string", "enum": ["Recreate", "RollingUpdate"],
                                 "description": "Type of deployment."},
                        "maxSurge": {"allOf": [{"$ref": "#/components/schemas/io.k8s.apimachinery.pkg.util.intstr.IntOrString"}]}
                    }},
                    "template": {"type": "object", "properties": {
                        "containers": {"type": "array", "items": {"allOf": [{"$ref": "#/components/schemas/io.k8s.api.core.v1.Container"}]}}
                    }},
                    "extra": {"type": "object", "x-kubernetes-preserve-unknown-fields": true,
                              "properties": {"known": {"type": "boolean"}}}
                }
            },
            "io.k8s.api.core.v1.Container": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {"type": "string", "description": "Container name."},
                    "image": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "resources": {"type": "object", "properties": {
                        "limits": {"type": "object", "additionalProperties": {"$ref": "#/components/schemas/io.k8s.apimachinery.pkg.api.resource.Quantity"}}
                    }}
                }
            },
            "io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "namespace": {"type": "string"},
                    "labels": {"type": "object", "additionalProperties": {"type": "string", "default": ""}}
                }
            },
            "io.k8s.apimachinery.pkg.util.intstr.IntOrString": {"type": "string", "format": "int-or-string"},
            "io.k8s.apimachinery.pkg.api.resource.Quantity": {"oneOf": [{"type": "string"}, {"type": "number"}]}
        }}
    }))
}

fn gvk() -> GroupVersionKind {
    GroupVersionKind::gvk("apps", "v1", "Deployment")
}

fn violations(value: Value) -> Vec<String> {
    document()
        .validate(&gvk(), &value)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn valid_document_passes() {
    let errors = violations(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": "web", "labels": {"app": "web"}},
        "spec": {
            "replicas": 2,
            "selector": {"app": "web"},
            "strategy": {"type": "RollingUpdate", "maxSurge": "25%"},
            "template": {"containers": [{"name": "app", "image": "nginx",
                         "resources": {"limits": {"cpu": 1, "memory": "64Mi"}}}]},
            "extra": {"known": true, "anything": {"goes": 1}}
        },
        "status": null
    }));
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn reports_type_required_enum_and_unknown_fields() {
    let errors = violations(json!({
        "metadata": {"name": 5, "lables": {}},
        "spec": {
            "replicas": "two",
            "selector": {"app": 1},
            "strategy": {"type": "BlueGreen", "maxSurge": true},
            "template": {"containers": [{"image": "nginx", "args": "x",
                         "resources": {"limits": {"cpu": []}}}]},
            "extra": {"known": "yes"}
        }
    }));
    assert_eq!(
        errors,
        [
            ".metadata.name: expected string, got integer",
            ".metadata.lables: unknown field",
            ".spec.replicas: expected integer, got string",
            ".spec.selector.app: expected string, got integer",
            r#".spec.strategy.type: must be one of "Recreate", "RollingUpdate""#,
            ".spec.strategy.maxSurge: expected integer or string, got boolean",
            ".spec.template.containers[0].name: required field is missing",
            ".spec.template.containers[0].args: expected array, got string",
            ".spec.template.containers[0].resources.limits.cpu: expected string or number, got array",
            ".spec.extra.known: expected boolean, got string",
        ]
    );
}

#[test]
fn reports_missing_required() {
    let errors = violations(json!({"spec": {}}));
    assert_eq!(
        errors,
        [
            ".spec.selector: required field is missing",
            ".spec.template: required field is missing"
        ]
    );
}

#[test]
fn unknown_kind_has_no_schema() {
    let doc = document();
    assert!(
        doc.validate(
            &GroupVersionKind::gvk("apps", "v1", "StatefulSet"),
            &json!({})
        )
        .is_none()
    );
}

#[test]
fn completes_fields_with_types() {
    let doc = document();
    let names = |path: &str| -> Vec<(String, String, bool)> {
        doc.completions(&gvk(), &FieldPath::parse(path))
            .into_iter()
            .map(|c| (c.name, c.type_label, c.required))
            .collect()
    };
    assert_eq!(
        names(".spec"),
        [
            ("selector".into(), "map[string]string".into(), true),
            ("template".into(), "object".into(), true),
            ("extra".into(), "object".into(), false),
            ("replicas".into(), "integer".into(), false),
            ("strategy".into(), "object".into(), false),
        ]
    );
    // A list path completes its items' fields.
    let containers = names(".spec.template.containers");
    assert_eq!(containers[0], ("name".into(), "string".into(), true));
    assert_eq!(names(".spec.template.containers[0]"), containers);
    assert!(names(".spec.nope").is_empty());

    let root = doc.completions(&gvk(), &FieldPath::root());
    let metadata = root.iter().find(|c| c.name == "metadata").unwrap();
    assert_eq!(metadata.type_label, "ObjectMeta");
    assert_eq!(metadata.description, "Standard object metadata.");
}

#[test]
fn describes_fields() {
    let doc = document();
    let info = doc
        .describe(&gvk(), &FieldPath::parse(".spec.strategy.type"))
        .unwrap();
    assert_eq!(info.type_label, "string");
    assert_eq!(info.enum_values, ["Recreate", "RollingUpdate"]);
    let info = doc
        .describe(&gvk(), &FieldPath::parse(".spec.template.containers"))
        .unwrap();
    assert_eq!(info.type_label, "[]Container");
    let info = doc
        .describe(&gvk(), &FieldPath::parse(".spec.strategy.maxSurge"))
        .unwrap();
    assert_eq!(info.type_label, "int-or-string");
    let info = doc
        .describe(
            &gvk(),
            &FieldPath::parse(".spec.template.containers[0].resources.limits"),
        )
        .unwrap();
    assert_eq!(info.type_label, "map[string]Quantity");
    assert_eq!(
        doc.describe(&gvk(), &FieldPath::root())
            .unwrap()
            .description,
        "Deployment enables declarative updates."
    );
}

#[test]
fn parses_index_and_paths() {
    let index: OpenApiIndex = serde_json::from_value(json!({"paths": {
        "api/v1": {"serverRelativeURL": "/openapi/v3/api/v1?hash=AB"}
    }}))
    .unwrap();
    assert_eq!(
        index.paths["api/v1"].server_relative_url,
        "/openapi/v3/api/v1?hash=AB"
    );
    assert_eq!(
        group_version_path(&GroupVersionKind::gvk("", "v1", "Pod")),
        "api/v1"
    );
    assert_eq!(group_version_path(&gvk()), "apis/apps/v1");
}
