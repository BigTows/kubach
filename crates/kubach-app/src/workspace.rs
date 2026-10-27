use std::collections::HashMap;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::Button;
use gpui_kit::component::{
    ActiveTheme as _, ResizableState, Sizable as _, WindowExt as _, resizable_panel, v_resizable,
};
use gpui_kit::*;
use kubach_core::status_bar::SyncSummary;
use kubach_i18n::t;
use kubach_ui::tokens::{SIDEBAR_WIDTH, TITLEBAR_HEIGHT};
use kubach_ui::{
    BackButton, Logo, MultiSelection, NavTree, NavTreeEvent, NavTreeState, PageHeader,
    WindowTitlebar,
};

use crate::agent;
use crate::cluster::{CatalogState, Cluster};
use crate::dock::{DOCK_HEIGHT, Dock, Forwards, ForwardsScreen, NewTerminal, RevealForward};
use crate::editor::EditorTabs;
use crate::screens::{
    self, AccessScreen, EventsScreen, GitOpsScreen, GoBack, HelmPage, HelmScreen, KustomizeScreen,
    Namespaces, OverviewScreen, PrometheusRegistry, ResourceBrowser, Reveal, ShowHelmPage,
    TopologyScreen, find_kind, set_active,
};
use crate::settings::{AppSettings, SettingsHost};
use crate::shell_env;
use crate::vault;

mod history;
pub(crate) mod hotbar;
mod nav;
mod status;

use history::{History, Location, Transition};
use nav::{Section, default_nav_state, kind_nav_id, parse_kind_nav_id};
use status::SYNC_POLL;

actions!(
    hotbar,
    [
        SelectCluster1,
        SelectCluster2,
        SelectCluster3,
        SelectCluster4,
        SelectCluster5,
        SelectCluster6,
        SelectCluster7,
        SelectCluster8,
        SelectCluster9
    ]
);

actions!(
    filter_bar,
    [
        /// Focuses the search of the list screen (`cmd-f` / `ctrl-f`).
        FocusSearch,
        /// Opens the namespace list of the list screen (`cmd-shift-n`).
        PickNamespaces
    ]
);

/// Applies `$m!(index, Action)` to each hotbar shortcut.
macro_rules! for_each_cluster_shortcut {
    ($m:ident) => {
        $m!(0, SelectCluster1);
        $m!(1, SelectCluster2);
        $m!(2, SelectCluster3);
        $m!(3, SelectCluster4);
        $m!(4, SelectCluster5);
        $m!(5, SelectCluster6);
        $m!(6, SelectCluster7);
        $m!(7, SelectCluster8);
        $m!(8, SelectCluster9);
    };
}

/// `cmd-1…9` (and `ctrl-1…9`) jump to the first nine hotbar tiles.
pub fn bind_keys(cx: &mut App) {
    macro_rules! bind {
        ($ix:literal, $action:ident) => {
            let key = $ix + 1;
            cx.bind_keys([
                KeyBinding::new(&format!("cmd-{key}"), $action, None),
                KeyBinding::new(&format!("ctrl-{key}"), $action, None),
            ]);
        };
    }
    for_each_cluster_shortcut!(bind);
    cx.bind_keys([
        KeyBinding::new("secondary-f", FocusSearch, None),
        KeyBinding::new("secondary-shift-n", PickNamespaces, None),
        KeyBinding::new("ctrl-`", NewTerminal, None),
        // Back to where a jump came from (#395): `⌘[` on macOS, `Ctrl+[`
        // and `Alt+←` elsewhere. A terminal keeps them: `Ctrl+[` is Esc
        // and `Alt+←` moves by word in a shell.
        KeyBinding::new("secondary-[", GoBack, None),
        KeyBinding::new(
            "ctrl-[",
            NoAction,
            Some(kubach_ui::terminal::TERMINAL_CONTEXT),
        ),
        KeyBinding::new(
            "alt-left",
            NoAction,
            Some(kubach_ui::terminal::TERMINAL_CONTEXT),
        ),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("alt-left", GoBack, None)]);
}

