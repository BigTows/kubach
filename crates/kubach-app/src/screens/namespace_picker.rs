//! The namespace selection shared by every list screen (resources, Events,
//! GitOps, Access, Helm), one per context, and the picker that edits it in
//! the filter bar of each screen.

use std::collections::HashMap;

use gpui_kit::*;
use kubach_core::cache::{ResourceCache, Subscription as Watch, WatchError, WatchStatus};
use kubach_core::state::start_namespaces;
use kubach_i18n::t;
use kubach_ui::{MultiSelect, MultiSelectEvent, MultiSelection};

use super::activity::watch_changes;
use super::namespaces::NamespaceScope;
use crate::cluster::{CatalogState, Cluster};
use crate::settings::AppSettings;
use crate::state::StateStore;

/// Namespaces of the cluster, watched through the cache.
struct NamespaceList {
    watch: Watch,
    names: Vec<SharedString>,
    /// The initial list completed (`names` is the whole cluster).
    loaded: bool,
    error: Option<WatchError>,
    _listener: Task<()>,
}

/// Whether the namespaces of the active context are known.
#[derive(Clone, Debug, PartialEq)]
pub enum NamespaceListing {
    Loading,
    /// The complete list.
    Listed(Vec<SharedString>),
    /// They can't be listed (forbidden, failing, no Namespace kind).
    Unavailable,
}

/// Namespace selection per context and the namespaces of the active one.
/// Screens observe it; changing it re-targets their watches.
pub struct Namespaces {
    cluster: Entity<Cluster>,
    /// Active context, whose entry in `scopes` is used.
    context: Option<String>,
    scopes: HashMap<String, NamespaceScope>,
    cache: Option<ResourceCache>,
    list: Option<NamespaceList>,
    /// Namespaces dropped from the selection because they were deleted.
    pruned: Vec<SharedString>,
    _subscription: Subscription,
}

