//! Editor tabs in the main window's content area. The first tab is the
//! screen behind them (the resource browser); every other tab is a
//! [`YamlEditor`] or a [`SecretEditor`].

use gpui_kit::component::IconName;
use gpui_kit::*;
use kubach_ui::{TabItem, TabStrip};

use super::secret::SecretEditor;
use super::{EditorHub, GlobalHub, OpenRequest, Opened, YamlEditor};
use crate::cluster::Cluster;

const KEY_CONTEXT: &str = "EditorTabs";

actions!(
    yaml_editor,
    [
        /// Closes the active editor tab (`cmd-w` / `ctrl-w`).
        CloseTab
    ]
);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-w", CloseTab, Some(KEY_CONTEXT))]);
}

/// The active tab after closing editor tab `closed`, with `remaining` editor
/// tabs left. `None` is the first (home) tab. Closing the active tab moves
/// to its right neighbour, else its left one, else home.
fn active_after_close(active: Option<usize>, closed: usize, remaining: usize) -> Option<usize> {
    match active {
        Some(active) if active == closed => match remaining {
            0 => None,
            _ => Some(closed.min(remaining - 1)),
        },
        Some(active) if active > closed => Some(active - 1),
        other => other,
    }
}

/// The editor in a tab.
#[derive(Clone)]
enum TabView {
    Yaml(Entity<YamlEditor>),
    Secret(Entity<SecretEditor>),
}

impl TabView {
    fn entity_id(&self) -> EntityId {
        match self {
            Self::Yaml(view) => view.entity_id(),
            Self::Secret(view) => view.entity_id(),
        }
    }

    fn context(&self, cx: &App) -> String {
        match self {
            Self::Yaml(view) => view.read(cx).context().to_owned(),
            Self::Secret(view) => view.read(cx).context().to_owned(),
        }
    }

    fn tab_title(&self, cx: &App) -> String {
        match self {
            Self::Yaml(view) => view.read(cx).tab_title(),
            Self::Secret(view) => view.read(cx).tab_title(),
        }
    }

    fn is_dirty(&self, cx: &App) -> bool {
        match self {
            Self::Yaml(view) => view.read(cx).is_dirty(cx),
            Self::Secret(view) => view.read(cx).is_dirty(cx),
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Yaml(view) => view.update(cx, |editor, cx| editor.focus_editor(window, cx)),
            Self::Secret(view) => view.update(cx, |editor, cx| editor.focus_editor(window, cx)),
        }
    }

    /// Whether this tab already edits what `request` asks for.
    fn shows(&self, request: &OpenRequest, cx: &App) -> bool {
        match (self, &request.target) {
            (Self::Yaml(view), Opened::Yaml(target)) => {
                let editor = view.read(cx);
                editor.context == request.connection.context && editor.target.same_object(target)
            }
            (Self::Secret(view), Opened::Secret(target)) => view
                .read(cx)
                .same_target(&request.connection.context, target),
            _ => false,
        }
    }

    fn into_any_element(self) -> AnyElement {
        match self {
            Self::Yaml(view) => view.into_any_element(),
            Self::Secret(view) => view.into_any_element(),
        }
    }
}

struct Tab {
    view: TabView,
    _observe: Subscription,
}

/// Hosts editor tabs in a view `V` (the main window's workspace): opens one
/// per request from the [`EditorHub`] and renders the tab strip.
pub struct EditorTabs<V> {
    cluster: Entity<Cluster>,
    tabs: Vec<Tab>,
    /// Index into `tabs`; `None` shows the home tab.
    active: Option<usize>,
    requests: Vec<OpenRequest>,
    field: fn(&mut V) -> &mut EditorTabs<V>,
    _subscriptions: Vec<Subscription>,
}