/// Root view of the main window: titlebar, sidebar and content area.
pub struct Workspace {
    /// Overview / Port forwards, or the section of the open kind.
    section: Section,
    cluster: Entity<Cluster>,
    /// Namespace selection of the list screens, one per context.
    namespaces: Entity<Namespaces>,
    browser: Entity<ResourceBrowser>,
    events: Entity<EventsScreen>,
    nav: Entity<NavTree>,
    /// Context whose sidebar state `nav` shows.
    context: Option<String>,
    /// Sidebar state (expanded sections, selected item) of other contexts.
    /// Their namespace selection is kept by [`Namespaces`].
    nav_memory: HashMap<String, NavTreeState>,
    /// Keeps hotbar shortcuts working while nothing inside has focus.
    focus: FocusHandle,
    /// Settings screen, shown in place of the content while open.
    settings: SettingsHost<Self>,
    /// Places jumps came from, for "← Back" (#395).
    history: History,
    /// Log and terminal tabs under the content.
    dock: Entity<Dock>,
    dock_split: Entity<ResizableState>,
    forwards_screen: Entity<ForwardsScreen>,
    /// An object of a forward in another context, revealed once the
    /// switch to that context has its catalog (#476).
    pending_reveal: Option<RevealForward>,
    /// Active port forwards, counted in the status bar.
    forwards: Entity<Forwards>,
    /// Watches of the active connection still loading or reconnecting.
    sync: SyncSummary,
    /// Operations (helm changes, drains, bulk actions) running, counted in
    /// the status bar (#382).
    operations_running: usize,
    /// Notice of an operation shown in the status bar (#459).
    operation_notice: Option<kubach_ui::OperationNotice>,
    /// Permissions and security checks (Access control group).
    access: Entity<AccessScreen>,
    helm: Entity<HelmScreen>,
    kustomize: Entity<KustomizeScreen>,
    gitops: Entity<GitOpsScreen>,
    topology: Entity<TopologyScreen>,
    /// Cluster statistics and facts (#414).
    overview: Entity<OverviewScreen>,
    /// YAML editor tabs over the content area.
    tabs: EditorTabs<Self>,
    /// The "shell environment not loaded" banner was closed.
    shell_env_dismissed: bool,
    /// Title last given to the OS window, set again only when the place
    /// changes (#465).
    window_title: String,
    /// FPS, CPU and memory in a corner, debug builds only (#291).
    #[cfg(debug_assertions)]
    debug: Entity<crate::debug_overlay::DebugOverlay>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cluster = cx.new(Cluster::new);
        PrometheusRegistry::install(&cluster, cx);
        let namespaces = cx.new(|cx| Namespaces::new(cluster.clone(), cx));
        let browser = cx.new(|cx| ResourceBrowser::new(cluster.clone(), namespaces.clone(), cx));
        let events = cx.new(|cx| EventsScreen::new(cluster.clone(), namespaces.clone(), cx));
        let nav = cx.new(|cx| {
            let mut nav = NavTree::new(cx);
            nav.set_state(default_nav_state(), cx);
            nav
        });
        let forwards = cx.new(Forwards::new);
        // Forwards of the last run restart as their contexts connect (#474).
        forwards.update(cx, |forwards, cx| forwards.restore(&cluster, cx));
        let dock = cx.new(|_| Dock::new(cluster.clone(), forwards.clone()));
        Dock::install(&dock, cx);
        let forwards_screen = cx.new(|cx| ForwardsScreen::new(forwards.clone(), cx));
        let access = cx.new(|cx| AccessScreen::new(cluster.clone(), namespaces.clone(), cx));
        let helm = cx.new(|cx| HelmScreen::new(cluster.clone(), namespaces.clone(), cx));
        let kustomize = cx.new(|cx| KustomizeScreen::new(cluster.clone(), cx));
        let gitops = cx.new(|cx| GitOpsScreen::new(cluster.clone(), namespaces.clone(), cx));
        let topology = cx.new(|cx| TopologyScreen::new(cluster.clone(), namespaces.clone(), cx));
        let overview = cx.new(|cx| OverviewScreen::new(cluster.clone(), namespaces.clone(), cx));
        // The first screen shown.
        set_active(&overview, true, cx);
        let mut subscriptions = vec![
            cx.observe(&dock, |_, _, cx| cx.notify()),
            cx.observe(&forwards, |_, _, cx| cx.notify()),
            cx.observe(&cluster, |this, _, cx| this.on_cluster_change(cx)),
            // After on_cluster_change: the context switch is taken in.
            cx.observe_in(&cluster, window, |this, _, window, cx| {
                this.reveal_pending(window, cx)
            }),
            // The status bar shows the selected namespaces.
            cx.observe(&namespaces, |_, _, cx| cx.notify()),
            cx.subscribe(&nav, |this, _, event: &NavTreeEvent, cx| match event {
                NavTreeEvent::Select(id) => {
                    // Not recorded: "back" keeps returning to the last jump.
                    this.navigated(Transition::Sidebar, cx);
                    this.open(id, cx)
                }
            }),
            // Charts open a release they installed through the sidebar: a
            // jump, recorded for "back".
            cx.subscribe(&helm, |this, _, ShowHelmPage(page), cx| {
                let section = match page {
                    HelmPage::Releases => Section::HelmReleases,
                    HelmPage::Charts => Section::HelmCharts,
                };
                this.navigated(Transition::ReleaseInstalled, cx);
                this.show_section(section, cx);
            }),
            cx.subscribe_in(&access, window, |this, _, reveal: &Reveal, window, cx| {
                this.reveal(reveal, window, cx)
            }),
            // Objects and managed resources of the GitOps screen.
            cx.subscribe_in(&gitops, window, |this, _, reveal: &Reveal, window, cx| {
                this.reveal(reveal, window, cx)
            }),
            cx.subscribe_in(&topology, window, |this, _, reveal: &Reveal, window, cx| {
                this.reveal(reveal, window, cx)
            }),
            // The source and pod of a port forward (#476).
            cx.subscribe_in(
                &forwards_screen,
                window,
                |this, _, reveal: &RevealForward, window, cx| {
                    this.reveal_forward(reveal.clone(), window, cx)
                },
            ),
            // "Managed by" links in the resource details.
            cx.subscribe_in(&browser, window, |this, _, reveal: &Reveal, window, cx| {
                this.reveal(reveal, window, cx)
            }),
        ];
        let tabs = EditorTabs::new(cluster.clone(), |this| &mut this.tabs, cx);
        #[cfg(debug_assertions)]
        let debug = crate::debug_overlay::DebugOverlay::new(cluster.clone(), forwards.clone(), cx);
        // Hotbar arrangement and security badges come from settings.
        if let Some(settings) = AppSettings::try_global(cx) {
            subscriptions.push(cx.observe(&settings, |_, _, cx| cx.notify()));
        }
        // Operations notify on every phase; the workspace redraws only when
        // the count or the notice in the status bar changes.
        let operations = kubach_ui::Operations::global(cx);
        subscriptions.push(cx.observe(&operations, |this, operations, cx| {
            let operations = operations.read(cx);
            let running = operations.running(cx);
            let notice = operations.notice().cloned();
            if running != this.operations_running || notice != this.operation_notice {
                this.operations_running = running;
                this.operation_notice = notice;
                cx.notify();
            }
        }));
        // Sidebar labels are kept in its nodes: rebuild them in the new
        // language.
        if let Some(language) = crate::i18n::global(cx) {
            subscriptions.push(cx.observe(&language, |this, _, cx| this.refresh_nav(cx)));
        }
        // MCP sessions and confirmations in the status bar.
        if let Some(agent) = agent::global(cx) {
            subscriptions.push(cx.observe(&agent, |_, _, cx| cx.notify()));
        }
        // Watches report to their own subscribers only; poll their state.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SYNC_POLL).await;
                if this.update(cx, |this, cx| this.poll_sync(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let settings = SettingsHost::new(|this| &mut this.settings, cluster.clone(), cx);
        let mut this = Self {
            section: Section::Overview,
            cluster,
            namespaces,
            browser,
            events,
            nav,
            context: None,
            nav_memory: HashMap::new(),
            focus: cx.focus_handle(),
            settings,
            history: History::default(),
            dock,
            dock_split: cx.new(|_| ResizableState::default()),
            forwards_screen,
            pending_reveal: None,
            forwards,
            window_title: String::new(),
            sync: SyncSummary::default(),
            operations_running: 0,
            operation_notice: None,
            access,
            helm,
            kustomize,
            gitops,
            topology,
            overview,
            tabs,
            shell_env_dismissed: false,
            #[cfg(debug_assertions)]
            debug,
            _subscriptions: subscriptions,
        };
        this.focus.focus(window, cx);
        this.on_cluster_change(cx);
        this
    }

    /// Selects `section` in the sidebar and shows it.
    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        let id: SharedString = section.nav_id().into();
        self.nav.update(cx, |nav, cx| nav.select(id.clone(), cx));
        self.open(&id, cx);
    }

