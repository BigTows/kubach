//! Which namespaces the list screens show for a context.

use gpui_kit::SharedString;
use kubach_core::state::{NamespaceChoice, NamespaceMemory, RECENT_MAX, StartNamespaces};
use kubach_ui::MultiSelection;

/// Names of the selected namespaces, `None` for all: what
/// [`ResourceCache::subscribe_many`](kubach_core::cache::ResourceCache::subscribe_many)
/// takes.
pub fn selected_names(selection: &MultiSelection) -> Option<Vec<String>> {
    match selection {
        MultiSelection::All => None,
        MultiSelection::Items(items) => Some(items.iter().map(ToString::to_string).collect()),
    }
}

/// Namespace selection and recently picked namespaces of one context,
/// shared by every kind.
#[derive(Clone, Debug, PartialEq)]
pub struct NamespaceScope {
    /// Never an empty `Items`: emptiness falls back to `default`.
    pub selection: MultiSelection,
    /// Most recent first.
    pub recent: Vec<SharedString>,
    /// The context's namespace: from the settings, else from kubeconfig,
    /// else `default`.
    pub default: SharedString,
}

impl NamespaceScope {
    /// Starts as [`kubach_core::state::start_namespaces`] picked.
    pub fn new(start: StartNamespaces) -> Self {
        Self {
            selection: match start.selection {
                NamespaceChoice::All => MultiSelection::All,
                NamespaceChoice::Only(names) => {
                    MultiSelection::Items(names.into_iter().map(Into::into).collect())
                }
            },
            recent: start.recent.into_iter().map(Into::into).collect(),
            default: start.default.into(),
        }
    }

    /// What to remember of this scope between runs.
    pub fn memory(&self) -> NamespaceMemory {
        let names = |list: &[SharedString]| list.iter().map(|n| n.to_string()).collect();
        NamespaceMemory {
            selection: match &self.selection {
                MultiSelection::All => NamespaceChoice::All,
                MultiSelection::Items(items) => NamespaceChoice::Only(names(items)),
            },
            recent: names(&self.recent),
        }
    }

    /// Takes a selection from the picker. Nothing picked means the default
    /// namespace; picked namespaces move to the front of the recent list.
    pub fn apply(&mut self, selection: MultiSelection) {
        self.selection = match selection {
            MultiSelection::Items(items) if items.is_empty() => {
                MultiSelection::Items(vec![self.default.clone()])
            }
            selection => selection,
        };
        for namespace in self.selection.items().iter().rev() {
            self.recent.retain(|recent| recent != namespace);
            self.recent.insert(0, namespace.clone());
        }
        self.recent.truncate(RECENT_MAX);
    }

    /// Whether objects of `namespace` are shown by the selection.
    pub fn shows(&self, namespace: &str) -> bool {
        self.selection.is_all() || self.selection.contains(namespace)
    }

    /// Adds `namespace` to the selection (to show an object opened from
    /// another screen) without dropping the namespaces already shown.
    /// Returns the selection before, or `None` if it was shown already.
    pub fn include(&mut self, namespace: &str) -> Option<MultiSelection> {
        if self.shows(namespace) {
            return None;
        }
        let before = self.selection.clone();
        let mut after = before.clone();
        after.toggle(&namespace.to_owned().into());
        self.apply(after);
        Some(before)
    }

    /// Puts back a selection replaced by [`include`](Self::include).
    pub fn restore(&mut self, selection: MultiSelection) {
        self.selection = match selection {
            MultiSelection::Items(items) if items.is_empty() => {
                MultiSelection::Items(vec![self.default.clone()])
            }
            selection => selection,
        };
    }

