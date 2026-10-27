//! What "Review changes" shows: per object, the field changes and a line
//! diff of a server-side dry run, or why it failed. UI-free.

use kubach_core::apply::edit::strip_for_edit;
use kubach_core::apply::server::InvalidField;
use kubach_core::apply::{
    ApplyError, ApplyOutcome, ChangeKind, DiffMode, FieldConflict, LineTag, diff_lines,
    retained_removals, semantic_diff, to_yaml,
};
use kubach_core::secrets::mask::mask_for_diff;
use kubach_ui::{DiffLineKind, DiffViewLine};
use kube::api::DynamicObject;
use serde_json::{Map, Value};

/// Longest value shown inline in the change list.
const VALUE_MAX: usize = 80;

/// One field change, ready to display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRow {
    pub path: String,
    pub kind: ChangeKind,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A successful (dry-run or real) apply of one object.
#[derive(Clone, Debug)]
pub struct ReviewItem {
    pub label: String,
    pub created: bool,
    pub changes: Vec<ChangeRow>,
    pub lines: Vec<DiffViewLine>,
    /// Fields the edit removes that the server keeps.
    pub retained: Vec<String>,
}

/// A failed apply of one object.
#[derive(Clone, Debug)]
pub enum Failure {
    Conflicts {
        label: String,
        conflicts: Vec<FieldConflict>,
    },
    Stale {
        label: String,
    },
    Invalid {
        label: String,
        message: String,
        fields: Vec<InvalidField>,
    },
    Other {
        message: String,
    },
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Failure::Conflicts { label, conflicts } => kubach_i18n::t!(
                "editor-failure-conflicts",
                label = label,
                count = conflicts.len()
            ),
            Failure::Stale { label } => kubach_i18n::t!("editor-failure-stale", label = label),
            Failure::Invalid { label, message, .. } => format!("{label}: {message}"),
            Failure::Other { message } => message.clone(),
        }
    }
}

impl From<ApplyError> for Failure {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Conflicts { label, conflicts } => Failure::Conflicts { label, conflicts },
            ApplyError::Stale { label } => Failure::Stale { label },
            ApplyError::Invalid {
                label,
                message,
                fields,
            } => Failure::Invalid {
                label,
                message,
                fields,
            },
            other => Failure::Other {
                message: other.to_string(),
            },
        }
    }
}

/// Builds the review of one outcome. `original` is the edit view the editor
/// loaded and `edited` what was applied, for objects being edited.
pub fn review_item(
    label: String,
    outcome: &ApplyOutcome,
    original: Option<&Value>,
    edited: &Value,
) -> ReviewItem {
    let mut before = outcome.before.as_ref().map(to_value);
    let mut after = to_value(&outcome.after);
    mask_for_diff(before.as_mut(), &mut after);
    let before_value = before.unwrap_or_else(|| Value::Object(Map::new()));
    let changes = semantic_diff(&before_value, &after, DiffMode::Full)
        .into_iter()
        .map(|change| ChangeRow {
            path: change.path.to_string(),
            kind: change.kind,
            before: change.before.as_ref().map(short),
            after: change.after.as_ref().map(short),
        })
        .collect();
    let before_text = if outcome.before.is_some() {
        to_yaml(&before_value)
    } else {
        String::new()
    };
    let lines = diff_view_lines(&before_text, &to_yaml(&after));
    let retained = original
        .map(|original| retained_removals(original, edited, &after))
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();
    ReviewItem {
        label,
        created: outcome.created(),
        changes,
        lines,
        retained,
    }
}

/// The edit view of an object as a value.
fn to_value(object: &DynamicObject) -> Value {
    let mut value = serde_json::to_value(object).unwrap_or(Value::Null);
    strip_for_edit(&mut value);
    value
}

/// A line diff of two YAML texts, for [`kubach_ui::DiffView`].
pub fn diff_view_lines(before: &str, after: &str) -> Vec<DiffViewLine> {
    diff_lines(before, after)
        .into_iter()
        .map(|line| DiffViewLine {
            kind: match line.tag {
                LineTag::Equal => DiffLineKind::Unchanged,
                LineTag::Delete => DiffLineKind::Removed,
                LineTag::Insert => DiffLineKind::Added,
            },
            old: line.old_line,
            new: line.new_line,
            text: line.text.into(),
        })
        .collect()
}

