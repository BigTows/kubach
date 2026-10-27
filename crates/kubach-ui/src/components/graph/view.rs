//! The interactive canvas: pan, zoom, minimap, hover, selection with its
//! chain, folding groups and search. Painting lives in [`super::paint`].

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::labels::{self, PlacedLabel};
use super::layout::{self, Layout, LayoutConfig, LayoutInput, LayoutNode, Pos, Rect};
use super::model::{GraphData, GraphTarget, Item, Visible};
use super::paint::{
    Colors, Frame, LABEL_ZOOM, MINIMAP_WIDTH, minimap_height, minimap_transform, paint_minimap,
};
use super::svg::{self, SvgPalette};
use kubach_i18n::t;

const CONTEXT: &str = "GraphView";

/// Size of a node box at zoom 1.
pub const GRAPH_NODE_WIDTH: f32 = 208.;
pub const GRAPH_NODE_HEIGHT: f32 = 52.;

const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 3.;
const ZOOM_STEP: f32 = 1.25;
/// Graphs with more items lay out on a background thread.
const SYNC_LAYOUT_LIMIT: usize = 150;
const LAYOUT_CACHE: usize = 8;
const ANIMATION: Duration = Duration::from_millis(220);
/// How close (in pixels) the pointer must be to an edge for its tooltip.
const EDGE_HIT: f32 = 5.;

/// Edge labels of the shown layout, placed by the first frame that draws
/// them (placing needs the text system).
pub(super) type LabelCache = Rc<RefCell<Option<Rc<Vec<Option<PlacedLabel>>>>>>;

actions!(
    graph_view,
    [
        ZoomIn,
        ZoomOut,
        ZoomReset,
        FitView,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        ClearSelection
    ]
);

pub(crate) fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("=", ZoomIn, context),
        KeyBinding::new("+", ZoomIn, context),
        KeyBinding::new("cmd-=", ZoomIn, context),
        KeyBinding::new("ctrl-=", ZoomIn, context),
        KeyBinding::new("-", ZoomOut, context),
        KeyBinding::new("cmd--", ZoomOut, context),
        KeyBinding::new("ctrl--", ZoomOut, context),
        KeyBinding::new("0", ZoomReset, context),
        KeyBinding::new("cmd-0", ZoomReset, context),
        KeyBinding::new("ctrl-0", ZoomReset, context),
        KeyBinding::new("f", FitView, context),
        KeyBinding::new("left", SelectLeft, context),
        KeyBinding::new("right", SelectRight, context),
        KeyBinding::new("up", SelectUp, context),
        KeyBinding::new("down", SelectDown, context),
        KeyBinding::new("escape", ClearSelection, context),
    ]);
}

/// What a [`GraphView`] reports.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphViewEvent {
    /// A box was clicked or picked with the arrow keys; `None` when the
    /// selection was cleared (click on empty canvas, Escape).
    Selected(Option<GraphTarget>),
    /// The pointer entered a box or left all of them. Hovering only
    /// highlights; it changes no selection.
    Hovered(Option<GraphTarget>),
    /// A node was double-clicked (open it).
    Activated(GraphTarget),
    /// A group was folded or unfolded by a double click.
    GroupToggled {
        group: SharedString,
        collapsed: bool,
    },
}

/// What is drawn now: data with its matching layout.
#[derive(Clone)]
pub(super) struct Shown {
    pub(super) data: Rc<GraphData>,
    pub(super) visible: Rc<Visible>,
    pub(super) layout: Arc<Layout>,
}

#[derive(Clone, Copy)]
enum Drag {
    Pan {
        start: Point<Pixels>,
        last: Point<Pixels>,
        moved: bool,
        clicks: usize,
    },
    Minimap,
}

struct Animation {
    from: (f32, Pos),
    to: (f32, Pos),
    started: Instant,
}

#[derive(Default)]
struct Search {
    query: String,
    matches: Vec<usize>,
    current: usize,
}

/// Interactive graph canvas. Feed it with [`GraphView::set_data`] and
/// subscribe to [`GraphViewEvent`]s.
pub struct GraphView {
    focus: FocusHandle,
    shown: Shown,
    laying_out: bool,
    generation: u64,
    cache: Vec<(u64, Arc<Layout>)>,
    collapsed: HashSet<SharedString>,
    known_groups: HashSet<SharedString>,
    zoom: f32,
    /// Canvas position of the graph origin.
    pan: Pos,
    animation: Option<Animation>,
    needs_fit: bool,
    canvas: Rc<Cell<Bounds<Pixels>>>,
    minimap_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    minimap: bool,
    /// The wheel pans the graph only once it has focus (a click); before
    /// that it scrolls whatever holds the graph (#460).
    wheel_needs_focus: bool,
    /// A wheel turn went past the unfocused graph: "click to pan" is shown
    /// until the pointer leaves or clicks.
    wheel_hint: bool,
    hovered: Option<usize>,
    /// Chain of the selected box: what stays lit while the rest dims.
    chain: Option<Rc<(Vec<bool>, Vec<bool>)>>,
    selected: Option<GraphTarget>,
    /// Full label of each visible edge.
    texts: Rc<Vec<Option<String>>>,
    labels: LabelCache,
    /// Edge whose full label is shown as a tooltip, and where the pointer
    /// came onto it.
    tip: Option<(usize, Point<Pixels>)>,
    search: Search,
    drag: Option<Drag>,
    _layout: Option<Task<()>>,
}

