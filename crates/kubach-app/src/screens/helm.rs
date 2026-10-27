//! Helm (ADR 0006), two pages shown by separate sidebar items. Releases:
//! the list in the selected namespaces, release details (values, manifest,
//! hooks, notes), revision history, diffs between revisions, and rollback,
//! uninstall and upgrade through the user's `helm`. Charts (`helm_charts`):
//! the user's chart repositories, chart search and install. One screen keeps
//! both pages, so they share the progress toast of `helm` calls and keep
//! their state while another sidebar item is shown.
//!
//! Reading is native (`kubach_core::helm`); nothing here writes the release
//! storage. The namespace selection is the shared one (`Namespaces`), so
//! every list screen shows the same namespaces.
//!
//! The release data (storage watch, driver, list) is the `HelmReleases`
//! model (`releases`); rendering is split by part: the list (`list`), a
//! release's details (`detail`), the diff between revisions (`diff`) and
//! the `helm` calls (`actions`).

mod actions;
mod detail;
mod diff;
mod list;
mod releases;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{EditorState, InputState};
use gpui_kit::component::{Disableable as _, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::cache::ResourceCache;
use kubach_core::helm::cli::HelmTarget;
use kubach_core::helm::repo::{self, HelmPaths, LoadedRepository};
use kubach_core::helm::updates::{self, ChartUpdate};
use kubach_core::helm::{ChartInfo, Driver, Release, StoredRevision};
use kubach_i18n::t;
use kubach_ui::ButtonLoading as _;
use kubach_ui::{ActionToast, EmptyState, FilterBar, MultiSelection, PageHeader, ResourceTable};

use self::releases::{HelmReleases, ReleasesEvent};
use super::activity::ScreenActivity;
use super::helm_charts::{ChartsEvent, HelmCharts};
use super::helm_release::{DiffSource, Tab};
use super::helm_upgrade::HelmUpgrade;
use super::namespace_picker::{NamespacePicker, Namespaces as SharedNamespaces};
use super::namespaces::selected_names;
use crate::cluster::{CatalogState, Cluster};
use crate::settings::AppSettings;

/// Namespaces the list is loaded for: `None` means all.
type Namespaces = Option<Vec<String>>;

enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// What the read-only viewer currently holds.
#[derive(Clone, PartialEq)]
struct ViewerKey {
    tab: Tab,
    revision: u32,
}

/// One open release.
struct Detail {
    namespace: String,
    name: String,
    /// All revisions, oldest first.
    history: Load<Vec<Release>>,
    /// Revision picked in the history; `None` follows the latest.
    revision: Option<u32>,
    tab: Tab,
    /// Values, manifests and hooks are masked until the user reveals them.
    revealed: bool,
    diff: Option<(u32, u32)>,
    diff_source: DiffSource,
    viewer: Option<Entity<EditorState>>,
    viewer_key: Option<ViewerKey>,
    history_table: Option<Entity<ResourceTable>>,
    /// Open upgrade panel; replaces the tabs.
    upgrade: Option<(Entity<HelmUpgrade>, Subscription)>,
    /// Latest storage object the history was loaded for; when the watch
    /// sees another one, the history is loaded again.
    seen: Option<StoredRevision>,
    _task: Task<()>,
}

impl Detail {
    fn revisions(&self) -> &[Release] {
        match &self.history {
            Load::Ready(history) => history,
            _ => &[],
        }
    }

    fn latest(&self) -> Option<&Release> {
        self.revisions().last()
    }

    /// The revision shown in the tabs.
    fn shown(&self) -> Option<&Release> {
        match self.revision {
            Some(revision) => self.find(revision),
            None => self.latest(),
        }
    }

    fn find(&self, revision: u32) -> Option<&Release> {
        self.revisions().iter().find(|r| r.revision == revision)
    }
}

/// Page of the screen, picked by the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelmPage {
    Releases,
    Charts,
}

impl HelmPage {
    fn title(self) -> String {
        match self {
            HelmPage::Releases => t!("helm-page-releases"),
            HelmPage::Charts => t!("helm-page-charts"),
        }
    }
}

