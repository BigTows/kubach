mod action_toast;
mod back_button;
mod bulk_progress;
mod chart_icon;
mod check_result;
mod chip_list;
mod compact_table;
mod confirm_dialog;
mod data_key;
#[cfg(debug_assertions)]
mod debug_hud;
mod diff_view;
mod donut;
mod download_progress;
mod empty_state;
mod event_list;
mod filter_bar;
mod flow_graph;
mod graph;
mod hotbar;
mod key_value;
mod line_chart;
pub mod line_selection;
mod loader;
mod loading_bar;
mod log_view;
mod logo;
mod multi_select;
mod nav_item;
mod nav_tree;
mod operations;
mod page_header;
mod panel;
mod port_chip;
mod replica_bar;
mod resource_table;
mod secret_input;
mod segment_tabs;
mod setting;
mod side_list;
mod side_panel;
mod split_button;
mod stat_card;
mod status;
mod status_bar;
mod status_counts;
mod status_cubes;
mod styled_spans;
mod tab_strip;
pub mod terminal;
mod traffic_legend;
mod truncated_text;
mod usage_bar;
mod window_titlebar;

pub use action_toast::{ACTION_TOAST_SUCCESS_TIMEOUT, ActionToast};
pub use back_button::{BACK_LABEL_MAX, BackButton, back_label};
pub use bulk_progress::{BulkProgress, BulkResult};
pub use chart_icon::ChartIcon;
pub use check_result::{CheckResult, CheckStatus};
pub use chip_list::ChipList;
pub use compact_table::CompactTable;
pub use confirm_dialog::{ConfirmDialog, ConfirmHandle, ConfirmLive, preview, typed_matches};
pub use data_key::{DATA_KEY_MAX_LINES, DataKeyBlock, data_key_editor_height};
#[cfg(debug_assertions)]
pub use debug_hud::{DEBUG_HUD_WIDTH, DebugHud};
pub use diff_view::{DiffLineKind, DiffRow, DiffView, DiffViewLine, fold_rows, fold_unchanged};
pub use donut::{DONUT_SIZE, Donut, DonutColor, DonutSegment, donut_value, ring_spans};
pub use download_progress::{DownloadProgress, format_bytes};
pub use empty_state::EmptyState;
pub use event_list::{EventEntry, EventList, EventTimeline, UsageMeter};
pub use filter_bar::{FilterBar, SEARCH_WIDTH, name_matches};
pub use flow_graph::{
    FLOW_NODE_HEIGHT, FLOW_NODE_WIDTH, FlowGraph, FlowNode, flow_edge_curve, flow_graph_size,
    flow_node_origin,
};
pub use graph::{
    EdgeStyle, GRAPH_NODE_HEIGHT, GRAPH_NODE_WIDTH, GraphBadge, GraphData, GraphEdge,
    GraphGeometry, GraphGroup, GraphNode, GraphTarget, GraphView, GraphViewEvent, PlacedLabel,
    SvgPalette, TrafficLevel, graph_geometry, graph_svg, layout as graph_layout, svg_to_png,
};
pub use hotbar::{Hotbar, HotbarTile, TileColor, TilePreview};
pub use key_value::{KeyValueList, LabelChip};
pub use line_chart::{
    ChartLine, ChartReadout, ChartThreshold, LineChart, ReadoutValue, chart_color, nearest_x,
    nice_ceiling, on_scale, readout, segments, value_at,
};
pub use line_selection::{
    LineSelection, LineSelector, SelectableLines, SelectionUnit, TextPoint, selectable,
};
pub use loader::{ButtonLoading, linear, spinner};
pub use loading_bar::{LOADING_BAR_HEIGHT, LoadingBar, segment_offset};
pub use log_view::{
    DEFAULT_LOG_CAPACITY, FollowState, LogBuffer, LogEntry, LogMatch, LogSearch, LogTag, LogView,
    PushResult, find_matches, tag_color,
};
pub use log_view::{LogTopNotice, LogViewEvent};
pub use logo::Logo;
pub use multi_select::{MultiSelect, MultiSelectEvent, MultiSelection};
pub use nav_item::NavItem;
pub use nav_tree::{NavNode, NavTree, NavTreeEvent, NavTreeState};
pub use operations::{OperationNotice, Operations, OperationsList};
pub use page_header::PageHeader;
pub use panel::Panel;
pub use port_chip::PortChip;
pub use replica_bar::{ReplicaBar, ReplicaCell};
pub use resource_table::ColumnOrderEvent;
pub use resource_table::{
    CellAlign, CheckState, ResourceCell, ResourceColumn, ResourceRow, ResourceTable,
    ResourceTableEvent,
};
pub use secret_input::{FilePickField, GeneratorControls, SecretField, UntrustedText, step_length};
pub use segment_tabs::{SegmentTab, SegmentTabs};
pub use setting::{
    NumberRow, PathHandler, PathRow, ResetButton, SelectRow, SettingGroup, SettingRow, ToggleRow,
    pick_path,
};
pub use side_list::{SIDE_LIST_WIDTH, SideList, SideListItem};
pub use side_panel::{
    SIDE_PANEL_CONTENT_MIN, SIDE_PANEL_MIN_WIDTH, SidePanel, SidePanelEvent, SidePanelState,
};
pub use split_button::SplitButton;
pub use stat_card::{STAT_CARD_MIN_WIDTH, StatCard};
pub use status::{StatusBadge, StatusDot};
pub use status_bar::{StatusBar, StatusBarItem};
pub use status_counts::StatusCounts;
pub use status_cubes::{ContainerCubeState, container_tooltip};
pub use status_cubes::{StatusCube, StatusCubes};
pub use styled_spans::{SpanColor, SpanStyle, StyledSpan, layer_highlights, span_highlights};
pub use tab_strip::{TabItem, TabStrip};
pub use terminal::{ClipboardPolicy, TerminalEvent, TerminalPalette, TerminalView};
pub use traffic_legend::TrafficLegend;
pub use truncated_text::TruncatedText;
pub use usage_bar::{UsageBar, usage_share, usage_tone};
pub use window_titlebar::{WindowControls, WindowTitlebar, is_client_decorated};

/// Registers component key bindings.
pub(crate) fn init(cx: &mut gpui_kit::App) {
    resource_table::init(cx);
    line_selection::init(cx);
    graph::init(cx);
}
