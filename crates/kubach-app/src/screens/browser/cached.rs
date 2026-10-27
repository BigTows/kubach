//! The last list of a kind from the disk cache (#300): shown while the
//! watch loads, and while there is no connection or API catalog yet, then
//! replaced by the live list in place (same table, selection and scroll).

use gpui_kit::*;
use kubach_core::cache::{Objects, SubscriptionSet, WatchStatus};
use kubach_core::list_cache::{CachedKind, CachedList, ListCache, ListKey, ListWriter};
use kubach_ui::{MultiSelection, format_since, unix_now};

use super::super::kinds::{self, KindEntry};
use super::super::namespaces::selected_names;
use super::super::usage::{UsageFeed, UsageKind};
use super::{KindView, ResourceBrowser, Scope, rows, usage_namespaces};
use crate::cluster::CatalogState;

/// The list cache and its background writer.
pub(super) struct DiskLists {
    cache: ListCache,
    writer: ListWriter,
}

impl DiskLists {
    /// `None` without a config directory.
    pub fn new() -> Option<Self> {
        let cache = ListCache::in_config_dir()?;
        let writer = ListWriter::new(cache.clone());
        Some(Self { cache, writer })
    }
}

/// Objects shown from the cache until the watch has loaded.
pub(super) struct CachedObjects {
    pub objects: Objects,
    /// When the list was stored (or last seen live), Unix seconds.
    pub saved_at: i64,
}

impl CachedObjects {
    /// "cached 5m ago".
    pub fn age_label(&self) -> String {
        kubach_i18n::t!(
            "browser-cached-ago",
            age = format_since(self.saved_at, unix_now())
        )
    }
}

/// Cache keys of `entry` in `scope` of `context`: one per namespace, or
/// one for all of them.
pub(super) fn list_keys(context: &str, entry: &KindEntry, scope: &Scope) -> Vec<ListKey> {
    let api = &entry.kind.api;
    ListKey::for_selection(
        context,
        &api.group,
        &api.kind,
        usage_namespaces(scope).as_deref(),
    )
}

impl ResourceBrowser {
    /// Whether there is no connection or API catalog to list with.
    fn cache_unusable(&self, cx: &App) -> bool {
        let cluster = self.cluster.read(cx);
        cluster.cache().is_none() || !matches!(cluster.catalog(), CatalogState::Ready(_))
    }

    /// Name of the active context, once the kubeconfig is read.
    fn context_name(&self, cx: &App) -> Option<String> {
        Some(self.cluster.read(cx).active()?.name.clone())
    }

    /// Cache keys of a live view, `None` for Secrets or without a context.
    pub(super) fn live_keys(
        &self,
        entry: &KindEntry,
        scope: &Scope,
        cx: &App,
    ) -> Option<Vec<ListKey>> {
        let keys = list_keys(&self.context_name(cx)?, entry, scope);
        keys.iter().all(ListKey::cacheable).then_some(keys)
    }

