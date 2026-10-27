//! Cluster OpenAPI v3 schemas: fetching, validation and completion.
//!
//! Validation is a pragmatic subset of what the API server checks: types,
//! `required`, `enum`, unknown fields, `x-kubernetes-preserve-unknown-fields`
//! and int-or-string. The server stays the final authority (dry-run).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kube::Client;
use kube::api::GroupVersionKind;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::path::{FieldPath, PathSegment};
use crate::sync;

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("failed to fetch {path}: {source}")]
    Fetch {
        path: String,
        #[source]
        source: Box<kube::Error>,
    },
    #[error("invalid OpenAPI document {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// One OpenAPI v3 document (a group-version), indexed by GVK.
#[derive(Debug, Default)]
pub struct SchemaDocument {
    schemas: Map<String, Value>,
    by_gvk: HashMap<(String, String, String), String>,
}

const REF_PREFIX: &str = "#/components/schemas/";
const MAX_DEPTH: usize = 64;

impl SchemaDocument {
    /// Builds from a parsed `/openapi/v3/...` document.
    pub fn from_value(mut document: Value) -> Self {
        let schemas = match document.pointer_mut("/components/schemas").map(Value::take) {
            Some(Value::Object(schemas)) => schemas,
            _ => Map::new(),
        };
        let mut by_gvk = HashMap::new();
        for (name, schema) in &schemas {
            let gvks = schema
                .get("x-kubernetes-group-version-kind")
                .and_then(Value::as_array);
            for gvk in gvks.into_iter().flatten() {
                let field = |key| gvk.get(key).and_then(Value::as_str).unwrap_or_default();
                by_gvk.insert(
                    (
                        field("group").to_string(),
                        field("version").to_string(),
                        field("kind").to_string(),
                    ),
                    name.clone(),
                );
            }
        }
        Self { schemas, by_gvk }
    }

    /// The top-level schema of a kind.
    pub fn root(&self, gvk: &GroupVersionKind) -> Option<&Value> {
        let name = self
            .by_gvk
            .get(&(gvk.group.clone(), gvk.version.clone(), gvk.kind.clone()))?;
        self.schemas.get(name)
    }

