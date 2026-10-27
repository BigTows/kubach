//! Keys of ConfigMaps and Secrets edited one by one (#275): what a value
//! holds (JSON, YAML, properties, plain text), its check, JSON formatting,
//! and ConfigMap `data` / `binaryData` read from and written back into the
//! object the YAML editor applies.
//!
//! A check never blocks saving: a value is just a string to Kubernetes, so
//! a JSON or YAML error is a warning shown under the key.

use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use serde_json::{Map, Value};

use crate::apply::yaml::{self, YamlError};
use crate::validation::{Field, ValidationError};

/// What a value holds, which picks its highlighting and its check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    Json,
    Yaml,
    /// `key=value` lines: `.properties`, `.env`.
    Properties,
    Text,
}

impl ContentKind {
    pub const ALL: [ContentKind; 4] = [Self::Json, Self::Yaml, Self::Properties, Self::Text];

    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Properties => "Properties",
            Self::Text => "Text",
        }
    }

    /// Language name of the code editor's highlighter.
    pub fn language(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Properties | Self::Text => "text",
        }
    }
}

/// The kind a key's extension names (`app.json`, `values.yaml`, `.env`),
/// `None` when the name says nothing.
pub fn kind_by_key(key: &str) -> Option<ContentKind> {
    let lower = key.to_ascii_lowercase();
    if lower == ".env" || lower.ends_with(".env") || lower.starts_with(".env.") {
        return Some(ContentKind::Properties);
    }
    let extension = lower.rsplit_once('.').map(|(_, ext)| ext)?;
    match extension {
        "json" | "jsonc" | "json5" => Some(ContentKind::Json),
        "yaml" | "yml" => Some(ContentKind::Yaml),
        "properties" | "env" => Some(ContentKind::Properties),
        "txt" | "conf" | "cfg" | "cnf" | "ini" | "toml" | "xml" | "html" | "sh" | "sql" | "pem"
        | "crt" | "key" | "csv" | "md" | "lua" | "py" | "js" => Some(ContentKind::Text),
        _ => None,
    }
}

/// The kind of `value` under `key`: by the key's extension, else by the
/// content.
pub fn detect(key: &str, value: &str) -> ContentKind {
    kind_by_key(key).unwrap_or_else(|| sniff(value))
}