/// A value on one line, clipped.
fn short(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let text = text.replace('\n', "⏎");
    if text.chars().count() <= VALUE_MAX {
        return text;
    }
    let mut clipped: String = text.chars().take(VALUE_MAX).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    fn outcome(before: Option<Value>, after: Value) -> ApplyOutcome {
        ApplyOutcome {
            before: before.map(object),
            after: object(after),
            dry_run: true,
            changes: Vec::new(),
            diff: String::new(),
        }
    }

    fn config_map(data: Value) -> Value {
        json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "x", "namespace": "ns", "uid": "u", "resourceVersion": "1",
                         "labels": {"keep": "me"}},
            "data": data
        })
    }

    #[test]
    fn reviews_an_update() {
        let before = config_map(json!({"a": "1", "b": "2"}));
        let after = config_map(json!({"a": "1", "b": "3"}));
        let item = review_item(
            "ConfigMap ns/x".into(),
            &outcome(Some(before), after),
            None,
            &json!({}),
        );
        assert!(!item.created);
        assert_eq!(
            item.changes,
            [ChangeRow {
                path: ".data.b".into(),
                kind: ChangeKind::Modified,
                before: Some("2".into()),
                after: Some("3".into()),
            }]
        );
        let changed: Vec<_> = item
            .lines
            .iter()
            .filter(|l| l.kind != DiffLineKind::Unchanged)
            .map(|l| (l.kind, l.text.to_string()))
            .collect();
        assert_eq!(
            changed,
            [
                (DiffLineKind::Removed, "  b: \"2\"".into()),
                (DiffLineKind::Added, "  b: \"3\"".into())
            ]
        );
        assert!(!item.lines.iter().any(|l| l.text.contains("uid")));
    }

    #[test]
    fn reviews_a_creation_as_all_added() {
        let item = review_item(
            "ConfigMap ns/x".into(),
            &outcome(None, config_map(json!({"a": "1"}))),
            None,
            &json!({}),
        );
        assert!(item.created);
        assert!(item.lines.iter().all(|l| l.kind == DiffLineKind::Added));
        assert_eq!(item.lines[0].new, Some(0));
    }

    #[test]
    fn warns_about_removals_the_server_keeps() {
        let live = config_map(json!({"a": "1"}));
        let mut original = live.clone();
        strip_for_edit(&mut original);
        let mut edited = original.clone();
        edited["metadata"].as_object_mut().unwrap().remove("labels");
        // Another manager owns the label: it stays.
        let item = review_item(
            "x".into(),
            &outcome(Some(live.clone()), live),
            Some(&original),
            &edited,
        );
        assert_eq!(item.retained, [".metadata.labels.keep"]);
    }

    #[test]
    fn never_shows_secret_values() {
        let secret = |data: Value| {
            json!({"apiVersion": "v1", "kind": "Secret",
                   "metadata": {"name": "s", "namespace": "ns"}, "data": data})
        };
        let before = secret(json!({"user": "YWRtaW4=", "pass": "b2xk"}));
        let after = secret(json!({"user": "YWRtaW4=", "pass": "bmV3", "token": "dA=="}));
        let item = review_item(
            "Secret ns/s".into(),
            &outcome(Some(before), after),
            None,
            &json!({}),
        );
        let text: String = item.lines.iter().map(|l| format!("{}\n", l.text)).collect();
        for secret in ["YWRtaW4=", "b2xk", "bmV3", "dA=="] {
            assert!(!text.contains(secret), "{text}");
            assert!(!format!("{:?}", item.changes).contains(secret));
        }
        let changes: Vec<_> = item
            .changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind))
            .collect();
        assert_eq!(
            changes,
            [
                (".data.pass", ChangeKind::Modified),
                (".data.token", ChangeKind::Added)
            ]
        );
    }

    #[test]
    fn clips_long_values() {
        assert_eq!(short(&json!("a\nb")), "a⏎b");
        assert_eq!(short(&json!(3)), "3");
        assert_eq!(
            short(&json!("x".repeat(100))).chars().count(),
            VALUE_MAX + 1
        );
    }
}