    /// Reads the first of `selections` found in the cache off the UI thread
    /// and hands it to `apply` (nothing when none is, or it is empty). A
    /// selection marked `true` only counts for a cluster-scoped kind.
    fn load_cached(
        &self,
        selections: Vec<(Vec<ListKey>, bool)>,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Self, Vec<ListKey>, CachedList, &mut Context<Self>) + 'static,
    ) -> Option<Task<()>> {
        let cache = self.disk.as_ref()?.cache.clone();
        Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    selections.into_iter().find_map(|(keys, cluster_only)| {
                        let list = cache.load_selection(&keys)?;
                        let fits = !cluster_only || !list.kind.namespaced;
                        (fits && !list.objects.is_empty()).then_some((keys, list))
                    })
                })
                .await;
            if let Some((keys, list)) = found {
                this.update(cx, |this, cx| apply(this, keys, list, cx)).ok();
            }
        }))
    }

    /// Starts reading the cached list for a live view that is still loading.
    pub(super) fn seed_from_cache(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.view else {
            return;
        };
        let Some(keys) = view.list_keys.clone() else {
            return;
        };
        if view.cached.is_some() || view.snapshot.status == WatchStatus::Ready {
            return;
        }
        let task = self.load_cached(vec![(keys, false)], cx, |this, keys, list, cx| {
            let Some(view) = &mut this.view else {
                return;
            };
            if view.list_keys.as_ref() != Some(&keys) || view.cached.is_some() {
                return;
            }
            view.cached = Some(CachedObjects {
                objects: list.objects,
                saved_at: list.saved_at,
            });
            // Drops it again at once if the watch has loaded meanwhile.
            this.refresh_view(cx);
        });
        if let Some(view) = &mut self.view {
            view._cache_load = task;
        }
    }

    /// Stores the loaded watches of the view, one entry per namespace (or
    /// one for all namespaces). Called on every change: the writer stores
    /// at most once per `WRITE_DELAY`.
    pub(super) fn save_to_cache(&self) {
        let (Some(disk), Some(view)) = (&self.disk, &self.view) else {
            return;
        };
        let Some(context) = view.list_keys.as_ref().and_then(|keys| keys.first()) else {
            return;
        };
        if view.provisional || view.cached.is_some() {
            return;
        }
        let api = &view.entry.kind.api;
        let kind = CachedKind::of(&view.entry.kind);
        for part in view.watches.subscriptions() {
            let snapshot = part.snapshot();
            if snapshot.status != WatchStatus::Ready {
                continue;
            }
            let namespace = part.key().namespace.as_deref();
            let key = ListKey::new(&context.context, &api.group, &api.kind, namespace);
            disk.writer.save(key, kind.clone(), snapshot.objects);
        }
    }

    /// Without a connection or API catalog: keeps showing the list of the
    /// kind (the live one just lost, else the one in the cache) instead of
    /// a placeholder. Returns whether there is such a view (or one loading).
    pub(super) fn show_provisional(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some((group, kind)), Some(context), Some(selection)) = (
            self.target.clone(),
            self.context_name(cx),
            self.scope().map(|scope| scope.selection.clone()),
        ) else {
            return false;
        };
        let picked = selected_names(&selection);
        let keys = ListKey::for_selection(&context, &group, &kind, picked.as_deref());
        if !keys.iter().all(ListKey::cacheable) {
            return false;
        }
        // A cluster-scoped kind is cached for all namespaces.
        let mut selections = vec![(keys.clone(), false)];
        if picked.is_some() {
            selections.push((vec![ListKey::new(&context, &group, &kind, None)], true));
        }
        if let Some(view) = &mut self.view
            && view
                .list_keys
                .as_ref()
                .is_some_and(|shown| selections.iter().any(|(keys, _)| keys == shown))
        {
            if !view.provisional {
                // The connection went away: its last list stays.
                view.provisional = true;
                view.cached = Some(CachedObjects {
                    objects: view.snapshot.objects.clone(),
                    saved_at: unix_now(),
                });
                view.watches = SubscriptionSet::new(Vec::new());
                view.usage = view
                    .usage
                    .take()
                    .map(|feed| UsageFeed::prometheus(feed.kind));
                view.update_bulk_actions();
            }
            return true;
        }
        self.view = None;
        if self
            .provisional_load
            .as_ref()
            .is_some_and(|(loading, _)| *loading == keys)
        {
            return true;
        }
        let task = self.load_cached(selections, cx, move |this, found, list, cx| {
            this.provisional_load = None;
            let still = this.target.as_ref() == Some(&(group.clone(), kind.clone()))
                && this.context_name(cx).as_deref() == Some(context.as_str())
                && this.view.is_none()
                && this.cache_unusable(cx);
            let selection = this.scope().map(|scope| scope.selection.clone());
            if let (true, Some(selection)) = (still, selection) {
                this.view = Some(this.provisional_view(found, list, &selection));
                this.update_picker(cx);
                cx.notify();
            }
        });
        self.provisional_load = task.map(|task| (keys, task));
        true
    }

    /// A view of a cached list, without watches.
    fn provisional_view(
        &self,
        keys: Vec<ListKey>,
        list: CachedList,
        selection: &MultiSelection,
    ) -> KindView {
        let kind = list.kind.resource_kind();
        let scope = Scope::new(kind.namespaced, selection);
        let plugin = self.registry.plugin_for_resource(&kind.api);
        let hidden_column = scope
            .single_namespace()
            .then(|| rows::namespace_column(plugin.as_ref()))
            .flatten();
        let watches = SubscriptionSet::new(Vec::new());
        let usage = UsageKind::of(&kind).map(UsageFeed::prometheus);
        let entry = KindEntry {
            title: kinds::title_of(&kind),
            kind,
        };
        let snapshot = watches.snapshot();
        let mut view = KindView::new(
            entry,
            scope,
            plugin,
            watches,
            snapshot,
            hidden_column,
            usage,
        );
        view.list_keys = Some(keys);
        view.provisional = true;
        view.cached = Some(CachedObjects {
            objects: list.objects,
            saved_at: list.saved_at,
        });
        view.apply_cached();
        view
    }
}

impl KindView {
    /// Shows the cached objects until the live list is complete (a
    /// selection of several namespaces loads them all), or fails with
    /// nothing to show.
    pub(super) fn apply_cached(&mut self) {
        let live = self.watches.snapshot();
        let loaded = match &live.status {
            WatchStatus::Ready => true,
            WatchStatus::Error(_) => !live.objects.is_empty(),
            WatchStatus::Loading => false,
        };
        if loaded && !self.provisional {
            self.cached = None;
        }
        self.snapshot = match &self.cached {
            Some(cached) => kubach_core::cache::MergedSnapshot {
                objects: cached.objects.clone(),
                ..live
            },
            None => live,
        };
    }

    /// Takes over what the user sees of `old`, a cached view of the same
    /// list: the table (scroll, columns), selection, checks and objects.
    pub(super) fn take_over(&mut self, old: KindView) {
        let same_columns =
            old.usage.is_some() == self.usage.is_some() && old.hidden_column == self.hidden_column;
        if same_columns {
            self.table = old.table;
            self._table_events = old._table_events;
            self.checked = old.checked;
        }
        self.selected = old.selected;
        self.cached = old.cached;
    }
}
