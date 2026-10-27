//! Placement of edge labels (#383): each label goes on a free stretch of
//! its edge, clear of every node box and of the labels placed before it.
//! Where the full text fits nowhere, a shorter one is tried (leading
//! ` · ` parts, then an ellipsis); a label that fits nowhere is left out,
//! and the canvas shows the full text in a tooltip.
//!
//! Pure geometry in graph coordinates, shared by the canvas and the SVG
//! export; text widths come from the caller.

use std::collections::HashMap;

use super::layout::{EdgeRoute, Pos, Rect, bezier_at};
use super::model::{GraphData, Visible};

/// Font size of a label at zoom 1.
pub const LABEL_FONT: f32 = 10.;
/// Height of a label pill.
pub const LABEL_HEIGHT: f32 = LABEL_FONT * 1.6;
/// Horizontal padding inside a pill.
pub const LABEL_PAD: f32 = 5.;
/// Free space kept between a label and a node box.
const NODE_MARGIN: Pos = Pos::new(4., 2.);
/// Free space kept between two labels.
const LABEL_GAP: f32 = 2.;
/// Separator of the parts of a label that can be dropped from the end.
const PART: &str = " · ";
/// Shortest text cut with an ellipsis, in characters before the `…`.
const MIN_CUT: usize = 3;
/// Spots tried along an edge, as a share of its length: the middle first.
const SPOTS: [f32; 13] = [
    0.5, 0.43, 0.57, 0.36, 0.64, 0.29, 0.71, 0.22, 0.78, 0.15, 0.85, 0.08, 0.92,
];
/// Grid cell of the collision index.
const CELL: f32 = 128.;

/// Where one label is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedLabel {
    /// The pill, in graph coordinates.
    pub rect: Rect,
    /// Drawn text: the full label or a shortened one.
    pub text: String,
    /// The full label.
    pub full: String,
    /// Whether `text` is shorter than the full label.
    pub shortened: bool,
}

/// Full label of each visible edge: its own, or `×N` for edges merged by
/// a folded group.
pub(crate) fn edge_texts(data: &GraphData, visible: &Visible) -> Vec<Option<String>> {
    visible
        .edges
        .iter()
        .map(|edge| {
            if edge.edges.len() > 1 {
                Some(format!("×{}", edge.edges.len()))
            } else {
                data.edges[edge.edges[0]]
                    .label
                    .as_ref()
                    .map(ToString::to_string)
            }
        })
        .collect()
}

/// Width of a pill holding text `width` wide.
pub fn pill_width(text_width: f32) -> f32 {
    text_width + 2. * LABEL_PAD
}

/// Texts to try for `label`, longest first: the label, then fewer
/// ` · `-separated parts (`3 req/s · 2.0% err` → `3 req/s`).
fn variants(label: &str) -> Vec<String> {
    let parts: Vec<&str> = label.split(PART).collect();
    (1..=parts.len())
        .rev()
        .map(|count| parts[..count].join(PART))
        .collect()
}

