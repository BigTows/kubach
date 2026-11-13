//! Bottom dock with log and terminal tabs, the port-forward dialog and the
//! port-forwards screen. Closing a tab drops its entity, which stops its stream or
//! session.
//!
//! Redraws (#166). A tab notifies for its own content, with every batch of
//! lines or chunk of output, so nothing observes a tab as a whole: the dock
//! listens for [`TabChanged`] and redraws only when its tab strip really
//! changed, and its render reads the strip it keeps, not the tabs. A tab
//! that is not shown (not the active one, or the dock is collapsed) is then
//! read by no render and observed by nobody: it keeps receiving and
//! buffering, and its notifications reach no window. The active tab is
//! embedded cached, so a redraw of the tab strip or of the window around
//! the dock reuses its last frame.

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
use crate::screens::cached;

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

/// What the tab strip shows of a tab (its title, context or state) may have
/// changed. Tabs emit it wherever they change one of those; the dock
/// compares the strip before redrawing, so emitting it without a change is
/// harmless.
pub(super) struct TabChanged;

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
    _changed: Subscription,
}

/// A dock tab as shown in the dock's tab strip. Nothing here changes with
/// the content of a tab (such as its number of lines): the strip is
/// redrawn when one of these differs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionSummary {
    id: u64,
    terminal: bool,
    title: String,
    /// Context the tab belongs to; `None` for a local terminal without one.
    context: Option<String>,
    /// Connecting or running.
    live: bool,
    /// Streaming or running (not just connecting).
    running: bool,
}

/// Puts `next` in place of `current`; whether they differed.
fn replace_if_changed<T: PartialEq>(current: &mut T, next: T) -> bool {
    let changed = *current != next;
    if changed {
        *current = next;
    }
    changed
}

/// What the workspace lays the dock out by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockShape {
    pub has_tabs: bool,
    pub expanded: bool,
}

/// Redraws the observing view when the shape of `dock` changes, and only
/// then: the dock also notifies for its tab strip, which it draws itself.
pub fn observe_shape<T: 'static>(dock: &Entity<Dock>, cx: &mut Context<T>) -> Subscription {
    let mut shape = dock.read(cx).shape();
    cx.observe(dock, move |_, dock, cx| {
        if replace_if_changed(&mut shape, dock.read(cx).shape()) {
            cx.notify();
        }
    })
}

pub struct Dock {
    cluster: Entity<Cluster>,
    forwards: Entity<Forwards>,
    tabs: Vec<DockTab>,
    /// The tab strip, one per tab: what the header is drawn from. Kept by
    /// [`refresh_summaries`](Self::refresh_summaries), so that drawing the
    /// header reads no tab.
    summaries: Vec<SessionSummary>,
    active: usize,
    expanded: bool,
    next_id: u64,
    /// Ends the tabs of the vault's contexts when it locks.
    _vault: Option<Subscription>,
    /// Titles of local and node shells are translated.
    _language: Option<Subscription>,
}

impl Dock {
    pub fn new(
        cluster: Entity<Cluster>,
        forwards: Entity<Forwards>,
        cx: &mut Context<Self>,
    ) -> Self {
        let vault = crate::vault::global(cx).map(|vault| {
            cx.observe(&vault, |dock, vault, cx| {
                if !vault.read(cx).is_unlocked() {
                    dock.vault_locked(cx);
                }
            })
        });
        let language = crate::i18n::global(cx)
            .map(|language| cx.observe(&language, |dock, _, cx| dock.refresh_summaries(cx)));
        Self {
            cluster,
            forwards,
            tabs: Vec::new(),
            summaries: Vec::new(),
            active: 0,
            expanded: true,
            next_id: 0,
            _vault: vault,
            _language: language,
        }
    }

