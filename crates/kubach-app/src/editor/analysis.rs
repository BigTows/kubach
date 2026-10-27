//! Local checks of the editor text: YAML syntax, kinds the cluster serves
//! and the OpenAPI schema, as line-positioned problems. UI-free.

use std::sync::Arc;

use kubach_core::ApiCatalog;
use kubach_core::apply::locate::{self, document_starts};
use kubach_core::apply::resolve::{ResolveError, ResolveErrorKind};
use kubach_core::apply::{FieldPath, Manifest, SchemaDocument, parse, resolve};
use kubach_i18n::t;
use kube::api::GroupVersionKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem at 0-based `line`, columns `start..end` (characters).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    /// A problem on the key at `path` of the document starting at 1-based
    /// `first_line` (its nearest written ancestor if it is not written).
    pub fn at_path(
        text: &str,
        first_line: usize,
        path: &FieldPath,
        severity: Severity,
        message: String,
    ) -> Self {
        let key = locate::locate(text, first_line, path);
        let (start, end) = if key.len == 0 {
            line_span(text, key.line)
        } else {
            (key.column, key.column + key.len)
        };
        Self {
            line: key.line,
            start,
            end,
            severity,
            message,
        }
    }
}

/// What the editor knows about the schema of a kind.
pub enum SchemaLookup {
    Ready(Arc<SchemaDocument>),
    /// Still being fetched.
    Pending,
    /// The cluster publishes none (or fetching failed).
    Unavailable,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub problems: Vec<Problem>,
    /// Kinds written in the text, whose schemas are wanted.
    pub kinds: Vec<GroupVersionKind>,
    /// Documents that parsed; 0 when the text does not parse.
    pub documents: usize,
    /// Some schema was not available yet.
    pub schema_pending: bool,
}

impl Analysis {
    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count()
    }
}

/// Checks `text`. Without a catalog, kinds are not checked; schemas come
/// from `schema`.
pub fn analyze(
    text: &str,
    catalog: Option<&ApiCatalog>,
    default_namespace: &str,
    schema: &dyn Fn(&GroupVersionKind) -> SchemaLookup,
) -> Analysis {
    let mut analysis = Analysis::default();
    for start in document_starts(text) {
        if let Some(gvk) = locate::kind_at(text, start)
            && !analysis.kinds.contains(&gvk)
        {
            analysis.kinds.push(gvk);
        }
    }
    let manifests = match parse(text) {
        Ok(manifests) => manifests,
        Err(error) => {
            let line = error.line.saturating_sub(1);
            let (first, end) = line_span(text, line);
            let start = if error.column > 0 {
                (error.column - 1).min(end)
            } else {
                first
            };
            analysis.problems.push(Problem {
                line,
                start,
                end,
                severity: Severity::Error,
                message: format!("YAML: {}", error.message),
            });
            return analysis;
        }
    };
    analysis.documents = manifests.len();
    for manifest in &manifests {
        if let Some(catalog) = catalog
            && let Err(error) = resolve(manifest, catalog, default_namespace)
        {
            let path = match &error.kind {
                ResolveErrorKind::MissingField(field) => FieldPath::root().key(field),
                ResolveErrorKind::UnknownKind { .. } => FieldPath::parse(".kind"),
                ResolveErrorKind::MissingName => FieldPath::parse(".metadata"),
                ResolveErrorKind::NamespaceOnClusterScoped { .. } => {
                    FieldPath::parse(".metadata.namespace")
                }
            };
            analysis.problems.push(Problem::at_path(
                text,
                manifest.line,
                &path,
                Severity::Error,
                resolve_message(&error.kind),
            ));
        }
        check_schema(text, manifest, schema, &mut analysis);
    }
    analysis.problems.sort_by_key(|p| (p.line, p.start));
    analysis
}

fn check_schema(
    text: &str,
    manifest: &Manifest,
    schema: &dyn Fn(&GroupVersionKind) -> SchemaLookup,
    analysis: &mut Analysis,
) {
    let field = |key: &str| manifest.value.get(key).and_then(|v| v.as_str());
    let (Some(api_version), Some(kind)) = (field("apiVersion"), field("kind")) else {
        return;
    };
    let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
    let gvk = GroupVersionKind::gvk(group, version, kind);
    let document = match schema(&gvk) {
        SchemaLookup::Ready(document) => document,
        SchemaLookup::Pending => {
            analysis.schema_pending = true;
            return;
        }
        SchemaLookup::Unavailable => return,
    };
    for violation in document
        .validate(&gvk, &manifest.value)
        .into_iter()
        .flatten()
    {
        // The server drops unknown fields (with a warning) rather than fail.
        let severity = if violation.message == "unknown field" {
            Severity::Warning
        } else {
            Severity::Error
        };
        let message = format!("{}: {}", violation.path, violation.message);
        analysis.problems.push(Problem::at_path(
            text,
            manifest.line,
            &violation.path,
            severity,
            message,
        ));
    }
}

