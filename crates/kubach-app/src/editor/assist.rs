//! Schema-driven completion and hovers for the YAML editor.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{CompletionProvider, HoverProvider, Rope, RopeExt as _};
use gpui_kit::{App, Task, Window};
use kubach_core::ApiCatalog;
use kubach_core::apply::locate::{key_at, key_lines, kind_at};
use kubach_core::apply::{FieldPath, PathSegment, SchemaDocument, path_at};
use kube::api::GroupVersionKind;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Documentation, Hover, HoverContents, MarkupContent, MarkupKind, Position, Range, TextEdit,
};

use super::analysis::SchemaLookup;

/// Schema of one group-version, as known to the editor.
pub enum SchemaSlot {
    Loading,
    Ready(Option<Arc<SchemaDocument>>),
    Failed(String),
}

/// OpenAPI documents by group-version, shared by the editor view (which
/// fetches them) and its completion/hover providers.
#[derive(Default)]
pub struct SchemaStore {
    slots: HashMap<(String, String), SchemaSlot>,
}

pub type SharedSchemas = Rc<RefCell<SchemaStore>>;

fn group_version(gvk: &GroupVersionKind) -> (String, String) {
    (gvk.group.clone(), gvk.version.clone())
}

impl SchemaStore {
    pub fn lookup(&self, gvk: &GroupVersionKind) -> SchemaLookup {
        match self.slots.get(&group_version(gvk)) {
            Some(SchemaSlot::Ready(Some(document))) => SchemaLookup::Ready(document.clone()),
            Some(SchemaSlot::Loading) | None => SchemaLookup::Pending,
            Some(SchemaSlot::Ready(None) | SchemaSlot::Failed(_)) => SchemaLookup::Unavailable,
        }
    }

    pub fn document(&self, gvk: &GroupVersionKind) -> Option<Arc<SchemaDocument>> {
        match self.lookup(gvk) {
            SchemaLookup::Ready(document) => Some(document),
            _ => None,
        }
    }

    /// Marks `gvk`'s group-version as loading; `false` if it already is
    /// known or being fetched.
    pub fn start(&mut self, gvk: &GroupVersionKind) -> bool {
        let key = group_version(gvk);
        if self.slots.contains_key(&key) {
            return false;
        }
        self.slots.insert(key, SchemaSlot::Loading);
        true
    }

    pub fn finish(&mut self, gvk: &GroupVersionKind, slot: SchemaSlot) {
        self.slots.insert(group_version(gvk), slot);
    }