/// Asks the workspace to show a page through the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShowHelmPage(pub HelmPage);

/// What to do once a `helm` call succeeded, besides reloading.
#[derive(Clone)]
enum AfterSuccess {
    Reload,
    /// The upgrade is done: close its panel.
    CloseUpgrade,
    /// The release is gone: back to the list.
    CloseRelease,
    /// The release is uninstalled: delete the claims it left behind (#273),
    /// then back to the list unless its history was kept.
    DeleteClaims {
        namespace: String,
        claims: Vec<kubach_core::helm::claims::LeftoverClaim>,
        close: bool,
        /// Where the uninstall was confirmed: the claims are deleted there,
        /// and only while it is still the active connection (#344).
        connection: kubach_core::Connection,
    },
    /// A repository command: read the repositories again (also on failure,
    /// which may have changed some).
    ReloadCharts,
    /// The chart is installed: open the new release in Releases.
    Installed {
        namespace: String,
        name: String,
    },
}

/// Claims of a release being looked up before its uninstall is confirmed;
/// the result is for that release only (#344).
struct UninstallLookup {
    namespace: String,
    name: String,
    _task: Task<()>,
}

impl UninstallLookup {
    fn is_for(&self, namespace: &str, name: &str) -> bool {
        self.namespace == namespace && self.name == name
    }
}

/// An install from the Charts page succeeded: its release shows helm's
/// output until dismissed, so closing the toast loses nothing (#271).
struct InstalledNotice {
    namespace: String,
    name: String,
    output: String,
    /// The output is unfolded.
    open: bool,
}

/// A `helm` call started from the screen and its outcome.
struct ActionRun {
    /// Progress and outcome, shown over the window.
    toast: ActionToast,
    outcome: Option<Result<String, String>>,
    _task: Task<()>,
}

pub struct HelmScreen {
    cluster: Entity<Cluster>,
    /// The shared namespace selection.
    namespaces: Entity<SharedNamespaces>,
    /// Page shown while active.
    page: HelmPage,
    /// On screen; otherwise no watches and nothing loads.
    active: bool,
    /// Connection the data was loaded from.
    cache: Option<ResourceCache>,
    /// Releases of the selected namespaces: storage watch, driver, list.
    releases: Entity<HelmReleases>,
    /// Created on first render, which has the window a table needs; rebuilt
    /// when the Namespace column comes or goes.
    table: Option<(Entity<ResourceTable>, bool)>,
    picker: Option<Entity<NamespacePicker>>,
    /// Search of the release list by name.
    search: Option<Entity<InputState>>,
    query: String,
    detail: Option<Detail>,
    action: Option<ActionRun>,
    installed: Option<InstalledNotice>,
    /// Created when the Charts page is first shown (it needs the window).
    charts: Option<(Entity<HelmCharts>, Subscription)>,
    /// The user's chart repositories with their cached indexes: newer chart
    /// versions of releases and the versions an upgrade picks from (#273).
    repos: Vec<LoadedRepository>,
    repos_task: Option<Task<()>>,
    /// Claims an uninstall of `(namespace, release)` would leave are being
    /// looked up.
    uninstall_lookup: Option<UninstallLookup>,
    /// Confirmations open, closed when their context is no longer active.
    confirms: crate::action_context::OpenConfirms,
    _subscriptions: Vec<Subscription>,
}

