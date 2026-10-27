//! Multi-document YAML <-> JSON values, keeping key order and source lines.

use serde_json::Value;

/// One document of a multi-document YAML text.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// Position among the non-empty documents, starting at 0.
    pub index: usize,
    /// First line of the document's content (1-based), for error reporting.
    pub line: usize,
    pub value: Value,
}

/// A YAML syntax error, positioned in the whole text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("document {}, line {line}: {message}", index + 1)]
pub struct YamlError {
    pub index: usize,
    /// 1-based line in the whole text.
    pub line: usize,
    /// 1-based column, 0 when unknown.
    pub column: usize,
    pub message: String,
}

/// A document's raw text and the line it starts on.
pub(crate) struct Chunk {
    pub(crate) text: String,
    pub(crate) line: usize,
}

/// Splits on `---` / `...` markers at column 0. YAML forbids such lines
/// inside content, so this split matches the parser's.
pub(crate) fn split_documents(text: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut start = 1;
    for (number, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l)) {
        let marker = |m: &str| {
            line.strip_prefix(m)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
        };
        if marker("---") || marker("...") {
            chunks.push(Chunk {
                text: std::mem::take(&mut current),
                line: start,
            });
            // Content may follow the marker on the same line (`--- !tag`).
            let rest = line[3..].trim_start();
            if marker("---") && !rest.is_empty() && !rest.starts_with('#') {
                current.push_str(rest);
                current.push('\n');
                start = number;
            } else {
                start = number + 1;
            }
            continue;
        }
        if current.is_empty() && is_blank(line) {
            start = number + 1;
        }
        current.push_str(line);
        current.push('\n');
    }
    chunks.push(Chunk {
        text: current,
        line: start,
    });
    chunks
}

pub(crate) fn is_blank(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with('#')
}

/// YAML 1.2 core schema: only `true`/`false` are booleans, so `name: n`
/// or `on: yes` stay strings (no "Norway problem").
pub(crate) fn options() -> serde_saphyr::Options {
    let mut options = serde_saphyr::Options::default();
    options.strict_booleans = true;
    options.with_snippet = false;
    options
}

/// Parses every non-empty document. Each must be a mapping.
pub fn parse(text: &str) -> Result<Vec<Manifest>, YamlError> {
    let mut manifests = Vec::new();
    for chunk in split_documents(text) {
        if chunk.text.lines().all(is_blank) {
            continue;
        }
        let index = manifests.len();
        let value: Value =
            serde_saphyr::from_str_with_options(&chunk.text, options()).map_err(|error| {
                let (line, column) = error
                    .location()
                    .map(|loc| (loc.line() as usize, loc.column() as usize))
                    .unwrap_or((1, 0));
                YamlError {
                    index,
                    line: chunk.line + line.max(1) - 1,
                    column,
                    message: first_line(&error.to_string()),
                }
            })?;
        if !value.is_object() {
            return Err(YamlError {
                index,
                line: chunk.line,
                column: 0,
                message: "document is not a mapping".into(),
            });
        }
        manifests.push(Manifest {
            index,
            line: chunk.line,
            value,
        });
    }
    Ok(manifests)
}

/// The library's message without its rendered source snippet.
pub(crate) fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Serializes one document in block style, keeping key order.
pub fn to_yaml(value: &Value) -> String {
    serde_saphyr::to_string(value).unwrap_or_else(|error| format!("# {error}\n"))
}

/// Serializes documents separated by `---`.
pub fn to_yaml_multi<'a>(values: impl IntoIterator<Item = &'a Value>) -> String {
    values
        .into_iter()
        .map(to_yaml)
        .collect::<Vec<_>>()
        .join("---\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_multiple_documents_with_lines() {
        let text = "\
# leading comment
apiVersion: v1
kind: ConfigMap
---
---
# only a comment
---
apiVersion: v1
kind: Secret
...
";
        let docs = parse(text).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].line, 2);
        assert_eq!(docs[0].value["kind"], "ConfigMap");
        assert_eq!(docs[1].index, 1);
        assert_eq!(docs[1].line, 8);
        assert_eq!(docs[1].value["kind"], "Secret");
    }

    #[test]
    fn keeps_key_order() {
        let docs = parse("kind: A\napiVersion: v1\nmetadata:\n  name: x\n").unwrap();
        let keys: Vec<_> = docs[0].value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["kind", "apiVersion", "metadata"]);
        assert_eq!(
            to_yaml(&docs[0].value),
            "kind: A\napiVersion: v1\nmetadata:\n  name: x\n"
        );
    }

    #[test]
    fn reports_document_and_absolute_line() {
        let text = "a: 1\n---\nb: 2\nc: [1, 2\nd: 3\n";
        let error = parse(text).unwrap_err();
        assert_eq!(error.index, 1);
        assert!(error.line >= 4, "{error:?}");
        assert!(!error.message.is_empty());
    }

    #[test]
    fn rejects_non_mapping_document() {
        let error = parse("a: 1\n---\n- x\n").unwrap_err();
        assert_eq!((error.index, error.line), (1, 3));
    }

    #[test]
    fn round_trips_awkward_scalars() {
        let value = json!({
            "data": {
                "flag": "true",
                "yes": "yes",
                "n": "n",
                "on": "on",
                "tilde": "~",
                "octal": "0755",
                "number": "08",
                "empty": "",
                "multi": "line one\nline two\n",
                "colon": "a: b",
            },
            "replicas": 3,
            "ratio": 0.5,
            "none": null,
            "list": [1, "two"],
            "emptyMap": {},
        });
        let text = to_yaml(&value);
        let back = parse(&text).unwrap();
        assert_eq!(back[0].value, value, "{text}");
    }

    #[test]
    fn yaml_1_1_booleans_are_strings() {
        let docs = parse("a: n\nb: yes\nc: On\nd: true\n").unwrap();
        assert_eq!(
            docs[0].value,
            json!({"a": "n", "b": "yes", "c": "On", "d": true})
        );
    }

    #[test]
    fn joins_documents() {
        let a = json!({"kind": "A"});
        let b = json!({"kind": "B"});
        assert_eq!(to_yaml_multi([&a, &b]), "kind: A\n---\nkind: B\n");
    }
}
