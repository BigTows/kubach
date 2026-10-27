//! Bottom dock with log and terminal tabs, the port-forward dialog and the
//! port-forwards screen. Closing a tab drops its entity, which stops its stream or
//! session.

mod containers;
mod debug_confirm;
mod entry;
mod forward_dialog;
mod forwards;
mod forwards_screen;
mod local_terminal;
mod logs;
mod node_shell;
mod target;
mod terminal;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;
use kubach_ui::{StatusDot, Tone};

pub use entry::stream_actions;
pub use forwards::{Forwards, find_forward};
pub use forwards_screen::{ForwardsScreen, RevealForward};
pub use local_terminal::init;
pub use node_shell::sweep as sweep_node_shells;
pub use target::{StreamRequest, http_scheme};

use self::local_terminal::LocalTerminalTab;
use self::logs::{LogsState, LogsTab};
use self::node_shell::NodeShellTab;
use self::terminal::{SessionState, TerminalKind, TerminalTab};
use crate::cluster::Cluster;

actions!(
    dock,
    [
        /// Opens a local terminal with the current context (`ctrl-\``).
        NewTerminal
    ]
);

/// Height of the dock's tab strip.
pub const DOCK_HEADER_HEIGHT: Pixels = px(34.);
/// Initial height of an expanded dock.
pub const DOCK_HEIGHT: Pixels = px(320.);

/// Opacity of a tab's context dot while its session connects.
const CONNECTING_DOT_OPACITY: f32 = 0.6;
/// Opacity of a tab's context dot once its session has ended.
const ENDED_DOT_OPACITY: f32 = 0.35;

/// Lets views that cannot reach the workspace (the details panel) open
/// requests in the dock of their window.
struct DockHandle(WeakEntity<Dock>);

impl Global for DockHandle {}

/// Opens `request` in the dock registered with [`Dock::install`].
pub fn open_request(request: StreamRequest, window: &mut Window, cx: &mut App) {
    let dock = cx
        .try_global::<DockHandle>()
        .and_then(|handle| handle.0.upgrade());
    if let Some(dock) = dock {
        dock.update(cx, |dock, cx| dock.open(request, window, cx));
    }
}

/// Opens a shell on `node` (#318) in the dock registered with
/// [`Dock::install`].
pub fn open_node_shell(node: String, window: &mut Window, cx: &mut App) {
    let dock = cx
        .try_global::<DockHandle>()
        .and_then(|handle| handle.0.upgrade());
    if let Some(dock) = dock {
        dock.update(cx, |dock, cx| {
            let cluster = dock.cluster.clone();
            let tab = cx.new(|cx| NodeShellTab::new(cluster, node, window, cx));
            dock.push(TabView::Node(tab), window, cx);
        });
    }
}

/// Active port-forwards of the dock registered with [`Dock::install`].
pub fn forwards(cx: &App) -> Option<Entity<Forwards>> {
    let dock = cx.try_global::<DockHandle>()?.0.upgrade()?;
    Some(dock.read(cx).forwards.clone())
}

/// Opens the port-forward dialog for `port` of `source` (#464): the local
/// port is picked there, through the dock registered with [`Dock::install`].
pub fn start_forward(
    source: kubach_core::streams::ForwardSource,
    port: u16,
    window: &mut Window,
    cx: &mut App,
) {
    let dock = cx
        .try_global::<DockHandle>()
        .and_then(|handle| handle.0.upgrade());
    if let Some(dock) = dock {
        let (cluster, forwards) = {
            let dock = dock.read(cx);
            (dock.cluster.clone(), dock.forwards.clone())
        };
        forward_dialog::open(cluster, forwards, source, Some(port), window, cx);
    }
}

#[derive(Clone)]
enum TabView {
    Logs(Entity<LogsTab>),
    Terminal(Entity<TerminalTab>),
    Local(Entity<LocalTerminalTab>),
    Node(Entity<NodeShellTab>),
}

struct DockTab {
    id: u64,
    view: TabView,
    _observe: Subscription,
}

/// A dock tab as shown in the dock's tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: u64,
    pub terminal: bool,
    pub title: String,
    /// Context the tab belongs to; `None` for a local terminal without one.
    pub context: Option<String>,
    pub status: String,
    /// Connecting or running.
    pub live: bool,
    /// Streaming or running (not just connecting).
    pub running: bool,
}

pub struct Dock {
    cluster: Entity<Cluster>,
    forwards: Entity<Forwards>,
    tabs: Vec<DockTab>,
    active: usize,
    expanded: bool,
    next_id: u64,
}