    /// Opens the sidebar item `id`: Overview, Port forwards or a resource kind.
    fn open(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        let catalog = match self.cluster.read(cx).catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        if let Some((group, kind)) = parse_kind_nav_id(id) {
            self.section = catalog
                .as_ref()
                .and_then(|catalog| find_kind(catalog, group, kind))
                .map(|(section, _)| Section::from_browse(section))
                .unwrap_or(Section::CustomResources);
            self.browser
                .update(cx, |browser, cx| browser.open_kind(group, kind, cx));
        } else {
            self.section = Section::ALL
                .into_iter()
                .find(|section| section.nav_id() == id.as_ref() && section.browse().is_none())
                .unwrap_or(Section::Overview);
            // The Helm group itself has no screen: its first page.
            if self.section == Section::Helm {
                self.section = Section::HelmReleases;
            }
            set_active(&self.browser, false, cx);
        }
        // Only the shown screen keeps watches and polls running.
        set_active(&self.events, self.section == Section::Events, cx);
        let page = self.section.access_page();
        if let Some(page) = page {
            self.access.update(cx, |access, cx| access.show(page, cx));
        }
        set_active(&self.access, page.is_some(), cx);
        let page = self.section.helm_page();
        if let Some(page) = page {
            self.helm.update(cx, |helm, cx| helm.show(page, cx));
        }
        set_active(&self.helm, page.is_some(), cx);
        set_active(&self.kustomize, self.section == Section::Kustomize, cx);
        set_active(&self.gitops, self.section == Section::GitOps, cx);
        set_active(&self.topology, self.section == Section::Topology, cx);
        set_active(&self.overview, self.section == Section::Overview, cx);
        cx.notify();
    }

