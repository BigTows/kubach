//! The list of one resource kind, opened from the sidebar: a filter bar
//! (namespaces, search by name, New), a live table with checkboxes and bulk
//! actions, and a details panel, all fed by the core cache.
//!
//! The table and the details are views of their own, embedded cached
//! (#363): each redraws alone when it changes, and a redraw of the browser
//! (the toolbar, a search) reuses both.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::cache::{
    MergedSnapshot, ResourceCache, SubscriptionSet, WatchFilter, WatchStatus,
};
use kubach_core::list_cache::ListKey;
use kubach_core::metrics::shared::MetricsServer;
use kubach_core::plugin::bulk;
use kubach_core::plugin::{Action, PluginRegistry, ResourcePlugin};
use kubach_core::secrets::mask::is_core_secret;
use kubach_core::state::kind_key;
use kubach_i18n::t;
use kubach_ui::{
    EmptyState, FilterBar, MultiSelection, ResourceRow, ResourceTable, ResourceTableEvent,
    SIDE_PANEL_CONTENT_MIN, SidePanel, SidePanelState, name_matches,
};
use kube::api::DynamicObject;

use super::access::Reveal;
use super::activity::{ScreenActivity, watch_set};
use super::column_order;
use super::kinds::{self, KindEntry};
use super::namespace_picker::{NamespacePicker, Namespaces};
use super::namespaces::NamespaceScope;
use super::panel_width;
use super::plugin_text;
use super::prometheus_link::{PrometheusRegistry, PrometheusStatus};
use super::rows;
use super::usage::{self, MetricsState, UsageFeed, UsageKind};
use crate::cluster::{CatalogState, Cluster};

mod actions;
mod cached;
mod detail_actions;
mod details;
mod namespace;
mod node_ops;
mod workload_pods;

pub(super) use actions::{ObjectActions, Subject, Where};
pub(super) use details::{DetailsEvent, DetailsTarget, OpenObject, ResourceDetails};

/// How often ages in the table are recomputed when nothing else changes.
const AGE_REFRESH: Duration = Duration::from_secs(10);

/// Where objects of a kind are listed.
#[derive(Clone, Debug, PartialEq)]
enum Scope {
    Cluster,
    All,
    Namespaces(Vec<SharedString>),
}

impl Scope {
    fn new(kind_namespaced: bool, selection: &MultiSelection) -> Self {
        match selection {
            _ if !kind_namespaced => Scope::Cluster,
            MultiSelection::All => Scope::All,
            MultiSelection::Items(items) => Scope::Namespaces(items.clone()),
        }
    }

    /// One namespace makes the Namespace column redundant.
    fn single_namespace(&self) -> bool {
        matches!(self, Scope::Namespaces(namespaces) if namespaces.len() == 1)
    }

    /// The scope as arguments of the messages that name it
    /// (`browser-list-count`): its variant, the namespace for one, and how
    /// many for several.
    fn message_args(&self) -> (&'static str, String, usize) {
        match self {
            Scope::Cluster => ("cluster", String::new(), 0),
            Scope::All => ("any", String::new(), 0),
            Scope::Namespaces(namespaces) if namespaces.len() == 1 => {
                ("one", namespaces[0].to_string(), 1)
            }
            Scope::Namespaces(namespaces) => ("many", String::new(), namespaces.len()),
        }
    }

    /// Where the objects of an action on this list are, for its messages.
    fn describe(&self) -> actions::Where {
        match self {
            Scope::Cluster => actions::Where::cluster(),
            Scope::All => actions::Where::any(),
            Scope::Namespaces(namespaces) if namespaces.len() == 1 => {
                actions::Where::one(namespaces[0].to_string())
            }
            Scope::Namespaces(namespaces) => actions::Where::many(namespaces.len()),
        }
    }
}

/// Namespaces whose pods get usage: `None` for all.
fn usage_namespaces(scope: &Scope) -> Option<Vec<String>> {
    match scope {
        Scope::Namespaces(namespaces) => Some(namespaces.iter().map(ToString::to_string).collect()),
        Scope::Cluster | Scope::All => None,
    }
}

/// The objects of one kind in the selected namespaces, as currently shown.
struct KindView {
    entry: KindEntry,
    scope: Scope,
    plugin: Arc<dyn ResourcePlugin>,
    watches: SubscriptionSet,
    snapshot: MergedSnapshot,
    /// Plugin column dropped from the table (Namespace for one namespace).
    hidden_column: Option<usize>,
    /// Created on first render, which has the window a table needs.
    table: Option<Entity<ResourceTable>>,
    selected: Option<String>,
    /// Keys of the checked rows; a new view (kind, namespaces or context
    /// changed) starts with none.
    checked: Vec<SharedString>,
    /// Actions offered for all checked objects.
    bulk_actions: Vec<Action>,
    /// CPU / Memory columns, for Pods and Nodes.
    usage: Option<UsageFeed>,
    /// Where the list is cached on disk (one key per namespace, or one for
    /// all); `None` for Secrets.
    list_keys: Option<Vec<ListKey>>,
    /// Shown from the disk cache until the watch has loaded.
    cached: Option<cached::CachedObjects>,
    /// Shown from the cache without a connection or API catalog: no
    /// watches, nothing can be done to the objects.
    provisional: bool,
    /// Language of the table's column headers.
    table_locale: kubach_i18n::Locale,
    _cache_load: Option<Task<()>>,
    _listener: Task<()>,
    _table_events: Option<Subscription>,
}