impl Dock {
    pub fn new(cluster: Entity<Cluster>, forwards: Entity<Forwards>) -> Self {
        Self {
            cluster,
            forwards,
            tabs: Vec::new(),
            active: 0,
            expanded: true,
            next_id: 0,
        }
    }

    /// Makes this dock the target of [`open_request`].
    pub fn install(dock: &Entity<Dock>, cx: &mut App) {
        cx.set_global(DockHandle(dock.downgrade()));
    }

    pub fn has_tabs(&self) -> bool {
        !self.tabs.is_empty()
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Opens `request`; a Debug tab first asks to add the ephemeral
    /// container (#346), unless the settings turned that off or there is
    /// no connection (the tab then explains).
    pub fn open(&mut self, request: StreamRequest, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            StreamRequest::Debug { pod, container }
                if debug_confirm::enabled(cx) && self.cluster.read(cx).connection().is_some() =>
            {
                let dock = cx.entity().downgrade();
                let cluster = self.cluster.clone();
                debug_confirm::open(dock, cluster, pod, container, window, cx);
            }
            request => self.open_tab(request, window, cx),
        }
    }

    /// Opens `request` without asking.
    fn open_tab(&mut self, request: StreamRequest, window: &mut Window, cx: &mut Context<Self>) {
        let cluster = self.cluster.clone();
        let view = match request {
            StreamRequest::Logs { target, container } => {
                TabView::Logs(cx.new(|cx| LogsTab::new(cluster, target, container, window, cx)))
            }
            StreamRequest::Shell { pod, container } => TabView::Terminal(cx.new(|cx| {
                TerminalTab::new(cluster, pod, container, TerminalKind::Shell, window, cx)
            })),
            StreamRequest::Debug { pod, container } => TabView::Terminal(cx.new(|cx| {
                TerminalTab::new(cluster, pod, container, TerminalKind::Debug, window, cx)
            })),
            StreamRequest::PortForward(source) => {
                forward_dialog::open(cluster, self.forwards.clone(), source, None, window, cx);
                return;
            }
        };
        self.push(view, window, cx);
    }

    /// Opens a local shell whose `KUBECONFIG` has the active context with
    /// `namespace` (#319).
    pub fn open_local(
        &mut self,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cluster = self.cluster.clone();
        let context = cluster
            .read(cx)
            .active()
            .map(|context| context.name.clone());
        let namespace = context.as_ref().and(namespace);
        let tab = cx.new(|cx| LocalTerminalTab::new(cluster, context, namespace, window, cx));
        self.push(TabView::Local(tab), window, cx);
    }