impl<V: 'static> EditorTabs<V> {
    /// `field` returns this host inside the view; `cluster` is the main
    /// window's, for the "other context" badges.
    pub fn new(
        cluster: Entity<Cluster>,
        field: fn(&mut V) -> &mut EditorTabs<V>,
        cx: &mut Context<V>,
    ) -> Self {
        let subscriptions = cx
            .try_global::<GlobalHub>()
            .map(|GlobalHub(hub)| hub.clone())
            .map(|hub: Entity<EditorHub>| {
                cx.subscribe(&hub, move |view, _, request: &OpenRequest, cx| {
                    field(view).requests.push(request.clone());
                    cx.notify();
                })
            })
            .into_iter()
            .collect();
        Self {
            cluster,
            tabs: Vec::new(),
            active: None,
            requests: Vec::new(),
            field,
            _subscriptions: subscriptions,
        }
    }

    /// Opens pending requests; an object already open in a tab of the same
    /// context gets that tab focused instead.
    fn take_requests(&mut self, window: &mut Window, cx: &mut Context<V>) {
        for request in std::mem::take(&mut self.requests) {
            let existing = self
                .tabs
                .iter()
                .position(|tab| tab.view.shows(&request, cx));
            let index = match existing {
                Some(index) => index,
                None => {
                    let cluster = self.cluster.clone();
                    let OpenRequest {
                        connection,
                        catalog,
                        target,
                    } = request;
                    let (view, observe) = match target {
                        Opened::Yaml(target) => {
                            let view = cx.new(|cx| {
                                YamlEditor::new(connection, catalog, target, cluster, window, cx)
                            });
                            let observe = cx.observe(&view, |_, _, cx| cx.notify());
                            (TabView::Yaml(view), observe)
                        }
                        Opened::Secret(target) => {
                            let view = cx.new(|cx| {
                                SecretEditor::new(connection, target, cluster, window, cx)
                            });
                            let observe = cx.observe(&view, |_, _, cx| cx.notify());
                            (TabView::Secret(view), observe)
                        }
                    };
                    self.tabs.push(Tab {
                        view,
                        _observe: observe,
                    });
                    self.tabs.len() - 1
                }
            };
            self.activate(Some(index), window, cx);
        }
    }

    /// Whether the home tab (the screen picked in the sidebar) is shown.
    pub fn home_active(&self) -> bool {
        self.active.is_none()
    }

    /// Shows the home tab ("back" to a screen, #395).
    pub fn show_home(&mut self, window: &mut Window, cx: &mut Context<V>) {
        if !self.home_active() {
            self.activate(None, window, cx);
        }
    }

    fn activate(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<V>) {
        self.active = index.filter(|&i| i < self.tabs.len());
        if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
            tab.view.focus(window, cx);
        }
        cx.notify();
    }

    /// Closes editor tab `index`, asking first if it has unsaved edits.
    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<V>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let id = tab.view.entity_id();
        if !tab.view.is_dirty(cx) {
            self.remove(id, cx);
            return;
        }
        let detail = kubach_i18n::t!("editor-close-detail", tab = tab.view.tab_title(cx));
        let title = kubach_i18n::t!("editor-close-title");
        let discard = kubach_i18n::t!("editor-discard-edits");
        let cancel = kubach_i18n::t!("ui-cancel");
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[discard.as_str(), cancel.as_str()],
            cx,
        );
        let field = self.field;
        cx.spawn_in(window, async move |view, cx| {
            if answer.await == Ok(0) {
                view.update(cx, |view, cx| field(view).remove(id, cx)).ok();
            }
        })
        .detach();
    }

    fn remove(&mut self, id: EntityId, cx: &mut Context<V>) {
        if let Some(index) = self.tabs.iter().position(|t| t.view.entity_id() == id) {
            self.tabs.remove(index);
            self.active = active_after_close(self.active, index, self.tabs.len());
            cx.notify();
        }
    }

    /// The tab strip over the active tab: `home` (titled `home_title`,
    /// marked with `home_accent`, the color of the current cluster) or an
    /// editor.
    pub fn render(
        &mut self,
        home_title: SharedString,
        home_accent: Option<Hsla>,
        home: AnyElement,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        self.take_requests(window, cx);
        let active_context = self.cluster.read(cx).active().map(|c| c.name.clone());
        let mut items = vec![
            TabItem::new(home_title)
                .icon(IconName::LayoutDashboard)
                .accent(home_accent),
        ];
        for tab in &self.tabs {
            let context = tab.view.context(cx);
            let mut item = TabItem::new(tab.view.tab_title(cx))
                .closable(true)
                .dirty(tab.view.is_dirty(cx));
            if active_context.as_deref() != Some(context.as_str()) {
                item = item.badge(context);
            }
            items.push(item);
        }

        let field = self.field;
        let host = cx.entity().downgrade();
        let select = host.clone();
        let strip = TabStrip::new("editor-tabs", items, self.active.map_or(0, |i| i + 1))
            .on_select(move |index, window, cx| {
                select
                    .update(cx, |view, cx| {
                        field(view).activate(index.checked_sub(1), window, cx)
                    })
                    .ok();
            })
            .on_close(move |index, window, cx| {
                if let Some(index) = index.checked_sub(1) {
                    host.update(cx, |view, cx| field(view).close(index, window, cx))
                        .ok();
                }
            });

        let content = match self.active.and_then(|i| self.tabs.get(i)) {
            Some(tab) => tab.view.clone().into_any_element(),
            None => home,
        };
        div()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(move |view, _: &CloseTab, window, cx| {
                let tabs = field(view);
                if let Some(index) = tabs.active {
                    tabs.close(index, window, cx);
                }
            }))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(strip)
            .child(div().flex_1().min_w_0().min_h_0().flex().child(content))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::active_after_close;

    #[test]
    fn closing_moves_to_a_neighbour() {
        // Home stays home.
        assert_eq!(active_after_close(None, 0, 2), None);
        // Closing the active tab: right neighbour, else left, else home.
        assert_eq!(active_after_close(Some(1), 1, 2), Some(1));
        assert_eq!(active_after_close(Some(2), 2, 2), Some(1));
        assert_eq!(active_after_close(Some(0), 0, 0), None);
        // Another tab closes: the active one keeps its tab.
        assert_eq!(active_after_close(Some(2), 0, 2), Some(1));
        assert_eq!(active_after_close(Some(0), 1, 2), Some(0));
    }
}