    /// Drops namespaces that no longer exist from the selection and the
    /// recent list; returns the ones dropped from the selection. If none is
    /// left, falls back to the default namespace, or all if it is gone too.
    pub fn prune(&mut self, existing: &[SharedString]) -> Vec<SharedString> {
        let exists = |namespace: &SharedString| existing.contains(namespace);
        self.recent.retain(exists);
        let MultiSelection::Items(items) = &mut self.selection else {
            return Vec::new();
        };
        let (kept, gone): (Vec<_>, Vec<_>) = items.drain(..).partition(exists);
        *items = kept;
        if items.is_empty() {
            self.selection = if exists(&self.default) {
                MultiSelection::Items(vec![self.default.clone()])
            } else {
                MultiSelection::All
            };
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::SharedString;
    use kubach_core::state::{NamespaceChoice, NamespaceMemory, start_namespaces};
    use kubach_ui::MultiSelection;

    use super::NamespaceScope;

    fn names(list: &[&str]) -> Vec<SharedString> {
        list.iter().map(|&name| name.into()).collect()
    }

    /// A scope of a context whose kubeconfig names `namespace`, with nothing
    /// remembered or configured.
    fn scope(namespace: Option<&str>) -> NamespaceScope {
        NamespaceScope::new(start_namespaces(None, None, namespace))
    }

    #[test]
    fn restores_a_remembered_scope() {
        let mut first = scope(Some("web"));
        first.apply(MultiSelection::Items(names(&["api", "queue"])));
        let memory = first.memory();
        assert_eq!(
            memory,
            NamespaceMemory {
                selection: NamespaceChoice::Only(vec!["api".into(), "queue".into()]),
                recent: vec!["api".into(), "queue".into()],
            }
        );
        let again = NamespaceScope::new(start_namespaces(Some(&memory), None, Some("web")));
        assert_eq!(again, first);

        first.apply(MultiSelection::All);
        let again = NamespaceScope::new(start_namespaces(Some(&first.memory()), None, None));
        assert_eq!(again.selection, MultiSelection::All);
        assert_eq!(again.recent, first.recent);
    }

    #[test]
    fn starts_in_the_context_namespace() {
        assert_eq!(scope(None).selection, MultiSelection::All);
        assert_eq!(
            scope(Some("web")).selection,
            MultiSelection::Items(names(&["web"]))
        );
    }

    #[test]
    fn empty_selection_falls_back_to_the_default() {
        let mut scope = scope(Some("web"));
        scope.apply(MultiSelection::All);
        assert_eq!(scope.selection, MultiSelection::All);
        scope.apply(MultiSelection::Items(Vec::new()));
        assert_eq!(scope.selection, MultiSelection::Items(names(&["web"])));
    }

    #[test]
    fn recent_keeps_latest_first_without_duplicates() {
        let mut scope = scope(None);
        scope.apply(MultiSelection::Items(names(&["web", "api"])));
        assert_eq!(scope.recent, names(&["web", "api"]));
        scope.apply(MultiSelection::Items(names(&["queue"])));
        scope.apply(MultiSelection::All);
        scope.apply(MultiSelection::Items(names(&["api"])));
        assert_eq!(scope.recent, names(&["api", "queue", "web"]));
        for ns in ["a", "b", "c", "d", "e"] {
            scope.apply(MultiSelection::Items(names(&[ns])));
        }
        assert_eq!(scope.recent, names(&["e", "d", "c", "b", "a"]));
    }

    #[test]
    fn including_adds_to_the_selection_and_can_be_undone() {
        let mut scope = scope(Some("web"));
        assert_eq!(scope.include("web"), None);
        let before = scope.include("api");
        assert_eq!(before, Some(MultiSelection::Items(names(&["web"]))));
        assert_eq!(
            scope.selection,
            MultiSelection::Items(names(&["web", "api"]))
        );
        assert_eq!(scope.recent.first().map(|n| n.as_ref()), Some("web"));
        scope.restore(before.unwrap());
        assert_eq!(scope.selection, MultiSelection::Items(names(&["web"])));

        // All namespaces already show everything.
        scope.apply(MultiSelection::All);
        assert!(scope.shows("anything"));
        assert_eq!(scope.include("api"), None);
    }

    #[test]
    fn deleted_namespaces_leave_the_selection() {
        let mut scope = scope(Some("default"));
        scope.apply(MultiSelection::Items(names(&["web", "old"])));
        let gone = scope.prune(&names(&["default", "web"]));
        assert_eq!(gone, names(&["old"]));
        assert_eq!(scope.selection, MultiSelection::Items(names(&["web"])));
        assert_eq!(scope.recent, names(&["web"]));

        // Nothing left: back to the default namespace, or all without it.
        let gone = scope.prune(&names(&["default"]));
        assert_eq!(gone, names(&["web"]));
        assert_eq!(scope.selection, MultiSelection::Items(names(&["default"])));
        scope.apply(MultiSelection::Items(names(&["web"])));
        scope.prune(&names(&["kube-system"]));
        assert_eq!(scope.selection, MultiSelection::All);
    }
}
