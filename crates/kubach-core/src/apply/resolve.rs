//! Resolving manifests to the kinds the cluster serves.

use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::yaml::Manifest;
use crate::discovery::{ApiCatalog, ResourceKind};

/// A manifest bound to a served kind, with name and effective namespace.
#[derive(Clone, Debug)]
pub struct ResolvedManifest {
    pub index: usize,
    pub line: usize,
    /// The kind at the manifest's own version (which may differ from the
    /// catalog's preferred one).
    pub kind: ResourceKind,
    pub name: String,
    /// `None` for cluster-scoped kinds.
    pub namespace: Option<String>,
    /// The document, with `metadata.namespace` filled in for namespaced kinds.
    pub value: Value,
}

impl ResolvedManifest {
    pub fn gvk(&self) -> GroupVersionKind {
        self.kind.gvk()
    }

    /// The document as an object for the API.
    pub fn to_object(&self) -> Result<DynamicObject, serde_json::Error> {
        serde_json::from_value(self.value.clone())
    }

    /// `Deployment web/nginx` style label.
    pub fn label(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{} {namespace}/{}", self.kind.api.kind, self.name),
            None => format!("{} {}", self.kind.api.kind, self.name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResolveErrorKind {
    #[error("`{0}` is missing")]
    MissingField(&'static str),
    #[error("the cluster does not serve {kind} in {api_version}")]
    UnknownKind { api_version: String, kind: String },
    #[error("`metadata.name` is missing (generateName is not supported by apply)")]
    MissingName,
    #[error("{kind} is cluster-scoped but has namespace \"{namespace}\"")]
    NamespaceOnClusterScoped { kind: String, namespace: String },
}

/// A resolve failure, located in the source text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("document {}, line {line}: {kind}", index + 1)]
pub struct ResolveError {
    pub index: usize,
    pub line: usize,
    pub kind: ResolveErrorKind,
}

/// Binds a manifest to a served kind. Namespaced kinds without a namespace
/// get `default_namespace`.
pub fn resolve(
    manifest: &Manifest,
    catalog: &ApiCatalog,
    default_namespace: &str,
) -> Result<ResolvedManifest, ResolveError> {
    let fail = |kind| ResolveError {
        index: manifest.index,
        line: manifest.line,
        kind,
    };
    let value = &manifest.value;
    let api_version = str_at(value, &["apiVersion"])
        .ok_or_else(|| fail(ResolveErrorKind::MissingField("apiVersion")))?;
    let kind_name =
        str_at(value, &["kind"]).ok_or_else(|| fail(ResolveErrorKind::MissingField("kind")))?;
    let (group, version) = match api_version.split_once('/') {
        Some((group, version)) => (group, version),
        None => ("", api_version),
    };
    let mut kind = catalog.find(group, kind_name).cloned().ok_or_else(|| {
        fail(ResolveErrorKind::UnknownKind {
            api_version: api_version.to_string(),
            kind: kind_name.to_string(),
        })
    })?;
    // Discovery keeps only the preferred version; other served versions
    // share plural and scope, and the server rejects unserved ones.
    kind.api.version = version.to_string();
    kind.api.api_version = api_version.to_string();

    let name = str_at(value, &["metadata", "name"])
        .filter(|name| !name.is_empty())
        .ok_or_else(|| fail(ResolveErrorKind::MissingName))?
        .to_string();
    let explicit_namespace =
        str_at(value, &["metadata", "namespace"]).filter(|namespace| !namespace.is_empty());

    let mut value = value.clone();
    let namespace = if kind.namespaced {
        let namespace = explicit_namespace.unwrap_or(default_namespace).to_string();
        if let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) {
            metadata.insert("namespace".into(), Value::String(namespace.clone()));
        }
        Some(namespace)
    } else {
        if let Some(namespace) = explicit_namespace {
            return Err(fail(ResolveErrorKind::NamespaceOnClusterScoped {
                kind: kind_name.to_string(),
                namespace: namespace.to_string(),
            }));
        }
        None
    };

    Ok(ResolvedManifest {
        index: manifest.index,
        line: manifest.line,
        kind,
        name,
        namespace,
        value,
    })
}

/// Resolves every manifest, collecting all errors.
pub fn resolve_all(
    manifests: &[Manifest],
    catalog: &ApiCatalog,
    default_namespace: &str,
) -> Result<Vec<ResolvedManifest>, Vec<ResolveError>> {
    let (ok, errors): (Vec<_>, Vec<_>) = manifests
        .iter()
        .map(|manifest| resolve(manifest, catalog, default_namespace))
        .partition(Result::is_ok);
    if errors.is_empty() {
        Ok(ok.into_iter().map(Result::unwrap).collect())
    } else {
        Err(errors.into_iter().map(Result::unwrap_err).collect())
    }
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::yaml::parse;
    use kube::api::ApiResource;
    use kube::discovery::{ApiCapabilities, Scope, verbs};

    fn catalog() -> ApiCatalog {
        let kind = |group: &str, version: &str, kind: &str, plural: &str, scope| {
            let gvk = GroupVersionKind::gvk(group, version, kind);
            (
                ApiResource::from_gvk_with_plural(&gvk, plural),
                ApiCapabilities {
                    scope,
                    subresources: Vec::new(),
                    operations: vec![verbs::LIST.to_string()],
                },
            )
        };
        ApiCatalog::from_resources([
            kind("", "v1", "ConfigMap", "configmaps", Scope::Namespaced),
            kind("", "v1", "Namespace", "namespaces", Scope::Cluster),
            kind(
                "autoscaling",
                "v2",
                "HorizontalPodAutoscaler",
                "horizontalpodautoscalers",
                Scope::Namespaced,
            ),
        ])
    }

    #[test]
    fn resolves_and_defaults_namespace() {
        let docs = parse(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n---\n\
             apiVersion: v1\nkind: Namespace\nmetadata:\n  name: ns\n",
        )
        .unwrap();
        let resolved = resolve_all(&docs, &catalog(), "dev").unwrap();
        assert_eq!(resolved[0].namespace.as_deref(), Some("dev"));
        assert_eq!(resolved[0].value["metadata"]["namespace"], "dev");
        assert_eq!(resolved[0].label(), "ConfigMap dev/cm");
        assert_eq!(resolved[1].namespace, None);
        assert_eq!(
            resolved[1].to_object().unwrap().metadata.name.unwrap(),
            "ns"
        );
    }

    #[test]
    fn uses_manifest_version() {
        let docs = parse(
            "apiVersion: autoscaling/v1\nkind: HorizontalPodAutoscaler\nmetadata:\n  name: h\n",
        )
        .unwrap();
        let resolved = resolve(&docs[0], &catalog(), "default").unwrap();
        assert_eq!(resolved.kind.api.api_version, "autoscaling/v1");
        assert_eq!(resolved.kind.api.plural, "horizontalpodautoscalers");
    }

    #[test]
    fn reports_errors_with_document_and_line() {
        let docs = parse(
            "apiVersion: v1\nkind: Widget\nmetadata:\n  name: w\n---\n\
             apiVersion: v1\nkind: ConfigMap\nmetadata: {}\n---\n\
             apiVersion: v1\nkind: Namespace\nmetadata:\n  name: n\n  namespace: x\n---\n\
             kind: ConfigMap\n",
        )
        .unwrap();
        let errors = resolve_all(&docs, &catalog(), "default").unwrap_err();
        let kinds: Vec<_> = errors.iter().map(|e| (e.index, e.line, &e.kind)).collect();
        assert_eq!(
            kinds,
            [
                (
                    0,
                    1,
                    &ResolveErrorKind::UnknownKind {
                        api_version: "v1".into(),
                        kind: "Widget".into()
                    }
                ),
                (1, 6, &ResolveErrorKind::MissingName),
                (
                    2,
                    10,
                    &ResolveErrorKind::NamespaceOnClusterScoped {
                        kind: "Namespace".into(),
                        namespace: "x".into()
                    }
                ),
                (3, 16, &ResolveErrorKind::MissingField("apiVersion")),
            ]
        );
        assert_eq!(
            errors[1].to_string(),
            "document 2, line 6: `metadata.name` is missing (generateName is not supported by apply)"
        );
    }
}
