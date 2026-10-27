use std::collections::HashSet;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::TruncatedText;
use crate::tokens::ROW_HEIGHT;

/// Indentation per nesting level.
const INDENT: Pixels = px(14.);

/// An entry of a [`NavTree`]: a leaf that can be selected or a group that
/// expands and collapses.
#[derive(Clone)]
pub struct NavNode {
    pub id: SharedString,
    pub label: SharedString,
    pub icon: Option<Lucide>,
    pub count: Option<usize>,
    pub children: Vec<NavNode>,
    group: bool,
}

impl NavNode {
    pub fn leaf(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            count: None,
            children: Vec::new(),
            group: false,
        }
    }

    /// A collapsible group; it stays a group even without children.
    pub fn group(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        children: Vec<NavNode>,
    ) -> Self {
        Self {
            children,
            group: true,
            ..Self::leaf(id, label)
        }
    }

    pub fn icon(mut self, icon: impl Into<Lucide>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn count(mut self, count: Option<usize>) -> Self {
        self.count = count;
        self
    }

    pub fn is_group(&self) -> bool {
        self.group
    }
}

/// A row of the flattened, currently visible tree.
struct Row<'a> {
    node: &'a NavNode,
    depth: usize,
    parent: Option<&'a SharedString>,
}

fn visible_rows<'a>(nodes: &'a [NavNode], expanded: &HashSet<SharedString>) -> Vec<Row<'a>> {
    fn walk<'a>(
        nodes: &'a [NavNode],
        depth: usize,
        parent: Option<&'a SharedString>,
        expanded: &HashSet<SharedString>,
        rows: &mut Vec<Row<'a>>,
    ) {
        for node in nodes {
            rows.push(Row {
                node,
                depth,
                parent,
            });
            if node.group && expanded.contains(&node.id) {
                walk(&node.children, depth + 1, Some(&node.id), expanded, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(nodes, 0, None, expanded, &mut rows);
    rows
}

/// Path of group ids leading to `id`, outermost first.
fn ancestors(nodes: &[NavNode], id: &str) -> Option<Vec<SharedString>> {
    for node in nodes {
        if node.id == id {
            return Some(Vec::new());
        }
        if let Some(mut path) = ancestors(&node.children, id) {
            path.insert(0, node.id.clone());
            return Some(path);
        }
    }
    None
}

/// What a [`NavTree`] remembers: expanded groups, the selected leaf and the
/// keyboard cursor. Plain data, so it can be saved per context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavTreeState {
    pub expanded: HashSet<SharedString>,
    pub selected: Option<SharedString>,
    cursor: Option<SharedString>,
}

impl NavTreeState {
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    pub fn set_expanded(&mut self, id: impl Into<SharedString>, expanded: bool) {
        let id = id.into();
        if expanded {
            self.expanded.insert(id);
        } else {
            self.expanded.remove(&id);
        }
    }

    pub fn toggle(&mut self, id: &SharedString) {
        let expanded = !self.is_expanded(id);
        self.set_expanded(id.clone(), expanded);
    }

    /// Selects `id` and expands its groups so it is visible.
    pub fn select(&mut self, nodes: &[NavNode], id: &SharedString) {
        if let Some(path) = ancestors(nodes, id) {
            self.expanded.extend(path);
        }
        self.selected = Some(id.clone());
        self.cursor = Some(id.clone());
    }

    /// Row the keyboard acts on: the cursor if visible, else the selection's
    /// nearest visible row, else the first row.
    fn cursor_index(&self, rows: &[Row]) -> Option<usize> {
        let find = |id: &SharedString| rows.iter().position(|row| &row.node.id == id);
        self.cursor
            .as_ref()
            .and_then(find)
            .or_else(|| self.selected.as_ref().and_then(find))
            .or((!rows.is_empty()).then_some(0))
    }

    /// Visible index of the cursor row.
    pub fn cursor(&self, nodes: &[NavNode]) -> Option<usize> {
        self.cursor_index(&visible_rows(nodes, &self.expanded))
    }

    /// Moves the cursor by `delta` visible rows, clamped.
    pub fn move_cursor(&mut self, nodes: &[NavNode], delta: isize) {
        let rows = visible_rows(nodes, &self.expanded);
        let Some(ix) = self.cursor_index(&rows) else {
            return;
        };
        let target = ix.saturating_add_signed(delta).min(rows.len() - 1);
        self.cursor = Some(rows[target].node.id.clone());
    }

    /// Collapses the cursor's group, or moves to its parent group.
    pub fn collapse(&mut self, nodes: &[NavNode]) {
        let rows = visible_rows(nodes, &self.expanded);
        let Some(row) = self.cursor_index(&rows).map(|ix| &rows[ix]) else {
            return;
        };
        if row.node.group && self.is_expanded(&row.node.id) {
            let id = row.node.id.clone();
            self.set_expanded(id, false);
        } else if let Some(parent) = row.parent {
            self.cursor = Some(parent.clone());
        }
    }

    /// Expands the cursor's group, or moves into its first child.
    pub fn expand(&mut self, nodes: &[NavNode]) {
        let rows = visible_rows(nodes, &self.expanded);
        let Some(row) = self.cursor_index(&rows).map(|ix| &rows[ix]) else {
            return;
        };
        if !row.node.group {
            return;
        }
        if !self.is_expanded(&row.node.id) {
            let id = row.node.id.clone();
            self.set_expanded(id, true);
        } else if let Some(child) = row.node.children.first() {
            self.cursor = Some(child.id.clone());
        }
    }

    /// Enter / Space on the cursor: toggles a group or selects a leaf.
    /// Returns the newly selected leaf.
    pub fn activate(&mut self, nodes: &[NavNode]) -> Option<SharedString> {
        let rows = visible_rows(nodes, &self.expanded);
        let row = &rows[self.cursor_index(&rows)?];
        let id = row.node.id.clone();
        if row.node.group {
            self.toggle(&id);
            None
        } else {
            self.cursor = Some(id.clone());
            self.selected = Some(id.clone());
            Some(id)
        }
    }
}

/// Emitted by [`NavTree`].
#[derive(Clone, Debug, PartialEq)]
pub enum NavTreeEvent {
    /// A leaf was picked by click or keyboard.
    Select(SharedString),
}

/// Sidebar tree: collapsible groups with nested, selectable leaves and an
/// optional count per row. Scrolls on its own; give it a bounded height.
///
/// Keyboard: ↑/↓ move, ← collapses (or goes to the parent), → expands (or
/// enters the group), Enter/Space opens.
pub struct NavTree {
    nodes: Vec<NavNode>,
    state: NavTreeState,
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
}

impl EventEmitter<NavTreeEvent> for NavTree {}

impl Focusable for NavTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl NavTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            nodes: Vec::new(),
            state: NavTreeState::default(),
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }

    pub fn set_nodes(&mut self, nodes: Vec<NavNode>, cx: &mut Context<Self>) {
        self.nodes = nodes;
        cx.notify();
    }

    pub fn nodes(&self) -> &[NavNode] {
        &self.nodes
    }

    pub fn state(&self) -> &NavTreeState {
        &self.state
    }

    /// Replaces expanded groups and selection, e.g. restoring a saved state.
    pub fn set_state(&mut self, state: NavTreeState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    /// Selects a leaf without emitting [`NavTreeEvent::Select`].
    pub fn select(&mut self, id: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.state.select(&self.nodes, &id.into());
        cx.notify();
    }

    fn emit_activation(&mut self, picked: Option<SharedString>, cx: &mut Context<Self>) {
        if let Some(id) = picked {
            cx.emit(NavTreeEvent::Select(id));
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        match keystroke.key.as_str() {
            "up" => self.state.move_cursor(&self.nodes, -1),
            "down" => self.state.move_cursor(&self.nodes, 1),
            "left" => self.state.collapse(&self.nodes),
            "right" => self.state.expand(&self.nodes),
            "enter" | "space" => {
                let picked = self.state.activate(&self.nodes);
                self.emit_activation(picked, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        if let Some(ix) = self.state.cursor(&self.nodes) {
            self.scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    fn on_row_click(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.state.cursor = Some(id.clone());
        let picked = self.state.activate(&self.nodes);
        self.emit_activation(picked, cx);
    }
}

impl Render for NavTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.focus_handle.is_focused(window);
        let rows = visible_rows(&self.nodes, &self.state.expanded);
        let cursor = self.state.cursor_index(&rows);
        let hover = theme.list_hover;
        let muted = theme.muted_foreground;
        let ring = theme.ring;

        let elements: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let node = row.node;
                let selected = self.state.selected.as_ref() == Some(&node.id);
                let (fg, bg) = if selected {
                    (theme.sidebar_accent_foreground, theme.sidebar_accent)
                } else {
                    (theme.sidebar_foreground, theme.transparent)
                };
                let border = if focused && cursor == Some(ix) {
                    ring
                } else {
                    theme.transparent
                };
                let chevron = node.group.then(|| {
                    let icon = if self.state.is_expanded(&node.id) {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    };
                    Icon::new(icon).size_3p5().text_color(muted)
                });
                let id = node.id.clone();
                div()
                    .id(ElementId::Name(node.id.clone()))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .h(ROW_HEIGHT)
                    .pr_2()
                    .pl(px(8.) + INDENT * row.depth as f32)
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .text_sm()
                    .text_color(fg)
                    .bg(bg)
                    .when(!selected, |row| row.hover(move |row| row.bg(hover)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.on_row_click(&id, window, cx)),
                    )
                    .when_some(node.icon, |row, icon| row.child(Icon::new(icon).size_4()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TruncatedText::new("label", node.label.clone())),
                    )
                    .when_some(node.count, |row, count| {
                        row.child(div().text_xs().text_color(muted).child(count.to_string()))
                    })
                    .children(chevron)
            })
            .collect();

        div()
            .id("nav-tree")
            .key_context("NavTree")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .size_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(elements)
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: `gpui_kit::*` exports a `test` attribute that would shadow std's.
    use super::{NavNode, NavTreeState, visible_rows};

    fn tree() -> Vec<NavNode> {
        vec![
            NavNode::leaf("overview", "Overview"),
            NavNode::group(
                "workloads",
                "Workloads",
                vec![NavNode::leaf("pods", "Pods"), NavNode::leaf("jobs", "Jobs")],
            ),
            NavNode::group(
                "custom",
                "Custom resources",
                vec![NavNode::group(
                    "cert-manager.io",
                    "cert-manager.io",
                    vec![NavNode::leaf("certs", "Certificate")],
                )],
            ),
        ]
    }

    fn ids(nodes: &[NavNode], state: &NavTreeState) -> Vec<String> {
        visible_rows(nodes, &state.expanded)
            .iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.node.id))
            .collect()
    }

    fn cursor_id(nodes: &[NavNode], state: &NavTreeState) -> String {
        let rows = visible_rows(nodes, &state.expanded);
        rows[state.cursor_index(&rows).unwrap()].node.id.to_string()
    }

    #[test]
    fn only_expanded_groups_show_children() {
        let nodes = tree();
        let mut state = NavTreeState::default();
        assert_eq!(ids(&nodes, &state), ["overview", "workloads", "custom"]);
        state.toggle(&"workloads".into());
        assert_eq!(
            ids(&nodes, &state),
            ["overview", "workloads", "  pods", "  jobs", "custom"]
        );
        // A collapsed outer group hides an expanded inner one.
        state.set_expanded("cert-manager.io", true);
        assert!(!ids(&nodes, &state).iter().any(|id| id.contains("certs")));
    }

    #[test]
    fn selecting_reveals_nested_leaves() {
        let nodes = tree();
        let mut state = NavTreeState::default();
        state.select(&nodes, &"certs".into());
        assert!(state.is_expanded("custom") && state.is_expanded("cert-manager.io"));
        assert_eq!(state.selected.as_deref(), Some("certs"));
        assert_eq!(cursor_id(&nodes, &state), "certs");
    }

    #[test]
    fn keyboard_moves_expands_and_collapses() {
        let nodes = tree();
        let mut state = NavTreeState::default();
        assert_eq!(cursor_id(&nodes, &state), "overview");
        state.move_cursor(&nodes, -1);
        assert_eq!(cursor_id(&nodes, &state), "overview");
        state.move_cursor(&nodes, 1);
        assert_eq!(cursor_id(&nodes, &state), "workloads");

        // → expands, → again enters, ← goes back to the parent, ← collapses.
        state.expand(&nodes);
        assert!(state.is_expanded("workloads"));
        state.expand(&nodes);
        assert_eq!(cursor_id(&nodes, &state), "pods");
        state.move_cursor(&nodes, 1);
        assert_eq!(state.activate(&nodes).as_deref(), Some("jobs"));
        assert_eq!(state.selected.as_deref(), Some("jobs"));
        state.collapse(&nodes);
        assert_eq!(cursor_id(&nodes, &state), "workloads");
        state.collapse(&nodes);
        assert!(!state.is_expanded("workloads"));

        // Enter on a group toggles it and selects nothing.
        assert_eq!(state.activate(&nodes), None);
        assert!(state.is_expanded("workloads"));
        state.move_cursor(&nodes, 10);
        assert_eq!(cursor_id(&nodes, &state), "custom");
    }

    #[test]
    fn hidden_cursor_falls_back_to_the_selection() {
        let nodes = tree();
        let mut state = NavTreeState::default();
        state.select(&nodes, &"jobs".into());
        state.move_cursor(&nodes, -1);
        assert_eq!(cursor_id(&nodes, &state), "pods");
        state.set_expanded("workloads", false);
        // `pods` is hidden and so is `jobs`: start from the top.
        assert_eq!(cursor_id(&nodes, &state), "overview");
        state.set_expanded("workloads", true);
        assert_eq!(cursor_id(&nodes, &state), "pods");
    }
}
