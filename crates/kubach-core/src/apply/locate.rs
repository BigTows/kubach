//! Mapping field paths to text positions and back, for diagnostics and
//! hovers.
//!
//! Indentation-based like [`super::cursor`], so it also works on text that
//! does not parse. Documents are separated by `---` / `...` lines.

use kube::api::GroupVersionKind;

use super::cursor::{KeyLine, Walker};
use super::path::FieldPath;

fn is_marker(line: &str) -> bool {
    ["---", "..."].iter().any(|marker| {
        line.strip_prefix(marker)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
    })
}

/// 0-based line range of the document containing `line`, markers excluded.
fn document_range(text: &str, line: usize) -> std::ops::Range<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let line = line.min(lines.len());
    let start = lines[..line]
        .iter()
        .rposition(|l| is_marker(l))
        .map_or(0, |marker| marker + 1);
    let end = lines[line..]
        .iter()
        .position(|l| is_marker(l))
        .map_or(lines.len(), |marker| line + marker);
    start..end.max(start)
}

/// Every key of the document containing 0-based `line`, in text order.
pub fn key_lines(text: &str, line: usize) -> Vec<KeyLine> {
    let range = document_range(text, line);
    let mut walker = Walker::recording();
    for (number, current) in text.lines().enumerate().skip(range.start).take(range.len()) {
        walker.walk(number, current);
    }
    walker.into_recorded()
}

/// Where `path` is written in the document starting at 1-based
/// `first_line` (as in [`super::Manifest::line`]). A path that is not
/// written (a missing required field, say) maps to its nearest written
/// ancestor; the root maps to the document's first line.
pub fn locate(text: &str, first_line: usize, path: &FieldPath) -> KeyLine {
    let start = first_line.saturating_sub(1);
    let keys = key_lines(text, start);
    let segments = path.segments();
    keys.into_iter()
        .filter(|key| segments.starts_with(key.path.segments()))
        .max_by_key(|key| key.path.segments().len())
        .unwrap_or(KeyLine {
            path: FieldPath::root(),
            line: start,
            column: 0,
            len: 0,
        })
}

/// The path of the key written on 0-based `line` at or before `column`,
/// for hovers (on the key or its value). On `- name: x`, the dash is the
/// list item and `name` the field.
pub fn key_at(text: &str, line: usize, column: usize) -> Option<FieldPath> {
    key_lines(text, line)
        .into_iter()
        .rfind(|key| key.line == line && key.column <= column)
        .map(|key| key.path)
}

/// The kind of the document containing 0-based `line`, from its top-level
/// `apiVersion` and `kind` (the text need not parse).
pub fn kind_at(text: &str, line: usize) -> Option<GroupVersionKind> {
    let range = document_range(text, line);
    let mut api_version = None;
    let mut kind = None;
    for current in text.lines().skip(range.start).take(range.len()) {
        let value = |key: &str| {
            let rest = current.strip_prefix(key)?.strip_prefix(':')?;
            let value = rest.split(" #").next()?.trim().trim_matches(['"', '\'']);
            (!value.is_empty()).then(|| value.to_string())
        };
        api_version = api_version.or_else(|| value("apiVersion"));
        kind = kind.or_else(|| value("kind"));
    }
    let api_version = api_version?;
    let (group, version) = api_version.split_once('/').unwrap_or(("", &api_version));
    Some(GroupVersionKind::gvk(group, version, &kind?))
}

/// 0-based first lines of every document that has content.
pub fn document_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut pending = Some(0);
    for (number, line) in text.lines().enumerate() {
        if is_marker(line) {
            pending = Some(number + 1);
            continue;
        }
        let blank = line.trim().is_empty() || line.trim_start().starts_with('#');
        if !blank && let Some(start) = pending.take() {
            starts.push(start);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    \"app.kubernetes.io/name\": web
spec:
  template:
    spec:
      containers:
      - name: app
        image: nginx
        args:
        - a
      - name: sidecar
        command: |
          image: not a key
        resources: {}
---
# second
apiVersion: v1
kind: ConfigMap
data:
  key: value
";

    fn at(path: &str) -> (usize, usize, usize) {
        let key = locate(TEXT, 1, &FieldPath::parse(path));
        (key.line, key.column, key.len)
    }

    #[test]
    fn locates_written_paths() {
        assert_eq!(at(".kind"), (1, 0, 4));
        assert_eq!(at(".metadata.name"), (3, 2, 4));
        assert_eq!(
            at(".metadata.labels[\"app.kubernetes.io/name\"]"),
            (5, 4, 24)
        );
        assert_eq!(at(".spec.template.spec.containers[0]"), (10, 6, 1));
        assert_eq!(at(".spec.template.spec.containers[0].name"), (10, 8, 4));
        assert_eq!(at(".spec.template.spec.containers[0].image"), (11, 8, 5));
        assert_eq!(at(".spec.template.spec.containers[0].args[0]"), (13, 8, 1));
        assert_eq!(
            at(".spec.template.spec.containers[1].resources"),
            (17, 8, 9)
        );
    }

    #[test]
    fn falls_back_to_nearest_ancestor() {
        // Missing field: its parent mapping.
        assert_eq!(at(".spec.template.spec.containers[1].image"), (14, 6, 1));
        assert_eq!(at(".spec.selector"), (6, 0, 4));
        // The root: the document's first line.
        assert_eq!(at("."), (0, 0, 0));
    }

    #[test]
    fn locates_in_later_documents() {
        let key = locate(TEXT, 20, &FieldPath::parse(".data.key"));
        assert_eq!((key.line, key.column), (23, 2));
        // A missing path in the second document stays in it.
        assert_eq!(locate(TEXT, 20, &FieldPath::parse(".spec")).line, 19);
    }

    #[test]
    fn finds_key_under_cursor() {
        let key = |line, column| key_at(TEXT, line, column).map(|p| p.to_string());
        assert_eq!(
            key(11, 10).as_deref(),
            Some(".spec.template.spec.containers[0].image")
        );
        assert_eq!(
            key(10, 6).as_deref(),
            Some(".spec.template.spec.containers[0]")
        );
        assert_eq!(
            key(10, 9).as_deref(),
            Some(".spec.template.spec.containers[0].name")
        );
        // On the value: still the key of that line.
        assert_eq!(
            key(11, 20).as_deref(),
            Some(".spec.template.spec.containers[0].image")
        );
        assert_eq!(key(11, 2), None);
        assert_eq!(key(23, 3).as_deref(), Some(".data.key"));
    }

    #[test]
    fn finds_document_kind() {
        let gvk = kind_at(TEXT, 5).unwrap();
        assert_eq!((gvk.group.as_str(), gvk.version.as_str()), ("apps", "v1"));
        assert_eq!(gvk.kind, "Deployment");
        let gvk = kind_at(TEXT, 21).unwrap();
        assert_eq!((gvk.group.as_str(), gvk.kind.as_str()), ("", "ConfigMap"));
        assert!(kind_at("data:\n  a: b\n", 0).is_none());
    }

    #[test]
    fn lists_document_starts() {
        assert_eq!(document_starts(TEXT), [0, 19]);
        assert_eq!(document_starts("---\n\n---\na: 1\n"), [3]);
    }
}