impl Namespaces {
    pub fn new(cluster: Entity<Cluster>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&cluster, |this, _, cx| this.sync(cx));
        let mut this = Self {
            cluster,
            context: None,
            scopes: HashMap::new(),
            cache: None,
            list: None,
            pruned: Vec::new(),
            _subscription: subscription,
        };
        this.sync(cx);
        this
    }

    /// Selection of the active context; `None` without one.
    pub fn scope(&self) -> Option<&NamespaceScope> {
        self.scopes.get(self.context.as_ref()?)
    }

    fn scope_mut(&mut self) -> Option<&mut NamespaceScope> {
        self.scopes.get_mut(self.context.as_ref()?)
    }

    /// Takes a selection from a picker.
    pub fn apply(&mut self, selection: MultiSelection, cx: &mut Context<Self>) {
        let Some(scope) = self.scope_mut() else {
            return;
        };
        scope.apply(selection);
        self.pruned.clear();
        self.remember(cx);
        cx.notify();
    }

    /// Adds `namespace` to the selection; returns the selection before
    /// (for "Undo") if it changed.
    pub fn include(&mut self, namespace: &str, cx: &mut Context<Self>) -> Option<MultiSelection> {
        let before = self.scope_mut()?.include(namespace)?;
        self.pruned.clear();
        self.remember(cx);
        cx.notify();
        Some(before)
    }

    /// Puts back a selection replaced by [`include`](Self::include), if the
    /// context is still the same.
    pub fn restore(&mut self, context: &str, selection: MultiSelection, cx: &mut Context<Self>) {
        if self.context.as_deref() != Some(context) {
            return;
        }
        if let Some(scope) = self.scope_mut() {
            scope.restore(selection);
            self.remember(cx);
            cx.notify();
        }
    }

    /// Saves the active context's selection for the next run.
    fn remember(&self, cx: &mut Context<Self>) {
        let (Some(context), Some(scope)) = (self.context.clone(), self.scope()) else {
            return;
        };
        let memory = scope.memory();
        if let Some(store) = StateStore::try_global(cx) {
            store.update(cx, |store, cx| {
                store.edit(cx, |state| state.remember_namespaces(&context, memory))
            });
        }
    }

    /// Whether the active context has a remembered selection.
    fn is_remembered(&self, cx: &App) -> bool {
        let (Some(context), Some(store)) = (&self.context, StateStore::try_global(cx)) else {
            return false;
        };
        store.read(cx).get().namespaces(context).is_some()
    }

    /// Starting scope of `context`: remembered, else configured, else from
    /// kubeconfig (`namespace`), else all namespaces.
    fn start_scope(context: &str, namespace: Option<&str>, cx: &App) -> NamespaceScope {
        let remembered = StateStore::try_global(cx)
            .and_then(|store| store.read(cx).get().namespaces(context).cloned());
        let configured = AppSettings::try_global(cx).and_then(|settings| {
            settings
                .read(cx)
                .get()
                .clusters
                .default_namespace(context)
                .map(str::to_owned)
        });
        NamespaceScope::new(start_namespaces(
            remembered.as_ref(),
            configured.as_deref(),
            namespace,
        ))
    }

    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    /// Namespaces of the cluster (empty until listed).
    pub fn names(&self) -> Vec<SharedString> {
        self.list
            .as_ref()
            .map(|list| list.names.clone())
            .unwrap_or_default()
    }

    /// Whether the namespaces are listed, still loading or unavailable.
    pub fn listing(&self, cx: &App) -> NamespaceListing {
        match &self.list {
            Some(list) if list.error.is_some() => NamespaceListing::Unavailable,
            Some(list) if list.loaded => NamespaceListing::Listed(list.names.clone()),
            Some(_) => NamespaceListing::Loading,
            // Not watched: only a catalog without the Namespace kind (or a
            // failed one) leaves it so; otherwise the connection is coming.
            None => match self.cluster.read(cx).catalog() {
                CatalogState::Ready(catalog) if catalog.find("", "Namespace").is_none() => {
                    NamespaceListing::Unavailable
                }
                CatalogState::Error(_) => NamespaceListing::Unavailable,
                _ => NamespaceListing::Loading,
            },
        }
    }

    /// API client of the active connection.
    pub fn client(&self, cx: &App) -> Option<kube::Client> {
        self.cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone())
    }

    /// Why the list of namespaces is missing, if it is.
    pub fn note(&self) -> Option<SharedString> {
        match self.list.as_ref().and_then(|list| list.error.as_ref()) {
            Some(error) if error.forbidden => Some(t!("picker-no-permission").into()),
            Some(_) => Some(t!("picker-unavailable").into()),
            None => None,
        }
    }

    /// Namespaces that left the selection because they were deleted.
    pub fn pruned(&self) -> &[SharedString] {
        &self.pruned
    }

    pub fn dismiss_pruned(&mut self, cx: &mut Context<Self>) {
        self.pruned.clear();
        cx.notify();
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let cache = cluster.cache().cloned();
        let context = cluster
            .active()
            .map(|c| (c.name.clone(), c.namespace.clone()));
        let catalog = match cluster.catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        let same_cache = match (&self.cache, &cache) {
            (Some(old), Some(new)) => old.same_as(new),
            (None, None) => true,
            _ => false,
        };
        if !same_cache {
            // New connection (context switch or reconnect): list again.
            self.list = None;
            self.pruned.clear();
            self.cache = cache;
        }
        self.context = context.as_ref().map(|(name, _)| name.clone());
        if let Some((name, namespace)) = context
            && !self.scopes.contains_key(&name)
        {
            let scope = Self::start_scope(&name, namespace.as_deref(), cx);
            self.scopes.insert(name, scope);
        }
        if self.list.is_none()
            && let (Some(cache), Some(catalog)) = (self.cache.clone(), catalog)
            && let Some(kind) = catalog.find("", "Namespace")
        {
            let watch = cache.subscribe(kind, None);
            let listener = watch_changes(watch.changes(), cx, Self::refresh);
            self.list = Some(NamespaceList {
                watch,
                names: Vec::new(),
                loaded: false,
                error: None,
                _listener: listener,
            });
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(list) = &mut self.list else {
            return;
        };
        let snapshot = list.watch.snapshot();
        list.names = snapshot
            .objects
            .iter()
            .filter_map(|object| object.metadata.name.clone().map(Into::into))
            .collect();
        let loaded = snapshot.status == WatchStatus::Ready;
        list.loaded |= loaded;
        list.error = match snapshot.status {
            WatchStatus::Error(error) => Some(error),
            _ => None,
        };
        // Deleted namespaces leave the selection (only once the list is
        // complete, so a loading list prunes nothing).
        if loaded {
            let names = list.names.clone();
            if let Some(scope) = self.scope_mut() {
                let before = scope.clone();
                let gone = scope.prune(&names);
                let changed = *scope != before;
                self.pruned.extend(gone);
                // A scope nobody picked is not remembered, so the settings
                // and kubeconfig still decide it next time.
                if changed && self.is_remembered(cx) {
                    self.remember(cx);
                }
            }
        }
        cx.notify();
    }
}