impl HelmScreen {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<SharedNamespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let releases = cx.new(|_| HelmReleases::new(cluster.clone()));
        let mut subscriptions = vec![
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
            cx.observe(&releases, |_, _, cx| cx.notify()),
            cx.subscribe(&releases, |this, _, event: &ReleasesEvent, cx| {
                let ReleasesEvent::Refreshed { listed } = *event;
                this.refreshed(Some(listed), cx);
            }),
        ];
        // The storage driver of a context can be picked in the settings.
        if let Some(settings) = AppSettings::try_global(cx) {
            subscriptions.push(cx.observe(&settings, |this, _, cx| this.sync(cx)));
        }
        // Downloading helm (the hint and buttons of a release) shows here;
        // the screen is embedded cached (#363).
        if let Some(downloads) = crate::tools::global(cx) {
            subscriptions.push(cx.observe(&downloads, |_, _, cx| cx.notify()));
        }
        // Table headers are kept in the tables: rebuild them in the new
        // language.
        if let Some(language) = crate::i18n::global(cx) {
            subscriptions.push(cx.observe(&language, |this, _, cx| {
                this.table = None;
                if let Some(detail) = &mut this.detail {
                    detail.history_table = None;
                }
                cx.notify();
            }));
        }
        // A chart icon arrived (#167).
        if let Some(images) = crate::images::RemoteImages::global(cx) {
            subscriptions.push(cx.observe(&images, |this, _, cx| {
                if let Some((table, _)) = &this.table {
                    table.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            }));
        }
        Self {
            cluster,
            namespaces,
            page: HelmPage::Releases,
            active: false,
            cache: None,
            releases,
            table: None,
            picker: None,
            search: None,
            query: String::new(),
            detail: None,
            action: None,
            installed: None,
            charts: None,
            repos: Vec::new(),
            repos_task: None,
            uninstall_lookup: None,
            confirms: Default::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Picks the page shown while the screen is active. Each page keeps
    /// its state.
    pub fn show(&mut self, page: HelmPage, cx: &mut Context<Self>) {
        let before = self.shown_page();
        self.page = page;
        self.shown_changed(before, cx);
    }

    /// The page on screen; `None` while inactive.
    fn shown_page(&self) -> Option<HelmPage> {
        self.active.then_some(self.page)
    }

    fn shown_changed(&mut self, before: Option<HelmPage>, cx: &mut Context<Self>) {
        let shown = self.shown_page();
        if before != shown {
            self.sync(cx);
            // Repositories may have changed in the terminal meanwhile.
            if shown == Some(HelmPage::Charts) {
                self.reload_charts(cx);
            }
            if shown == Some(HelmPage::Releases) {
                self.reload_repos(cx);
            }
            cx.notify();
        }
    }

    /// Reads the repository list and cached indexes again, natively and
    /// off the UI thread; unchanged indexes are reused, also the ones the
    /// Charts page already parsed.
    pub(super) fn reload_repos(&mut self, cx: &mut Context<Self>) {
        let Some(paths) = HelmPaths::from_env() else {
            return;
        };
        let mut previous = self.repos.clone();
        if let Some((charts, _)) = &self.charts {
            previous.extend(charts.read(cx).repositories().iter().cloned());
        }
        let load = cx.background_spawn(async move { repo::load(&paths, &previous) });
        self.repos_task = Some(cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                this.repos_task = None;
                // Unreadable: no updates shown rather than an error here.
                this.repos = result.unwrap_or_default();
                this.update_rows(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// A newer version of `chart` in the user's repositories.
    pub(super) fn chart_update(&self, chart: &ChartInfo) -> Option<ChartUpdate> {
        updates::available_update(&self.repos, chart)
    }

    fn reload_charts(&self, cx: &mut Context<Self>) {
        if let Some((charts, _)) = &self.charts {
            charts.update(cx, |charts, cx| charts.reload(cx));
        }
    }

    fn ensure_charts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.charts.is_some() || self.shown_page() != Some(HelmPage::Charts) {
            return;
        }
        let screen = cx.entity().downgrade();
        let charts = cx.new(|cx| HelmCharts::new(screen, window, cx));
        let subscription = cx.subscribe_in(
            &charts,
            window,
            |this, _, event: &ChartsEvent, window, cx| match event {
                ChartsEvent::Run { title, command } => this.run_helm(
                    title.clone(),
                    command.clone(),
                    None,
                    AfterSuccess::ReloadCharts,
                    cx,
                ),
                ChartsEvent::Install(input) => {
                    let input = (**input).clone();
                    this.confirm_install(input.target, input.request, input.previewed, window, cx)
                }
            },
        );
        charts.update(cx, |charts, cx| charts.reload(cx));
        self.charts = Some((charts, subscription));
    }

    /// Context, kubeconfig and storage driver an install runs with (the
    /// namespace is set by the install panel); `None` without a connection.
    pub(super) fn install_target(&self, cx: &App) -> Option<HelmTarget> {
        self.cluster.read(cx).connection()?;
        self.helm_target("", cx)
    }

    /// Namespaces of the cluster, for the install panel to tell a new
    /// namespace from an existing one.
    pub(super) fn shared_namespaces(&self) -> Entity<SharedNamespaces> {
        self.namespaces.clone()
    }

    /// Namespace an install starts with: the only selected namespace, else
    /// the context's, else `default`.
    pub(super) fn install_namespace(&self, cx: &App) -> String {
        if let Some(Some(namespaces)) = self.wanted(cx)
            && let [namespace] = namespaces.as_slice()
        {
            return namespace.clone();
        }
        self.cluster
            .read(cx)
            .active()
            .and_then(|context| context.namespace.clone())
            .unwrap_or_else(|| "default".to_owned())
    }

    fn selection(&self, cx: &App) -> Option<MultiSelection> {
        Some(self.namespaces.read(cx).scope()?.selection.clone())
    }

    fn wanted(&self, cx: &App) -> Option<Namespaces> {
        Some(selected_names(&self.selection(cx)?))
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        // A confirmation asked in another context must not stay open.
        self.confirms.close_stale(&self.cluster, cx);
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
            // Context switch or reconnect: start over.
            self.cache = cache;
            self.releases.update(cx, |releases, _| releases.reset());
            self.detail = None;
            self.uninstall_lookup = None;
            self.action = self.action.take().filter(|run| run.outcome.is_none());
            // The install panel lists namespaces and installs into the
            // context it was opened in (#344).
            if let Some((charts, _)) = &self.charts {
                charts.update(cx, |charts, cx| charts.close_install(cx));
            }
        }
        if !self.active {
            // Hidden: no watches. Known summaries are kept, so showing the
            // screen again only fetches what changed.
            self.releases.update(cx, |releases, _| releases.stop());
            cx.notify();
            return;
        }
        let preference = self.preference(cx);
        if let (Some(cache), Some(catalog), Some(wanted)) =
            (self.cache.clone(), catalog, self.wanted(cx))
            && self.releases.read(cx).needs_watch(&wanted, preference)
        {
            self.watch_storage(&cache, &catalog, wanted, preference, cx);
        }
        cx.notify();
    }

    /// Storage driver picked in the settings of the open context.
    fn preference(&self, cx: &App) -> Option<Driver> {
        let context = self.cluster.read(cx).active()?.name.clone();
        let settings = AppSettings::try_global(cx)?;
        settings.read(cx).get().clusters.helm_driver(&context)
    }

    /// Watches the release storage of `namespaces`; the list follows it.
    fn watch_storage(
        &mut self,
        cache: &ResourceCache,
        catalog: &kubach_core::ApiCatalog,
        namespaces: Namespaces,
        preference: Option<Driver>,
        cx: &mut Context<Self>,
    ) {
        self.releases.update(cx, |releases, cx| {
            releases.watch(cache, catalog, namespaces, preference, cx)
        });
        // The open release may have left the selected namespaces.
        let left = match (
            &self.detail,
            self.releases.read(cx).loaded_for().cloned().flatten(),
        ) {
            (Some(detail), Some(namespaces)) => !namespaces.contains(&detail.namespace),
            _ => false,
        };
        if left {
            self.close_detail(cx);
        }
        self.refresh(cx);
    }

    /// Reads the storage watches again (see `HelmReleases::refresh`).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let refreshed = self
            .releases
            .update(cx, |releases, cx| releases.refresh(cx));
        self.refreshed(refreshed, cx);
    }

    /// After the releases were read again (`Some`): updates the rows when
    /// the list was updated, and reloads the open release when it changed.
    fn refreshed(&mut self, listed: Option<bool>, cx: &mut Context<Self>) {
        let Some(listed) = listed else {
            return;
        };
        if listed {
            self.update_rows(cx);
        }
        self.follow_detail(cx);
        cx.notify();
    }

    fn stored_of(&self, namespace: &str, name: &str, cx: &App) -> Option<StoredRevision> {
        self.releases.read(cx).stored_of(namespace, name)
    }

    /// Focuses the search of the shown page (`⌘F`).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.shown_page() {
            None => {}
            Some(HelmPage::Releases) => {
                if self.detail.is_some() {
                    self.close_detail(cx);
                }
                if let Some(search) = &self.search {
                    search.update(cx, |search, cx| search.focus(window, cx));
                }
            }
            Some(HelmPage::Charts) => {
                if let Some((charts, _)) = &self.charts {
                    charts.update(cx, |charts, cx| charts.focus_search(window, cx));
                }
            }
        }
    }

