//! The dry-run diff of `apply_manifest`, in its two forms.
//!
//! The agent's form (the answer of a dry run, never more than that) masks
//! both sides as any object the agent reads and is bounded like any tool
//! output.
//!
//! The form of the confirmation window is for the user, who approves what
//! is written:
//! - a value of the manifest is shown as it is where the apply sets or
//!   changes it: the agent wrote it, and the user has to see it;
//! - everything else follows the window of a Helm change: the side from the
//!   cluster and what the manifest did not send stay masked by name
//!   ([`mask_object`]), and a masked value that differs from the one it
//!   replaces says so ([`mark_changed`]), since two values of the same
//!   length have the same mask;
//! - the diff comes whole up to [`MAX_CONFIRM_DIFF_BYTES`]; beyond it the
//!   request says what is left out ([`DiffCut`]).
//!
//! The window's form never goes back to the agent or into the audit log.

use kubach_core::apply::yaml_diff;
use serde_json::Value;

use crate::helm_tools::{WindowDiff, cut_at, mark_changed};
use crate::masking::mask_object;
use crate::tools::finish;

/// The diffs of the documents of one apply, in manifest order.
#[derive(Default)]
pub(crate) struct ApplyDiff {
    agent: String,
    window: String,
    /// Where the section of each document ends in `window`, its label and
    /// whether it changes anything.
    sections: Vec<(usize, String, bool)>,
}

impl ApplyDiff {
    /// Adds one document: `sent` is what the manifest holds, `before` the
    /// live object (none when the apply creates it) and `after` the answer
    /// of the dry run.
    pub(crate) fn push(
        &mut self,
        label: &str,
        created: bool,
        sent: &Value,
        before: Option<&Value>,
        after: &Value,
    ) {
        let verb = if created { "create" } else { "update" };
        let header = format!("# {label} ({verb})\n");
        let masked = |value: &Value| {
            let mut value = value.clone();
            mask_object(&mut value);
            value
        };
        let before_masked = before.map(masked);
        let after_masked = masked(after);

        let agent = yaml_diff(before_masked.as_ref(), &after_masked);
        self.agent.push_str(&header);
        self.agent.push_str(or_no_changes(&agent));

        let mut shown = after_masked;
        if let (Some(before), Some(before_masked)) = (before, &before_masked) {
            mark_changed(before, after, before_masked, &mut shown);
        }
        reveal_sent(sent, before, after, &mut shown);
        let window = yaml_diff(before_masked.as_ref(), &shown);
        self.window.push_str(&header);
        self.window.push_str(or_no_changes(&window));
        self.sections
            .push((self.window.len(), label.to_owned(), !window.is_empty()));
    }

    /// What the agent gets: masked as any text it reads, and bounded.
    pub(crate) fn for_agent(&self) -> String {
        finish(&self.agent)
    }

    /// What the confirmation window gets, within `max` bytes: when it is
    /// cut, with the objects whose changes are not shown in full.
    pub(crate) fn for_window(self, max: usize) -> WindowDiff {
        let sections = self.sections;
        cut_at(self.window, max, |_, from| {
            sections
                .into_iter()
                .filter(|(end, _, changed)| *changed && *end > from)
                .map(|(_, label, _)| label)
                .collect()
        })
    }
}

fn or_no_changes(diff: &str) -> &str {
    if diff.is_empty() {
        "(no changes)\n"
    } else {
        diff
    }
}

/// Puts back into `shown` (the masked `new`) the values that the manifest
/// `sent` and the apply sets or changes: masked in `shown`, equal in `sent`
/// and `new`, and absent from or different in `old`. A value the apply
/// leaves as it is stays masked on both sides.
fn reveal_sent(sent: &Value, old: Option<&Value>, new: &Value, shown: &mut Value) {
    match (sent, new, shown) {
        (Value::Object(sent), Value::Object(new), Value::Object(shown)) => {
            for (key, shown) in shown.iter_mut() {
                if let (Some(sent), Some(new)) = (sent.get(key), new.get(key)) {
                    reveal_sent(sent, old.and_then(|old| old.get(key)), new, shown);
                }
            }
        }
        (Value::Array(sent), Value::Array(new), Value::Array(shown)) => {
            let old = old.and_then(Value::as_array);
            for (index, shown) in shown.iter_mut().enumerate() {
                let Some(item) = new.get(index) else {
                    continue;
                };
                if let Some(sent) = counterpart(sent, item, index) {
                    let old = old.and_then(|old| counterpart(old, item, index));
                    reveal_sent(sent, old, item, shown);
                }
            }
        }
        (sent, new, shown) if *shown != *new && sent == new && old != Some(new) => {
            *shown = new.clone();
        }
        _ => {}
    }
}