/// `text` cut to `keep` characters with an ellipsis.
fn cut(text: &str, keep: usize) -> String {
    let mut out: String = text.chars().take(keep).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// Point at share `s` of a route (by segment, not by arc length).
fn point_on(route: &EdgeRoute, s: f32) -> Pos {
    let count = route.segments.len();
    let at = s.clamp(0., 1.) * count as f32;
    let segment = (at.floor() as usize).min(count - 1);
    bezier_at(&route.segments[segment], at - segment as f32)
}

/// Rectangles bucketed by grid cell, for quick overlap tests.
#[derive(Default)]
struct Index {
    rects: Vec<Rect>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

impl Index {
    fn cells_of(rect: &Rect) -> impl Iterator<Item = (i32, i32)> {
        let x0 = (rect.x / CELL).floor() as i32;
        let x1 = (rect.right() / CELL).floor() as i32;
        let y0 = (rect.y / CELL).floor() as i32;
        let y1 = (rect.bottom() / CELL).floor() as i32;
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
    }

    fn insert(&mut self, rect: Rect) {
        let ix = self.rects.len();
        self.rects.push(rect);
        for cell in Self::cells_of(&rect) {
            self.cells.entry(cell).or_default().push(ix);
        }
    }

    fn hits(&self, rect: &Rect) -> bool {
        Self::cells_of(rect).any(|cell| {
            self.cells
                .get(&cell)
                .is_some_and(|ixs| ixs.iter().any(|&ix| self.rects[ix].intersects(rect)))
        })
    }
}

fn grow(rect: &Rect, dx: f32, dy: f32) -> Rect {
    Rect::new(rect.x - dx, rect.y - dy, rect.w + 2. * dx, rect.h + 2. * dy)
}

/// Places `labels[e]` on `routes[e]` around the `nodes` boxes. `measure`
/// gives the width of a text at [`LABEL_FONT`] in graph units. Labels are
/// placed in order, each clear of the earlier ones.
pub fn place_labels(
    nodes: &[Rect],
    routes: &[Option<EdgeRoute>],
    labels: &[Option<String>],
    measure: &mut dyn FnMut(&str) -> f32,
) -> Vec<Option<PlacedLabel>> {
    let mut obstacles = Index::default();
    for node in nodes {
        obstacles.insert(grow(node, NODE_MARGIN.x, NODE_MARGIN.y));
    }
    let mut placed = Index::default();
    let free = |rect: &Rect, obstacles: &Index, placed: &Index| {
        !obstacles.hits(rect) && !placed.hits(&grow(rect, LABEL_GAP, LABEL_GAP))
    };
    let h = LABEL_HEIGHT;
    // Centered on the line first, then just above or below it, then
    // starting or ending at it (beside an arc or a steep stretch). As
    // `(dx in widths, dy)` from a centered pill.
    let lift = h / 2. + 1.5;
    let offsets = [(0., 0.), (0., -lift), (0., lift), (0.5, 0.), (-0.5, 0.)];
    let mut out = Vec::with_capacity(labels.len());
    for (e, label) in labels.iter().enumerate() {
        let (Some(label), Some(Some(route))) = (label, routes.get(e)) else {
            out.push(None);
            continue;
        };
        if label.is_empty() {
            out.push(None);
            continue;
        }
        let spots: Vec<Pos> = SPOTS.iter().map(|&s| point_on(route, s)).collect();
        let fit = |w: f32| {
            offsets.iter().find_map(|&(dx, dy)| {
                // Anchored pills keep a few units from the line.
                let dx = dx * (w + 6.);
                spots.iter().find_map(|p| {
                    let rect = Rect::new(p.x + dx - w / 2., p.y + dy - h / 2., w, h);
                    free(&rect, &obstacles, &placed).then_some(rect)
                })
            })
        };
        let mut found = None;
        let texts = variants(label);
        for text in &texts {
            if let Some(rect) = fit(pill_width(measure(text))) {
                found = Some((rect, text.clone()));
                break;
            }
        }
        if found.is_none() {
            // The shortest part, cut further.
            let shortest = texts.last().map_or(label.as_str(), String::as_str);
            let chars = shortest.chars().count();
            for keep in (MIN_CUT..chars).rev() {
                let text = cut(shortest, keep);
                if let Some(rect) = fit(pill_width(measure(&text))) {
                    found = Some((rect, text));
                    break;
                }
            }
        }
        out.push(found.map(|(rect, text)| {
            placed.insert(rect);
            PlacedLabel {
                shortened: text != *label,
                full: label.clone(),
                rect,
                text,
            }
        }));
    }
    out
}

/// Index of the placed label at `point`.
pub fn label_at(labels: &[Option<PlacedLabel>], point: Pos) -> Option<usize> {
    labels
        .iter()
        .rposition(|label| label.as_ref().is_some_and(|l| l.rect.contains(point)))
}

/// Distance from `point` to a route, sampled as a polyline.
pub fn distance_to_route(route: &EdgeRoute, point: Pos) -> f32 {
    let mut best = f32::MAX;
    for segment in &route.segments {
        let mut previous = segment[0];
        for i in 1..=16 {
            let next = bezier_at(segment, i as f32 / 16.);
            best = best.min(distance_to_line(previous, next, point));
            previous = next;
        }
    }
    best
}

fn distance_to_line(a: Pos, b: Pos, p: Pos) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx * dx + dy * dy;
    let t = if len < 1e-6 {
        0.
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len).clamp(0., 1.)
    };
    let (cx, cy) = (a.x + dx * t - p.x, a.y + dy * t - p.y);
    (cx * cx + cy * cy).sqrt()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        LABEL_GAP, PlacedLabel, cut, distance_to_route, grow, label_at, place_labels, variants,
    };
    use crate::components::graph::layout::{
        self, EdgeRoute, LayoutConfig, LayoutInput, LayoutNode, Pos, Rect,
    };

    /// Width of a text as the SVG export guesses it.
    fn measure(text: &str) -> f32 {
        text.chars().count() as f32 * 6.
    }

    /// No label covers a node or another label.
    pub(crate) fn assert_clear(nodes: &[Rect], labels: &[Option<PlacedLabel>]) {
        let placed: Vec<&PlacedLabel> = labels.iter().flatten().collect();
        for label in &placed {
            for node in nodes {
                assert!(
                    !label.rect.intersects(node),
                    "label {:?} covers node {node:?}",
                    label.text
                );
            }
        }
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert!(
                    !grow(&a.rect, LABEL_GAP / 2., LABEL_GAP / 2.).intersects(&b.rect),
                    "labels {:?} and {:?} overlap",
                    a.text,
                    b.text
                );
            }
        }
    }

    fn laid_out(n: usize, edges: &[(usize, usize)], layers: &[usize]) -> layout::Layout {
        let nodes = (0..n)
            .map(|v| {
                let mut node = LayoutNode::new(208., 52.);
                node.layer = layers.get(v).copied();
                node
            })
            .collect();
        layout::layout(&LayoutInput {
            nodes,
            edges: edges.to_vec(),
            bends: Vec::new(),
            groups: 0,
            config: LayoutConfig::default(),
        })
    }

    #[test]
    fn parts_drop_from_the_end_then_cut() {
        assert_eq!(
            variants("3 req/s · 2.0% err · p95 50 ms"),
            vec![
                "3 req/s · 2.0% err · p95 50 ms",
                "3 req/s · 2.0% err",
                "3 req/s"
            ]
        );
        assert_eq!(variants("20Ki/s"), vec!["20Ki/s"]);
        assert_eq!(cut("hop.example.com → 80", 4), "hop.…");
        assert_eq!(cut("ab cd", 3), "ab…");
    }

    #[test]
    fn a_label_wider_than_the_gap_is_shortened_off_the_nodes() {
        // Two columns, one edge: the gap between them is 96 wide.
        let layout = laid_out(2, &[(0, 1)], &[0, 1]);
        let labels = vec![Some("3 req/s · 2.0% err · p95 50 ms".to_owned())];
        let placed = place_labels(&layout.nodes, &layout.edges, &labels, &mut measure);
        assert_clear(&layout.nodes, &placed);
        let label = placed[0].as_ref().expect("placed");
        assert_eq!(label.text, "3 req/s");
        assert!(label.shortened);
        // Between the columns, on the line.
        assert!(label.rect.x > layout.nodes[0].right());
        assert!(label.rect.right() < layout.nodes[1].x);
        // A short label stays whole, in the middle.
        let labels = vec![Some("1 ready".to_owned())];
        let placed = place_labels(&layout.nodes, &layout.edges, &labels, &mut measure);
        let label = placed[0].as_ref().expect("placed");
        assert!(!label.shortened);
        let mid = layout.edges[0].as_ref().unwrap().midpoint;
        assert!(label.rect.contains(mid));
    }

    #[test]
    fn a_long_single_part_is_cut_with_an_ellipsis() {
        let layout = laid_out(2, &[(0, 1)], &[0, 1]);
        let labels = vec![Some("hop.example.com → 80".to_owned())];
        let placed = place_labels(&layout.nodes, &layout.edges, &labels, &mut measure);
        assert_clear(&layout.nodes, &placed);
        let label = placed[0].as_ref().expect("placed");
        assert!(label.text.ends_with('…') && label.text.starts_with("hop."));
        assert!(label.shortened);
    }

    #[test]
    fn labels_of_converging_edges_do_not_overlap() {
        // Four sources into one target and one source into four targets.
        let mut edges = vec![(0, 4), (1, 4), (2, 4), (3, 4)];
        edges.extend([(5, 6), (5, 7), (5, 8), (5, 9)]);
        let layers = [0, 0, 0, 0, 1, 0, 1, 1, 1, 1];
        let layout = laid_out(10, &edges, &layers);
        let labels: Vec<Option<String>> = (0..edges.len())
            .map(|e| Some(format!("{e}0 req/s · 1.0% err")))
            .collect();
        let placed = place_labels(&layout.nodes, &layout.edges, &labels, &mut measure);
        assert_clear(&layout.nodes, &placed);
        assert!(placed.iter().filter(|p| p.is_some()).count() >= 6);
    }

    #[test]
    fn edges_without_labels_or_routes_get_nothing() {
        let layout = laid_out(2, &[(0, 1), (1, 1)], &[0, 1]);
        let labels = vec![None, Some("x".to_owned())];
        let placed = place_labels(&layout.nodes, &layout.edges, &labels, &mut measure);
        assert_eq!(placed, vec![None, None]);
    }

    #[test]
    fn hit_tests() {
        let route = EdgeRoute {
            kind: layout::RouteKind::Forward,
            segments: vec![[
                Pos::new(0., 0.),
                Pos::new(50., 0.),
                Pos::new(50., 0.),
                Pos::new(100., 0.),
            ]],
            bounds: Rect::new(0., 0., 100., 0.),
            midpoint: Pos::new(50., 0.),
        };
        assert!(distance_to_route(&route, Pos::new(40., 3.)) < 3.5);
        assert!(distance_to_route(&route, Pos::new(40., 30.)) > 29.);
        let labels = vec![
            None,
            Some(PlacedLabel {
                rect: Rect::new(10., 10., 20., 16.),
                text: "a".into(),
                full: "a".into(),
                shortened: false,
            }),
        ];
        assert_eq!(label_at(&labels, Pos::new(15., 20.)), Some(1));
        assert_eq!(label_at(&labels, Pos::new(5., 20.)), None);
    }
}