impl EventEmitter<GraphViewEvent> for GraphView {}

impl Focusable for GraphView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GraphView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            shown: Shown {
                data: Rc::default(),
                visible: Rc::default(),
                layout: Arc::default(),
            },
            laying_out: false,
            generation: 0,
            cache: Vec::new(),
            collapsed: HashSet::new(),
            known_groups: HashSet::new(),
            zoom: 1.,
            pan: Pos::new(0., 0.),
            animation: None,
            needs_fit: true,
            canvas: Rc::default(),
            minimap_bounds: Rc::default(),
            minimap: true,
            wheel_needs_focus: false,
            wheel_hint: false,
            hovered: None,
            chain: None,
            selected: None,
            texts: Rc::default(),
            labels: Rc::default(),
            tip: None,
            search: Search::default(),
            drag: None,
            _layout: None,
        }
    }

    /// Replaces the graph. Nodes that stay keep their place where possible;
    /// a structurally equal graph (only titles, tones, badges changed)
    /// reuses the layout. Large graphs lay out in the background while the
    /// previous one stays on screen.
    pub fn set_data(&mut self, data: GraphData, cx: &mut Context<Self>) {
        for group in &data.groups {
            if self.known_groups.insert(group.id.clone()) && group.collapsed {
                self.collapsed.insert(group.id.clone());
            }
        }
        self.relayout(Rc::new(data), cx);
    }

    /// The graph last passed to [`GraphView::set_data`] that is on screen.
    pub fn data(&self) -> &GraphData {
        &self.shown.data
    }

    /// The graph on screen as a standalone SVG: its current layout, with
    /// folded groups as drawn, in `palette`'s colors (#327).
    pub fn to_svg(&self, palette: &SvgPalette) -> String {
        svg::render(
            &self.shown.data,
            &self.shown.visible,
            &self.shown.layout,
            palette,
        )
    }

    /// Whether a newer graph is being laid out.
    pub fn is_laying_out(&self) -> bool {
        self.laying_out
    }

    pub fn selected(&self) -> Option<&GraphTarget> {
        self.selected.as_ref()
    }

    /// Selects a box without emitting an event; a node in a collapsed
    /// group selects the group.
    pub fn select(&mut self, target: Option<GraphTarget>, cx: &mut Context<Self>) {
        self.selected = target.map(|t| {
            self.item_of(&t)
                .map(|item| self.shown.visible.keys[item].clone())
                .unwrap_or(t)
        });
        self.update_chain();
        cx.notify();
    }

    pub fn is_group_collapsed(&self, group: &str) -> bool {
        self.collapsed.contains(group)
    }

    /// Folds or unfolds a group without emitting an event.
    pub fn set_group_collapsed(
        &mut self,
        group: SharedString,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.collapsed.insert(group)
        } else {
            self.collapsed.remove(&group)
        };
        if changed {
            let data = self.shown.data.clone();
            self.relayout(data, cx);
        }
    }

    pub fn minimap(&self) -> bool {
        self.minimap
    }

    pub fn set_minimap(&mut self, minimap: bool, cx: &mut Context<Self>) {
        self.minimap = minimap;
        cx.notify();
    }

    /// For a graph embedded in a scrolling page: the wheel and pinch reach
    /// the graph only after a click on it focuses it, and scroll the page
    /// otherwise. Off by default: a graph filling its screen takes the
    /// wheel right away.
    pub fn set_wheel_needs_focus(&mut self, needs_focus: bool, cx: &mut Context<Self>) {
        self.wheel_needs_focus = needs_focus;
        cx.notify();
    }

    /// Whether the wheel goes to the graph now, with focus as it is.
    fn takes_wheel(&self, window: &Window, cx: &App) -> bool {
        !self.wheel_needs_focus || self.focus.contains_focused(window, cx)
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.zoom_by_step(ZOOM_STEP, cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.zoom_by_step(1. / ZOOM_STEP, cx);
    }

    /// Zoom 1 around the middle of the view.
    pub fn zoom_reset(&mut self, cx: &mut Context<Self>) {
        let zoom = self.animation.as_ref().map_or(self.zoom, |a| a.to.0);
        self.zoom_by_step(1. / zoom, cx);
    }

    /// Zooms and pans so the whole graph fits.
    pub fn fit_to_view(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.fit_target() {
            self.animate_to(target, cx);
        }
    }

    /// Highlights boxes whose id or title contains `query` (any case) and
    /// centers the first. Returns the number of matches; an empty query
    /// clears the search.
    pub fn search(&mut self, query: &str, cx: &mut Context<Self>) -> usize {
        self.search.query = query.trim().to_lowercase();
        self.search.current = 0;
        self.update_matches();
        if let Some(&first) = self.search.matches.first() {
            self.center_on_item(first, cx);
        }
        cx.notify();
        self.search.matches.len()
    }

    /// Centers the next search match; returns `(position, count)`.
    pub fn search_next(&mut self, cx: &mut Context<Self>) -> Option<(usize, usize)> {
        self.step_match(1, cx)
    }

    pub fn search_previous(&mut self, cx: &mut Context<Self>) -> Option<(usize, usize)> {
        self.step_match(-1, cx)
    }

    /// Centers a box, zooming in when the view is too far out to read it.
    pub fn center_on(&mut self, target: &GraphTarget, cx: &mut Context<Self>) {
        if let Some(item) = self.item_of(target) {
            self.center_on_item(item, cx);
        }
    }

    // ---- layout ----

    fn relayout(&mut self, data: Rc<GraphData>, cx: &mut Context<Self>) {
        let visible = Rc::new(Visible::build(&data, &self.collapsed));
        let key = layout_key(&data, &visible);
        self.generation += 1;
        if let Some((_, layout)) = self.cache.iter().find(|(k, _)| *k == key) {
            let layout = layout.clone();
            self.laying_out = false;
            self._layout = None;
            self.apply(data, visible, layout, cx);
            return;
        }
        let input = layout_input(&data, &visible, Some(&self.shown));
        if visible.items.len() <= SYNC_LAYOUT_LIMIT {
            let layout = Arc::new(layout::layout(&input));
            self.remember(key, layout.clone());
            self.laying_out = false;
            self._layout = None;
            self.apply(data, visible, layout, cx);
            return;
        }
        self.laying_out = true;
        let generation = self.generation;
        let computed = cx.background_spawn(async move { layout::layout(&input) });
        self._layout = Some(cx.spawn(async move |this, cx| {
            let layout = Arc::new(computed.await);
            this.update(cx, |this, cx| {
                this.remember(key, layout.clone());
                if this.generation == generation {
                    this.laying_out = false;
                    this.apply(data, visible, layout, cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn remember(&mut self, key: u64, layout: Arc<Layout>) {
        self.cache.retain(|(k, _)| *k != key);
        self.cache.insert(0, (key, layout));
        self.cache.truncate(LAYOUT_CACHE);
    }

    fn apply(
        &mut self,
        data: Rc<GraphData>,
        visible: Rc<Visible>,
        layout: Arc<Layout>,
        cx: &mut Context<Self>,
    ) {
        // Fit a first graph, or one sharing nothing with the previous.
        let unrelated = {
            let before: HashSet<&GraphTarget> = self.shown.visible.keys.iter().collect();
            !visible.keys.iter().any(|key| before.contains(key))
        };
        self.texts = Rc::new(labels::edge_texts(&data, &visible));
        self.labels = Rc::default();
        self.shown = Shown {
            data,
            visible,
            layout,
        };
        self.hovered = None;
        self.tip = None;
        if let Some(selected) = self.selected.take() {
            self.selected = self
                .item_of(&selected)
                .map(|item| self.shown.visible.keys[item].clone());
        }
        self.update_chain();
        self.update_matches();
        if unrelated {
            self.needs_fit = true;
        }
        cx.notify();
    }

    /// Lights the chain of the selected box.
    fn update_chain(&mut self) {
        self.chain = self
            .selected
            .as_ref()
            .and_then(|s| self.item_of(s))
            .map(|item| Rc::new(self.shown.visible.chain(item)));
    }

    /// Item drawing `target`: the node, or its collapsed group.
    fn item_of(&self, target: &GraphTarget) -> Option<usize> {
        let visible = &self.shown.visible;
        if let Some(item) = visible.keys.iter().position(|k| k == target) {
            return Some(item);
        }
        match target {
            GraphTarget::Node(id) => {
                let node = self.shown.data.nodes.iter().position(|n| &n.id == id)?;
                visible.item_of_node[node]
            }
            GraphTarget::Group(_) => None,
        }
    }

    fn update_matches(&mut self) {
        let query = &self.search.query;
        let mut matches = Vec::new();
        if !query.is_empty() {
            let visible = &self.shown.visible;
            let mut seen = vec![false; visible.items.len()];
            for (ix, node) in self.shown.data.nodes.iter().enumerate() {
                let hit = node.id.to_lowercase().contains(query)
                    || node.title.to_lowercase().contains(query);
                if let (true, Some(item)) = (hit, visible.item_of_node[ix])
                    && !seen[item]
                {
                    seen[item] = true;
                    matches.push(item);
                }
            }
            for (item, kind) in visible.items.iter().enumerate() {
                if let Item::Group(g) = kind
                    && !seen[item]
                    && visible.groups[*g].title.to_lowercase().contains(query)
                {
                    matches.push(item);
                }
            }
            let layout = &self.shown.layout;
            // Nodes before folded groups, then left to right, top to bottom.
            let folded = |item: usize| matches!(visible.items[item], Item::Group(_));
            matches.sort_by(|&a, &b| {
                let (ra, rb) = (layout.nodes[a], layout.nodes[b]);
                folded(a)
                    .cmp(&folded(b))
                    .then(ra.x.total_cmp(&rb.x))
                    .then(ra.y.total_cmp(&rb.y))
            });
        }
        self.search.current = self.search.current.min(matches.len().saturating_sub(1));
        self.search.matches = matches;
    }

    fn step_match(&mut self, step: isize, cx: &mut Context<Self>) -> Option<(usize, usize)> {
        let count = self.search.matches.len();
        if count == 0 {
            return None;
        }
        let current = (self.search.current as isize + step).rem_euclid(count as isize) as usize;
        self.search.current = current;
        self.center_on_item(self.search.matches[current], cx);
        cx.notify();
        Some((current, count))
    }

    // ---- viewport ----

    fn canvas_size(&self) -> (f32, f32) {
        let size = self.canvas.get().size;
        (f32::from(size.width), f32::from(size.height))
    }

    fn fit_target(&self) -> Option<(f32, Pos)> {
        let (w, h) = self.canvas_size();
        let content = self.shown.layout.bounds;
        if w <= 0. || h <= 0. || self.shown.visible.items.is_empty() {
            return None;
        }
        let margin = 32.;
        let zoom = ((w - 2. * margin) / content.w.max(1.))
            .min((h - 2. * margin) / content.h.max(1.))
            .clamp(MIN_ZOOM, 1.);
        let center = content.center();
        Some((
            zoom,
            Pos::new(w / 2. - center.x * zoom, h / 2. - center.y * zoom),
        ))
    }

    fn zoom_by_step(&mut self, factor: f32, cx: &mut Context<Self>) {
        let (w, h) = self.canvas_size();
        let (zoom, pan) = self
            .animation
            .as_ref()
            .map_or((self.zoom, self.pan), |a| a.to);
        let target = zoomed(zoom, pan, factor, Pos::new(w / 2., h / 2.));
        self.animate_to(target, cx);
    }

    fn zoom_at(&mut self, factor: f32, at: Pos, cx: &mut Context<Self>) {
        self.animation = None;
        (self.zoom, self.pan) = zoomed(self.zoom, self.pan, factor, at);
        cx.notify();
    }

    fn center_on_item(&mut self, item: usize, cx: &mut Context<Self>) {
        let Some(rect) = self.shown.layout.nodes.get(item).copied() else {
            return;
        };
        let (w, h) = self.canvas_size();
        let zoom = self.zoom.max(0.8);
        let c = rect.center();
        self.animate_to(
            (zoom, Pos::new(w / 2. - c.x * zoom, h / 2. - c.y * zoom)),
            cx,
        );
    }

    /// Centers `item` only when it is not fully in view.
    fn reveal(&mut self, item: usize, cx: &mut Context<Self>) {
        let Some(rect) = self.shown.layout.nodes.get(item).copied() else {
            return;
        };
        let (w, h) = self.canvas_size();
        let view = Rect::new(
            -self.pan.x / self.zoom,
            -self.pan.y / self.zoom,
            w / self.zoom,
            h / self.zoom,
        );
        let inside = view.contains(Pos::new(rect.x, rect.y))
            && view.contains(Pos::new(rect.right(), rect.bottom()));
        if !inside {
            self.center_on_item(item, cx);
        }
    }

    fn animate_to(&mut self, to: (f32, Pos), cx: &mut Context<Self>) {
        self.animation = Some(Animation {
            from: (self.zoom, self.pan),
            to,
            started: Instant::now(),
        });
        cx.notify();
    }

    /// Advances the running animation; true while it continues.
    fn step_animation(&mut self) -> bool {
        let Some(animation) = &self.animation else {
            return false;
        };
        let t = (animation.started.elapsed().as_secs_f32() / ANIMATION.as_secs_f32()).min(1.);
        let ease = 1. - (1. - t).powi(3);
        let (z0, p0) = animation.from;
        let (z1, p1) = animation.to;
        // Interpolate the zoom geometrically and the graph point at the
        // canvas center linearly, so the motion does not swing sideways.
        let zoom = z0 * (z1 / z0).powf(ease);
        let (w, h) = self.canvas_size();
        let center = |z: f32, p: Pos| Pos::new((w / 2. - p.x) / z, (h / 2. - p.y) / z);
        let (c0, c1) = (center(z0, p0), center(z1, p1));
        let c = Pos::new(c0.x + (c1.x - c0.x) * ease, c0.y + (c1.y - c0.y) * ease);
        self.zoom = zoom;
        self.pan = Pos::new(w / 2. - c.x * zoom, h / 2. - c.y * zoom);
        if t >= 1. {
            self.animation = None;
            false
        } else {
            true
        }
    }

    /// Graph point under a window position.
    fn to_graph(&self, position: Point<Pixels>) -> Pos {
        let origin = self.canvas.get().origin;
        Pos::new(
            (f32::from(position.x - origin.x) - self.pan.x) / self.zoom,
            (f32::from(position.y - origin.y) - self.pan.y) / self.zoom,
        )
    }

    fn item_at(&self, point: Pos) -> Option<usize> {
        self.shown
            .layout
            .nodes
            .iter()
            .rposition(|rect| rect.contains(point))
    }

    /// Expanded group whose title row is at `point`.
    fn group_header_at(&self, point: Pos) -> Option<usize> {
        let header = LayoutConfig::default().group_header;
        self.shown
            .layout
            .groups
            .iter()
            .position(|rect| rect.is_some_and(|r| r.contains(point) && point.y <= r.y + header))
    }

    // ---- input ----

    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if std::mem::take(&mut self.wheel_hint) {
            cx.notify();
        }
        self.animation = None;
        self.tip = None;
        if self.minimap
            && let Some(bounds) = self.minimap_bounds.get()
            && bounds.contains(&event.position)
        {
            self.drag = Some(Drag::Minimap);
            self.minimap_to(event.position, cx);
            return;
        }
        self.drag = Some(Drag::Pan {
            start: event.position,
            last: event.position,
            moved: false,
            clicks: event.click_count,
        });
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        match &mut self.drag {
            Some(Drag::Pan {
                start, last, moved, ..
            }) => {
                let delta = position - *last;
                *last = position;
                let distance = position - *start;
                if !*moved && f32::from(distance.x).abs() + f32::from(distance.y).abs() < 3. {
                    return;
                }
                if !*moved {
                    // Count the slop the pointer moved before the drag began.
                    *moved = true;
                    self.pan.x += f32::from(distance.x);
                    self.pan.y += f32::from(distance.y);
                } else {
                    self.pan.x += f32::from(delta.x);
                    self.pan.y += f32::from(delta.y);
                }
                cx.notify();
            }
            Some(Drag::Minimap) => self.minimap_to(position, cx),
            None => {}
        }
    }

    fn release(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        cx.notify();
        let Drag::Pan {
            moved: false,
            clicks,
            ..
        } = drag
        else {
            return;
        };
        let point = self.to_graph(position);
        let item = self.item_at(point);
        if clicks >= 2 {
            let group = match item.map(|i| self.shown.visible.items[i]) {
                Some(Item::Group(g)) => Some(g),
                Some(Item::Node(_)) => {
                    if let Some(item) = item {
                        let key = self.shown.visible.keys[item].clone();
                        cx.emit(GraphViewEvent::Activated(key));
                    }
                    None
                }
                None => self.group_header_at(point),
            };
            if let Some(g) = group {
                self.toggle_group(g, cx);
            }
            return;
        }
        match item {
            // A second click on the selected box clears the selection.
            Some(item) if self.selected.as_ref().and_then(|s| self.item_of(s)) == Some(item) => {
                self.clear_selection(cx)
            }
            Some(item) => self.pick(item, cx),
            None if self.group_header_at(point).is_some() => {}
            None => self.clear_selection(cx),
        }
    }

    fn pick(&mut self, item: usize, cx: &mut Context<Self>) {
        let key = self.shown.visible.keys[item].clone();
        self.selected = Some(key.clone());
        self.update_chain();
        cx.emit(GraphViewEvent::Selected(Some(key)));
        cx.notify();
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            self.chain = None;
            cx.emit(GraphViewEvent::Selected(None));
            cx.notify();
        }
    }

    fn toggle_group(&mut self, group: usize, cx: &mut Context<Self>) {
        let id = self.shown.visible.groups[group].id.clone();
        let collapsed = !self.collapsed.contains(&id);
        if collapsed {
            self.collapsed.insert(id.clone());
        } else {
            self.collapsed.remove(&id);
        }
        cx.emit(GraphViewEvent::GroupToggled {
            group: id,
            collapsed,
        });
        let data = self.shown.data.clone();
        self.relayout(data, cx);
    }

    fn hover_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            return;
        }
        let over_minimap = self.minimap
            && self
                .minimap_bounds
                .get()
                .is_some_and(|b| b.contains(&position));
        let point = self.to_graph(position);
        let item = if over_minimap {
            None
        } else {
            self.item_at(point)
        };
        let tip = match item {
            None if !over_minimap => self.tip_at(point),
            _ => None,
        };
        let tip_changed = self.tip.map(|(e, _)| e) != tip;
        if tip_changed {
            self.tip = tip.map(|e| (e, position));
        }
        if !self.set_hovered(item, cx) && tip_changed {
            cx.notify();
        }
    }

    /// Edge at `point` whose full label is not on screen: its label is
    /// shortened, left out for lack of room, or too small at this zoom.
    fn tip_at(&self, point: Pos) -> Option<usize> {
        let placed = self.labels.borrow().clone();
        let readable = self.zoom >= LABEL_ZOOM;
        if readable
            && let Some(placed) = &placed
            && let Some(e) = labels::label_at(placed, point)
        {
            return placed[e].as_ref().is_some_and(|l| l.shortened).then_some(e);
        }
        let reach = EDGE_HIT / self.zoom;
        let routes = &self.shown.layout.edges;
        let (e, _) = self
            .texts
            .iter()
            .enumerate()
            .filter(|(_, text)| text.is_some())
            .filter_map(|(e, _)| {
                let route = routes.get(e)?.as_ref()?;
                let b = route.bounds;
                let near = Rect::new(b.x - reach, b.y - reach, b.w + 2. * reach, b.h + 2. * reach);
                if !near.contains(point) {
                    return None;
                }
                let distance = labels::distance_to_route(route, point);
                (distance <= reach).then_some((e, distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        let shown_whole = readable
            && placed
                .as_ref()
                .and_then(|p| p.get(e).cloned().flatten())
                .is_some_and(|l| !l.shortened);
        (!shown_whole).then_some(e)
    }

    /// Returns whether the hovered box changed (and the view notified).
    fn set_hovered(&mut self, item: Option<usize>, cx: &mut Context<Self>) -> bool {
        if self.hovered == item {
            return false;
        }
        self.hovered = item;
        cx.emit(GraphViewEvent::Hovered(
            item.map(|item| self.shown.visible.keys[item].clone()),
        ));
        cx.notify();
        true
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Pixels(p) => Pos::new(f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => Pos::new(l.x * 24., l.y * 24.),
        };
        if event.modifiers.platform || event.modifiers.control {
            let at = self.to_canvas(event.position);
            self.zoom_at((delta.y * 0.01).exp(), at, cx);
        } else {
            self.animation = None;
            self.pan.x += delta.x;
            self.pan.y += delta.y;
            cx.notify();
        }
    }

    fn to_canvas(&self, position: Point<Pixels>) -> Pos {
        let origin = self.canvas.get().origin;
        Pos::new(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    fn minimap_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(bounds) = self.minimap_bounds.get() else {
            return;
        };
        let map = minimap_transform(&self.shown.layout.bounds, bounds);
        let point = map.to_graph(position);
        let (w, h) = self.canvas_size();
        self.animation = None;
        self.pan = Pos::new(w / 2. - point.x * self.zoom, h / 2. - point.y * self.zoom);
        cx.notify();
    }

    /// Selection step with the arrow keys.
    fn step_selection(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let visible = &self.shown.visible;
        let layout = &self.shown.layout;
        if visible.items.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|s| self.item_of(s));
        let next = match current {
            None => layout.order.first().and_then(|c| c.first().copied()),
            Some(item) => {
                let y = layout.nodes[item].center().y;
                let nearest = |candidates: Vec<usize>| {
                    candidates.into_iter().min_by(|&a, &b| {
                        let da = (layout.nodes[a].center().y - y).abs();
                        let db = (layout.nodes[b].center().y - y).abs();
                        da.total_cmp(&db)
                    })
                };
                match direction {
                    Direction::Right => nearest(
                        visible
                            .edges
                            .iter()
                            .filter(|e| e.from == item)
                            .map(|e| e.to)
                            .collect(),
                    ),
                    Direction::Left => nearest(
                        visible
                            .edges
                            .iter()
                            .filter(|e| e.to == item)
                            .map(|e| e.from)
                            .collect(),
                    ),
                    Direction::Up | Direction::Down => {
                        let column = &layout.order[layout.layers[item]];
                        let at = column.iter().position(|&v| v == item).unwrap_or(0);
                        if direction == Direction::Up {
                            at.checked_sub(1).map(|i| column[i])
                        } else {
                            column.get(at + 1).copied()
                        }
                    }
                }
            }
        };
        if let Some(next) = next {
            self.pick(next, cx);
            self.reveal(next, cx);
        }
    }

    // ---- painting ----

    fn frame(&self, cx: &App) -> Frame {
        let theme = cx.theme();
        let visible = &self.shown.visible;
        let mut matched = vec![false; visible.items.len()];
        for &m in &self.search.matches {
            matched[m] = true;
        }
        Frame {
            shown: self.shown.clone(),
            zoom: self.zoom,
            pan: self.pan,
            hovered: self.hovered,
            chain: self.chain.clone(),
            texts: self.texts.clone(),
            labels: self.labels.clone(),
            selected: self.selected.as_ref().and_then(|s| self.item_of(s)),
            searching: !self.search.query.is_empty(),
            matched,
            current_match: self.search.matches.get(self.search.current).copied(),
            colors: Colors {
                node: theme.popover,
                node_border: theme.border,
                node_hover: theme.ring,
                selected: theme.foreground,
                text: theme.foreground,
                muted: theme.muted_foreground,
                edge: theme.ring,
                edge_hot: theme.info,
                edge_dim: theme.border,
                group: theme.group_box,
                group_border: theme.border,
                grid: theme.chart_grid,
                matched: theme.selection,
                success: theme.success,
                warning: theme.warning,
                danger: theme.danger,
                info: theme.info,
                traffic: crate::theme::traffic_palette(cx),
            },
            font: theme.font_family.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Zoom and pan after zooming by `factor` around canvas point `at`.
fn zoomed(zoom: f32, pan: Pos, factor: f32, at: Pos) -> (f32, Pos) {
    let new = (zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let g = Pos::new((at.x - pan.x) / zoom, (at.y - pan.y) / zoom);
    (new, Pos::new(at.x - g.x * new, at.y - g.y * new))
}

/// Hash of everything the layout depends on.
fn layout_key(data: &GraphData, visible: &Visible) -> u64 {
    let mut hasher = DefaultHasher::new();
    visible.keys.hash(&mut hasher);
    visible.band.hash(&mut hasher);
    visible.groups.len().hash(&mut hasher);
    for item in &visible.items {
        layer_hint(data, visible, *item).hash(&mut hasher);
    }
    for edge in &visible.edges {
        (edge.from, edge.to).hash(&mut hasher);
    }
    hasher.finish()
}

fn layer_hint(data: &GraphData, visible: &Visible, item: Item) -> Option<usize> {
    match item {
        Item::Node(ix) => data.nodes[ix].layer,
        Item::Group(g) => visible.groups[g]
            .members
            .iter()
            .filter_map(|&m| data.nodes[m].layer)
            .min(),
    }
}

/// Layout input of `visible`; `previous` keeps what stays where it was.
pub(super) fn layout_input(
    data: &GraphData,
    visible: &Visible,
    previous: Option<&Shown>,
) -> LayoutInput {
    let before: HashMap<&GraphTarget, Pos> = previous
        .into_iter()
        .flat_map(|previous| previous.visible.keys.iter().zip(&previous.layout.nodes))
        .map(|(key, rect)| (key, Pos::new(rect.x, rect.y)))
        .collect();
    let nodes = visible
        .items
        .iter()
        .enumerate()
        .map(|(ix, &item)| LayoutNode {
            width: GRAPH_NODE_WIDTH,
            height: GRAPH_NODE_HEIGHT,
            layer: layer_hint(data, visible, item),
            group: visible.band[ix],
            previous: before.get(&visible.keys[ix]).copied(),
        })
        .collect();
    // Bends of edges that stay, by their ends.
    let routes: HashMap<(&GraphTarget, &GraphTarget), Vec<f32>> = previous
        .into_iter()
        .flat_map(|previous| {
            previous
                .visible
                .edges
                .iter()
                .zip(&previous.layout.edges)
                .map(move |(edge, route)| (previous, edge, route))
        })
        .filter_map(|(previous, edge, route)| {
            let bends = route.as_ref()?.bends();
            let keys = &previous.visible.keys;
            Some(((&keys[edge.from], &keys[edge.to]), bends))
        })
        .collect();
    let bends = visible
        .edges
        .iter()
        .map(|e| {
            routes
                .get(&(&visible.keys[e.from], &visible.keys[e.to]))
                .cloned()
        })
        .collect();
    let large = visible.items.len() > 600;
    LayoutInput {
        nodes,
        bends,
        edges: visible.edges.iter().map(|e| (e.from, e.to)).collect(),
        groups: visible.groups.len(),
        config: LayoutConfig {
            sweeps: if large { 4 } else { 8 },
            ..LayoutConfig::default()
        },
    }
}

impl Render for GraphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_fit
            && let Some((zoom, pan)) = self.fit_target()
        {
            self.needs_fit = false;
            self.zoom = zoom;
            self.pan = pan;
        }
        if self.step_animation() {
            window.request_animation_frame();
        }
        let frame = Rc::new(self.frame(cx));
        let theme = cx.theme();
        let canvas_cell = self.canvas.clone();
        let minimap_cell = self.minimap_bounds.clone();
        let needs_fit = self.needs_fit;
        let view = cx.entity().downgrade();
        let cursor = match (self.drag, self.hovered) {
            (Some(Drag::Pan { moved: true, .. }), _) => CursorStyle::ClosedHand,
            (_, Some(_)) => CursorStyle::PointingHand,
            _ => CursorStyle::Arrow,
        };
        let zoom_label = format!("{:.0}%", self.zoom * 100.);
        let content = self.shown.layout.bounds;
        let show_minimap = self.minimap && !self.shown.visible.items.is_empty();
        let map_size = size(px(MINIMAP_WIDTH), px(minimap_height(&content)));
        if !show_minimap {
            self.minimap_bounds.set(None);
        }

        let paint_frame = frame.clone();
        let graph = canvas(
            move |bounds, window, _| {
                if canvas_cell.get() != bounds {
                    canvas_cell.set(bounds);
                    if needs_fit {
                        window.request_animation_frame();
                    }
                }
            },
            move |bounds, _, window, cx| {
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    paint_frame.paint(bounds, window, cx);
                });
                // Window-wide, so a drag continues outside the canvas.
                let moves = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        moves
                            .update(cx, |this, cx| {
                                if this.drag.is_some() {
                                    this.drag_to(event.position, cx);
                                }
                            })
                            .ok();
                    }
                });
                let ups = view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        ups.update(cx, |this, cx| this.release(event.position, cx))
                            .ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();

        let toolbar = div()
            .absolute()
            .top_2()
            .right_2()
            .flex()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("graph-zoom-out")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Minus)
                    .tooltip(format!("{}  −", t!("ui-graph-zoom-out")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_out(cx))),
            )
            .child(
                Button::new("graph-zoom-reset")
                    .ghost()
                    .xsmall()
                    .label(zoom_label)
                    .tooltip(format!("{}  0", t!("ui-graph-actual-size")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_reset(cx))),
            )
            .child(
                Button::new("graph-zoom-in")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Plus)
                    .tooltip(format!("{}  +", t!("ui-graph-zoom-in")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_in(cx))),
            )
            .child(
                Button::new("graph-fit")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Maximize)
                    .tooltip(format!("{}  F", t!("ui-graph-fit")))
                    .on_click(cx.listener(|this, _, _, cx| this.fit_to_view(cx))),
            )
            .child(
                Button::new("graph-minimap")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Map)
                    .selected(self.minimap)
                    .tooltip(t!("ui-graph-minimap"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let minimap = !this.minimap;
                        this.set_minimap(minimap, cx);
                    })),
            );

        // The full text of a shortened or hidden edge label.
        let tooltip = self.tip.and_then(|(e, at)| {
            let text = self.texts.get(e)?.clone()?;
            let origin = self.canvas.get().origin;
            Some(
                div()
                    .absolute()
                    .left(at.x - origin.x + px(12.))
                    .top(at.y - origin.y + px(14.))
                    .max_w(px(360.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .text_xs()
                    .text_color(theme.popover_foreground)
                    .child(text),
            )
        });
        let minimap = show_minimap.then(|| {
            let frame = frame.clone();
            let canvas_bounds = self.canvas.clone();
            div()
                .absolute()
                .bottom_2()
                .right_2()
                .w(map_size.width)
                .h(map_size.height)
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .overflow_hidden()
                .child(
                    canvas(
                        move |bounds, _, _| minimap_cell.set(Some(bounds)),
                        move |bounds, _, window, _| {
                            paint_minimap(&frame, bounds, canvas_bounds.get(), window);
                        },
                    )
                    .size_full(),
                )
        });

        div()
            .id("graph-view")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor(cursor)
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_in(cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_out(cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom_reset(cx)))
            .on_action(cx.listener(|this, _: &FitView, _, cx| this.fit_to_view(cx)))
            .on_action(
                cx.listener(|this, _: &SelectLeft, _, cx| this.step_selection(Direction::Left, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectRight, _, cx| {
                    this.step_selection(Direction::Right, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectUp, _, cx| this.step_selection(Direction::Up, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectDown, _, cx| this.step_selection(Direction::Down, cx)),
            )
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                if this.selected.is_some() {
                    this.clear_selection(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.press(event, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.hover_at(event.position, cx)
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    let tip = this.tip.take().is_some() | std::mem::take(&mut this.wheel_hint);
                    if !this.set_hovered(None, cx) && tip {
                        cx.notify();
                    }
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                if !this.takes_wheel(window, cx) {
                    // Not focused yet: the page holding the graph scrolls.
                    if !this.wheel_hint {
                        this.wheel_hint = true;
                        cx.notify();
                    }
                    return;
                }
                // The graph pans or zooms; the panel around it stays put.
                cx.stop_propagation();
                this.scroll(event, cx)
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                if !this.takes_wheel(window, cx) {
                    return;
                }
                let at = this.to_canvas(event.position);
                this.zoom_at(1. + event.delta, at, cx);
            }))
            .child(graph)
            .children(tooltip)
            .child(toolbar)
            .children(minimap)
            .when(self.wheel_hint, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(theme.popover)
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t!("ui-graph-click-to-pan")),
                        ),
                )
            })
            .when(self.laying_out, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_2()
                        .left_2()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!("ui-graph-laying-out")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_ZOOM, MIN_ZOOM, Pos, zoomed};

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let at = Pos::new(300., 200.);
        let (zoom, pan) = zoomed(1., Pos::new(40., -10.), 2., at);
        assert_eq!(zoom, 2.);
        let before = Pos::new((at.x - 40.) / 1., (at.y + 10.) / 1.);
        let after = Pos::new((at.x - pan.x) / zoom, (at.y - pan.y) / zoom);
        assert!((before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3);
        assert_eq!(zoomed(1., Pos::default(), 100., at).0, MAX_ZOOM);
        assert_eq!(zoomed(1., Pos::default(), 0., at).0, MIN_ZOOM);
    }
}
