//! Semantic and text diffs between a live object and its edited version.

use serde_json::{Map, Value};
use similar::{ChangeTag, TextDiff};

use super::edit::strip_for_edit;
use super::path::{FieldPath, PathSegment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// One changed field.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: FieldPath,
    pub kind: ChangeKind,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// How fields missing from the new version are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMode {
    /// Both sides are whole objects: a missing field is a removal.
    Full,
    /// The new side is an apply configuration: fields it leaves out are
    /// kept (or defaulted) by the server, so they are not removals.
    Apply,
}

/// Field-level changes from `before` to `after`, ignoring server-managed
/// noise (status, managedFields, resourceVersion…). Lists whose items all
/// have a `name` are matched by name, others by position.
pub fn semantic_diff(before: &Value, after: &Value, mode: DiffMode) -> Vec<Change> {
    let mut before = before.clone();
    let mut after = after.clone();
    strip_for_edit(&mut before);
    strip_for_edit(&mut after);
    let mut changes = Vec::new();
    diff_value(&FieldPath::root(), &before, &after, mode, &mut changes);
    changes
}

fn diff_value(
    path: &FieldPath,
    before: &Value,
    after: &Value,
    mode: DiffMode,
    out: &mut Vec<Change>,
) {
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => diff_map(path, a, b, mode, out),
        (Value::Array(a), Value::Array(b)) => diff_list(path, a, b, mode, out),
        (a, b) if same_scalar(a, b) => {}
        (a, b) => out.push(Change {
            path: path.clone(),
            kind: ChangeKind::Modified,
            before: Some(a.clone()),
            after: Some(b.clone()),
        }),
    }
}

/// Equal, treating `1` and `1.0` as the same number.
fn same_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y || x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn diff_map(
    path: &FieldPath,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
    mode: DiffMode,
    out: &mut Vec<Change>,
) {
    for (key, b) in after {
        match before.get(key) {
            Some(a) => diff_value(&path.key(key), a, b, mode, out),
            None => out.push(added(path.key(key), b)),
        }
    }
    if mode == DiffMode::Full {
        for (key, a) in before {
            if !after.contains_key(key) {
                out.push(removed(path.key(key), a));
            }
        }
    }
}

fn diff_list(
    path: &FieldPath,
    before: &[Value],
    after: &[Value],
    mode: DiffMode,
    out: &mut Vec<Change>,
) {
    let name = |item: &Value| item.get("name").and_then(Value::as_str).map(str::to_owned);
    let named = |items: &[Value]| items.iter().all(|item| name(item).is_some());
    if !before.is_empty() && !after.is_empty() && named(before) && named(after) {
        for (index, b) in after.iter().enumerate() {
            match before.iter().find(|a| name(a) == name(b)) {
                Some(a) => diff_value(&path.index(index), a, b, mode, out),
                None => out.push(added(path.index(index), b)),
            }
        }
        for (index, a) in before.iter().enumerate() {
            if !after.iter().any(|b| name(a) == name(b)) {
                out.push(removed(path.index(index), a));
            }
        }
        return;
    }
    for index in 0..before.len().max(after.len()) {
        match (before.get(index), after.get(index)) {
            (Some(a), Some(b)) => diff_value(&path.index(index), a, b, mode, out),
            (None, Some(b)) => out.push(added(path.index(index), b)),
            (Some(a), None) => out.push(removed(path.index(index), a)),
            (None, None) => {}
        }
    }
}

fn added(path: FieldPath, value: &Value) -> Change {
    Change {
        path,
        kind: ChangeKind::Added,
        before: None,
        after: Some(value.clone()),
    }
}

fn removed(path: FieldPath, value: &Value) -> Change {
    Change {
        path,
        kind: ChangeKind::Removed,
        before: Some(value.clone()),
        after: None,
    }
}

/// Fields removed from `original` in `edited` that are still in `after`,
/// the server's result of applying `edited`.
///
/// Server-side apply removes a field only when the applier was its sole
/// owner; fields co-owned by other managers (or defaulted again by the
/// server) stay. Named list items are matched by `name`.
pub fn retained_removals(original: &Value, edited: &Value, after: &Value) -> Vec<FieldPath> {
    let mut after = after.clone();
    strip_for_edit(&mut after);
    let mut out = Vec::new();
    for change in semantic_diff(original, edited, DiffMode::Full) {
        if change.kind == ChangeKind::Removed
            && let Some(removed) = &change.before
        {
            retained(&change.path, removed, &after, &mut out);
        }
    }
    out
}

/// The parts of `removed` (once at `path`) still present in `after`.
fn retained(path: &FieldPath, removed: &Value, after: &Value, out: &mut Vec<FieldPath>) {
    let name = removed.get("name").and_then(Value::as_str);
    if let (Some(PathSegment::Index(_)), Some(name)) = (path.segments().last(), name) {
        let kept = path
            .parent()
            .get(after)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("name").and_then(Value::as_str) == Some(name))
            });
        if kept {
            out.push(path.clone());
        }
        return;
    }
    match (removed, path.get(after)) {
        (Value::Object(fields), Some(Value::Object(_))) => {
            for (key, value) in fields {
                retained(&path.key(key), value, after, out);
            }
        }
        (_, Some(_)) => out.push(path.clone()),
        (_, None) => {}
    }
}

/// Unified diff (`diff -u` style) of two texts, with 3 lines of context.
pub fn unified_diff(before: &str, after: &str, before_label: &str, after_label: &str) -> String {
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(before_label, after_label)
        .to_string()
}