    /// Fetch errors, for the status line.
    pub fn errors(&self) -> Vec<String> {
        self.slots
            .values()
            .filter_map(|slot| match slot {
                SchemaSlot::Failed(error) => Some(error.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Metadata the server sets; never worth typing in a manifest.
const SERVER_SET_METADATA: &[&str] = &[
    "creationTimestamp",
    "deletionGracePeriodSeconds",
    "deletionTimestamp",
    "generation",
    "managedFields",
    "resourceVersion",
    "selfLink",
    "uid",
];

/// A completion candidate: replace columns `start..end` of `line`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub documentation: String,
    pub insert: String,
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub kind: SuggestionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Field,
    Value,
}

/// Completions at 0-based `line`:`column` (characters): field names when
/// typing a key, enum values (or kinds and API versions from the catalog)
/// when typing a value.
pub fn suggest(
    text: &str,
    line: usize,
    column: usize,
    schema: &dyn Fn(&GroupVersionKind) -> Option<Arc<SchemaDocument>>,
    catalog: Option<&ApiCatalog>,
) -> Vec<Suggestion> {
    let current = text.lines().nth(line).unwrap_or_default();
    let prefix: String = current.chars().take(column).collect();
    let mut rest = prefix.trim_start();
    while let Some(item) = rest.strip_prefix("- ") {
        rest = item.trim_start();
    }
    if rest.starts_with('#') {
        return Vec::new();
    }
    let rest_start = column - rest.chars().count();

    if let Some((key, value)) = rest.split_once(": ").or_else(|| {
        rest.strip_suffix(':')
            .filter(|key| !key.contains(' '))
            .map(|key| (key, ""))
    }) {
        let typed = value.trim_start();
        let start = column - typed.chars().count();
        let key = key.trim();
        let Some(path) = key_at(text, line, rest_start) else {
            return Vec::new();
        };
        let values = value_candidates(text, line, &path, key, schema, catalog);
        let typed_lower = typed.to_lowercase();
        return values
            .into_iter()
            .filter(|(value, _)| value.to_lowercase().starts_with(&typed_lower) && value != typed)
            .map(|(value, detail)| Suggestion {
                insert: if value_needs_space(&prefix) {
                    format!(" {value}")
                } else {
                    value.clone()
                },
                label: value,
                detail,
                documentation: String::new(),
                line,
                start,
                end: column,
                kind: SuggestionKind::Value,
            })
            .collect();
    }

    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return Vec::new();
    }
    let Some(gvk) = kind_at(text, line) else {
        return Vec::new();
    };
    let Some(document) = schema(&gvk) else {
        return Vec::new();
    };
    let mapping = path_at(text, line, rest_start);
    // Keys already written in this mapping.
    let written: Vec<String> = key_lines(text, line)
        .into_iter()
        .filter(|key| key.line != line && key.path.parent() == mapping)
        .filter_map(|key| match key.path.segments().last() {
            Some(PathSegment::Key(name)) => Some(name.clone()),
            _ => None,
        })
        .collect();
    let typed = rest.to_lowercase();
    let server_set: &[&str] = match mapping.segments() {
        [] => &["status"],
        [PathSegment::Key(key)] if key == "metadata" => SERVER_SET_METADATA,
        _ => &[],
    };
    document
        .completions(&gvk, &mapping)
        .into_iter()
        .filter(|c| c.name.to_lowercase().starts_with(&typed) && !written.contains(&c.name))
        .filter(|c| !server_set.contains(&c.name.as_str()))
        .map(|c| Suggestion {
            detail: if c.required {
                kubach_i18n::t!("editor-field-required", schema_type = c.type_label)
            } else {
                c.type_label
            },
            documentation: c.description,
            insert: format!("{}: ", c.name),
            label: c.name,
            line,
            start: rest_start,
            end: column,
            kind: SuggestionKind::Field,
        })
        .collect()
}

/// `key:` typed without the space: the value needs one.
fn value_needs_space(prefix: &str) -> bool {
    prefix.trim_end() == prefix && prefix.ends_with(':')
}

fn value_candidates(
    text: &str,
    line: usize,
    path: &FieldPath,
    key: &str,
    schema: &dyn Fn(&GroupVersionKind) -> Option<Arc<SchemaDocument>>,
    catalog: Option<&ApiCatalog>,
) -> Vec<(String, String)> {
    let top_level = path.segments().len() == 1;
    if top_level && key == "kind" {
        let mut kinds: Vec<(String, String)> = catalog
            .map(|c| c.kinds())
            .unwrap_or_default()
            .iter()
            .map(|k| (k.api.kind.clone(), k.api.api_version.clone()))
            .collect();
        kinds.sort();
        kinds.dedup_by(|a, b| a.0 == b.0);
        return kinds;
    }
    if top_level && key == "apiVersion" {
        let mut versions: Vec<(String, String)> = catalog
            .map(|c| c.kinds())
            .unwrap_or_default()
            .iter()
            .map(|k| (k.api.api_version.clone(), String::new()))
            .collect();
        versions.sort();
        versions.dedup();
        return versions;
    }
    let Some(gvk) = kind_at(text, line) else {
        return Vec::new();
    };
    let Some(info) = schema(&gvk).and_then(|document| document.describe(&gvk, path)) else {
        return Vec::new();
    };
    let mut values: Vec<(String, String)> = info
        .enum_values
        .into_iter()
        .map(|value| (value, info.type_label.clone()))
        .collect();
    if info.type_label == "boolean" {
        values = ["true", "false"]
            .map(|v| (v.to_string(), "boolean".to_string()))
            .to_vec();
    }
    values
}

/// Markdown hover text for the field at 0-based `line`:`column`.
pub fn describe_at(
    text: &str,
    line: usize,
    column: usize,
    schema: &dyn Fn(&GroupVersionKind) -> Option<Arc<SchemaDocument>>,
) -> Option<String> {
    let gvk = kind_at(text, line)?;
    let path = key_at(text, line, column)?;
    let info = schema(&gvk)?.describe(&gvk, &path)?;
    let mut markdown = format!("`{path}`: **{}**", info.type_label);
    if !info.description.is_empty() {
        markdown.push_str("\n\n");
        markdown.push_str(&info.description);
    }
    if !info.enum_values.is_empty() {
        let values: Vec<String> = info.enum_values.iter().map(|v| format!("`{v}`")).collect();
        markdown.push_str("\n\n");
        markdown.push_str(&kubach_i18n::t!(
            "editor-one-of",
            values = values.join(", ")
        ));
    }
    Some(markdown)
}

/// Completion and hover provider backed by the shared schema store.
pub struct SchemaAssist {
    pub schemas: SharedSchemas,
    pub catalog: Option<ApiCatalog>,
}

impl SchemaAssist {
    fn lookup(&self) -> impl Fn(&GroupVersionKind) -> Option<Arc<SchemaDocument>> + '_ {
        |gvk| self.schemas.borrow().document(gvk)
    }
}

impl CompletionProvider for SchemaAssist {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<gpui_kit::Result<CompletionResponse>> {
        let position = text.offset_to_position(offset);
        let suggestions = suggest(
            &text.to_string(),
            position.line as usize,
            position.character as usize,
            &self.lookup(),
            self.catalog.as_ref(),
        );
        let items = suggestions
            .into_iter()
            .map(|s| CompletionItem {
                label: s.label,
                kind: Some(match s.kind {
                    SuggestionKind::Field => CompletionItemKind::FIELD,
                    SuggestionKind::Value => CompletionItemKind::ENUM_MEMBER,
                }),
                detail: (!s.detail.is_empty()).then_some(s.detail),
                documentation: (!s.documentation.is_empty())
                    .then_some(Documentation::String(s.documentation)),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: Range {
                        start: Position::new(s.line as u32, s.start as u32),
                        end: Position::new(s.line as u32, s.end as u32),
                    },
                    new_text: s.insert,
                })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
            && !new_text.is_empty()
    }
}

impl HoverProvider for SchemaAssist {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<gpui_kit::Result<Option<Hover>>> {
        let position = text.offset_to_position(offset);
        let hover = describe_at(
            &text.to_string(),
            position.line as usize,
            position.character as usize,
            &self.lookup(),
        )
        .map(|markdown| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: None,
        });
        Task::ready(Ok(hover))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Arc<SchemaDocument> {
        Arc::new(SchemaDocument::from_value(json!({
            "components": {"schemas": {
                "io.k8s.api.apps.v1.Deployment": {
                    "type": "object",
                    "properties": {
                        "apiVersion": {"type": "string"},
                        "kind": {"type": "string"},
                        "metadata": {"type": "object", "properties": {
                            "name": {"type": "string", "description": "Name of the object."},
                            "uid": {"type": "string"},
                            "labels": {"type": "object", "additionalProperties": {"type": "string"}}
                        }},
                        "spec": {"type": "object", "required": ["selector"], "properties": {
                            "replicas": {"type": "integer"},
                            "paused": {"type": "boolean"},
                            "selector": {"type": "object"},
                            "strategy": {"type": "object", "properties": {
                                "type": {"type": "string", "enum": ["Recreate", "RollingUpdate"],
                                         "description": "Type of deployment."}
                            }}
                        }}
                    },
                    "x-kubernetes-group-version-kind": [{"group": "apps", "kind": "Deployment", "version": "v1"}]
                }
            }}
        })))
    }

    const TEXT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 1
  re
  strategy:
    type: Ro
  paused: t
";

    fn at(line: usize, column: usize) -> Vec<Suggestion> {
        suggest(TEXT, line, column, &|_| Some(document()), None)
    }

    fn labels(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn suggests_fields_of_the_mapping() {
        // Fields written elsewhere in `.spec` are not offered again.
        let all = at(6, 2);
        assert_eq!(labels(&all), ["selector"]);
        assert_eq!(all[0].detail, "object (required)");
        let typed = at(6, 4);
        assert!(typed.is_empty(), "{typed:?}");
        let top = suggest(
            "apiVersion: apps/v1\nkind: Deployment\nme",
            2,
            2,
            &|_| Some(document()),
            None,
        );
        assert_eq!(labels(&top), ["metadata"]);
        assert_eq!((top[0].start, top[0].end), (0, 2));
        assert_eq!(top[0].insert, "metadata: ");
        // Server-set metadata is not offered.
        let text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  ";
        let meta = suggest(text, 3, 2, &|_| Some(document()), None);
        assert_eq!(labels(&meta), ["labels", "name"]);
    }

    #[test]
    fn suggests_enum_and_boolean_values() {
        let values = at(8, 12);
        assert_eq!(labels(&values), ["RollingUpdate"]);
        assert_eq!((values[0].start, values[0].end), (10, 12));
        assert_eq!(labels(&at(9, 11)), ["true"]);
    }

    #[test]
    fn suggests_kinds_from_the_catalog() {
        use kube::discovery::{ApiCapabilities, ApiResource, Scope};
        let caps = ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec!["list".into()],
        };
        let resource = |group: &str, kind: &str| ApiResource {
            group: group.into(),
            version: "v1".into(),
            api_version: if group.is_empty() {
                "v1".into()
            } else {
                format!("{group}/v1")
            },
            kind: kind.into(),
            plural: kind.to_lowercase(),
        };
        let catalog = ApiCatalog::from_resources(vec![
            (resource("", "ConfigMap"), caps.clone()),
            (resource("apps", "Deployment"), caps),
        ]);
        let found = suggest("kind: Con", 0, 9, &|_| None, Some(&catalog));
        assert_eq!(labels(&found), ["ConfigMap"]);
        assert_eq!(found[0].detail, "v1");
        let found = suggest("apiVersion:", 0, 11, &|_| None, Some(&catalog));
        assert_eq!(labels(&found), ["apps/v1", "v1"]);
        assert_eq!(found[0].insert, " apps/v1");
    }

    #[test]
    fn describes_hovered_fields() {
        let hover = describe_at(TEXT, 8, 5, &|_| Some(document())).unwrap();
        assert!(
            hover.starts_with("`.spec.strategy.type`: **string**"),
            "{hover}"
        );
        assert!(hover.contains("Type of deployment."));
        assert!(hover.contains("`Recreate`, `RollingUpdate`"));
        assert!(describe_at(TEXT, 3, 3, &|_| None).is_none());
    }
}