static PROPERTY_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(export\s+)?[A-Za-z_][A-Za-z0-9_.\-]*\s*=").expect("valid regex")
});
static YAML_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Za-z0-9_."'/\-]+:(\s|$)|- )"#).expect("valid regex"));
static INI_SECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\[[^\]",{}]*\]\s*$"#).expect("valid regex"));

/// Guesses the kind from the content. Stable while typing: an unfinished
/// JSON object is still JSON (and shows its error).
fn sniff(value: &str) -> ContentKind {
    let trimmed = value.trim_start();
    if trimmed.starts_with('{') {
        return ContentKind::Json;
    }
    if trimmed.starts_with('[') {
        let first = trimmed.lines().next().unwrap_or_default().trim();
        let ini = INI_SECTION.is_match(first) && serde_json::from_str::<Value>(value).is_err();
        return if ini {
            ContentKind::Text
        } else {
            ContentKind::Json
        };
    }
    let lines: Vec<&str> = value
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect();
    let Some(first) = lines.first() else {
        return ContentKind::Text;
    };
    if lines
        .iter()
        .all(|line| PROPERTY_LINE.is_match(line.trim_start()))
    {
        return ContentKind::Properties;
    }
    if value.trim().lines().count() >= 2 && YAML_LINE.is_match(first) {
        return ContentKind::Yaml;
    }
    ContentKind::Text
}

/// Where a value fails its check; lines and columns from 0.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {}: {message}", line + 1)]
pub struct ValueProblem {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Checks `text` as `kind`. Empty values and plain text always pass.
pub fn check(kind: ContentKind, text: &str) -> Option<ValueProblem> {
    if text.trim().is_empty() {
        return None;
    }
    match kind {
        ContentKind::Json => check_json(text).err(),
        ContentKind::Yaml => check_yaml(text),
        ContentKind::Properties => check_properties(text),
        ContentKind::Text => None,
    }
}

fn check_json(text: &str) -> Result<Value, ValueProblem> {
    serde_json::from_str::<Value>(text).map_err(|error| {
        let message = error.to_string();
        // serde_json appends " at line L column C"; the position is kept apart.
        let message = message
            .rsplit_once(" at line ")
            .map_or(message.as_str(), |(head, _)| head)
            .to_owned();
        ValueProblem {
            line: error.line().saturating_sub(1),
            column: error.column().saturating_sub(1),
            message: format!("Invalid JSON: {message}"),
        }
    })
}

fn check_yaml(text: &str) -> Option<ValueProblem> {
    for chunk in yaml::split_documents(text) {
        if chunk.text.lines().all(yaml::is_blank) {
            continue;
        }
        if let Err(error) =
            serde_saphyr::from_str_with_options::<Value>(&chunk.text, yaml::options())
        {
            let (line, column) = error
                .location()
                .map(|loc| (loc.line() as usize, loc.column() as usize))
                .unwrap_or((1, 1));
            return Some(ValueProblem {
                line: (chunk.line + line.max(1) - 1).saturating_sub(1),
                column: column.saturating_sub(1),
                message: format!("Invalid YAML: {}", yaml::first_line(&error.to_string())),
            });
        }
    }
    None
}

/// `key=value` or `key: value` lines; `#` and `!` comments, `export`
/// prefixes and `\` continuations are allowed.
fn check_properties(text: &str) -> Option<ValueProblem> {
    let mut continued = false;
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let continues = trailing_backslashes(trimmed) % 2 == 1;
        if continued {
            continued = continues;
            continue;
        }
        continued = continues;
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let key = body
            .find(['=', ':'])
            .map(|at| body[..at].trim())
            .unwrap_or_default();
        if key.is_empty() {
            return Some(ValueProblem {
                line: number,
                column: line.len() - line.trim_start().len(),
                message: "Expected key=value".into(),
            });
        }
    }
    None
}

fn trailing_backslashes(line: &str) -> usize {
    line.chars().rev().take_while(|c| *c == '\\').count()
}

/// `text` as JSON indented by two spaces, keys in their order.
pub fn format_json(text: &str) -> Result<String, ValueProblem> {
    let value = check_json(text)?;
    let mut formatted = serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_owned());
    if text.ends_with('\n') {
        formatted.push('\n');
    }
    Ok(formatted)
}

/// ConfigMap and Secret keys: `[-._a-zA-Z0-9]+`, at most 253 characters,
/// not `.` or `..`.
pub fn validate_key(key: &str) -> Result<(), ValidationError> {
    crate::secrets::validate_key(key)
}

// ----------------------------------------------------------------- ConfigMap

/// A ConfigMap value: text from `data`, or `binaryData` kept as its base64.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataValue {
    Text(String),
    Binary(String),
}

impl DataValue {
    pub fn binary(bytes: &[u8]) -> Self {
        Self::Binary(STANDARD.encode(bytes))
    }

    /// Size in bytes (decoded for binary values).
    pub fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(base64) => STANDARD
                .decode(base64.trim())
                .map_or_else(|_| base64_len(base64), |bytes| bytes.len()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn base64_len(s: &str) -> usize {
    let s = s.trim();
    let padding = s.chars().rev().take_while(|c| *c == '=').count();
    (s.len() / 4 * 3).saturating_sub(padding)
}

/// One key of a ConfigMap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataEntry {
    pub key: String,
    pub value: DataValue,
}

impl DataEntry {
    pub fn text(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: DataValue::Text(value.into()),
        }
    }
}

/// Keys of a ConfigMap object: `data` in order, then `binaryData`.
pub fn configmap_entries(object: &Value) -> Vec<DataEntry> {
    let field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                (key.clone(), text)
            })
            .collect::<Vec<_>>()
    };
    let mut entries: Vec<DataEntry> = field("data")
        .into_iter()
        .map(|(key, text)| DataEntry::text(key, text))
        .collect();
    entries.extend(
        field("binaryData")
            .into_iter()
            .map(|(key, base64)| DataEntry {
                key,
                value: DataValue::Binary(base64),
            }),
    );
    entries
}

/// Every key valid and used once (a key may not be in both `data` and
/// `binaryData`).
pub fn validate_entries(entries: &[DataEntry]) -> Result<(), ValidationError> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        validate_key(&entry.key)?;
        if !seen.insert(entry.key.as_str()) {
            return Err(ValidationError::Duplicate {
                field: Field::SecretKey,
                value: entry.key.clone(),
            });
        }
    }
    Ok(())
}