impl KindView {
    /// The listed kind as the subject of an action.
    fn subject(&self) -> Subject {
        Subject::new(
            self.entry.kind.clone(),
            self.plugin.clone(),
            self.entry.title.to_string(),
        )
    }

    fn new(
        entry: KindEntry,
        scope: Scope,
        plugin: Arc<dyn ResourcePlugin>,
        watches: SubscriptionSet,
        snapshot: MergedSnapshot,
        hidden_column: Option<usize>,
        usage: Option<UsageFeed>,
    ) -> Self {
        Self {
            entry,
            scope,
            plugin,
            watches,
            snapshot,
            hidden_column,
            table: None,
            selected: None,
            checked: Vec::new(),
            bulk_actions: Vec::new(),
            usage,
            list_keys: None,
            cached: None,
            provisional: false,
            table_locale: kubach_i18n::locale(),
            _cache_load: None,
            _listener: Task::ready(()),
            _table_events: None,
        }
    }

    /// Objects whose name matches the search `query`.
    fn matching<'a>(&'a self, query: &'a str) -> impl Iterator<Item = &'a Arc<DynamicObject>> {
        self.snapshot.objects.iter().filter(move |object| {
            name_matches(object.metadata.name.as_deref().unwrap_or_default(), query)
        })
    }

    fn rows(&self, query: &str) -> Vec<ResourceRow> {
        self.matching(query)
            .map(|object| {
                let mut row = rows::row(self.plugin.as_ref(), object);
                if let Some(ix) = self.hidden_column.filter(|&ix| ix < row.cells.len()) {
                    row.cells.remove(ix);
                }
                if let Some(feed) = &self.usage {
                    row.cells.extend(usage::cells(&feed.state, object));
                }
                row
            })
            .collect()
    }

    /// Checked objects still in the cache, in table order.
    fn checked_objects(&self) -> Vec<Arc<DynamicObject>> {
        let by_key: HashMap<String, &Arc<DynamicObject>> = self
            .snapshot
            .objects
            .iter()
            .map(|object| (rows::object_key(object), object))
            .collect();
        self.checked
            .iter()
            .filter_map(|key| by_key.get(key.as_ref()).map(|&object| object.clone()))
            .collect()
    }

    fn update_bulk_actions(&mut self) {
        self.bulk_actions = if self.checked.is_empty() || self.provisional {
            Vec::new()
        } else {
            bulk::common_actions(self.plugin.as_ref(), &self.checked_objects())
        };
    }

    fn selected_object(&self) -> Option<&Arc<DynamicObject>> {
        let key = self.selected.as_deref()?;
        self.snapshot
            .objects
            .iter()
            .find(|object| rows::object_key(object) == key)
    }