/// The namespace dropdown of one screen's filter bar, editing the shared
/// [`Namespaces`].
pub struct NamespacePicker {
    id: &'static str,
    namespaces: Entity<Namespaces>,
    select: Entity<MultiSelect>,
    /// The listed kind has no namespace: the picker is muted.
    cluster_scoped: bool,
    _select_events: Subscription,
    _subscriptions: Vec<Subscription>,
}

impl NamespacePicker {
    pub fn new(
        id: &'static str,
        namespaces: Entity<Namespaces>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (select, select_events) = Self::make_select(id, window, cx);
        let mut subscriptions = vec![cx.observe(&namespaces, |this, _, cx| this.update(cx))];
        // "All namespaces" is fixed when the dropdown is made: a language
        // switch makes it again.
        if let Some(language) = crate::i18n::global(cx) {
            subscriptions.push(cx.observe_in(&language, window, |this, _, window, cx| {
                let (select, events) = Self::make_select(this.id, window, cx);
                this.select = select;
                this._select_events = events;
                this.update(cx);
            }));
        }
        let mut this = Self {
            id,
            namespaces,
            select,
            cluster_scoped: false,
            _select_events: select_events,
            _subscriptions: subscriptions,
        };
        this.update(cx);
        this
    }

    fn make_select(
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<MultiSelect>, Subscription) {
        let select = cx.new(|cx| {
            MultiSelect::new(id, window, cx)
                .all_label(t!("picker-all-namespaces"))
                .allow_custom(true)
        });
        let events = cx.subscribe(&select, |this, _, event: &MultiSelectEvent, cx| {
            let MultiSelectEvent::Change(selection) = event;
            let selection = selection.clone();
            this.namespaces
                .update(cx, |namespaces, cx| namespaces.apply(selection, cx));
        });
        (select, events)
    }

    /// Mutes the picker (shown as "Cluster-scoped") for kinds without a
    /// namespace.
    pub fn set_cluster_scoped(&mut self, cluster_scoped: bool, cx: &mut Context<Self>) {
        if self.cluster_scoped != cluster_scoped {
            self.cluster_scoped = cluster_scoped;
            self.update(cx);
        }
    }

    /// Opens the list with the focus in its search (`⌘⇧N`, the status bar).
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select.update(cx, |select, cx| select.open(window, cx));
    }

    fn update(&mut self, cx: &mut Context<Self>) {
        let namespaces = self.namespaces.read(cx);
        let scope = namespaces.scope().cloned();
        let names = namespaces.names();
        let note = namespaces.note();
        let cluster_scoped = self.cluster_scoped;
        let disabled = cluster_scoped || scope.is_none();
        self.select.update(cx, |select, cx| {
            select.set_items(names, cx);
            select.set_note(note, cx);
            select.set_disabled(disabled, cx);
            select.set_label_override(
                cluster_scoped.then(|| t!("picker-cluster-scoped").into()),
                cx,
            );
            if let Some(scope) = scope {
                select.set_recent(scope.recent, cx);
                select.set_selection(scope.selection, cx);
            }
        });
        cx.notify();
    }
}

impl Render for NamespacePicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.select.clone()
    }
}