/// Replaces `data` and `binaryData` of `object` with `entries`, in their
/// order. An empty map is dropped, so applying it removes the field.
pub fn set_configmap_entries(
    object: &mut Value,
    entries: &[DataEntry],
) -> Result<(), ValidationError> {
    validate_entries(entries)?;
    let Some(object) = object.as_object_mut() else {
        return Ok(());
    };
    let mut data = Map::new();
    let mut binary = Map::new();
    for entry in entries {
        match &entry.value {
            DataValue::Text(text) => data.insert(entry.key.clone(), Value::String(text.clone())),
            DataValue::Binary(base64) => {
                binary.insert(entry.key.clone(), Value::String(base64.clone()))
            }
        };
    }
    for (name, map) in [("data", data), ("binaryData", binary)] {
        if map.is_empty() {
            object.remove(name);
        } else {
            object.insert(name.to_owned(), Value::Object(map));
        }
    }
    Ok(())
}

/// Why the YAML text can't be edited by keys.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeysError {
    #[error("The YAML has an error: {0}")]
    Yaml(#[from] YamlError),
    #[error("The text must be a single ConfigMap document, found {0}")]
    Documents(usize),
    #[error("The document is a {0}, not a ConfigMap")]
    NotConfigMap(String),
    #[error("{0}")]
    Key(#[from] ValidationError),
}

/// The single ConfigMap of an editor's text.
pub fn read_configmap(text: &str) -> Result<Value, KeysError> {
    let mut manifests = yaml::parse(text)?;
    if manifests.len() != 1 {
        return Err(KeysError::Documents(manifests.len()));
    }
    let value = manifests.remove(0).value;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let api_version = value
        .get("apiVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind != "ConfigMap" || api_version != "v1" {
        return Err(KeysError::NotConfigMap(if kind.is_empty() {
            "document without kind".into()
        } else {
            kind.to_owned()
        }));
    }
    Ok(value)
}

/// `text` (one ConfigMap) with its keys replaced by `entries`, as YAML.
/// Everything else in the object is kept; comments are not.
pub fn rewrite_configmap(text: &str, entries: &[DataEntry]) -> Result<String, KeysError> {
    let mut value = read_configmap(text)?;
    set_configmap_entries(&mut value, entries)?;
    Ok(yaml::to_yaml(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_follows_the_key_extension() {
        assert_eq!(detect("app.json", "plain"), ContentKind::Json);
        assert_eq!(detect("values.YML", "{}"), ContentKind::Yaml);
        assert_eq!(detect("app.properties", ""), ContentKind::Properties);
        assert_eq!(detect(".env", ""), ContentKind::Properties);
        assert_eq!(detect("prod.env", ""), ContentKind::Properties);
        assert_eq!(detect("nginx.conf", "{ x }"), ContentKind::Text);
        assert_eq!(kind_by_key("LOG_LEVEL"), None);
    }

    #[test]
    fn kind_follows_the_content_without_an_extension() {
        assert_eq!(detect("config", "{\"a\": 1}"), ContentKind::Json);
        // Unfinished JSON stays JSON while typing.
        assert_eq!(detect("config", "{\"a\": "), ContentKind::Json);
        assert_eq!(detect("list", "[1, 2]"), ContentKind::Json);
        assert_eq!(detect("my", "[mysqld]\nport=3306\n"), ContentKind::Text);
        assert_eq!(
            detect("settings", "server:\n  port: 8080\n"),
            ContentKind::Yaml
        );
        assert_eq!(detect("items", "- a\n- b\n"), ContentKind::Yaml);
        assert_eq!(
            detect("vars", "# comment\nFOO=1\nexport BAR=two\n"),
            ContentKind::Properties
        );
        assert_eq!(detect("level", "debug"), ContentKind::Text);
        assert_eq!(detect("note", "Note: one line"), ContentKind::Text);
        assert_eq!(detect("empty", ""), ContentKind::Text);
    }

    #[test]
    fn json_errors_are_positioned() {
        assert_eq!(check(ContentKind::Json, "{\"a\": 1}"), None);
        assert_eq!(check(ContentKind::Json, "  "), None);
        let problem = check(ContentKind::Json, "{\n  \"a\": 1,\n}").unwrap();
        assert_eq!(problem.line, 2);
        assert!(problem.message.starts_with("Invalid JSON: "), "{problem}");
        assert!(!problem.message.contains("at line"), "{problem}");
        assert!(problem.to_string().starts_with("line 3: "));
    }

    #[test]
    fn yaml_errors_are_positioned_across_documents() {
        assert_eq!(check(ContentKind::Yaml, "a: 1\n---\nb: 2\n"), None);
        let problem = check(ContentKind::Yaml, "a: 1\n---\nb: [1, 2\n").unwrap();
        assert!(problem.line >= 2, "{problem:?}");
        assert!(problem.message.starts_with("Invalid YAML: "), "{problem}");
    }

    #[test]
    fn properties_lines_need_a_key() {
        let ok = "# c\n! c\nFOO=1\nbar: 2\nlong=a \\\n  continued\n\nexport X=1\n";
        assert_eq!(check(ContentKind::Properties, ok), None);
        let problem = check(ContentKind::Properties, "FOO=1\njust words\n").unwrap();
        assert_eq!(problem.line, 1);
        assert_eq!(check(ContentKind::Properties, "=value").unwrap().line, 0);
        assert_eq!(check(ContentKind::Text, "{ broken"), None);
    }

    #[test]
    fn formats_json_keeping_key_order() {
        assert_eq!(
            format_json("{\"b\":1,\"a\":[1,2]}").unwrap(),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(format_json("{}\n").unwrap(), "{}\n");
        assert!(format_json("{").is_err());
    }

    #[test]
    fn reads_and_writes_configmap_keys() {
        let mut object = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "app"},
            "data": {"b": "2", "a": "1"},
            "binaryData": {"logo.png": "AAEC"},
            "immutable": false
        });
        let entries = configmap_entries(&object);
        assert_eq!(
            entries,
            vec![
                DataEntry::text("b", "2"),
                DataEntry::text("a", "1"),
                DataEntry {
                    key: "logo.png".into(),
                    value: DataValue::Binary("AAEC".into())
                },
            ]
        );
        assert_eq!(entries[2].value.len(), 3);

        // Rename b → config.json, drop the binary key, add one.
        let edited = vec![
            DataEntry::text("config.json", "{\"x\": 1}"),
            DataEntry::text("a", "1"),
        ];
        set_configmap_entries(&mut object, &edited).unwrap();
        assert_eq!(
            object,
            json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {"name": "app"},
                "data": {"config.json": "{\"x\": 1}", "a": "1"},
                "immutable": false
            })
        );
        // `data` keeps its place among the fields.
        let keys: Vec<&String> = object.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["apiVersion", "kind", "metadata", "data", "immutable"]
        );

        set_configmap_entries(&mut object, &[]).unwrap();
        assert!(object.get("data").is_none());
    }

    #[test]
    fn rejects_bad_and_repeated_keys() {
        let mut object = json!({"kind": "ConfigMap"});
        let bad = [DataEntry::text("a b", "")];
        assert!(matches!(
            set_configmap_entries(&mut object, &bad),
            Err(ValidationError::InvalidChars { .. })
        ));
        let twice = [
            DataEntry::text("a", ""),
            DataEntry {
                key: "a".into(),
                value: DataValue::binary(b"x"),
            },
        ];
        assert_eq!(
            validate_entries(&twice).unwrap_err().to_string(),
            "Key a is used twice"
        );
        assert!(validate_entries(&[DataEntry::text("..", "")]).is_err());
        assert_eq!(object, json!({"kind": "ConfigMap"}));
    }

    #[test]
    fn rewrites_the_editor_text() {
        let text = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: app\ndata:\n  a: '1'\n";
        let rewritten =
            rewrite_configmap(text, &[DataEntry::text("b", "{\n  \"x\": 1\n}")]).unwrap();
        let value = read_configmap(&rewritten).unwrap();
        assert_eq!(value["data"], json!({"b": "{\n  \"x\": 1\n}"}));
        assert_eq!(value["metadata"]["name"], "app");

        assert!(matches!(
            read_configmap("kind: Secret\napiVersion: v1\n"),
            Err(KeysError::NotConfigMap(kind)) if kind == "Secret"
        ));
        assert!(matches!(
            read_configmap(&format!("{text}---\n{text}")),
            Err(KeysError::Documents(2))
        ));
        assert!(matches!(read_configmap("a: [1"), Err(KeysError::Yaml(_))));
    }
}
