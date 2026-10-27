//! A left-to-right graph in columns: titled columns of compact nodes joined
//! by curved connectors. The caller places nodes (column, row); the graph
//! only draws them. Hovering a node can highlight a set of nodes (e.g. the
//! chain through it): their connectors turn the accent color, the rest dim.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::Tone;

pub const FLOW_NODE_WIDTH: Pixels = px(232.);
pub const FLOW_NODE_HEIGHT: Pixels = px(48.);
const ROW_GAP: Pixels = px(10.);
const COLUMN_GAP: Pixels = px(72.);
const HEADER_HEIGHT: Pixels = px(28.);
const DOT: Pixels = px(6.);

type ClickHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(&(usize, bool), &mut Window, &mut App)>;

/// One box of a [`FlowGraph`].
#[derive(Clone, Debug)]
pub struct FlowNode {
    pub column: usize,
    pub row: usize,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    /// Color of the dot before the subtitle.
    pub tone: Tone,
    /// Shows a pointer and reports clicks.
    pub clickable: bool,
}

impl FlowNode {
    pub fn new(column: usize, row: usize, title: impl Into<SharedString>) -> Self {
        Self {
            column,
            row,
            title: title.into(),
            subtitle: None,
            tone: Tone::Neutral,
            clickable: false,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub fn clickable(mut self, clickable: bool) -> Self {
        self.clickable = clickable;
        self
    }
}

/// Top-left corner of the node at `(column, row)`, relative to the graph.
pub fn flow_node_origin(column: usize, row: usize) -> Point<Pixels> {
    point(
        (FLOW_NODE_WIDTH + COLUMN_GAP) * column as f32,
        HEADER_HEIGHT + (FLOW_NODE_HEIGHT + ROW_GAP) * row as f32,
    )
}

/// Size of a graph with `columns` columns and `rows` rows in the tallest.
pub fn flow_graph_size(columns: usize, rows: usize) -> Size<Pixels> {
    let width = if columns == 0 {
        px(0.)
    } else {
        FLOW_NODE_WIDTH * columns as f32 + COLUMN_GAP * (columns - 1) as f32
    };
    let height = if rows == 0 {
        HEADER_HEIGHT
    } else {
        HEADER_HEIGHT + FLOW_NODE_HEIGHT * rows as f32 + ROW_GAP * (rows - 1) as f32
    };
    size(width, height)
}

/// Start, end and control points of the connector from `from` to `to`:
/// out of the right edge of one node, into the left edge of the other.
pub fn flow_edge_curve(from: &FlowNode, to: &FlowNode) -> [Point<Pixels>; 4] {
    let half = FLOW_NODE_HEIGHT / 2.;
    let a = flow_node_origin(from.column, from.row);
    let b = flow_node_origin(to.column, to.row);
    let start = point(a.x + FLOW_NODE_WIDTH, a.y + half);
    let end = point(b.x, b.y + half);
    let bend = (end.x - start.x) / 2.;
    [
        start,
        point(start.x + bend, start.y),
        point(end.x - bend, end.y),
        end,
    ]
}

/// Columns of nodes joined by connectors, laid out by the caller.
#[derive(IntoElement)]
pub struct FlowGraph {
    id: ElementId,
    columns: Vec<SharedString>,
    nodes: Vec<FlowNode>,
    edges: Vec<(usize, usize)>,
    highlight: Option<Vec<usize>>,
    on_click: Option<ClickHandler>,
    on_hover: Option<HoverHandler>,
}

impl FlowGraph {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            columns: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            highlight: None,
            on_click: None,
            on_hover: None,
        }
    }