    /// Banner for watches that keep failing while data is shown: its text
    /// and the raw error for the tooltip. Short outages never get here: the
    /// cache retries them quietly (#316).
    fn error_banner(&self) -> Option<(String, String)> {
        let errors = &self.snapshot.errors;
        let (_, first) = errors.first()?;
        let title = &self.entry.title;
        let details = first.message.clone();
        let partial = errors.len() < self.watches.subscriptions().len();
        if !partial {
            let text = if first.connection {
                t!("browser-banner-connection-lost")
            } else {
                t!("browser-banner-cant-refresh", title = title)
            };
            return Some((text, details));
        }
        let names = errors
            .iter()
            .map(|(namespace, _)| match namespace {
                Some(namespace) => namespace.clone(),
                None => t!("browser-all-namespaces"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let text = if errors.iter().all(|(_, error)| error.forbidden) {
            t!(
                "browser-banner-no-access",
                title = title,
                namespaces = names
            )
        } else {
            t!(
                "browser-banner-cant-load",
                title = title,
                namespaces = names
            )
        };
        Some((text, details))
    }
}

/// Where the browser was on a kind: what "back" shows again (#395).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserPlace {
    /// Key of the selected object (`namespace/name`, or `name`).
    pub selected: Option<String>,
    /// The search by name.
    pub query: String,
}

pub struct ResourceBrowser {
    cluster: Entity<Cluster>,
    registry: PluginRegistry,
    /// `(group, kind)` opened from the sidebar; `None` while another screen
    /// is shown, which stops every watch of the browser.
    target: Option<(String, String)>,
    /// Namespace selection shared by the list screens.
    namespaces: Entity<Namespaces>,
    /// Copy of the active context's selection, so screens that read it
    /// through the browser (Helm) see it without a `cx`.
    scope: Option<NamespaceScope>,
    cache: Option<ResourceCache>,
    /// Created on first render, which has the window it needs.
    picker: Option<Entity<NamespacePicker>>,
    /// Filter by name, above the table.
    search: Option<Entity<InputState>>,
    /// The search to put in on the next render, which has the window it
    /// needs: empty when the kind changed, the old one on "back" (#395).
    pending_search: Option<String>,
    view: Option<KindView>,
    /// Bulk actions and those of the details: confirmed and run there,
    /// the last one kept across kinds until dismissed.
    actions: Entity<ObjectActions>,
    /// The details of the selected object, a view of its own (#363).
    details: Entity<ResourceDetails>,
    /// Requests of the details' buttons; subscribed on first render, which
    /// has the window their confirmations need.
    details_events: Option<Subscription>,
    /// Width of the details column, dragged by its left edge (#309).
    details_panel: Entity<SidePanelState>,
    /// Last lists on disk (#300).
    disk: Option<cached::DiskLists>,
    /// Reading the cached list of the kind while not connected.
    provisional_load: Option<(Vec<ListKey>, Task<()>)>,
    _subscriptions: Vec<Subscription>,
    _ages: Task<()>,
}

impl ResourceBrowser {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<Namespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
        ];
        // Searches and connections of Prometheus re-render the tables and
        // details that wait for them; the usage polling follows them even
        // while the browser is not on screen.
        if let Some(registry) = PrometheusRegistry::try_global(cx) {
            subscriptions.push(cx.observe(&registry, |this, _, cx| {
                this.sync_usage(cx);
                cx.notify();
            }));
        }
        let ages = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_REFRESH).await;
                let refreshed = this.update(cx, |this, cx| {
                    this.update_rows(cx);
                    // "cached 5m ago" beside the count of a list from disk.
                    if this.view.as_ref().is_some_and(|view| view.cached.is_some()) {
                        cx.notify();
                    }
                });
                if refreshed.is_err() {
                    break;
                }
            }
        });
        let details_panel = panel_width::remembered("browser.details", details::DETAILS_WIDTH, cx);
        let details = cx.new(|cx| ResourceDetails::new(cluster.clone(), cx));
        let actions = cx.new(|cx| ObjectActions::new(cluster.clone(), &details, cx));
        // A run starting or ending turns the action buttons off or on.
        subscriptions.push(cx.observe(&actions, |_, _, cx| cx.notify()));
        // Links of the details go to the workspace.
        subscriptions.push(cx.subscribe(&details, |_, _, reveal: &Reveal, cx| {
            cx.emit(reveal.clone())
        }));
        // Whatever the browser redraws for may change the selected object
        // or its state: the details redraw only if what they show changed.
        subscriptions.push(cx.observe_self(|this, cx| this.update_details(cx)));
        let mut this = Self {
            cluster,
            registry: PluginRegistry::with_builtins(),
            target: None,
            namespaces,
            scope: None,
            cache: None,
            picker: None,
            search: None,
            pending_search: None,
            view: None,
            actions,
            details,
            details_events: None,
            details_panel,
            disk: cached::DiskLists::new(),
            provisional_load: None,
            _subscriptions: subscriptions,
            _ages: ages,
        };
        this.sync(cx);
        this
    }

    /// Shows the objects of `(group, kind)`.
    pub fn open_kind(&mut self, group: &str, kind: &str, cx: &mut Context<Self>) {
        let target = Some((group.to_owned(), kind.to_owned()));
        if self.target != target {
            self.target = target;
            // A new kind starts without the previous kind's search.
            self.pending_search = Some(String::new());
            self.sync(cx);
        }
    }

    /// Selects `namespace/name` of the opened kind (from another screen).
    /// The caller adds its namespace to the selection first (see
    /// [`Namespaces::include`]).
    /// An empty `name` only shows the list (a CRD's "Show objects" link).
    pub fn reveal(&mut self, namespace: Option<&str>, name: &str, cx: &mut Context<Self>) {
        // The search could hide the object.
        self.pending_search = Some(String::new());
        self.sync(cx);
        if name.is_empty() {
            self.select(None, cx);
            return;
        }
        let key = match namespace {
            Some(namespace) => format!("{namespace}/{name}"),
            None => name.to_owned(),
        };
        self.select(Some(key), cx);
    }

    /// The selection and search of the opened kind, to come back to.
    pub fn place(&self, cx: &App) -> Option<BrowserPlace> {
        let view = self.view.as_ref()?;
        Some(BrowserPlace {
            selected: view.selected.clone(),
            query: self.query(cx),
        })
    }

    /// "Deployment web" with an object selected, else "Deployments".
    pub fn place_label(&self) -> String {
        let selected = self.view.as_ref().and_then(|view| {
            let name = view.selected.as_deref()?;
            let name = name.rsplit_once('/').map_or(name, |(_, name)| name);
            Some(t!(
                "browser-place-object",
                kind = view.entry.kind.api.kind.as_str(),
                name = name
            ))
        });
        selected.unwrap_or_else(|| self.title().to_string())
    }

    /// Shows `place` of the opened kind again ("back", #395). Call after
    /// [`open_kind`](Self::open_kind).
    pub fn restore(&mut self, place: &BrowserPlace, cx: &mut Context<Self>) {
        self.pending_search = Some(place.query.clone());
        self.sync(cx);
        self.select(place.selected.clone(), cx);
    }

    /// Called when another screen is shown: stops all watches of the browser.
    fn hide(&mut self, cx: &mut Context<Self>) {
        self.details
            .update(cx, |details, cx| details.set_target(None, cx));
        if self.target.is_some() {
            self.target = None;
            self.sync(cx);
        }
    }

    /// Namespace selection of the active context (kept in [`Namespaces`]).
    pub(super) fn scope(&self) -> Option<&NamespaceScope> {
        self.scope.as_ref()
    }

    /// Focuses the search field (`⌘F`).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.focus(window, cx));
        }
    }

    /// Opens the namespace list (`⌘⇧N`, the status bar).
    pub fn open_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cluster_scoped = self
            .view
            .as_ref()
            .is_some_and(|view| !view.entry.kind.namespaced);
        if let Some(picker) = self.picker.as_ref().filter(|_| !cluster_scoped) {
            picker.update(cx, |picker, cx| picker.open(window, cx));
        }
    }

    fn select(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if let Some(view) = &mut self.view {
            view.selected = key;
            cx.notify();
        }
    }

    /// Hands the selected object, as listed now, to the details.
    fn update_details(&mut self, cx: &mut Context<Self>) {
        let running = self.actions.read(cx).running();
        let target = self.view.as_ref().and_then(|view| {
            // Typed even when listed from an old disk cache (#372).
            let object = kubach_core::discovery::typed(
                view.selected_object()?.clone(),
                &view.entry.kind.api,
            );
            let namespace = object.metadata.namespace.as_deref();
            let reconnecting = view.watches.subscriptions().iter().any(|watch| {
                let ns = watch.key().namespace.as_deref();
                (ns.is_none() || ns == namespace) && watch.is_reconnecting()
            });
            Some(DetailsTarget {
                kind: view.entry.kind.clone(),
                plugin: view.plugin.clone(),
                object,
                provisional: view.provisional,
                from_disk: view.cached.is_some(),
                reconnecting,
                running,
            })
        });
        self.details
            .update(cx, |details, cx| details.set_target(target, cx));
    }

    /// Brings watches in line with the cluster, kind and namespaces: starts
    /// what is shown, drops (and so stops) what no longer is.
    fn sync(&mut self, cx: &mut Context<Self>) {
        // A confirmation asked in another context must not stay open.
        let cluster = self.cluster.read(cx);
        let cache = cluster.cache().cloned();
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
            // New connection (context switch or reconnect): start over,
            // except for a list from the cache, replaced in place below.
            self.view = self.view.take().filter(|view| view.provisional);
            self.cache = cache;
        }
        self.scope = self.namespaces.read(cx).scope().cloned();

        let visible_cache = self.cache.clone().filter(|_| self.target.is_some());
        let (Some(cache), Some(catalog)) = (visible_cache, catalog) else {
            if !self.show_provisional(cx) {
                self.view = None;
                self.provisional_load = None;
            }
            self.update_picker(cx);
            cx.notify();
            return;
        };

        let entry = self
            .target
            .as_ref()
            .and_then(|(group, kind)| kinds::find(&catalog, group, kind))
            .map(|(_, entry)| entry);
        let (Some(entry), Some(selection)) = (entry, self.scope().map(|s| s.selection.clone()))
        else {
            self.view = None;
            self.update_picker(cx);
            cx.notify();
            return;
        };
        let scope = Scope::new(entry.kind.namespaced, &selection);
        self.provisional_load = None;
        let unchanged = self.view.as_ref().is_some_and(|view| {
            !view.provisional
                && view.entry.id() == entry.id()
                && view.entry.kind.api.version == entry.kind.api.version
                && view.scope == scope
        });
        if !unchanged {
            let metrics = self.cluster.read(cx).metrics_server();
            let mut view = self.open(&cache, metrics, entry, scope, cx);
            if let Some(old) = self.view.take().filter(|old| {
                old.provisional && old.list_keys.is_some() && old.list_keys == view.list_keys
            }) {
                view.take_over(old);
                view.apply_cached();
            }
            self.view = Some(view);
            self.seed_from_cache(cx);
            // Look for Prometheus now rather than on the next frame.
            self.sync_usage(cx);
        }
        self.update_picker(cx);
        cx.notify();
    }

    fn open(
        &self,
        cache: &ResourceCache,
        metrics: Option<MetricsServer>,
        entry: KindEntry,
        scope: Scope,
        cx: &mut Context<Self>,
    ) -> KindView {
        let watches = cache.subscribe_many(
            &entry.kind,
            usage_namespaces(&scope).as_deref(),
            WatchFilter::default(),
        );
        let listener = watch_set(&watches, cx, Self::refresh_view);
        // The details of a selected pod or node share this poll. Without
        // metrics-server the feed waits for Prometheus, see `sync_usage`.
        let usage = UsageKind::of(&entry.kind).map(|kind| match metrics {
            Some(server) => {
                let scope = usage::poll_scope(kind, usage_namespaces(&scope));
                let states = usage::metrics_server_states(server.subscribe(scope));
                UsageFeed::metrics_server(kind, Self::watch_usage(states, cx))
            }
            None => UsageFeed::prometheus(kind),
        });
        let plugin = self.registry.plugin_for_resource(&entry.kind.api);
        let hidden_column = scope
            .single_namespace()
            .then(|| rows::namespace_column(plugin.as_ref()))
            .flatten();
        let list_keys = self.live_keys(&entry, &scope, cx);
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
        view.list_keys = list_keys;
        view._listener = listener;
        view
    }

    /// Feeds polled usage into the table of the current view.
    fn watch_usage(mut states: usage::States, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(state) = states.next().await {
                let updated = this.update(cx, |this, cx| {
                    if let Some(feed) = this.view.as_mut().and_then(|v| v.usage.as_mut()) {
                        let note = feed.note();
                        feed.state = state;
                        // The note beside the count ("metrics unavailable").
                        if feed.note() != note {
                            cx.notify();
                        }
                    }
                    this.update_rows(cx);
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// Without metrics-server, usage comes from the context's Prometheus
    /// (#179): the same connection as the details panel, from the app-wide
    /// registry. Runs on every render and registry change, so polling
    /// follows the registry (#186): it restarts when another server is
    /// picked or "Detect again" reconnects, stops when there is none (Off,
    /// not found), and starts when one turns up after "metrics
    /// unavailable".
    fn sync_usage(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_ref().filter(|view| !view.provisional) else {
            return;
        };
        if !view
            .usage
            .as_ref()
            .is_some_and(UsageFeed::follows_prometheus)
        {
            return;
        }
        let namespaces = usage_namespaces(&view.scope);
        let Some(status) = prometheus_status(&self.cluster, cx) else {
            return;
        };
        let Some(feed) = self.view.as_mut().and_then(|view| view.usage.as_mut()) else {
            return;
        };
        if let Some(prometheus) = feed.follow(&status) {
            let samples = usage::poll_prometheus(feed.kind, prometheus, namespaces);
            let states = samples.map(MetricsState::from_result).boxed();
            feed._task = Some(Self::watch_usage(states, cx));
        }
    }

    fn refresh_view(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &mut self.view else {
            return;
        };
        view.apply_cached();
        self.save_to_cache();
        let Some(view) = &mut self.view else {
            return;
        };
        // Objects changed (e.g. a CronJob got suspended): recompute what
        // applies to the checked ones.
        view.update_bulk_actions();
        self.update_rows(cx);
        cx.notify();
    }

    /// Rebuilds the table rows; redraws only if they changed, so the age
    /// refresh and usage polls cost nothing while nothing visible changes
    /// (#355). Callers that change more than the rows notify themselves.
    fn update_rows(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.view else {
            return;
        };
        let query = self.query(cx);
        let changed = match &view.table {
            Some(table) => {
                let rows = view.rows(&query);
                table.update(cx, |table, cx| table.set_rows(rows, cx))
            }
            None => false,
        };
        if changed {
            cx.notify();
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search
            .as_ref()
            .map(|search| search.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn ensure_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.pending_search.take();
        if let Some(search) = self.search.clone() {
            if let Some(query) = pending
                && search.read(cx).value() != query.as_str()
            {
                search.update(cx, |search, cx| search.set_value(query, window, cx));
                self.update_rows(cx);
                cx.notify();
            }
            return;
        }
        let search = FilterBar::search_state("", window, cx);
        self._subscriptions
            .extend(crate::i18n::localized_placeholder(
                &search,
                || t!("browser-search-placeholder"),
                window,
                cx,
            ));
        if let Some(query) = pending.filter(|query| !query.is_empty()) {
            search.update(cx, |search, cx| search.set_value(query, window, cx));
        }
        self._subscriptions
            .push(cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.update_rows(cx);
                    cx.notify();
                }
            }));
        self.search = Some(search);
    }

    fn ensure_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let browser = cx.entity().downgrade();
        let Some(view) = &mut self.view else {
            return;
        };
        // Column headers are in the language the table was made in: after
        // a switch the table is made again (losing the checked rows).
        let locale = kubach_i18n::locale();
        if view.table.is_some() && view.table_locale != locale {
            view.table = None;
            view._table_events = None;
            view.checked.clear();
            view.update_bulk_actions();
        }
        if view.table.is_some() {
            return;
        }
        view.table_locale = locale;
        let mut columns = rows::columns(view.plugin.as_ref());
        if view.usage.is_some() {
            columns.extend(usage::columns());
        }
        // Every column the kind can show, for the remembered order.
        let all_columns: Vec<SharedString> = columns.iter().map(|c| c.key().clone()).collect();
        if let Some(ix) = view.hidden_column.filter(|&ix| ix < columns.len()) {
            columns.remove(ix);
        }
        let rows = view.rows(&query);
        let table = cx.new(|cx| {
            let mut table = ResourceTable::new(columns, window, cx)
                .checkable(cx)
                .movable_columns(cx)
                .on_select(move |row, _, cx| {
                    let key = row.key.to_string();
                    browser
                        .update(cx, |browser, cx| browser.select(Some(key), cx))
                        .ok();
                });
            table.set_rows(rows, cx);
            table
        });
        let api = &view.entry.kind.api;
        let kind = kind_key(&api.group, &api.kind);
        column_order::remember(&table, kind, all_columns, cx);
        view._table_events = Some(cx.subscribe(
            &table,
            |this, _, event: &ResourceTableEvent, cx| {
                let ResourceTableEvent::CheckedChanged(keys) = event;
                if let Some(view) = &mut this.view {
                    view.checked = keys.clone();
                    view.update_bulk_actions();
                    cx.notify();
                }
            },
        ));
        view.table = Some(table);
    }

    fn clear_checked(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = self.view.as_ref().and_then(|view| view.table.clone()) {
            table.update(cx, |table, cx| table.clear_checked(cx));
        }
    }

    /// Asks for confirmation if `action` needs it, then runs it on every
    /// checked object.
    fn request_bulk(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = &self.view else {
            return;
        };
        let objects = view.checked_objects();
        let subject = view.subject();
        let where_ = view.scope.describe();
        self.actions.update(cx, |actions, cx| {
            actions.request_action(action, objects, subject, where_, window, cx)
        });
    }

    /// Close drops the selection; actions go to [`ObjectActions`].
    fn on_details_event(
        &mut self,
        _: &Entity<ResourceDetails>,
        event: &DetailsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DetailsEvent::Close => {
                if let Some(table) = self.view.as_ref().and_then(|v| v.table.clone()) {
                    table.update(cx, |table, cx| table.clear_selection(cx));
                }
                self.select(None, cx);
            }
            // The browser's details have no screen of their own to go back to.
            DetailsEvent::Back => {}
            _ => {
                let Some(view) = &self.view else {
                    return;
                };
                let where_ = view.scope.describe();
                self.actions.update(cx, |actions, cx| {
                    actions.on_details_event(
                        event,
                        |kind, plugin| self.subject_for(kind, plugin),
                        where_,
                        window,
                        cx,
                    )
                });
            }
        }
    }

    /// The subject of an action the details ask for on an object of `kind`.
    pub(super) fn subject_for(
        &self,
        kind: &kubach_core::ResourceKind,
        plugin: &Arc<dyn ResourcePlugin>,
    ) -> Subject {
        actions::subject_for(self.view.as_ref().map(KindView::subject), kind, plugin)
    }

    fn ensure_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            return;
        }
        let namespaces = self.namespaces.clone();
        self.picker =
            Some(cx.new(|cx| NamespacePicker::new("namespace-picker", namespaces, window, cx)));
        self.update_picker(cx);
    }

    /// Mutes the picker for kinds without a namespace.
    fn update_picker(&self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        let cluster_scoped = self
            .view
            .as_ref()
            .is_some_and(|view| !view.entry.kind.namespaced);
        picker.update(cx, |picker, cx| {
            picker.set_cluster_scoped(cluster_scoped, cx)
        });
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.set_value("", window, cx));
        }
        self.update_rows(cx);
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        match (&self.view, &self.target) {
            (Some(view), _) => view.entry.title.clone().into(),
            (None, Some((_, kind))) => kind.clone().into(),
            (None, None) => t!("browser-resources").into(),
        }
    }

    /// Placeholder while there is nothing to list (not connected, kind not
    /// served).
    fn render_unavailable(&self, cx: &App) -> AnyElement {
        let cluster = self.cluster.read(cx);
        let state = match cluster.state() {
            ConnectionState::Disconnected => {
                Some(EmptyState::new(IconName::Globe, t!("browser-no-cluster")))
            }
            ConnectionState::Connecting => Some(EmptyState::new(
                IconName::LoaderCircle,
                t!("browser-connecting"),
            )),
            ConnectionState::Error(error) => Some(
                EmptyState::new(IconName::CircleX, t!("browser-not-connected"))
                    .description(error.clone()),
            ),
            ConnectionState::Connected { .. } => None,
        };
        let state = state.unwrap_or_else(|| match cluster.catalog() {
            CatalogState::Pending => {
                EmptyState::new(IconName::LoaderCircle, t!("browser-discovering"))
            }
            CatalogState::Error(error) => {
                EmptyState::new(IconName::CircleX, t!("browser-discovery-failed"))
                    .description(error.clone())
            }
            CatalogState::Ready(_) => EmptyState::new(IconName::Inbox, t!("browser-not-served"))
                .description(t!("browser-not-served-description")),
        });
        state.into_any_element()
    }

    /// "Selected: N", the actions valid for all checked objects, and a way
    /// to clear the selection.
    fn render_action_bar(
        &self,
        view: &KindView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let running = self.actions.read(cx).running();
        let none_common = view.bulk_actions.is_empty().then(|| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t!("browser-no-common-action"))
        });
        let selected = div()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground)
            .child(t!("browser-selected", count = view.checked.len()));
        let buttons: Vec<_> = view
            .bulk_actions
            .iter()
            .enumerate()
            .map(|(ix, action)| {
                let action = action.clone();
                Button::new(("bulk-action", ix))
                    .xsmall()
                    .label(plugin_text::action_title(&action))
                    .disabled(running)
                    .map(|button| {
                        if action.dangerous {
                            button.danger()
                        } else {
                            button.outline()
                        }
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.request_bulk(action.clone(), window, cx)
                    }))
            })
            .collect();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(selected)
            .children(buttons)
            .children(none_common)
            .child(
                Button::new("bulk-clear")
                    .ghost()
                    .xsmall()
                    .label(t!("browser-clear-selection"))
                    .on_click(cx.listener(|this, _, _, cx| this.clear_checked(cx))),
            )
    }

    fn render_list(&self, view: &KindView, cx: &mut Context<Self>) -> AnyElement {
        let action_bar =
            (!view.checked.is_empty()).then(|| self.render_action_bar(view, cx).into_any_element());
        let title = view.entry.title.as_str();
        let (scope, namespace, namespaces) = view.scope.message_args();
        let empty = view.snapshot.objects.is_empty();
        let placeholder =
            match &view.snapshot.status {
                WatchStatus::Loading if empty => Some(EmptyState::new(
                    IconName::LoaderCircle,
                    t!("browser-loading", title = title),
                )),
                WatchStatus::Error(error) if empty && error.forbidden => Some(
                    EmptyState::new(IconName::Ban, t!("browser-no-access", title = title))
                        .description(t!(
                            "browser-no-access-description",
                            title = title,
                            scope = scope,
                            namespace = namespace.as_str(),
                            namespaces = namespaces,
                            error = error.message.as_str()
                        )),
                ),
                WatchStatus::Error(error) if empty => Some(
                    EmptyState::new(IconName::CircleX, t!("browser-cant-load", title = title))
                        .description(if error.connection {
                            t!(
                                "browser-lost-connection-retrying",
                                error = error.message.as_str()
                            )
                        } else {
                            t!("browser-error-retrying", error = error.message.as_str())
                        }),
                ),
                WatchStatus::Ready if empty && view.snapshot.errors.is_empty() => Some(
                    EmptyState::new(IconName::Inbox, t!("browser-empty", title = title))
                        .description(t!(
                            "browser-nothing-found",
                            scope = scope,
                            namespace = namespace.as_str(),
                            namespaces = namespaces
                        )),
                ),
                _ => None,
            };
        if let Some(placeholder) = placeholder {
            return div().flex_1().child(placeholder).into_any_element();
        }
        let Some(table) = &view.table else {
            return div().flex_1().into_any_element();
        };
        let query = self.query(cx);
        let shown = view.matching(&query).count();
        if shown == 0 && !view.snapshot.objects.is_empty() {
            let placeholder = FilterBar::no_matches(
                title,
                &query,
                cx.listener(|this, _, window, cx| this.clear_search(window, cx)),
            );
            return div().flex_1().child(placeholder).into_any_element();
        }
        let theme = cx.theme();

        let partial = view.snapshot.errors.len() < view.watches.subscriptions().len();
        let banner = view.error_banner().map(|(text, details)| {
            let color = if partial { theme.warning } else { theme.danger };
            let details = SharedString::from(details);
            div()
                .id("watch-error-banner")
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(details.clone()).build(window, cx)
                })
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(color)
                .text_sm()
                .text_color(color)
                .child(text)
        });
        let count = match view.snapshot.objects.len() {
            total if shown < total => t!("browser-shown-of", shown = shown, total = total),
            total => total.to_string(),
        };
        let metrics_note = view
            .usage
            .as_ref()
            .and_then(UsageFeed::note)
            .map(|note| format!(" · {note}"))
            .unwrap_or_default();
        let cache_note = view
            .cached
            .as_ref()
            .map(|cached| format!(" · {}", cached.age_label()))
            .unwrap_or_default();
        // Without a connection the cached list stays, marked as such.
        let offline = view.provisional.then(|| {
            let text = match self.cluster.read(cx).state() {
                ConnectionState::Error(error) => {
                    t!("browser-offline-error", error = error.as_str())
                }
                ConnectionState::Disconnected => t!("browser-offline-disconnected"),
                _ => t!("browser-offline-connecting"),
            };
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        });
        let toolbar = action_bar.unwrap_or_else(|| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{}{metrics_note}{cache_note}",
                    t!(
                        "browser-list-count",
                        count = count.as_str(),
                        title = title,
                        scope = scope,
                        namespace = namespace.as_str(),
                        namespaces = namespaces
                    )
                ))
                .into_any_element()
        });
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(offline)
            .children(banner)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    // Cached: a tick of the details' ages or a redraw of
                    // the toolbar does not lay the rows out again (#363).
                    .child(super::cached(table, cx)),
            )
            .into_any_element()
    }

    /// Hint after deleted namespaces left the selection.
    fn render_pruned(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let pruned = self.namespaces.read(cx).pruned().to_vec();
        if pruned.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let text = t!(
            "browser-pruned",
            count = pruned.len(),
            names = pruned.join(", ")
        );
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
                .child(
                    Button::new("dismiss-pruned")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.namespaces
                                .update(cx, |namespaces, cx| namespaces.dismiss_pruned(cx));
                        })),
                ),
        )
    }
}
/// The Prometheus of the active context from the app-wide registry,
/// shared by the tables and the details; `None` without a connection.
/// Starts the search or connection the settings ask for (ADR 0009).
fn prometheus_status(cluster: &Entity<Cluster>, cx: &mut App) -> Option<PrometheusStatus> {
    let cluster = cluster.read(cx);
    cluster.connection()?;
    let context = cluster.active()?.name.clone();
    let registry = PrometheusRegistry::try_global(cx)?;
    Some(registry.update(cx, |registry, cx| registry.ensure(&context, cx)))
}