    /// Opens the kind of `reveal` in the sidebar and selects the object. A
    /// namespace that isn't shown is added to the selection, with a toast
    /// to undo that.
    fn reveal(&mut self, reveal: &Reveal, window: &mut Window, cx: &mut Context<Self>) {
        let CatalogState::Ready(catalog) = self.cluster.read(cx).catalog() else {
            return;
        };
        let Some((_, entry)) = find_kind(catalog, &reveal.group, &reveal.kind) else {
            return;
        };
        let entry = entry.clone();
        // A jump: "back" returns to the place it leaves.
        self.navigated(Transition::Reveal, cx);
        let namespace = reveal
            .namespace
            .as_deref()
            .filter(|_| entry.kind.namespaced);
        if let Some(namespace) = namespace {
            let before = self
                .namespaces
                .update(cx, |namespaces, cx| namespaces.include(namespace, cx));
            let context = self.namespaces.read(cx).context().map(ToOwned::to_owned);
            if let (Some(before), Some(context)) = (before, context) {
                self.notify_namespace_added(namespace, context, before, window, cx);
            }
        }
        let id = kind_nav_id(&entry);
        self.nav.update(cx, |nav, cx| nav.select(id.clone(), cx));
        self.open(&id, cx);
        self.browser.update(cx, |browser, cx| {
            browser.reveal(reveal.namespace.as_deref(), &reveal.name, cx)
        });
    }

    /// Reveals an object a port forward goes to, switching to the forward's
    /// context first when it is not the active one (#476).
    fn reveal_forward(
        &mut self,
        reveal: RevealForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.cluster.read(cx).active_name().map(ToOwned::to_owned);
        match reveal.context.clone() {
            Some(context) if active.as_deref() != Some(context.as_str()) => {
                self.pending_reveal = Some(reveal);
                self.cluster
                    .update(cx, |cluster, cx| cluster.connect(context, cx));
            }
            _ => {
                self.pending_reveal = None;
                self.reveal(&reveal.reveal, window, cx);
            }
        }
    }

