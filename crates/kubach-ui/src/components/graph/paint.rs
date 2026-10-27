//! Painting of a [`GraphView`](super::GraphView) frame: everything is drawn
//! directly (quads, paths and shaped text), and only what lies in the
//! viewport, with less detail the farther out the view is.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::*;

use super::labels::{self, LABEL_FONT, LABEL_PAD, PlacedLabel};
use super::layout::{self, Pos, Rect};
use super::model::{EdgeStyle, Item};
use super::view::{LabelCache, Shown};
use crate::tokens::{TINT_OPACITY, Tone};
use kubach_i18n::t;

/// Below this zoom nodes are drawn without text.
const TEXT_ZOOM: f32 = 0.4;
/// Below this zoom nodes show only their title (icons, subtitles and
/// badges would be too small to read and cost shaping on every zoom step).
const DETAIL_ZOOM: f32 = 0.55;
/// Edge labels show from this zoom.
pub(super) const LABEL_ZOOM: f32 = 0.6;
/// Below this zoom edges are flattened coarsely.
const STRAIGHT_ZOOM: f32 = 0.3;
/// Dashes per frame before dash patterns stretch.
const DASH_BUDGET: f32 = 12_000.;
pub(super) const MINIMAP_WIDTH: f32 = 200.;

/// Theme colors used while painting.
#[derive(Clone)]
pub(super) struct Colors {
    pub(super) node: Hsla,
    pub(super) node_border: Hsla,
    pub(super) node_hover: Hsla,
    pub(super) selected: Hsla,
    pub(super) text: Hsla,
    pub(super) muted: Hsla,
    pub(super) edge: Hsla,
    pub(super) edge_hot: Hsla,
    pub(super) edge_dim: Hsla,
    pub(super) group: Hsla,
    pub(super) group_border: Hsla,
    pub(super) grid: Hsla,
    pub(super) matched: Hsla,
    pub(super) success: Hsla,
    pub(super) warning: Hsla,
    pub(super) danger: Hsla,
    pub(super) info: Hsla,
    /// By [`TrafficLevel`](super::model::TrafficLevel).
    pub(super) traffic: [Hsla; 5],
}

impl Colors {
    fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Neutral => self.muted,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
            Tone::Info => self.info,
        }
    }
}

/// Snapshot of the view handed to the paint closure.
pub(super) struct Frame {
    pub(super) shown: Shown,
    pub(super) zoom: f32,
    pub(super) pan: Pos,
    pub(super) hovered: Option<usize>,
    /// Chain of the selected box.
    pub(super) chain: Option<Rc<(Vec<bool>, Vec<bool>)>>,
    pub(super) texts: Rc<Vec<Option<String>>>,
    pub(super) labels: LabelCache,
    pub(super) selected: Option<usize>,
    pub(super) searching: bool,
    pub(super) matched: Vec<bool>,
    pub(super) current_match: Option<usize>,
    pub(super) colors: Colors,
    pub(super) font: SharedString,
}

impl Frame {
    fn screen(&self, origin: Point<Pixels>, p: Pos) -> Point<Pixels> {
        point(
            origin.x + px(self.pan.x + p.x * self.zoom),
            origin.y + px(self.pan.y + p.y * self.zoom),
        )
    }

    fn screen_rect(&self, origin: Point<Pixels>, r: &Rect) -> Bounds<Pixels> {
        Bounds::new(
            self.screen(origin, Pos::new(r.x, r.y)),
            size(px(r.w * self.zoom), px(r.h * self.zoom)),
        )
    }

    /// Visible graph area, with a margin for strokes and labels.
    fn viewport(&self, bounds: &Bounds<Pixels>) -> Rect {
        let margin = 24. / self.zoom;
        Rect::new(
            -self.pan.x / self.zoom - margin,
            -self.pan.y / self.zoom - margin,
            f32::from(bounds.size.width) / self.zoom + 2. * margin,
            f32::from(bounds.size.height) / self.zoom + 2. * margin,
        )
    }

    /// Edge labels of the layout, placed on first use.
    fn placed_labels(&self, window: &Window) -> Rc<Vec<Option<PlacedLabel>>> {
        if let Some(placed) = self.labels.borrow().clone() {
            return placed;
        }
        let layout = &self.shown.layout;
        let placed = Rc::new(labels::place_labels(
            &layout.nodes,
            &layout.edges,
            &self.texts,
            &mut |text| {
                let text = SharedString::from(text.to_owned());
                let shaped = shape(
                    &text,
                    px(LABEL_FONT),
                    FontWeight::NORMAL,
                    self.colors.muted,
                    &self.font,
                    window,
                );
                // A little slack for hinting at other zooms.
                f32::from(shaped.width) + 1.
            },
        ));
        *self.labels.borrow_mut() = Some(placed.clone());
        placed
    }