/// "Managed by" links of the details ask the workspace to open an object.
impl EventEmitter<Reveal> for ResourceBrowser {}

impl ScreenActivity for ResourceBrowser {
    /// Inactive, the browser stops every watch and poll and forgets the
    /// kind; [`ResourceBrowser::open_kind`] activates it with a kind.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if !active {
            self.hide(cx);
        }
    }
}

impl Render for ResourceBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_table(window, cx);
        self.ensure_picker(window, cx);
        self.ensure_search(window, cx);
        if self.details_events.is_none() {
            let details = self.details.clone();
            self.details_events = Some(cx.subscribe_in(&details, window, Self::on_details_event));
        }
        self.sync_usage(cx);

        let mut actions: Vec<AnyElement> = Vec::new();
        // New resources go to the first selected namespace, else the
        // context's, else `default`.
        let new_namespace = self
            .scope()
            .and_then(|scope| scope.selection.items().first().map(ToString::to_string))
            .or_else(|| self.cluster.read(cx).active()?.namespace.clone())
            .unwrap_or_else(|| "default".into());
        let secrets = self
            .target
            .as_ref()
            .is_some_and(|(group, kind)| is_core_secret(group, kind));
        if secrets {
            actions.push(
                crate::editor::new_secret_button(&self.cluster, new_namespace.clone(), cx)
                    .into_any_element(),
            );
        }
        let namespaces = self
            .target
            .as_ref()
            .is_some_and(|(group, kind)| kubach_core::namespace::is_namespace_kind(group, kind));
        if namespaces {
            actions.push(
                super::namespace_form::new_namespace_button(&self.cluster, cx).into_any_element(),
            );
        } else {
            actions.push(crate::editor::new_button(
                &self.cluster,
                new_namespace,
                self.target.clone(),
                cx,
            ));
        }

        // The list column (hints, toolbar, table) and, beside it from the
        // top, the details of the selected object.
        let root = div().size_full().min_w_0().min_h_0().flex().gap_4().p_8();
        let list = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .children(self.render_pruned(cx));

        let Some(view) = &self.view else {
            return root.child(list.child(self.render_unavailable(cx)));
        };
        // Cached: the details redraw on their own (ages, events, metrics,
        // a new version of the object), not with the table or toolbar.
        let details = view.selected_object().map(|_| {
            SidePanel::new("details-panel", &self.details_panel)
                .child(super::cached(&self.details, cx))
        });
        let mut toolbar = FilterBar::new().actions(actions);
        // A cluster-scoped kind has no namespace to pick.
        if let Some(picker) = self.picker.as_ref().filter(|_| view.entry.kind.namespaced) {
            toolbar = toolbar.filter(picker.clone());
        }
        if let Some(search) = &self.search {
            toolbar = toolbar.search(search);
        }
        let list = list.when(details.is_some(), |list| list.min_w(SIDE_PANEL_CONTENT_MIN));
        root.child(
            list.child(toolbar).child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_list(view, cx)),
            ),
        )
        .children(details)
    }
}