    /// The pending reveal of [`reveal_forward`](Self::reveal_forward), once
    /// its context is active with a catalog; dropped when another context
    /// is picked meanwhile.
    fn reveal_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending_reveal else {
            return;
        };
        let cluster = self.cluster.read(cx);
        let active = cluster.active_name();
        if active != pending.context.as_deref() {
            // Another context was picked in the meantime.
            self.pending_reveal = None;
            return;
        }
        match cluster.catalog() {
            CatalogState::Ready(_) if self.context == pending.context => {}
            CatalogState::Error(_) => {
                self.pending_reveal = None;
                return;
            }
            _ => return,
        }
        if let Some(pending) = self.pending_reveal.take() {
            self.reveal(&pending.reveal, window, cx);
        }
    }

    /// The place shown now, as "back" would record it.
    fn location(&self, cx: &App) -> Location {
        let nav_id = self
            .nav
            .read(cx)
            .state()
            .selected
            .clone()
            .unwrap_or_else(|| Section::Overview.nav_id().into());
        let browser = self.browser.read(cx);
        let (label, place) = match self.section.browse() {
            Some(_) => (browser.place_label(), browser.place(cx)),
            None => (self.section.history_label(), None),
        };
        Location {
            nav_id,
            label,
            browser: place,
            namespaces: self
                .namespaces
                .read(cx)
                .scope()
                .map(|scope| scope.selection.clone()),
        }
    }

    /// Called before leaving the place shown by `transition`: a jump (a
    /// [`Reveal`], the charts opening the releases) records it for "back".
    fn navigated(&mut self, transition: Transition, cx: &mut Context<Self>) {
        if !transition.is_jump() {
            return;
        }
        let from = self.location(cx);
        self.history.record(from);
        cx.notify();
    }

    /// Where "back" goes now, if anywhere.
    fn back_target(&self, cx: &App) -> Option<Location> {
        self.history.target(&self.location(cx)).cloned()
    }

    /// "← Back": shows the place the last jump came from as it was (the
    /// browser's selection and search, the namespaces shown).
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.location(cx);
        let Some(target) = self.history.back(&current) else {
            return;
        };
        self.tabs.show_home(window, cx);
        if let Some(selection) = history::namespaces_to_restore(&current, &target) {
            let context = self.namespaces.read(cx).context().map(ToOwned::to_owned);
            if let Some(context) = context {
                self.namespaces.update(cx, |namespaces, cx| {
                    namespaces.restore(&context, selection, cx)
                });
            }
        }
        let id = target.nav_id.clone();
        self.nav.update(cx, |nav, cx| nav.select(id.clone(), cx));
        self.open(&id, cx);
        if let Some(place) = &target.browser {
            self.browser
                .update(cx, |browser, cx| browser.restore(place, cx));
        }
        cx.notify();
    }

    /// "Namespace X added to the selection" with an Undo button that puts
    /// back the selection `before`.
    fn notify_namespace_added(
        &self,
        namespace: &str,
        context: String,
        before: MultiSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::button::ButtonVariants as _;
        use gpui_kit::component::notification::Notification;
        let namespaces = self.namespaces.downgrade();
        let message = t!("workspace-namespace-added", namespace = namespace);
        let notification = Notification::info(message)
            .action(move |_, _, _| {
                let (namespaces, context, before) =
                    (namespaces.clone(), context.clone(), before.clone());
                Button::new("namespace-undo")
                    .small()
                    .ghost()
                    .label(t!("workspace-undo"))
                    .on_click(move |_, _, cx| {
                        let (context, before) = (context.clone(), before.clone());
                        namespaces
                            .update(cx, |namespaces, cx| {
                                namespaces.restore(&context, before, cx)
                            })
                            .ok();
                    })
            })
            .autohide(true);
        window.push_notification(notification, cx);
    }

    /// The screen shown under the editor tabs, if it has a filter bar.
    fn list_screen(&self) -> Option<ListScreen> {
        if !self.tabs.home_active() {
            return None;
        }
        match self.section {
            Section::Events => Some(ListScreen::Events),
            Section::GitOps => Some(ListScreen::GitOps),
            Section::Topology => Some(ListScreen::Topology),
            section if section.helm_page().is_some() => Some(ListScreen::Helm),
            section if section.access_page().is_some() => Some(ListScreen::Access),
            section if section.browse().is_some() => Some(ListScreen::Browser),
            _ => None,
        }
    }

    /// `cmd-f`: focuses the search of the list screen.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.list_screen() {
            Some(ListScreen::Browser) => self
                .browser
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            Some(ListScreen::Events) => self
                .events
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            Some(ListScreen::GitOps) => self
                .gitops
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            Some(ListScreen::Access) => self
                .access
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            Some(ListScreen::Topology) => self
                .topology
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            Some(ListScreen::Helm) => self
                .helm
                .update(cx, |screen, cx| screen.focus_search(window, cx)),
            None => {}
        }
    }

    /// `cmd-shift-n` and the status bar: opens the namespace list of the
    /// list screen.
    fn pick_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.list_screen() {
            Some(ListScreen::Browser) => self
                .browser
                .update(cx, |screen, cx| screen.open_namespaces(window, cx)),
            Some(ListScreen::Events) => self
                .events
                .update(cx, |screen, cx| screen.open_namespaces(window, cx)),
            Some(ListScreen::GitOps) => self
                .gitops
                .update(cx, |screen, cx| screen.open_namespaces(window, cx)),
            Some(ListScreen::Access) => self
                .access
                .update(cx, |screen, cx| screen.open_namespaces(window, cx)),
            Some(ListScreen::Topology) => self
                .topology
                .update(cx, |screen, cx| screen.open_namespaces(window, cx)),
            Some(ListScreen::Helm) => {
                // Namespaces filter the releases, not the charts.
                if self.section != Section::HelmReleases {
                    self.show_section(Section::HelmReleases, cx);
                }
                self.helm
                    .update(cx, |screen, cx| screen.open_namespaces(window, cx))
            }
            None => {}
        }
    }

    /// `ctrl-\``, the menu and the status bar: a local terminal with the
    /// active context and the namespace shown (when exactly one is).
    fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let namespace = self
            .namespaces
            .read(cx)
            .scope()
            .map(|scope| match &scope.selection {
                MultiSelection::Items(items) if items.len() == 1 => items[0].to_string(),
                _ => scope.default.to_string(),
            });
        self.dock
            .update(cx, |dock, cx| dock.open_local(namespace, window, cx));
    }

    /// Where the window is, for the OS window title (#465):
    /// "Network › Port forwards", "Workloads › Deployment web".
    fn crumbs(&self, cx: &App) -> Vec<String> {
        match self.section.browse() {
            Some(_) => vec![self.section.label(), self.browser.read(cx).place_label()],
            None => self
                .section
                .parent()
                .map(Section::label)
                .into_iter()
                .chain([self.section.label()])
                .collect(),
        }
    }

    /// Gives the OS window (Window menu, task switchers) the current place.
    fn update_window_title(&mut self, crumbs: &[String], window: &mut Window) {
        let mut title = crumbs.join(" › ");
        title.push_str(if title.is_empty() {
            "Kubach"
        } else {
            " — Kubach"
        });
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    fn render_titlebar(&self, back: Option<AnyElement>, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        WindowTitlebar::new("titlebar")
            .h(TITLEBAR_HEIGHT)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(theme.title_bar_border)
            .children(back.map(|back| {
                div()
                    .absolute()
                    .left_2()
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    // A press on the button must not start a titlebar drag.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(back)
            }))
            .child(Logo::new())
    }

    /// "← Topology" in the titlebar while a jump can be undone (#395).
    fn render_back(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.back_target(cx)?;
        Some(
            BackButton::new("history-back", target.label.clone())
                .tooltip(history::back_tooltip(&target.label))
                .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx)))
                .into_any_element(),
        )
    }

    /// Warns that the login shell environment failed to load, until closed.
    fn render_shell_env_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.shell_env_dismissed {
            return None;
        }
        let error = shell_env::error(cx)?;
        Some(
            Alert::warning("shell-env", shell_env::banner_message(&error))
                .banner()
                .small()
                .on_close(cx.listener(|this, _, _, cx| {
                    this.shell_env_dismissed = true;
                    cx.notify();
                })),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .w(SIDEBAR_WIDTH)
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_2()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(div().flex_1().min_h_0().child(self.nav.clone()))
    }

    /// `content` with the bottom dock under it once a log or terminal tab
    /// is open.
    fn with_dock(&self, content: AnyElement, cx: &App) -> AnyElement {
        let dock = self.dock.read(cx);
        if !dock.has_tabs() {
            return content;
        }
        if !dock.is_expanded() {
            return div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().flex().child(content))
                .child(self.dock.clone())
                .into_any_element();
        }
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .child(
                v_resizable("workspace-dock")
                    .with_state(&self.dock_split)
                    .child(resizable_panel().child(content))
                    .child(
                        resizable_panel()
                            .size(DOCK_HEIGHT)
                            .size_range(px(120.)..Pixels::MAX)
                            .child(self.dock.clone()),
                    ),
            )
            .into_any_element()
    }

    /// The screen of the section. Screens are embedded cached (#355,
    /// #363): redraws of the status bar, sidebar, dock or debug overlay
    /// reuse their last frame. Each screen observes what its render reads.
    /// The resource browser is not cached itself: its table and details are
    /// (see [`ResourceBrowser`]).
    fn render_content(&self, cx: &App) -> AnyElement {
        if self.section == Section::PortForwards {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.forwards_screen, cx))
                .into_any_element();
        }
        if self.section == Section::Events {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.events, cx))
                .into_any_element();
        }
        if self.section.access_page().is_some() {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.access, cx))
                .into_any_element();
        }
        if self.section.helm_page().is_some() {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.helm, cx))
                .into_any_element();
        }
        if self.section == Section::Kustomize {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.kustomize, cx))
                .into_any_element();
        }
        if self.section == Section::Topology {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.topology, cx))
                .into_any_element();
        }
        if self.section == Section::GitOps {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.gitops, cx))
                .into_any_element();
        }
        if self.section.browse().is_some() {
            // Not cached itself: the browser embeds its table and details
            // cached (#363). A cached view that redraws redraws everything
            // inside it, and a tick of the table or the details makes the
            // views around it redraw; so the cache sits on those two, and a
            // redraw of the status bar, sidebar or debug overlay only
            // re-renders the browser's toolbar around them.
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(self.browser.clone())
                .into_any_element();
        }
        if self.section == Section::Overview && self.connected(cx) {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(screens::cached(&self.overview, cx))
                .into_any_element();
        }
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_8()
            .p_8()
            .child(PageHeader::new(self.section.label()))
            .child(self.render_cluster_status(cx))
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(debug_assertions)]
        crate::debug_overlay::frame_started(&self.debug, cx);
        let theme = cx.theme();
        let mut root = div()
            .size_full()
            .flex()
            .bg(kubach_ui::theme::window_background(cx))
            .text_color(theme.foreground)
            .track_focus(&self.focus)
            // Any input postpones the idle lock of the vault.
            .capture_any_mouse_down(|_, _, cx| vault::touch(cx))
            .capture_key_down(|_, _, cx| vault::touch(cx));
        macro_rules! on_shortcut {
            ($ix:literal, $action:ident) => {
                root = root
                    .on_action(cx.listener(|this, _: &$action, _, cx| this.select_nth($ix, cx)));
            };
        }
        for_each_cluster_shortcut!(on_shortcut);
        root = root
            .on_action(
                cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PickNamespaces, window, cx| {
                    this.pick_namespaces(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| this.open_terminal(window, cx)),
            );
        #[cfg(debug_assertions)]
        {
            root = crate::debug_overlay::attach(root, &self.debug, cx);
        }

        // Settings are app-wide, so they take the whole window: no hotbar,
        // sidebar, tabs or dock that would suggest they belong to a cluster.
        if let Some(settings) = self.settings.screen(window, cx) {
            return root.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_titlebar(None, cx))
                    .child(div().flex_1().min_h_0().flex().child(settings)),
            );
        }

        // "Back" (#395): `⌘[` / `Alt+←` once the focused details panel has
        // nothing to go back to, and the mouse's back button anywhere the
        // details panel under the pointer doesn't take it.
        root = root
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.go_back(window, cx)),
            );
        let back = self.render_back(cx);
        let crumbs = self.crumbs(cx);
        self.update_window_title(&crumbs, window);
        let home = self.render_content(cx);
        let title = match self.section.browse() {
            Some(_) => self.browser.read(cx).title(),
            None => self.section.label().into(),
        };
        // The home tab carries the hotbar color of the current cluster.
        let accent = self.cluster.read(cx).active().map(|context| {
            crate::hotbar::tile_color(&hotbar::hotbar_prefs(cx), &context.name).color(cx)
        });
        let content = self.tabs.render(title, accent, home, window, cx);
        let content = self.with_dock(content, cx);
        // The hotbar spans the full height; its top inset sits under the
        // traffic lights, level with the titlebar.
        // Kubeconfig files dropped on the cluster panel are added.
        let hotbar = self.render_hotbar(cx).into_any_element();
        let hotbar = vault::drop_zone("hotbar-drop", hotbar, cx).flex_shrink_0();
        root.child(hotbar).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(self.render_titlebar(back, cx))
                .children(self.render_shell_env_banner(cx))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .min_h_0()
                        .child(self.render_sidebar(cx))
                        .child(content),
                )
                .child(self.render_status_bar(cx)),
        )
    }
}

/// Screens with a filter bar, the targets of `cmd-f` and `cmd-shift-n`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListScreen {
    Browser,
    Events,
    GitOps,
    Access,
    Helm,
    Topology,
}