    fn push(&mut self, view: TabView, window: &mut Window, cx: &mut Context<Self>) {
        let observe = match &view {
            TabView::Logs(tab) => cx.observe(tab, |_, _, cx| cx.notify()),
            TabView::Terminal(tab) => cx.observe(tab, |_, _, cx| cx.notify()),
            TabView::Local(tab) => cx.observe(tab, |_, _, cx| cx.notify()),
            TabView::Node(tab) => cx.observe(tab, |_, _, cx| cx.notify()),
        };
        self.next_id += 1;
        self.tabs.push(DockTab {
            id: self.next_id,
            view,
            _observe: observe,
        });
        self.active = self.tabs.len() - 1;
        self.expanded = true;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Closes a tab, stopping its stream or session.
    pub fn close(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = self.tabs.remove(index);
        match &tab.view {
            TabView::Terminal(terminal) => {
                terminal.update(cx, |terminal, cx| terminal.shutdown(cx))
            }
            TabView::Local(terminal) => terminal.update(cx, |terminal, cx| terminal.shutdown(cx)),
            TabView::Node(terminal) => terminal.update(cx, |terminal, cx| terminal.shutdown(cx)),
            TabView::Logs(_) => {}
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        cx.notify();
    }

    /// Shows a tab, expanding the dock.
    pub fn activate(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.active = index;
            self.expanded = true;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(index).map(|tab| tab.id) {
            self.activate(id, window, cx);
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        let focus = match self.tabs.get(self.active).map(|tab| &tab.view) {
            Some(TabView::Terminal(tab)) => tab.read(cx).focus_handle(cx),
            Some(TabView::Local(tab)) => tab.read(cx).focus_handle(cx),
            Some(TabView::Node(tab)) => tab.read(cx).focus_handle(cx),
            _ => return,
        };
        focus.focus(window, cx);
    }

    /// Every tab with its state, in tab order.
    pub fn sessions(&self, cx: &App) -> Vec<SessionSummary> {
        self.tabs
            .iter()
            .map(|tab| match &tab.view {
                TabView::Logs(logs) => {
                    let logs = logs.read(cx);
                    let lines = logs.line_count(cx);
                    let (status, live) = match logs.state() {
                        LogsState::Starting => (t!("dock-connecting"), true),
                        LogsState::Streaming => (t!("dock-logs-streaming", lines = lines), true),
                        LogsState::Finished => (t!("dock-logs-ended", lines = lines), false),
                        LogsState::Failed(error) => (error.clone(), false),
                    };
                    SessionSummary {
                        id: tab.id,
                        terminal: false,
                        title: logs.title(),
                        context: logs.context().map(str::to_owned),
                        status,
                        live,
                        running: *logs.state() == LogsState::Streaming,
                    }
                }
                TabView::Terminal(terminal) => {
                    let terminal = terminal.read(cx);
                    SessionSummary {
                        id: tab.id,
                        terminal: true,
                        title: terminal.title(),
                        context: terminal.context().map(str::to_owned),
                        status: terminal.state().label(),
                        live: terminal.state().is_live(),
                        running: *terminal.state() == SessionState::Running,
                    }
                }
                TabView::Local(terminal) => {
                    let terminal = terminal.read(cx);
                    SessionSummary {
                        id: tab.id,
                        terminal: true,
                        title: terminal.title(),
                        context: terminal.context().map(str::to_owned),
                        status: terminal.state().label(),
                        live: terminal.state().is_live(),
                        running: *terminal.state() == SessionState::Running,
                    }
                }
                TabView::Node(terminal) => {
                    let terminal = terminal.read(cx);
                    SessionSummary {
                        id: tab.id,
                        terminal: true,
                        title: terminal.title(),
                        context: terminal.context().map(str::to_owned),
                        status: terminal.state().label(),
                        live: terminal.state().is_live(),
                        running: *terminal.state() == SessionState::Running,
                    }
                }
            })
            .collect()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let summaries = self.sessions(cx);
        let prefs = crate::hotbar::prefs(cx);
        let tabs = summaries.into_iter().map(|summary| {
            let id = summary.id;
            let icon = if summary.terminal {
                IconName::SquareTerminal
            } else {
                IconName::FileText
            };
            // The dot has the hotbar color of the tab's context (#461),
            // fading once the session is no longer running; without a
            // context it shows the state.
            let dot = match &summary.context {
                Some(context) => {
                    let opacity = match (summary.running, summary.live) {
                        (true, _) => 1.,
                        (false, true) => CONNECTING_DOT_OPACITY,
                        (false, false) => ENDED_DOT_OPACITY,
                    };
                    let color = crate::hotbar::tile_color(&prefs, context).color(cx);
                    StatusDot::color(color.opacity(opacity))
                }
                None => StatusDot::new(match (summary.running, summary.live) {
                    (true, _) => Tone::Success,
                    (false, true) => Tone::Warning,
                    (false, false) => Tone::Neutral,
                }),
            };
            Tab::new()
                .prefix(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .pl_2()
                        .child(dot)
                        .child(Icon::new(icon).size_3p5()),
                )
                .label(summary.title)
                .suffix(
                    Button::new(("close-dock-tab", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(t!("dock-tab-close"))
                        .on_click(cx.listener(move |dock, _, _, cx| {
                            cx.stop_propagation();
                            dock.close(id, cx);
                        })),
                )
        });
        let expanded = self.expanded;
        div()
            .h(DOCK_HEADER_HEIGHT)
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .pr_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    TabBar::new("dock-tabs")
                        .underline()
                        .xsmall()
                        .selected_index(self.active)
                        .on_click(cx.listener(|dock, index: &usize, window, cx| {
                            dock.select(*index, window, cx)
                        }))
                        .children(tabs),
                ),
            )
            .child(
                Button::new("dock-toggle")
                    .ghost()
                    .xsmall()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .tooltip(if expanded {
                        t!("dock-collapse")
                    } else {
                        t!("dock-expand")
                    })
                    .on_click(cx.listener(|dock, _, _, cx| dock.toggle(cx))),
            )
    }
}

impl Render for Dock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = self
            .expanded
            .then(|| self.tabs.get(self.active))
            .flatten()
            .map(|tab| match &tab.view {
                TabView::Logs(view) => view.clone().into_any_element(),
                TabView::Terminal(view) => view.clone().into_any_element(),
                TabView::Local(view) => view.clone().into_any_element(),
                TabView::Node(view) => view.clone().into_any_element(),
            });
        div()
            .map(|dock| {
                if self.expanded {
                    dock.size_full()
                } else {
                    dock.w_full().h(DOCK_HEADER_HEIGHT).flex_shrink_0()
                }
            })
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(self.render_header(cx))
            .children(body.map(|body| div().flex_1().min_h_0().child(body)))
    }
}