    /// Column titles, left to right.
    pub fn columns(mut self, columns: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    pub fn nodes(mut self, nodes: impl IntoIterator<Item = FlowNode>) -> Self {
        self.nodes = nodes.into_iter().collect();
        self
    }

    /// Connectors as `(from, to)` node indices; `from` is left of `to`.
    pub fn edges(mut self, edges: impl IntoIterator<Item = (usize, usize)>) -> Self {
        self.edges = edges.into_iter().collect();
        self
    }

    /// Node indices to emphasize, with connectors between them. Everything
    /// else is dimmed. `None` shows all alike.
    pub fn highlight(mut self, nodes: Option<Vec<usize>>) -> Self {
        self.highlight = nodes;
        self
    }

    /// Called with the index of a clicked [`FlowNode::clickable`] node.
    pub fn on_click(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Called with a node index and whether the pointer entered or left it.
    pub fn on_hover(
        mut self,
        handler: impl Fn(&(usize, bool), &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_hover = Some(Rc::new(handler));
        self
    }

    fn rows(&self) -> usize {
        self.nodes.iter().map(|n| n.row + 1).max().unwrap_or(0)
    }
}

impl RenderOnce for FlowGraph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let graph_size = flow_graph_size(self.columns.len(), self.rows());
        let highlighted = |ix: usize| {
            self.highlight
                .as_ref()
                .is_none_or(|nodes| nodes.contains(&ix))
        };
        let (edge, accent, dim) = (theme.ring, theme.info, theme.border);
        let mut curves: Vec<([Point<Pixels>; 4], Hsla, bool)> = self
            .edges
            .iter()
            .filter_map(|&(from, to)| {
                let curve = flow_edge_curve(self.nodes.get(from)?, self.nodes.get(to)?);
                Some(match &self.highlight {
                    None => (curve, edge, false),
                    Some(_) if highlighted(from) && highlighted(to) => (curve, accent, true),
                    Some(_) => (curve, dim, false),
                })
            })
            .collect();
        // Emphasized connectors on top.
        curves.sort_by_key(|(_, _, on_top)| *on_top);

        let headers = self.columns.iter().enumerate().map(|(column, title)| {
            let origin = flow_node_origin(column, 0);
            div()
                .absolute()
                .left(origin.x)
                .top_0()
                .w(FLOW_NODE_WIDTH)
                .h(HEADER_HEIGHT)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(title.clone())
        });

        let nodes = self.nodes.iter().enumerate().map(|(ix, node)| {
            let origin = flow_node_origin(node.column, node.row);
            let emphasized = self.highlight.is_some() && highlighted(ix);
            let dimmed = !highlighted(ix);
            let on_click = self.on_click.clone().filter(|_| node.clickable);
            let on_hover = self.on_hover.clone();
            div()
                .id(ix)
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(FLOW_NODE_WIDTH)
                .h(FLOW_NODE_HEIGHT)
                .px_3()
                .flex()
                .flex_col()
                .justify_center()
                .gap_0p5()
                .rounded_md()
                .border_1()
                .border_color(if emphasized { accent } else { theme.border })
                .bg(theme.background)
                .child(div().absolute().inset_0().rounded_md().bg(theme.group_box))
                .when(dimmed, |this| this.opacity(0.45))
                .hover(|this| this.border_color(theme.ring))
                .when_some(on_click, |this, on_click| {
                    this.cursor_pointer()
                        .on_click(move |_, window, cx| on_click(&ix, window, cx))
                })
                .when_some(on_hover, |this, on_hover| {
                    this.on_hover(move |hovered, window, cx| on_hover(&(ix, *hovered), window, cx))
                })
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(node.title.clone()),
                )
                .when_some(node.subtitle.clone(), |this, subtitle| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .size(DOT)
                                    .rounded_full()
                                    .bg(node.tone.color(cx)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                })
        });

        div()
            .id(self.id.clone())
            .relative()
            .flex_shrink_0()
            .w(graph_size.width)
            .h(graph_size.height)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let origin = bounds.origin;
                        for (curve, color, on_top) in &curves {
                            let width = if *on_top { px(1.5) } else { px(1.) };
                            let [start, a, b, end] = curve.map(|p| p + origin);
                            let mut path = PathBuilder::stroke(width);
                            path.move_to(start);
                            path.cubic_bezier_to(end, a, b);
                            if let Ok(path) = path.build() {
                                window.paint_path(path, *color);
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .w(graph_size.width)
                .h(graph_size.height),
            )
            .children(headers)
            .children(nodes)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px};

    use super::{
        COLUMN_GAP, FLOW_NODE_HEIGHT, FLOW_NODE_WIDTH, FlowNode, HEADER_HEIGHT, ROW_GAP,
        flow_edge_curve, flow_graph_size, flow_node_origin,
    };

    #[test]
    fn nodes_sit_in_columns_and_rows() {
        assert_eq!(flow_node_origin(0, 0), point(px(0.), HEADER_HEIGHT));
        assert_eq!(
            flow_node_origin(2, 3),
            point(
                (FLOW_NODE_WIDTH + COLUMN_GAP) * 2.,
                HEADER_HEIGHT + (FLOW_NODE_HEIGHT + ROW_GAP) * 3.
            )
        );
    }

    #[test]
    fn size_fits_the_last_node() {
        let size = flow_graph_size(4, 3);
        let last = flow_node_origin(3, 2);
        assert_eq!(size.width, last.x + FLOW_NODE_WIDTH);
        assert_eq!(size.height, last.y + FLOW_NODE_HEIGHT);
        assert_eq!(flow_graph_size(0, 0).width, px(0.));
        assert_eq!(flow_graph_size(4, 0).height, HEADER_HEIGHT);
    }

    #[test]
    fn edge_runs_between_facing_sides() {
        let from = FlowNode::new(0, 0, "a");
        let to = FlowNode::new(1, 2, "b");
        let [start, a, b, end] = flow_edge_curve(&from, &to);
        assert_eq!(start.x, FLOW_NODE_WIDTH);
        assert_eq!(end.x, FLOW_NODE_WIDTH + COLUMN_GAP);
        assert_eq!(start.y, HEADER_HEIGHT + FLOW_NODE_HEIGHT / 2.);
        assert_eq!(end.y, flow_node_origin(1, 2).y + FLOW_NODE_HEIGHT / 2.);
        // Horizontal tangents at both ends.
        assert_eq!((a.y, b.y), (start.y, end.y));
        assert!(start.x < a.x && b.x < end.x);
    }
}
