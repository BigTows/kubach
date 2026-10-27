//! A general graph canvas: layered auto-layout, pan and zoom, minimap,
//! folding groups, hover chains, selection and search. It knows nothing
//! about what nodes stand for; callers map their model onto [`GraphData`].

mod labels;
pub mod layout;
mod model;
mod paint;
mod svg;
mod view;

pub use labels::PlacedLabel;
pub use model::{
    EdgeStyle, GraphBadge, GraphData, GraphEdge, GraphGroup, GraphNode, GraphTarget, TrafficLevel,
};
pub use svg::{GraphGeometry, SvgPalette, graph_geometry, graph_svg, svg_to_png};
pub use view::{GRAPH_NODE_HEIGHT, GRAPH_NODE_WIDTH, GraphView, GraphViewEvent};

pub(crate) fn init(cx: &mut gpui_kit::App) {
    view::init(cx);
}