    /// Follows `$ref` and single-element `allOf` wrappers.
    fn resolve<'a>(&'a self, mut schema: &'a Value) -> &'a Value {
        for _ in 0..MAX_DEPTH {
            if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
                match target
                    .strip_prefix(REF_PREFIX)
                    .and_then(|name| self.schemas.get(name))
                {
                    Some(next) => schema = next,
                    None => return schema,
                }
            } else if let Some([only]) = schema
                .get("allOf")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                && schema.get("type").is_none()
                && schema.get("properties").is_none()
            {
                schema = only;
            } else {
                break;
            }
        }
        schema
    }

    /// Checks a document against its kind's schema. `None` when the kind
    /// has no schema.
    pub fn validate(&self, gvk: &GroupVersionKind, document: &Value) -> Option<Vec<Violation>> {
        let root = self.root(gvk)?;
        let mut out = Vec::new();
        self.check(root, document, &FieldPath::root(), 0, &mut out);
        Some(out)
    }

    fn check(
        &self,
        schema: &Value,
        value: &Value,
        path: &FieldPath,
        depth: usize,
        out: &mut Vec<Violation>,
    ) {
        if depth > MAX_DEPTH || value.is_null() {
            // null clears a field in apply; the server validates the rest.
            return;
        }
        let schema = self.resolve(schema);
        let flag = |key| schema.get(key).and_then(Value::as_bool) == Some(true);
        let mut push = |message: String| {
            out.push(Violation {
                path: path.clone(),
                message,
            })
        };

        if flag("x-kubernetes-int-or-string")
            || schema.get("format").and_then(Value::as_str) == Some("int-or-string")
        {
            if !(value.is_string() || is_integer(value)) {
                push(format!(
                    "expected integer or string, got {}",
                    type_name(value)
                ));
            }
            return;
        }

        if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
            for part in all {
                self.check(part, value, path, depth + 1, out);
            }
            return self.check_own(schema, value, path, depth, out);
        }

        // anyOf/oneOf as type alternatives (e.g. Quantity: string | number).
        if schema.get("type").is_none()
            && let Some(alternatives) = schema
                .get("anyOf")
                .or_else(|| schema.get("oneOf"))
                .and_then(Value::as_array)
            && alternatives
                .iter()
                .all(|alt| self.resolve(alt).get("type").is_some())
        {
            let matches = alternatives.iter().any(|alt| {
                let mut errors = Vec::new();
                self.check(alt, value, path, depth + 1, &mut errors);
                errors.is_empty()
            });
            if !matches {
                let types: Vec<_> = alternatives
                    .iter()
                    .filter_map(|alt| self.resolve(alt).get("type").and_then(Value::as_str))
                    .collect();
                push(format!(
                    "expected {}, got {}",
                    types.join(" or "),
                    type_name(value)
                ));
            }
            return;
        }

        self.check_own(schema, value, path, depth, out);
    }

    /// Checks `type`, `enum` and object/array contents of a resolved schema.
    fn check_own(
        &self,
        schema: &Value,
        value: &Value,
        path: &FieldPath,
        depth: usize,
        out: &mut Vec<Violation>,
    ) {
        let mut push = |message: String| {
            out.push(Violation {
                path: path.clone(),
                message,
            })
        };
        let declared = schema.get("type").and_then(Value::as_str);
        let kind = declared.or_else(|| schema.get("properties").map(|_| "object"));
        let ok = match kind {
            Some("object") => value.is_object(),
            Some("array") => value.is_array(),
            Some("string") => value.is_string(),
            Some("integer") => is_integer(value),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            _ => true,
        };
        if !ok {
            push(format!(
                "expected {}, got {}",
                kind.unwrap_or("?"),
                type_name(value)
            ));
            return;
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
            && !allowed.contains(value)
        {
            let list: Vec<_> = allowed.iter().map(Value::to_string).collect();
            push(format!("must be one of {}", list.join(", ")));
        }

        match value {
            Value::Object(map) => self.check_object(schema, map, path, depth, out),
            Value::Array(items) => {
                if let Some(item_schema) = schema.get("items") {
                    for (index, item) in items.iter().enumerate() {
                        self.check(item_schema, item, &path.index(index), depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn check_object(
        &self,
        schema: &Value,
        map: &Map<String, Value>,
        path: &FieldPath,
        depth: usize,
        out: &mut Vec<Violation>,
    ) {
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        let preserve = schema
            .get("x-kubernetes-preserve-unknown-fields")
            .and_then(Value::as_bool)
            == Some(true);

        for field in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !map.contains_key(field) {
                out.push(Violation {
                    path: path.key(field),
                    message: "required field is missing".into(),
                });
            }
        }

        for (key, value) in map {
            let child = path.key(key);
            if let Some(property) = properties.and_then(|p| p.get(key)) {
                self.check(property, value, &child, depth + 1, out);
            } else if let Some(additional) = additional.filter(|a| a.is_object()) {
                self.check(additional, value, &child, depth + 1, out);
            } else if properties.is_some_and(|p| !p.is_empty())
                && !preserve
                && additional.and_then(Value::as_bool) != Some(true)
            {
                out.push(Violation {
                    path: child,
                    message: "unknown field".into(),
                });
            }
        }
    }

    /// The schema at `path` inside a kind, descending through `$ref`s,
    /// `additionalProperties` and array items.
    fn schema_at(&self, gvk: &GroupVersionKind, path: &[PathSegment]) -> Option<&Value> {
        let mut schema = self.resolve(self.root(gvk)?);
        for segment in path {
            schema = match segment {
                PathSegment::Key(key) => schema
                    .get("properties")
                    .and_then(|p| p.get(key))
                    .or_else(|| schema.get("additionalProperties").filter(|a| a.is_object()))?,
                PathSegment::Index(_) => schema.get("items")?,
            };
            schema = self.resolve(schema);
        }
        Some(schema)
    }

    /// Type and docs of the field at `path`, for hovers.
    pub fn describe(&self, gvk: &GroupVersionKind, path: &FieldPath) -> Option<FieldInfo> {
        let (parent, last) = match path.segments().split_last() {
            Some((last, parent)) => (parent, Some(last)),
            None => (&[][..], None),
        };
        let raw = match last {
            None => self.root(gvk)?,
            Some(last) => {
                let parent = self.schema_at(gvk, parent)?;
                match last {
                    PathSegment::Key(key) => parent
                        .get("properties")
                        .and_then(|p| p.get(key))
                        .or_else(|| parent.get("additionalProperties").filter(|a| a.is_object()))?,
                    PathSegment::Index(_) => parent.get("items")?,
                }
            }
        };
        Some(self.field_info(raw))
    }

    fn field_info(&self, raw: &Value) -> FieldInfo {
        let schema = self.resolve(raw);
        let description = raw
            .get("description")
            .or_else(|| schema.get("description"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let enum_values = schema
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        FieldInfo {
            type_label: self.type_label(raw, 0),
            description,
            enum_values,
        }
    }

    /// Fields that can be written in the mapping at `path`. For a list
    /// path, the fields of its items. Sorted with required fields first.
    pub fn completions(&self, gvk: &GroupVersionKind, path: &FieldPath) -> Vec<Completion> {
        let Some(mut schema) = self.schema_at(gvk, path.segments()) else {
            return Vec::new();
        };
        if schema.get("type").and_then(Value::as_str) == Some("array")
            && let Some(items) = schema.get("items")
        {
            schema = self.resolve(items);
        }
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return Vec::new();
        };
        let mut out: Vec<_> = properties
            .iter()
            .map(|(name, raw)| {
                let info = self.field_info(raw);
                Completion {
                    name: name.clone(),
                    required: required.contains(&name.as_str()),
                    type_label: info.type_label,
                    description: info.description,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.required
                .cmp(&a.required)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// The `spec` of a new object of `gvk` with only its required fields,
    /// each set to a placeholder of its type (#320). `None` when the kind
    /// has no schema or no `spec`.
    pub fn required_spec(&self, gvk: &GroupVersionKind) -> Option<Value> {
        let root = self.resolve(self.root(gvk)?);
        let spec = root.get("properties")?.get("spec")?;
        Some(self.required_value(spec, 0))
    }

    /// A placeholder value for `raw`: objects hold their required fields,
    /// arrays one item when items have required fields, scalars a zero
    /// value (or the first enum value).
    fn required_value(&self, raw: &Value, depth: usize) -> Value {
        let schema = self.resolve(raw);
        if depth > 8 {
            return Value::Null;
        }
        if let Some(first) = schema
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
        {
            return first.clone();
        }
        if schema
            .get("x-kubernetes-int-or-string")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Value::String(String::new());
        }
        let required = |schema: &Value| -> Vec<String> {
            schema
                .get("required")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        match schema.get("type").and_then(Value::as_str) {
            Some("string") => Value::String(String::new()),
            Some("integer") | Some("number") => Value::from(0),
            Some("boolean") => Value::Bool(false),
            Some("array") => match schema.get("items") {
                Some(items) if !required(self.resolve(items)).is_empty() => {
                    Value::Array(vec![self.required_value(items, depth + 1)])
                }
                _ => Value::Array(Vec::new()),
            },
            _ => {
                let properties = schema.get("properties");
                let mut out = Map::new();
                for key in required(schema) {
                    let value = properties
                        .and_then(|p| p.get(&key))
                        .map(|property| self.required_value(property, depth + 1))
                        .unwrap_or(Value::Null);
                    out.insert(key, value);
                }
                Value::Object(out)
            }
        }
    }

    /// `string`, `[]Container`, `map[string]string`, `int-or-string`…
    fn type_label(&self, raw: &Value, depth: usize) -> String {
        if depth > 8 {
            return "object".into();
        }
        let reference = raw
            .get("$ref")
            .or_else(|| raw.pointer("/allOf/0/$ref"))
            .and_then(Value::as_str);
        let schema = self.resolve(raw);
        if schema
            .get("x-kubernetes-int-or-string")
            .and_then(Value::as_bool)
            == Some(true)
            || schema.get("format").and_then(Value::as_str) == Some("int-or-string")
        {
            return "int-or-string".into();
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("array") => match schema.get("items") {
                Some(items) => format!("[]{}", self.type_label(items, depth + 1)),
                None => "[]".into(),
            },
            Some("object") | None if schema.get("properties").is_none() => {
                match schema.get("additionalProperties").filter(|a| a.is_object()) {
                    Some(values) => format!("map[string]{}", self.type_label(values, depth + 1)),
                    None => short_name(reference).unwrap_or_else(|| {
                        schema
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("any")
                            .into()
                    }),
                }
            }
            Some("object") | None => short_name(reference).unwrap_or_else(|| "object".into()),
            Some(other) => other.to_string(),
        }
    }
}

fn short_name(reference: Option<&str>) -> Option<String> {
    let name = reference?.strip_prefix(REF_PREFIX)?;
    Some(name.rsplit('.').next().unwrap_or(name).to_string())
}

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A schema violation at a path in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub path: FieldPath,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Type and docs of one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub type_label: String,
    pub description: String,
    pub enum_values: Vec<String>,
}

/// A field name candidate for autocomplete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub name: String,
    pub type_label: String,
    pub description: String,
    pub required: bool,
}

/// `/openapi/v3` index: group-version path -> document URL.
#[derive(Debug, Deserialize)]
struct OpenApiIndex {
    paths: HashMap<String, IndexEntry>,
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    #[serde(rename = "serverRelativeURL")]
    server_relative_url: String,
}

/// Per-connection cache of OpenAPI v3 documents, fetched lazily per
/// group-version. Call [`SchemaCache::clear`] when CRDs change.
pub struct SchemaCache {
    client: Client,
    index: Mutex<Option<Arc<HashMap<String, String>>>>,
    documents: Mutex<HashMap<String, Arc<SchemaDocument>>>,
}

impl SchemaCache {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            index: Mutex::default(),
            documents: Mutex::default(),
        }
    }

    pub fn clear(&self) {
        *sync::lock(&self.index) = None;
        sync::lock(&self.documents).clear();
    }

    /// The document describing `gvk`'s group-version, or `None` if the
    /// cluster publishes none for it.
    pub async fn document(
        &self,
        gvk: &GroupVersionKind,
    ) -> Result<Option<Arc<SchemaDocument>>, SchemaError> {
        let key = group_version_path(gvk);
        if let Some(document) = sync::lock(&self.documents).get(&key) {
            return Ok(Some(document.clone()));
        }
        let index = self.index().await?;
        let Some(url) = index.get(&key) else {
            return Ok(None);
        };
        let document = Arc::new(SchemaDocument::from_value(self.fetch(url).await?));
        sync::lock(&self.documents).insert(key, document.clone());
        Ok(Some(document))
    }

    async fn index(&self) -> Result<Arc<HashMap<String, String>>, SchemaError> {
        if let Some(index) = sync::lock(&self.index).clone() {
            return Ok(index);
        }
        let path = "/openapi/v3";
        let index: OpenApiIndex =
            serde_json::from_value(self.fetch(path).await?).map_err(|source| {
                SchemaError::Parse {
                    path: path.into(),
                    source,
                }
            })?;
        let index = Arc::new(
            index
                .paths
                .into_iter()
                .map(|(gv, entry)| (gv, entry.server_relative_url))
                .collect(),
        );
        *sync::lock(&self.index) = Some(Arc::clone(&index));
        Ok(index)
    }

    async fn fetch(&self, path: &str) -> Result<Value, SchemaError> {
        let request = http::Request::get(path)
            .header(http::header::ACCEPT, "application/json")
            .body(Vec::new())
            .expect("static request is valid");
        let text =
            self.client
                .request_text(request)
                .await
                .map_err(|source| SchemaError::Fetch {
                    path: path.into(),
                    source: Box::new(source),
                })?;
        serde_json::from_str(&text).map_err(|source| SchemaError::Parse {
            path: path.into(),
            source,
        })
    }
}

/// `api/v1` for the core group, `apis/<group>/<version>` otherwise.
fn group_version_path(gvk: &GroupVersionKind) -> String {
    if gvk.group.is_empty() {
        format!("api/{}", gvk.version)
    } else {
        format!("apis/{}/{}", gvk.group, gvk.version)
    }
}

#[cfg(test)]
mod tests;