/// Unified diff of two documents' YAML, with the same noise stripped as in
/// [`semantic_diff`].
pub fn yaml_diff(before: Option<&Value>, after: &Value) -> String {
    let render = |value: &Value| {
        let mut value = value.clone();
        strip_for_edit(&mut value);
        super::yaml::to_yaml(&value)
    };
    let before = before.map(render).unwrap_or_default();
    unified_diff(&before, &render(after), "live", "edited")
}

/// A line of a full side-by-side-ready diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub tag: LineTag,
    /// 0-based line numbers on each side.
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineTag {
    Equal,
    Delete,
    Insert,
}

/// Every line of both texts tagged, for rendering a diff view.
pub fn diff_lines(before: &str, after: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(before, after)
        .iter_all_changes()
        .map(|change| DiffLine {
            tag: match change.tag() {
                ChangeTag::Equal => LineTag::Equal,
                ChangeTag::Delete => LineTag::Delete,
                ChangeTag::Insert => LineTag::Insert,
            },
            old_line: change.old_index(),
            new_line: change.new_index(),
            text: change.value().trim_end_matches('\n').to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live() -> Value {
        json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "web", "resourceVersion": "1", "uid": "u",
                         "managedFields": [{"manager": "x"}]},
            "spec": {
                "replicas": 1,
                "progressDeadlineSeconds": 600,
                "template": {"spec": {"containers": [
                    {"name": "sidecar", "image": "envoy:1"},
                    {"name": "app", "image": "nginx:1", "imagePullPolicy": "IfNotPresent"}
                ]}}
            },
            "status": {"replicas": 1}
        })
    }

    fn edited() -> Value {
        json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "web", "resourceVersion": "1", "labels": {"a": "b"}},
            "spec": {
                "replicas": 3.0,
                "template": {"spec": {"containers": [
                    {"name": "app", "image": "nginx:2"}
                ]}}
            }
        })
    }

    fn summary(changes: &[Change]) -> Vec<(String, ChangeKind)> {
        changes
            .iter()
            .map(|c| (c.path.to_string(), c.kind))
            .collect()
    }

    #[test]
    fn apply_mode_ignores_omitted_fields() {
        let changes = semantic_diff(&live(), &edited(), DiffMode::Apply);
        assert_eq!(
            summary(&changes),
            [
                (".metadata.labels".into(), ChangeKind::Added),
                (".spec.replicas".into(), ChangeKind::Modified),
                (
                    ".spec.template.spec.containers[0].image".into(),
                    ChangeKind::Modified
                ),
                (
                    ".spec.template.spec.containers[0]".into(),
                    ChangeKind::Removed
                ),
            ]
        );
        assert_eq!(changes[2].before, Some(json!("nginx:1")));
    }

    #[test]
    fn full_mode_reports_removals_but_not_noise() {
        let changes = semantic_diff(&live(), &edited(), DiffMode::Full);
        let paths: Vec<_> = summary(&changes).into_iter().map(|(p, _)| p).collect();
        assert!(paths.contains(&".spec.progressDeadlineSeconds".to_string()));
        assert!(paths.contains(&".spec.template.spec.containers[0].imagePullPolicy".to_string()));
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("status") || p.contains("uid"))
        );
    }

    #[test]
    fn identical_objects_have_no_changes() {
        assert!(semantic_diff(&live(), &live(), DiffMode::Full).is_empty());
    }

    #[test]
    fn positional_lists() {
        let changes = semantic_diff(
            &json!({"args": ["a", "b"]}),
            &json!({"args": ["a", "c", "d"]}),
            DiffMode::Full,
        );
        assert_eq!(
            summary(&changes),
            [
                (".args[1]".into(), ChangeKind::Modified),
                (".args[2]".into(), ChangeKind::Added)
            ]
        );
    }

    #[test]
    fn finds_retained_removals() {
        let original = json!({
            "metadata": {"name": "x", "labels": {"a": "1", "b": "2"}},
            "spec": {"containers": [{"name": "app"}, {"name": "sidecar"}]}
        });
        let edited = json!({
            "metadata": {"name": "x"},
            "spec": {"containers": [{"name": "app"}]}
        });
        // The server kept label `b` (owned by someone else) and the sidecar.
        let after = json!({
            "metadata": {"name": "x", "labels": {"b": "2"}, "uid": "u"},
            "spec": {"containers": [{"name": "sidecar"}, {"name": "app"}]}
        });
        let kept: Vec<_> = retained_removals(&original, &edited, &after)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(kept, [".metadata.labels.b", ".spec.containers[1]"]);

        let removed = json!({"metadata": {"name": "x"}, "spec": {"containers": [{"name": "app"}]}});
        assert!(retained_removals(&original, &edited, &removed).is_empty());
    }

    #[test]
    fn text_diffs() {
        let diff = unified_diff("a: 1\nb: 2\n", "a: 1\nb: 3\n", "live", "edited");
        assert!(diff.starts_with("--- live\n+++ edited\n@@"), "{diff}");
        assert!(diff.contains("-b: 2\n+b: 3\n"));

        let lines = diff_lines("a\nb\n", "a\nc\n");
        let tags: Vec<_> = lines.iter().map(|l| (l.tag, l.text.as_str())).collect();
        assert_eq!(
            tags,
            [
                (LineTag::Equal, "a"),
                (LineTag::Delete, "b"),
                (LineTag::Insert, "c")
            ]
        );

        let yaml = yaml_diff(Some(&live()), &edited());
        assert!(yaml.contains("-  replicas: 1\n"), "{yaml}");
        assert!(yaml.contains("+  replicas: 3"), "{yaml}");
        assert!(!yaml.contains("resourceVersion"));
        assert!(!yaml.contains("status"));
    }
}