/// Why a document can't be bound to a served kind, in the interface
/// language.
pub fn resolve_message(kind: &ResolveErrorKind) -> String {
    match kind {
        ResolveErrorKind::MissingField(field) => t!("editor-missing-field", field = *field),
        ResolveErrorKind::UnknownKind { api_version, kind } => {
            t!("editor-unknown-kind", kind = kind, version = api_version)
        }
        ResolveErrorKind::MissingName => t!("editor-missing-name"),
        ResolveErrorKind::NamespaceOnClusterScoped { kind, namespace } => {
            t!("editor-cluster-scoped", kind = kind, namespace = namespace)
        }
    }
}

/// [`resolve_message`] with the document and line it is about.
pub fn resolve_error(error: &ResolveError) -> String {
    t!(
        "editor-resolve-error",
        document = error.index + 1,
        line = error.line,
        message = resolve_message(&error.kind)
    )
}

/// Columns of the content of 0-based `line`: first non-space to the end.
fn line_span(text: &str, line: usize) -> (usize, usize) {
    let content = text.lines().nth(line).unwrap_or_default();
    let end = content.chars().count();
    let start = content.chars().take_while(|c| c.is_whitespace()).count();
    (start.min(end), end)
}

/// Debounce by generation: each edit bumps it, and work scheduled for an
/// older generation is skipped.
#[derive(Debug, Default)]
pub struct Debounce {
    generation: u64,
}

impl Debounce {
    /// Marks a new edit; returns its generation.
    pub fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Whether no edit came after `generation`.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::discovery::{ApiCapabilities, ApiResource, Scope};
    use serde_json::json;

    fn catalog() -> ApiCatalog {
        let caps = ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec!["list".into(), "get".into(), "patch".into()],
        };
        let config_map = ApiResource {
            group: String::new(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            plural: "configmaps".into(),
        };
        ApiCatalog::from_resources(vec![(config_map, caps)])
    }

    fn schema() -> Arc<SchemaDocument> {
        Arc::new(SchemaDocument::from_value(json!({
            "components": {"schemas": {
                "io.k8s.api.core.v1.ConfigMap": {
                    "type": "object",
                    "required": ["metadata"],
                    "properties": {
                        "apiVersion": {"type": "string"},
                        "kind": {"type": "string"},
                        "metadata": {"type": "object", "properties": {
                            "name": {"type": "string"},
                            "namespace": {"type": "string"}
                        }},
                        "data": {"type": "object", "additionalProperties": {"type": "string"}},
                        "immutable": {"type": "boolean"}
                    },
                    "x-kubernetes-group-version-kind": [{"group": "", "kind": "ConfigMap", "version": "v1"}]
                }
            }}
        })))
    }

    fn check(text: &str, lookup: &dyn Fn(&GroupVersionKind) -> SchemaLookup) -> Analysis {
        analyze(text, Some(&catalog()), "default", lookup)
    }

    fn ready(_: &GroupVersionKind) -> SchemaLookup {
        SchemaLookup::Ready(schema())
    }

    #[test]
    fn positions_syntax_errors() {
        let analysis = check("apiVersion: v1\nkind: ConfigMap\ndata: [a, b\n", &ready);
        assert_eq!(analysis.documents, 0);
        let [problem] = analysis.problems.as_slice() else {
            panic!("{:?}", analysis.problems);
        };
        assert_eq!(problem.severity, Severity::Error);
        assert!(problem.message.starts_with("YAML: "), "{problem:?}");
        assert!(problem.line >= 2, "{problem:?}");
        // Kinds are still known, so completion works on broken text.
        assert_eq!(analysis.kinds[0].kind, "ConfigMap");
    }

    #[test]
    fn maps_schema_violations_to_keys() {
        let text = "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: x
  colour: red
immutable: yes
";
        let analysis = check(text, &ready);
        assert_eq!(analysis.documents, 1);
        let found: Vec<_> = analysis
            .problems
            .iter()
            .map(|p| (p.line, p.start, p.end, p.severity))
            .collect();
        assert_eq!(
            found,
            [(4, 2, 8, Severity::Warning), (5, 0, 9, Severity::Error)]
        );
        assert!(
            analysis.problems[0]
                .message
                .starts_with(".metadata.colour: ")
        );
        assert_eq!(analysis.errors(), 1);
    }

    #[test]
    fn reports_resolve_errors_on_their_key() {
        let text = "apiVersion: v1\nkind: Widget\nmetadata:\n  name: x\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  labels: {}\n";
        let analysis = check(text, &|_| SchemaLookup::Unavailable);
        let found: Vec<_> = analysis
            .problems
            .iter()
            .map(|p| (p.line, p.start))
            .collect();
        assert_eq!(found, [(1, 0), (7, 0)]);
        assert!(analysis.problems[0].message.contains("Widget"));
        assert!(analysis.problems[1].message.contains("metadata.name"));
    }

    #[test]
    fn notes_pending_schemas() {
        let text = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n";
        let analysis = check(text, &|_| SchemaLookup::Pending);
        assert!(analysis.schema_pending);
        assert!(analysis.problems.is_empty());
        assert!(check("", &ready).problems.is_empty());
    }

    #[test]
    fn debounce_keeps_only_the_last_edit() {
        let mut debounce = Debounce::default();
        let first = debounce.bump();
        let second = debounce.bump();
        assert!(!debounce.is_current(first));
        assert!(debounce.is_current(second));
    }
}