    /// Opens the namespace list (`⌘⇧N`) of the Releases page, the one the
    /// namespaces filter; the workspace shows that page first.
    pub fn open_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = &self.picker {
            picker.update(cx, |picker, cx| picker.open(window, cx));
        }
    }

    /// Fetches every release of the list and the open release again. Not
    /// needed for freshness (the watch keeps the list current); a way out
    /// when something looks off.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let refreshed = self.releases.update(cx, |releases, cx| releases.reload(cx));
        self.refreshed(refreshed, cx);
        if let Some(detail) = &self.detail {
            let (namespace, name) = (detail.namespace.clone(), detail.name.clone());
            self.open_release(namespace, name, cx);
        }
        cx.notify();
    }

    fn render_unavailable(&self, cx: &App) -> Option<AnyElement> {
        let state = match self.cluster.read(cx).state() {
            ConnectionState::Disconnected => {
                EmptyState::new(IconName::Globe, t!("helm-no-cluster"))
            }
            ConnectionState::Connecting => {
                EmptyState::new(IconName::LoaderCircle, t!("helm-connecting"))
            }
            ConnectionState::Error(error) => {
                EmptyState::new(IconName::CircleX, t!("helm-not-connected"))
                    .description(error.clone())
            }
            ConnectionState::Connected { .. } => return None,
        };
        Some(state.into_any_element())
    }
}