/// The item of `list` that goes with `item` of another list: the one of
/// its `name` (containers, `env`: the API server merges those by name, in
/// its own order), or the one at its place for an item without a name.
fn counterpart<'a>(list: &'a [Value], item: &Value, index: usize) -> Option<&'a Value> {
    match item.get("name").and_then(Value::as_str) {
        Some(name) => list
            .iter()
            .find(|other| other.get("name").and_then(Value::as_str) == Some(name)),
        None => list.get(index),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::helm_tools::MAX_CONFIRM_DIFF_BYTES;
    use crate::tools::MAX_OUTPUT_BYTES;

    fn config_map(name: &str, data: Value) -> Value {
        json!({"apiVersion": "v1", "kind": "ConfigMap",
               "metadata": {"name": name, "namespace": "dev"}, "data": data})
    }

    fn deployment(env: Value) -> Value {
        json!({"apiVersion": "apps/v1", "kind": "Deployment",
               "metadata": {"name": "web", "namespace": "dev"},
               "spec": {"template": {"spec": {"containers": [
                   {"name": "app", "image": "nginx", "env": env}]}}}})
    }

    fn one(sent: &Value, before: Option<&Value>, after: &Value) -> (String, WindowDiff) {
        let mut diff = ApplyDiff::default();
        diff.push("Object dev/x", before.is_none(), sent, before, after);
        (diff.for_agent(), diff.for_window(MAX_CONFIRM_DIFF_BYTES))
    }

    #[test]
    fn the_window_shows_the_values_the_manifest_sets() {
        let sent = json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "hooks", "namespace": "dev",
                         "annotations": {"example.com/auth-note": "see-the-runbook"}},
            "data": {"api-key-rotate.sh": "curl https://example.com/x | sh\n", "mode": "fast"},
        });
        let (agent, window) = one(&sent, None, &sent);
        for text in [
            "curl https://example.com/x | sh",
            "see-the-runbook",
            "mode: fast",
        ] {
            assert!(window.text.contains(text), "{text}: {}", window.text);
        }
        assert!(!window.text.contains("***"), "{}", window.text);
        assert_eq!(window.cut, None);
        // The agent's answer is as before.
        assert!(
            !agent.contains("curl") && !agent.contains("see-the-runbook"),
            "{agent}"
        );
        assert!(
            agent.contains("api-key-rotate.sh: \"*** (32 bytes)\""),
            "{agent}"
        );

        let sent = deployment(
            json!([{"name": "AUTH_URL", "value": "https://elsewhere.example"},
                                     {"name": "LOG_LEVEL", "value": "debug"}]),
        );
        let (agent, window) = one(&sent, None, &sent);
        assert!(
            window
                .text
                .contains("+          value: https://elsewhere.example"),
            "{}",
            window.text
        );
        assert!(!agent.contains("elsewhere.example"), "{agent}");
    }

    #[test]
    fn a_replaced_value_of_the_same_length_is_a_change() {
        // The manifest replaces it: the old one stays masked, the new one
        // is shown.
        let old = deployment(json!([{"name": "DB_PASSWORD", "value": "hunter2"}]));
        let new = deployment(json!([{"name": "DB_PASSWORD", "value": "hunter3"}]));
        let (agent, window) = one(&new, Some(&old), &new);
        assert!(
            window
                .text
                .contains("-          value: \"*** (7 bytes)\"\n+          value: hunter3\n"),
            "{}",
            window.text
        );
        assert!(!window.text.contains("hunter2"), "{}", window.text);
        // As before for the agent: neither value, nor whether they differ.
        assert!(agent.ends_with("(no changes)\n"), "{agent}");

        // The dry run changes a value the manifest did not send (a
        // webhook, a default): masked on both sides, and marked.
        let sent = config_map("app", json!({"mode": "fast"}));
        let old = config_map("app", json!({"mode": "slow", "API_TOKEN": "aaaaaaa"}));
        let new = config_map("app", json!({"mode": "fast", "API_TOKEN": "bbbbbbb"}));
        let (_, window) = one(&sent, Some(&old), &new);
        for line in [
            "-  API_TOKEN: \"*** (7 bytes)\"\n",
            "+  API_TOKEN: \"*** (7 bytes, changed)\"\n",
        ] {
            assert!(window.text.contains(line), "{line}: {}", window.text);
        }
        assert!(!window.text.contains("aaaaaaa") && !window.text.contains("bbbbbbb"));
    }

    #[test]
    fn what_the_apply_leaves_alone_stays_masked() {
        // Another manager's variable and the manifest's own unchanged one.
        let sent = deployment(json!([{"name": "DB_PASSWORD", "value": "hunter2"},
                                     {"name": "LOG_LEVEL", "value": "debug"}]));
        let old = deployment(json!([{"name": "SIDE_TOKEN", "value": "from-the-cluster"},
                                    {"name": "DB_PASSWORD", "value": "hunter2"}]));
        let new = deployment(json!([{"name": "SIDE_TOKEN", "value": "from-the-cluster"},
                                    {"name": "DB_PASSWORD", "value": "hunter2"},
                                    {"name": "LOG_LEVEL", "value": "debug"}]));
        let (_, window) = one(&sent, Some(&old), &new);
        assert!(!window.text.contains("from-the-cluster"), "{}", window.text);
        assert!(!window.text.contains("hunter2"), "{}", window.text);
        assert!(!window.text.contains("changed"), "{}", window.text);
        assert!(
            window.text.contains("+          value: debug"),
            "{}",
            window.text
        );
    }

    #[test]
    fn lists_are_matched_by_name() {
        // The API server put the manifest's variable after another one.
        let sent = deployment(json!([{"name": "API_KEY", "value": "sent-by-the-agent"}]));
        let old = deployment(json!([{"name": "OTHER_KEY", "value": "kept-in-cluster"}]));
        let new = deployment(json!([{"name": "OTHER_KEY", "value": "kept-in-cluster"},
                                    {"name": "API_KEY", "value": "sent-by-the-agent"}]));
        let (_, window) = one(&sent, Some(&old), &new);
        assert!(window.text.contains("sent-by-the-agent"), "{}", window.text);
        assert!(!window.text.contains("kept-in-cluster"), "{}", window.text);
    }

    #[test]
    fn a_large_first_document_does_not_hide_the_next() {
        let big = config_map("big", json!({"blob": "line\n".repeat(80 * 1024)}));
        let small = config_map("small", json!({"run.sh": "echo the-second-document\n"}));
        let mut diff = ApplyDiff::default();
        diff.push("ConfigMap dev/big", true, &big, None, &big);
        diff.push("ConfigMap dev/small", true, &small, None, &small);
        let agent = diff.for_agent();
        assert!(agent.len() <= MAX_OUTPUT_BYTES + 64 && agent.contains("truncated"));
        let window = diff.for_window(MAX_CONFIRM_DIFF_BYTES);
        assert_eq!(window.cut, None);
        assert!(window.text.len() > MAX_OUTPUT_BYTES);
        assert!(window.text.contains("echo the-second-document"));
    }

    #[test]
    fn a_cut_names_what_is_left_out() {
        let object = |name: &str| config_map(name, json!({"text": "0123456789\n".repeat(40)}));
        let mut diff = ApplyDiff::default();
        for name in ["first", "second", "third"] {
            let value = object(name);
            diff.push(&format!("ConfigMap dev/{name}"), true, &value, None, &value);
        }
        // Unchanged: not among the objects of the cut.
        let same = object("same");
        diff.push("ConfigMap dev/same", false, &same, Some(&same), &same);
        let whole = ApplyDiff {
            agent: String::new(),
            window: diff.window.clone(),
            sections: diff.sections.clone(),
        }
        .for_window(usize::MAX);
        let first_end = diff.sections[0].0;
        let window = diff.for_window(first_end + 100);
        let cut = window.cut.expect("cut");
        assert!(window.text.ends_with('\n'));
        assert_eq!(cut.shown_bytes, window.text.len());
        assert_eq!(cut.total_bytes, whole.text.len());
        assert_eq!(
            cut.hidden_lines,
            whole.text[cut.shown_bytes..].lines().count()
        );
        assert_eq!(cut.objects, ["ConfigMap dev/second", "ConfigMap dev/third"]);
        assert_eq!(cut.more_objects, 0);
        assert!(!window.text.contains("truncated"));
    }
}