    /// Whether an item is faded out by the selection's chain or a search.
    pub(super) fn dimmed(&self, item: usize) -> bool {
        match &self.chain {
            Some(chain) => !chain.0[item],
            None => self.searching && !self.matched[item],
        }
    }

    pub(super) fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let origin = bounds.origin;
        let view = self.viewport(&bounds);
        self.paint_grid(bounds, window);
        self.paint_groups(origin, &view, window, cx);
        self.paint_edges(origin, &view, window, cx);
        self.paint_nodes(origin, &view, window, cx);
    }

    fn paint_grid(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let mut step = 24. * self.zoom;
        while step < 16. {
            step *= 2.;
        }
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let dot = 1.5;
        let mut y = self.pan.y.rem_euclid(step);
        while y < h {
            let mut x = self.pan.x.rem_euclid(step);
            while x < w {
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(x - dot / 2.), px(y - dot / 2.)),
                        size(px(dot), px(dot)),
                    ),
                    self.colors.grid,
                ));
                x += step;
            }
            y += step;
        }
    }

    fn paint_groups(&self, origin: Point<Pixels>, view: &Rect, window: &mut Window, cx: &mut App) {
        let visible = &self.shown.visible;
        let z = self.zoom;
        let c = &self.colors;
        let faded = self.chain.is_some() || self.searching;
        for (g, rect) in self.shown.layout.groups.iter().enumerate() {
            let Some(rect) = rect.filter(|r| r.intersects(view)) else {
                continue;
            };
            let bounds = self.screen_rect(origin, &rect);
            window.paint_quad(quad(
                bounds,
                px(10. * z),
                c.group,
                px(1.),
                c.group_border,
                BorderStyle::Solid,
            ));
            let group = &visible.groups[g];
            // Readable at any zoom: the title keeps a minimum size.
            let size = (11. * z).max(10.);
            let pad = 12. * z;
            let title = format!("{}  ·  {}", group.title, group.members.len());
            let color = if faded { c.muted.opacity(0.6) } else { c.muted };
            paint_text(
                &title.into(),
                bounds.origin + point(px(pad), px((28. * z - size * 1.3) / 2.).max(px(2.))),
                px(size),
                FontWeight::MEDIUM,
                color,
                bounds.size.width - px(2. * pad),
                &self.font,
                window,
                cx,
            );
        }
    }

    fn paint_edges(&self, origin: Point<Pixels>, view: &Rect, window: &mut Window, cx: &mut App) {
        let z = self.zoom;
        let c = &self.colors;
        let visible = &self.shown.visible;
        let data = &self.shown.data;
        let mut batches: Vec<StrokeBatch> = Vec::new();
        let mut labels: Vec<&PlacedLabel> = Vec::new();
        let placed = (z >= LABEL_ZOOM).then(|| self.placed_labels(window));
        let arrow = (6. * z).clamp(2.5, 10.);
        let mut pieces = Vec::new();
        for (e, edge) in visible.edges.iter().enumerate() {
            let Some(Some(route)) = self.shown.layout.edges.get(e) else {
                continue;
            };
            if !route.bounds.intersects(view) {
                continue;
            }
            let first = &data.edges[edge.edges[0]];
            // Merged edges keep a tone only when all of them share it.
            let tone = first.tone;
            let tone = if edge.edges.iter().all(|&i| data.edges[i].tone == tone) {
                tone
            } else {
                Tone::Neutral
            };
            let weight = edge
                .edges
                .iter()
                .filter_map(|&i| data.edges[i].weight)
                .fold(None, |acc: Option<f32>, w| {
                    Some(acc.map_or(w, |a| a.max(w)))
                });
            // Merged traffic edges show the worst level.
            let traffic = edge
                .edges
                .iter()
                .filter_map(|&i| data.edges[i].traffic)
                .max();
            // Lit: on the selection's chain, or (outside a dimmed part)
            // an edge of the hovered box.
            let chained = self.chain.as_ref().map(|chain| chain.1[e]);
            let touched = self.hovered.is_some_and(|h| edge.from == h || edge.to == h);
            let (lit, dim) = match chained {
                Some(on) => (on, !on),
                None => (touched, self.searching && !touched),
            };
            let base = match (traffic, tone) {
                (Some(level), _) => c.traffic[level.index()],
                (None, Tone::Neutral) => c.edge,
                (None, tone) => c.tone(tone),
            };
            let color = if dim {
                c.edge_dim
            } else if lit && tone == Tone::Neutral && traffic.is_none() {
                c.edge_hot
            } else {
                base
            };
            let mut width = 1. + 3. * weight.unwrap_or(0.);
            if lit {
                width += 0.5;
            }
            let width = (width * 2.).round() / 2.;
            // Dashes are unreadable far out.
            let style = if z < 0.35 {
                EdgeStyle::Solid
            } else {
                first.style
            };
            let hot = lit;
            let ix = match batches.iter().position(|b| {
                b.color == color && b.width == width && b.style == style && b.hot == hot
            }) {
                Some(ix) => ix,
                None => {
                    batches.push(StrokeBatch::new(color, width, style, hot, z));
                    batches.len() - 1
                }
            };
            let batch = &mut batches[ix];
            // Only the pieces in view: a long curve would otherwise be
            // tessellated (and dashed) far off screen.
            pieces.clear();
            for segment in &route.segments {
                layout::clip_bezier(segment, view, &mut pieces);
            }
            // Far out curves are a few pixels tall: fewer segments look
            // the same and cost less to tessellate.
            let max_steps = if z < 0.15 {
                1
            } else if z < STRAIGHT_ZOOM {
                4
            } else {
                16
            };
            let (ox, oy) = (
                f32::from(origin.x) + self.pan.x,
                f32::from(origin.y) + self.pan.y,
            );
            for piece in &pieces {
                let curve = piece.map(|p| Pos::new(ox + p.x * z, oy + p.y * z));
                batch.curve(curve, max_steps);
            }
            if z >= 0.2 && view.contains(route.end()) {
                let end = route.end();
                let dir = route.end_direction();
                let tip = self.screen(origin, end);
                let back = point(tip.x - px(dir.x * arrow), tip.y - px(dir.y * arrow));
                let (nx, ny) = (-dir.y * arrow * 0.55, dir.x * arrow * 0.55);
                batch.arrow([
                    tip,
                    point(back.x + px(nx), back.y + px(ny)),
                    point(back.x - px(nx), back.y - px(ny)),
                ]);
            }
            if let Some(Some(label)) = placed.as_ref().map(|p| p.get(e).and_then(Option::as_ref))
                && (hot || self.chain.is_none() && !self.searching)
                && label.rect.intersects(view)
            {
                labels.push(label);
            }
        }
        // Many dashed edges in view: somewhat longer dashes, and past that
        // solid lines, rather than a slow frame.
        let dashes: f32 = batches.iter().map(StrokeBatch::dash_count).sum();
        let dash_scale = (dashes / DASH_BUDGET).max(1.);
        let dash_scale = if dash_scale > 2. {
            f32::INFINITY
        } else {
            dash_scale
        };
        // Emphasized edges on top.
        batches.sort_by_key(|b| b.hot);
        for batch in batches {
            let color = batch.color;
            for path in batch.finish(dash_scale) {
                window.paint_path(path, color);
            }
        }
        if labels.len() > 400 {
            return;
        }
        // Placed clear of the nodes: drawn over the edges, under nothing.
        let size = px(LABEL_FONT * z);
        for label in labels {
            let pill = self.screen_rect(origin, &label.rect);
            let shaped = shape(
                &label.text.clone().into(),
                size,
                FontWeight::NORMAL,
                c.muted,
                &self.font,
                window,
            );
            let center_y = pill.origin.y + pill.size.height / 2.;
            window.paint_quad(quad(
                pill,
                px(4. * z),
                c.node,
                px(1.),
                c.node_border,
                BorderStyle::Solid,
            ));
            shaped
                .paint(
                    point(pill.origin.x + px(LABEL_PAD * z), center_y - size * 0.65),
                    size * 1.3,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
        }
    }

    fn paint_nodes(&self, origin: Point<Pixels>, view: &Rect, window: &mut Window, cx: &mut App) {
        let z = self.zoom;
        let c = &self.colors;
        let visible = &self.shown.visible;
        let data = &self.shown.data;
        let layers_icon = Lucide::FolderClosed.path();
        for (item, rect) in self.shown.layout.nodes.iter().enumerate() {
            if !rect.intersects(view) {
                continue;
            }
            let bounds = self.screen_rect(origin, rect);
            let alpha = if self.dimmed(item) { 0.3 } else { 1. };
            let radius = px(6. * z);
            let (title, subtitle, icon, tone, badges, count) = match visible.items[item] {
                Item::Node(ix) => {
                    let node = &data.nodes[ix];
                    (
                        node.title.clone(),
                        node.subtitle.clone(),
                        node.icon.clone(),
                        node.tone,
                        node.badges.as_slice(),
                        None,
                    )
                }
                Item::Group(g) => {
                    let group = &visible.groups[g];
                    (
                        group.title.clone(),
                        Some(SharedString::from(t!(
                            "ui-graph-group-items",
                            count = group.members.len()
                        ))),
                        Some(layers_icon.clone()),
                        group.tone,
                        &[][..],
                        Some(group.members.len()),
                    )
                }
            };
            // A folded group: a second card peeking out behind.
            if count.is_some() {
                let offset = px(4. * z);
                window.paint_quad(quad(
                    Bounds::new(bounds.origin + point(offset, offset), bounds.size),
                    radius,
                    c.node.opacity(alpha),
                    px(1.),
                    c.node_border.opacity(alpha),
                    BorderStyle::Solid,
                ));
            }
            let selected = self.selected == Some(item);
            let hovered = self.hovered == Some(item);
            let (border, border_width) = if selected {
                (c.selected, 1.5)
            } else if hovered {
                (c.node_hover, 1.)
            } else {
                (c.node_border, 1.)
            };
            // Hover: a soft halo, nothing else changes.
            if hovered && !selected {
                let halo = px(3.);
                window.paint_quad(quad(
                    bounds.dilate(halo),
                    radius + halo,
                    transparent_black(),
                    halo,
                    c.node_hover.opacity(0.25 * alpha),
                    BorderStyle::Solid,
                ));
            }
            if self.searching && self.matched[item] {
                let ring = px(if self.current_match == Some(item) {
                    3.
                } else {
                    2.
                });
                window.paint_quad(quad(
                    bounds.dilate(ring),
                    radius + ring,
                    transparent_black(),
                    ring,
                    c.matched,
                    BorderStyle::Solid,
                ));
            }
            window.paint_quad(quad(
                bounds,
                radius,
                c.node.opacity(alpha),
                px(border_width),
                border.opacity(alpha),
                BorderStyle::Solid,
            ));
            let dot = 6. * z;
            if z < TEXT_ZOOM {
                // Overview: a status dot only.
                let d = (dot * 1.6).max(2.);
                window.paint_quad(quad(
                    Bounds::new(
                        bounds.origin + point(px(10. * z), bounds.size.height / 2. - px(d / 2.)),
                        size(px(d), px(d)),
                    ),
                    px(d / 2.),
                    c.tone(tone).opacity(alpha),
                    px(0.),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
                continue;
            }
            let pad = 10. * z;
            let mut text_x = 12. * z;
            let detail = z >= DETAIL_ZOOM;
            if let Some(icon) = icon.filter(|_| detail) {
                let icon_size = 16. * z;
                window
                    .paint_svg(
                        Bounds::new(
                            bounds.origin + point(px(pad), px(9. * z)),
                            size(px(icon_size), px(icon_size)),
                        ),
                        icon,
                        None,
                        TransformationMatrix::unit(),
                        c.muted.opacity(alpha),
                        cx,
                    )
                    .ok();
                text_x = pad + icon_size + 8. * z;
            }
            let width = f32::from(bounds.size.width);
            paint_text(
                &title,
                bounds.origin + point(px(text_x), px(7. * z)),
                px(12.5 * z),
                FontWeight::MEDIUM,
                c.text.opacity(alpha),
                px(width - text_x - pad),
                &self.font,
                window,
                cx,
            );
            // Second row: status dot, subtitle, badges from the right.
            let row_y = 29. * z;
            let row_h = 16. * z;
            let mut right = width - pad;
            let badge_size = px(10. * z);
            for badge in badges.iter().rev().filter(|_| detail) {
                let tone = c.tone(badge.tone);
                let shaped = shape(
                    &badge.label,
                    badge_size,
                    FontWeight::MEDIUM,
                    tone.opacity(alpha),
                    &self.font,
                    window,
                );
                let w = f32::from(shaped.width) + 10. * z;
                if right - w < text_x + 40. * z {
                    break;
                }
                right -= w;
                let pill = Bounds::new(
                    bounds.origin + point(px(right), px(row_y)),
                    size(px(w), px(row_h)),
                );
                window.paint_quad(quad(
                    pill,
                    px(row_h / 2.),
                    tone.opacity(TINT_OPACITY * alpha),
                    px(0.),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
                shaped
                    .paint(
                        pill.origin + point(px(5. * z), px((row_h - 13. * z) / 2.)),
                        px(13. * z),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
                right -= 4. * z;
            }
            window.paint_quad(quad(
                Bounds::new(
                    bounds.origin + point(px(text_x), px(row_y + (row_h - dot) / 2.)),
                    size(px(dot), px(dot)),
                ),
                px(dot / 2.),
                c.tone(tone).opacity(alpha),
                px(0.),
                transparent_black(),
                BorderStyle::Solid,
            ));
            if let Some(subtitle) = subtitle.filter(|_| detail) {
                let x = text_x + dot + 6. * z;
                paint_text(
                    &subtitle,
                    bounds.origin + point(px(x), px(row_y + (row_h - 14. * z) / 2.)),
                    px(11. * z),
                    FontWeight::NORMAL,
                    c.muted.opacity(alpha),
                    px(right - x - 4. * z),
                    &self.font,
                    window,
                    cx,
                );
            }
        }
    }
}

/// Edge strokes of one look, flattened here into short polylines (cheap to
/// stroke) and built in chunks, because a GPUI path indexes its vertices
/// with `u16`. Dashed lines are collected first and cut at the end, when
/// the frame knows how many dashes all of them would make.
struct StrokeBatch {
    color: Hsla,
    width: f32,
    style: EdgeStyle,
    hot: bool,
    stroke: Pixels,
    /// On and off lengths in pixels.
    dash: Option<(f32, f32)>,
    /// Dashed polylines and their total length.
    dashed: Vec<Vec<Point<Pixels>>>,
    dashed_length: f32,
    path: PathBuilder,
    segments: usize,
    arrows: PathBuilder,
    arrow_count: usize,
    done: Vec<Path<Pixels>>,
}

impl StrokeBatch {
    /// Line segments per path: well under the `u16` vertex limit.
    const SEGMENTS: usize = 3000;

    fn new(color: Hsla, width: f32, style: EdgeStyle, hot: bool, zoom: f32) -> Self {
        let stroke = px((width * zoom).max(0.75));
        let dash = match style {
            EdgeStyle::Solid => None,
            EdgeStyle::Dashed => Some(((6. * zoom).max(2.), (4. * zoom).max(2.))),
            EdgeStyle::Dotted => Some(((2. * zoom).max(1.), (4. * zoom).max(2.))),
            EdgeStyle::LongDash => Some(((14. * zoom).max(4.), (6. * zoom).max(2.))),
        };
        Self {
            color,
            width,
            style,
            hot,
            stroke,
            dash,
            dashed: Vec::new(),
            dashed_length: 0.,
            path: stroke_builder(stroke),
            segments: 0,
            arrows: PathBuilder::fill(),
            arrow_count: 0,
            done: Vec::new(),
        }
    }

    /// Dashes this batch would cut at the base pattern.
    fn dash_count(&self) -> f32 {
        self.dash
            .map_or(0., |(on, off)| self.dashed_length / (on + off))
    }

    /// Adds a curve given in screen pixels as a polyline of at most
    /// `max_steps` segments (about one per 16 px).
    fn curve(&mut self, curve: [Pos; 4], max_steps: usize) {
        let [a, b, c, d] = curve;
        let dist = |p: Pos, q: Pos| ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
        let hull = dist(a, b) + dist(b, c) + dist(c, d);
        let steps = ((hull / 16.).ceil() as usize).clamp(1, max_steps.max(1));
        let points = (0..=steps).map(|i| {
            let p = layout::bezier_at(&curve, i as f32 / steps as f32);
            point(px(p.x), px(p.y))
        });
        if self.dash.is_some() {
            let line: Vec<Point<Pixels>> = points.collect();
            self.dashed_length += hull.min(dist(a, d) * 1.6);
            self.dashed.push(line);
            return;
        }
        for (i, p) in points.enumerate() {
            if i == 0 {
                self.path.move_to(p);
            } else {
                self.path.line_to(p);
            }
        }
        self.segments += steps;
        if self.segments >= Self::SEGMENTS {
            self.flush_path();
        }
    }

    fn arrow(&mut self, [tip, left, right]: [Point<Pixels>; 3]) {
        self.arrows.move_to(tip);
        self.arrows.line_to(left);
        self.arrows.line_to(right);
        self.arrows.close();
        self.arrow_count += 1;
        if self.arrow_count >= Self::SEGMENTS {
            let arrows = std::mem::replace(&mut self.arrows, PathBuilder::fill());
            if let Ok(path) = arrows.build() {
                self.done.push(path);
            }
            self.arrow_count = 0;
        }
    }

    fn flush_path(&mut self) {
        let path = std::mem::replace(&mut self.path, stroke_builder(self.stroke));
        if self.segments > 0
            && let Ok(path) = path.build()
        {
            self.done.push(path);
        }
        self.segments = 0;
    }

    /// Built paths: strokes, then arrowheads. Dashes are stretched by
    /// `dash_scale` (at least 1) to bound their number; an infinite scale
    /// draws the dashed lines solid.
    fn finish(mut self, dash_scale: f32) -> Vec<Path<Pixels>> {
        if dash_scale.is_infinite() {
            for line in std::mem::take(&mut self.dashed) {
                self.path.move_to(line[0]);
                for &p in &line[1..] {
                    self.path.line_to(p);
                }
                self.segments += line.len() - 1;
                if self.segments >= Self::SEGMENTS {
                    self.flush_path();
                }
            }
        }
        if let Some((on, off)) = self.dash {
            let (on, off) = (on * dash_scale, off * dash_scale);
            for line in std::mem::take(&mut self.dashed) {
                // (drawing, length left in the current dash or gap)
                let mut state = (true, on);
                for w in line.windows(2) {
                    let (px0, py0) = (f32::from(w[0].x), f32::from(w[0].y));
                    let (dx, dy) = (f32::from(w[1].x) - px0, f32::from(w[1].y) - py0);
                    let len = (dx * dx + dy * dy).sqrt();
                    let at = |t: f32| point(px(px0 + dx * t / len), px(py0 + dy * t / len));
                    let mut t = 0.;
                    while t < len {
                        let (drawing, left) = state;
                        let step = left.min(len - t);
                        if drawing {
                            self.path.move_to(at(t));
                            self.path.line_to(at(t + step));
                            self.segments += 1;
                        }
                        t += step;
                        state = if left - step <= 1e-3 {
                            (!drawing, if drawing { off } else { on })
                        } else {
                            (drawing, left - step)
                        };
                    }
                }
                if self.segments >= Self::SEGMENTS {
                    self.flush_path();
                }
            }
        }
        self.flush_path();
        if self.arrow_count > 0
            && let Ok(path) = std::mem::replace(&mut self.arrows, PathBuilder::fill()).build()
        {
            self.done.push(path);
        }
        self.done
    }
}

fn shape(
    text: &SharedString,
    font_size: Pixels,
    weight: FontWeight,
    color: Hsla,
    family: &SharedString,
    window: &Window,
) -> ShapedLine {
    let runs = [TextRun {
        len: text.len(),
        font: Font {
            weight,
            ..font(family.clone())
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    window
        .text_system()
        .shape_line(text.clone(), font_size, &runs, None)
}

/// Paints one line of text, truncated with an ellipsis to `max_width`.
#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: &SharedString,
    origin: Point<Pixels>,
    font_size: Pixels,
    weight: FontWeight,
    color: Hsla,
    max_width: Pixels,
    family: &SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() || max_width < px(8.) {
        return;
    }
    let mut line = shape(text, font_size, weight, color, family, window);
    if line.width > max_width {
        let font = Font {
            weight,
            ..font(family.clone())
        };
        let runs = [TextRun {
            len: text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        let mut wrapper = cx.text_system().line_wrapper(font, font_size);
        let (truncated, runs) =
            wrapper.truncate_line(text.clone(), max_width, "…", &runs, TruncateFrom::End);
        line = window
            .text_system()
            .shape_line(truncated, font_size, &runs, None);
    }
    line.paint(origin, font_size * 1.3, TextAlign::Left, None, window, cx)
        .ok();
}

/// Maps the graph bounds into the minimap.
pub(super) struct MinimapTransform {
    scale: f32,
    origin: Point<Pixels>,
    content: Rect,
}

impl MinimapTransform {
    fn to_screen(&self, p: Pos) -> Point<Pixels> {
        point(
            self.origin.x + px((p.x - self.content.x) * self.scale),
            self.origin.y + px((p.y - self.content.y) * self.scale),
        )
    }

    fn rect(&self, r: &Rect) -> Bounds<Pixels> {
        Bounds::new(
            self.to_screen(Pos::new(r.x, r.y)),
            size(px(r.w * self.scale), px(r.h * self.scale)),
        )
    }

    pub(super) fn to_graph(&self, p: Point<Pixels>) -> Pos {
        Pos::new(
            f32::from(p.x - self.origin.x) / self.scale + self.content.x,
            f32::from(p.y - self.origin.y) / self.scale + self.content.y,
        )
    }
}

pub(super) fn minimap_transform(content: &Rect, bounds: Bounds<Pixels>) -> MinimapTransform {
    let pad = 8.;
    let (w, h) = (
        f32::from(bounds.size.width) - 2. * pad,
        f32::from(bounds.size.height) - 2. * pad,
    );
    let scale = (w / content.w.max(1.)).min(h / content.h.max(1.));
    let used = (content.w * scale, content.h * scale);
    MinimapTransform {
        scale,
        origin: bounds.origin + point(px(pad + (w - used.0) / 2.), px(pad + (h - used.1) / 2.)),
        content: *content,
    }
}

/// Height of the minimap for content of this shape.
pub(super) fn minimap_height(content: &Rect) -> f32 {
    if content.w <= 0. {
        return 120.;
    }
    (MINIMAP_WIDTH * content.h / content.w).clamp(64., 160.)
}

pub(super) fn paint_minimap(
    frame: &Frame,
    bounds: Bounds<Pixels>,
    canvas: Bounds<Pixels>,
    window: &mut Window,
) {
    let c = &frame.colors;
    let layout = &frame.shown.layout;
    let map = minimap_transform(&layout.bounds, bounds);
    for rect in layout.groups.iter().flatten() {
        window.paint_quad(quad(
            map.rect(rect),
            px(2.),
            c.group,
            px(0.),
            transparent_black(),
            BorderStyle::Solid,
        ));
    }
    for (item, rect) in layout.nodes.iter().enumerate() {
        let color = if frame.searching && frame.matched[item] {
            c.matched
        } else if frame.selected == Some(item) {
            c.selected
        } else if frame.dimmed(item) {
            c.edge_dim
        } else {
            c.muted.opacity(0.5)
        };
        let mut r = map.rect(rect);
        r.size.height = r.size.height.max(px(1.));
        r.size.width = r.size.width.max(px(1.));
        window.paint_quad(fill(r, color));
    }
    let view = frame.viewport(&canvas);
    let margin = 24. / frame.zoom;
    let view = Rect::new(
        view.x + margin,
        view.y + margin,
        view.w - 2. * margin,
        view.h - 2. * margin,
    );
    let view_bounds = map.rect(&view);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        window.paint_quad(quad(
            view_bounds,
            px(2.),
            c.edge_dim.opacity(0.5),
            px(1.),
            c.edge,
            BorderStyle::Solid,
        ));
    });
}

/// A stroke flattened coarser than GPUI's default: edges are thin, a
/// third of a pixel off is invisible and saves most of the tessellation.
fn stroke_builder(width: Pixels) -> PathBuilder {
    PathBuilder::stroke(width).with_style(PathStyle::Stroke(
        StrokeOptions::default()
            .with_line_width(f32::from(width))
            .with_tolerance(0.35),
    ))
}

#[cfg(test)]
mod tests {
    use super::{minimap_height, minimap_transform};
    use crate::components::graph::layout::{Pos, Rect};
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn minimap_maps_both_ways() {
        let content = Rect::new(-100., 50., 2000., 1000.);
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(200.), px(100.)));
        let map = minimap_transform(&content, bounds);
        let p = Pos::new(900., 550.);
        let back = map.to_graph(map.to_screen(p));
        assert!((back.x - p.x).abs() < 0.01 && (back.y - p.y).abs() < 0.01);
        assert_eq!(minimap_height(&Rect::new(0., 0., 1000., 10_000.)), 160.);
        assert_eq!(minimap_height(&Rect::new(0., 0., 10_000., 10.)), 64.);
    }
}