impl EventEmitter<ShowHelmPage> for HelmScreen {}

impl ScreenActivity for HelmScreen {
    /// Hidden, the screen stops its namespace watch and loads nothing.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let before = self.shown_page();
        self.active = active;
        self.shown_changed(before, cx);
    }
}

impl Render for HelmScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filters(window, cx);
        self.ensure_table(window, cx);
        self.ensure_detail_views(window, cx);
        self.ensure_charts(window, cx);

        let page = self.page;
        let header = PageHeader::new(page.title());
        let root = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(header);
        if page == HelmPage::Charts {
            // Browsing charts needs no cluster; installing checks for one.
            if let Some((charts, _)) = &self.charts {
                return root.child(div().flex_1().min_h_0().flex().child(charts.clone()));
            }
            return root;
        }
        let reloading = self.cache.is_some() && self.releases.read(cx).busy();
        let mut filters = FilterBar::new().action(
            Button::new("helm-refresh")
                .outline()
                .small()
                .icon(IconName::RefreshCw)
                .spinning(reloading)
                .tooltip(if reloading {
                    t!("helm-reloading-releases")
                } else {
                    t!("helm-reload-releases")
                })
                .disabled(self.cache.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
        );
        if let Some(picker) = &self.picker {
            filters = filters.filter(picker.clone());
        }
        // The search filters the list; a release's details have none.
        if let (Some(search), None) = (&self.search, &self.detail) {
            filters = filters.search(search);
        }
        let root = root.child(filters);
        if let Some(unavailable) = self.render_unavailable(cx) {
            return root.child(unavailable);
        }
        let body = match &self.detail {
            Some(detail) => self.render_detail(detail, cx),
            None => self.render_list(cx),
        };
        root.child(div().flex_1().min_h_0().flex().child(body))
    }
}
