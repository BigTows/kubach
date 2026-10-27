//! Unified text diffs between two revisions of a release.

use similar::TextDiff;

use super::{Release, Sensitive, values_to_yaml};

/// Lines of context around each change, as in `diff -u`.
const CONTEXT: usize = 3;

/// Unified diffs between two revisions; an empty string means no changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionDiff {
    pub from: u32,
    pub to: u32,
    /// Rendered manifests (hooks excluded).
    pub manifest: Sensitive<String>,
    /// User-supplied values as YAML.
    pub user_values: Sensitive<String>,
    /// Computed values (defaults + user values) as YAML.
    pub computed_values: Sensitive<String>,
}

impl RevisionDiff {
    pub fn is_empty(&self) -> bool {
        self.manifest.expose().is_empty()
            && self.user_values.expose().is_empty()
            && self.computed_values.expose().is_empty()
    }
}

/// Compares revision `from` with revision `to`.
pub fn diff_revisions(from: &Release, to: &Release) -> RevisionDiff {
    let label = |release: &Release, what: &str| format!("{what} (revision {})", release.revision);
    let manifest = unified_diff(
        from.manifest.expose(),
        to.manifest.expose(),
        &label(from, "manifest"),
        &label(to, "manifest"),
    );
    let user_values = unified_diff(
        &values_to_yaml(from.user_values.expose()),
        &values_to_yaml(to.user_values.expose()),
        &label(from, "values"),
        &label(to, "values"),
    );
    let computed_values = unified_diff(
        &values_to_yaml(from.computed_values().values.expose()),
        &values_to_yaml(to.computed_values().values.expose()),
        &label(from, "computed values"),
        &label(to, "computed values"),
    );
    RevisionDiff {
        from: from.revision,
        to: to.revision,
        manifest: Sensitive::new(manifest),
        user_values: Sensitive::new(user_values),
        computed_values: Sensitive::new(computed_values),
    }
}

/// `diff -u` style text diff; empty when the texts are equal.
pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(CONTEXT)
        .missing_newline_hint(false)
        .header(old_label, new_label)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helm::codec::tests::release_json;
    use crate::helm::{decode_release, encode_release};

    fn release(revision: u32, replicas: u32) -> Release {
        decode_release(encode_release(&release_json(revision, "deployed", replicas)).as_bytes())
            .unwrap()
    }

    #[test]
    fn unified_diff_marks_changes() {
        let diff = unified_diff("a\nb\nc\n", "a\nB\nc\n", "old", "new");
        assert_eq!(diff, "--- old\n+++ new\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
        assert_eq!(unified_diff("same\n", "same\n", "a", "b"), "");
    }

    #[test]
    fn diffs_manifests_and_values() {
        let diff = diff_revisions(&release(1, 1), &release(2, 3));
        assert_eq!((diff.from, diff.to), (1, 2));

        let manifest = diff.manifest.expose();
        assert!(manifest.starts_with("--- manifest (revision 1)\n+++ manifest (revision 2)\n"));
        assert!(
            manifest.contains("-  replicas: 1\n+  replicas: 3\n"),
            "{manifest}"
        );

        let values = diff.user_values.expose();
        assert!(
            values.contains("-replicaCount: 1\n+replicaCount: 3\n"),
            "{values}"
        );
        assert!(diff.computed_values.expose().contains("+replicaCount: 3"));
        assert!(!diff.is_empty());
    }

    #[test]
    fn identical_revisions_have_empty_diff() {
        let diff = diff_revisions(&release(1, 2), &release(1, 2));
        assert!(diff.is_empty());
    }
}