    /// The vault locked (by hand, idle timer, screen lock): every tab on
    /// one of its contexts ends its stream or session and stays, saying
    /// why (ADR 0008). Each tab knows whether it is on such a context. A
    /// new kind of tab has to be named here to compile.
    fn vault_locked(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            match &tab.view {
                TabView::Logs(tab) => tab.update(cx, |tab, cx| tab.vault_locked(cx)),
                TabView::Terminal(tab) => tab.update(cx, |tab, cx| tab.vault_locked(cx)),
                TabView::Node(tab) => tab.update(cx, |tab, cx| tab.vault_locked(cx)),
                // The local shell keeps running with its own kubeconfig
                // file (ADR 0015).
                TabView::Local(_) => {}
            }
        }
    }

    /// Makes this dock the target of [`open_request`].
    pub fn install(dock: &Entity<Dock>, cx: &mut App) {
        cx.set_global(DockHandle(dock.downgrade()));
    }

    pub fn shape(&self) -> DockShape {
        DockShape {
            has_tabs: !self.tabs.is_empty(),
            expanded: self.expanded,
        }
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
        // Not `observe`: a tab notifies with every batch of lines or
        // chunk of output, shown or not (#166).
        fn changed<T: EventEmitter<TabChanged>>(
            tab: &Entity<T>,
            cx: &mut Context<Dock>,
        ) -> Subscription {
            cx.subscribe(tab, |dock, _, _: &TabChanged, cx| {
                dock.refresh_summaries(cx)
            })
        }
        let changed = match &view {
            TabView::Logs(tab) => changed(tab, cx),
            TabView::Terminal(tab) => changed(tab, cx),
            TabView::Local(tab) => changed(tab, cx),
            TabView::Node(tab) => changed(tab, cx),
        };
        self.next_id += 1;
        self.tabs.push(DockTab {
            id: self.next_id,
            view,
            _changed: changed,
        });
        self.active = self.tabs.len() - 1;
        self.expanded = true;
        self.summaries = self.summarize(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Redraws the tab strip if a tab now shows something else in it.
    fn refresh_summaries(&mut self, cx: &mut Context<Self>) {
        let summaries = self.summarize(cx);
        if replace_if_changed(&mut self.summaries, summaries) {
            cx.notify();
        }
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
        self.summaries = self.summarize(cx);
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

    /// Every tab as the tab strip shows it, in tab order.
    fn summarize(&self, cx: &App) -> Vec<SessionSummary> {
        let terminal = |id, title, context: Option<&str>, state: &SessionState| SessionSummary {
            id,
            terminal: true,
            title,
            context: context.map(str::to_owned),
            live: state.is_live(),
            running: *state == SessionState::Running,
        };
        self.tabs
            .iter()
            .map(|tab| match &tab.view {
                TabView::Logs(logs) => {
                    let logs = logs.read(cx);
                    SessionSummary {
                        id: tab.id,
                        terminal: false,
                        title: logs.title(),
                        context: logs.context().map(str::to_owned),
                        live: matches!(logs.state(), LogsState::Starting | LogsState::Streaming),
                        running: *logs.state() == LogsState::Streaming,
                    }
                }
                TabView::Terminal(tab_view) => {
                    let view = tab_view.read(cx);
                    terminal(tab.id, view.title(), view.context(), view.state())
                }
                TabView::Local(tab_view) => {
                    let view = tab_view.read(cx);
                    terminal(tab.id, view.title(), view.context(), view.state())
                }
                TabView::Node(tab_view) => {
                    let view = tab_view.read(cx);
                    terminal(tab.id, view.title(), view.context(), view.state())
                }
            })
            .collect()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let summaries = self.summaries.clone();
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
            // Cached: the tab is drawn again when it notifies, not when the
            // tab strip or the window around the dock is.
            .map(|tab| match &tab.view {
                TabView::Logs(view) => cached(view, cx).into_any_element(),
                TabView::Terminal(view) => cached(view, cx).into_any_element(),
                TabView::Local(view) => cached(view, cx).into_any_element(),
                TabView::Node(view) => cached(view, cx).into_any_element(),
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

#[cfg(test)]
mod tests {
    use super::{SessionSummary, replace_if_changed};

    fn summary(title: &str, running: bool) -> SessionSummary {
        SessionSummary {
            id: 1,
            terminal: false,
            title: title.into(),
            context: Some("minikube".into()),
            live: true,
            running,
        }
    }

    /// The tab strip is redrawn for a new title or state, and for a tab
    /// that opened or closed; a tab that only reports again is not a
    /// change.
    #[test]
    fn tab_strip_changes_only_with_what_it_shows() {
        let mut strip = vec![summary("web-0", false)];
        assert!(!replace_if_changed(
            &mut strip,
            vec![summary("web-0", false)]
        ));
        assert!(replace_if_changed(&mut strip, vec![summary("web-0", true)]));
        assert_eq!(strip, vec![summary("web-0", true)]);
        assert!(replace_if_changed(
            &mut strip,
            vec![summary("web-0/app", true)]
        ));
        assert!(replace_if_changed(
            &mut strip,
            vec![summary("web-0/app", true), summary("web-1", false)]
        ));
        assert!(replace_if_changed(&mut strip, Vec::new()));
        assert!(!replace_if_changed(&mut strip, Vec::new()));
    }
}
