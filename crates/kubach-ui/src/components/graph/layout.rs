//! Layered (Sugiyama-style) left-to-right graph layout.
//!
//! Pure geometry without GPUI, so it runs on a background thread and is
//! tested directly. Steps:
//!
//! 1. Break cycles (DFS back edges point the other way while layering).
//! 2. Assign layers: the caller's hint where given, otherwise the longest
//!    path from the sources, with sources pulled next to their successors.
//! 3. Insert dummy vertices on long edges inside one group, so they bend
//!    around nodes instead of crossing them.
//! 4. Order each layer to reduce crossings: barycenter sweeps, keeping the
//!    best order seen. Groups stay contiguous; previous positions anchor
//!    nodes that were already shown, so an update does not reshuffle them.
//! 5. Place nodes vertically per group band (isotonic regression keeps the
//!    order and gaps while pulling nodes toward their neighbors), then
//!    stack the bands, wrapping them into columns when the stack would be
//!    much taller than wide.

/// A point in graph coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

impl Pos {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in graph coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Pos {
        Pos::new(self.x + self.w / 2., self.y + self.h / 2.)
    }

    pub fn contains(&self, p: Pos) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    /// Whether the two overlap with a positive area.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }

    /// Smallest rectangle around `points`.
    pub fn around(points: impl IntoIterator<Item = Pos>) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in points {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        if x0 > x1 {
            return Rect::default();
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// Spacing of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutConfig {
    /// Horizontal space between layers.
    pub layer_gap: f32,
    /// Vertical space between nodes of a layer.
    pub node_gap: f32,
    /// Vertical space between group boxes.
    pub group_gap: f32,
    /// Inner padding of a group box.
    pub group_padding: f32,
    /// Height of a group box title above its nodes.
    pub group_header: f32,
    /// Height reserved for an edge passing through a layer.
    pub dummy_size: f32,
    /// Ordering sweeps (each one down and one up).
    pub sweeps: usize,
    /// Passes pulling nodes toward their neighbors' height.
    pub passes: usize,
    /// Width to height ratio aimed at by wrapping group bands into
    /// columns; 0 keeps one column.
    pub aspect: f32,
    /// Keep nodes with a previous position where they were: their order
    /// changes only for a clearly better one, and they move only when new
    /// nodes need room. Off, previous positions are ignored.
    pub stable: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            layer_gap: 96.,
            node_gap: 14.,
            group_gap: 36.,
            group_padding: 14.,
            group_header: 28.,
            dummy_size: 6.,
            sweeps: 6,
            passes: 24,
            aspect: 1.6,
            stable: true,
        }
    }
}

/// One node to place.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutNode {
    pub width: f32,
    pub height: f32,
    /// Layer the caller wants (0 is leftmost). Empty layers are dropped.
    pub layer: Option<usize>,
    /// Index of the group box holding the node.
    pub group: Option<usize>,
    /// Top-left corner in the previous layout, to keep the node in place.
    pub previous: Option<Pos>,
}

impl LayoutNode {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            layer: None,
            group: None,
            previous: None,
        }
    }
}

/// Everything a layout depends on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutInput {
    pub nodes: Vec<LayoutNode>,
    /// `(from, to)` node indices.
    pub edges: Vec<(usize, usize)>,
    /// Per edge (may be shorter than `edges`): y of the bend points of its
    /// previous route ([`EdgeRoute::bends`]), so long edges stay put too.
    pub bends: Vec<Option<Vec<f32>>>,
    /// Number of groups; bands are stacked in this order.
    pub groups: usize,
    pub config: LayoutConfig,
}

/// How an edge runs relative to the layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    /// Left to right, out of the source's right side.
    Forward,
    /// Between nodes of one layer: an arc on the right.
    Flat,
    /// Against the layers (a cycle): right to left.
    Back,
}

/// Geometry of one edge: cubic Bézier segments `[start, c1, c2, end]`.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgeRoute {
    pub kind: RouteKind,
    pub segments: Vec<[Pos; 4]>,
    /// Encloses every segment with its control points.
    pub bounds: Rect,
    /// Middle of the route, for a label.
    pub midpoint: Pos,
}

impl EdgeRoute {
    pub fn start(&self) -> Pos {
        self.segments[0][0]
    }

    /// Y of the points where the route bends through layers.
    pub fn bends(&self) -> Vec<f32> {
        self.segments[..self.segments.len() - 1]
            .iter()
            .map(|segment| segment[3].y)
            .collect()
    }

    pub fn end(&self) -> Pos {
        self.segments[self.segments.len() - 1][3]
    }

    /// Unit direction at the end, for the arrowhead.
    pub fn end_direction(&self) -> Pos {
        let [_, _, c2, end] = self.segments[self.segments.len() - 1];
        let (dx, dy) = (end.x - c2.x, end.y - c2.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-3 {
            Pos::new(1., 0.)
        } else {
            Pos::new(dx / len, dy / len)
        }
    }
}

/// Result of [`layout`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Box of each input node.
    pub nodes: Vec<Rect>,
    /// Layer of each input node.
    pub layers: Vec<usize>,
    /// Nodes of each layer, top to bottom.
    pub order: Vec<Vec<usize>>,
    /// Route of each input edge; `None` for a self-loop or a bad index.
    pub edges: Vec<Option<EdgeRoute>>,
    /// Box of each group; `None` when the group has no nodes.
    pub groups: Vec<Option<Rect>>,
    /// Everything above.
    pub bounds: Rect,
}

/// Point of a cubic Bézier at `t`.
pub fn bezier_at(segment: &[Pos; 4], t: f32) -> Pos {
    let [a, b, c, d] = segment;
    let u = 1. - t;
    let (w0, w1, w2, w3) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
    Pos::new(
        w0 * a.x + w1 * b.x + w2 * c.x + w3 * d.x,
        w0 * a.y + w1 * b.y + w2 * c.y + w3 * d.y,
    )
}

/// Appends the parts of a cubic Bézier that may cross `view`: halves are
/// split off until each part is no larger than the view, and parts whose
/// hull misses the view are dropped.
pub fn clip_bezier(segment: &[Pos; 4], view: &Rect, out: &mut Vec<[Pos; 4]>) {
    fn split(s: &[Pos; 4]) -> ([Pos; 4], [Pos; 4]) {
        let mid = |a: Pos, b: Pos| Pos::new((a.x + b.x) / 2., (a.y + b.y) / 2.);
        let (ab, bc, cd) = (mid(s[0], s[1]), mid(s[1], s[2]), mid(s[2], s[3]));
        let (abc, bcd) = (mid(ab, bc), mid(bc, cd));
        let m = mid(abc, bcd);
        ([s[0], ab, abc, m], [m, bcd, cd, s[3]])
    }
    fn walk(s: &[Pos; 4], view: &Rect, out: &mut Vec<[Pos; 4]>, depth: u32) {
        let hull = Rect::around(s.iter().copied());
        // A flat hull has no area; widen it so it still meets the view.
        let hull = Rect::new(hull.x - 0.5, hull.y - 0.5, hull.w + 1., hull.h + 1.);
        if !hull.intersects(view) {
            return;
        }
        if depth >= 12 || (hull.w <= view.w && hull.h <= view.h) {
            out.push(*s);
            return;
        }
        let (a, b) = split(s);
        walk(&a, view, out, depth + 1);
        walk(&b, view, out, depth + 1);
    }
    walk(segment, view, out, 0);
}

/// A vertex being ordered: a node or a dummy on a long edge.
struct Vertex {
    layer: usize,
    /// 0 for ungrouped, otherwise group index + 1.
    band: usize,
    height: f32,
    dummy: bool,
    /// Previous center y, relative to the previous band top.
    anchor: Option<f32>,
    /// Starting place in the layer: the anchor, or for a new vertex one
    /// derived from anchored neighbors.
    seed: Option<f32>,
}

/// Lays out `input`. Deterministic: equal input gives equal output.
pub fn layout(input: &LayoutInput) -> Layout {
    let n = input.nodes.len();
    let config = &input.config;
    if n == 0 {
        return Layout {
            edges: vec![None; input.edges.len()],
            groups: vec![None; input.groups],
            ..Layout::default()
        };
    }

    // Unique edges between different, existing nodes.
    let mut unique: Vec<(usize, usize)> = input
        .edges
        .iter()
        .copied()
        .filter(|&(a, b)| a != b && a < n && b < n)
        .collect();
    unique.sort_unstable();
    unique.dedup();

    let layers = assign_layers(input, &unique);
    let layer_count = layers.iter().max().map_or(0, |l| l + 1);

    // Band of a node: its group (bounded by the declared group count).
    let band_of = |v: usize| match input.nodes[v].group {
        Some(g) if g < input.groups => g + 1,
        _ => 0,
    };

    // Vertices: nodes, then dummies.
    let mut vertices: Vec<Vertex> = (0..n)
        .map(|v| Vertex {
            layer: layers[v],
            band: band_of(v),
            height: input.nodes[v].height,
            dummy: false,
            anchor: None,
            seed: None,
        })
        .collect();
    let band_count = input.groups + 1;
    let base = set_anchors(input, &mut vertices, band_count);
    let mut bends: std::collections::HashMap<(usize, usize), &Vec<f32>> =
        std::collections::HashMap::new();
    if config.stable {
        for (edge, bend) in input.edges.iter().zip(&input.bends) {
            if let Some(bend) = bend {
                bends.entry(*edge).or_insert(bend);
            }
        }
    }

    // Ordering edges between adjacent layers, and dummy chains of edges.
    let mut chains: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    let mut adjacent: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in &unique {
        let (la, lb) = (layers[a], layers[b]);
        if lb == la + 1 {
            adjacent.push((a, b));
        } else if lb > la + 1 && band_of(a) == band_of(b) {
            let mut chain = Vec::with_capacity(lb - la - 1);
            let mut previous = a;
            let band = band_of(a);
            let bend = bends
                .get(&(a, b))
                .filter(|bend| bend.len() == lb - la - 1 && base[band] < f32::MAX);
            for layer in la + 1..lb {
                let d = vertices.len();
                vertices.push(Vertex {
                    layer,
                    band,
                    height: config.dummy_size,
                    dummy: true,
                    anchor: bend.map(|bend| bend[layer - la - 1] - base[band]),
                    seed: None,
                });
                adjacent.push((previous, d));
                chain.push(d);
                previous = d;
            }
            adjacent.push((previous, b));
            chains.push((a, b, chain));
        }
    }
    let count = vertices.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(a, b) in &adjacent {
        succs[a].push(b);
        preds[b].push(a);
    }

    seed_vertices(&mut vertices, &preds, &succs, &chains);
    let order = order_layers(&vertices, &preds, &succs, layer_count, config);

    // Vertical placement inside bands, then bands stacked.
    let mut y = vec![0f32; count];
    for band in 0..band_count {
        place_band(band, &order, &vertices, &preds, &succs, config, &mut y);
    }

    // Layer columns.
    let mut layer_width = vec![0f32; layer_count];
    for (node, &l) in input.nodes.iter().zip(&layers) {
        layer_width[l] = layer_width[l].max(node.width);
    }
    let mut layer_x = vec![0f32; layer_count];
    for l in 1..layer_count {
        layer_x[l] = layer_x[l - 1] + layer_width[l - 1] + config.layer_gap;
    }

    // Band extents (relative).
    let mut extent = vec![(f32::MAX, f32::MIN); band_count];
    for (v, vertex) in vertices.iter().enumerate() {
        let e = &mut extent[vertex.band];
        e.0 = e.0.min(y[v]);
        e.1 = e.1.max(y[v] + vertex.height);
    }
    let mut span = vec![(f32::MAX, f32::MIN); band_count];
    for v in 0..n {
        let s = &mut span[vertices[v].band];
        s.0 = s.0.min(layer_x[layers[v]]);
        s.1 = s.1.max(layer_x[layers[v]] + input.nodes[v].width);
    }
    let inner = config.group_header + config.group_padding;
    let block_height = |band: usize| {
        let (top, bottom) = extent[band];
        if band == 0 {
            bottom - top
        } else {
            inner + (bottom - top) + config.group_padding
        }
    };
    let bands: Vec<usize> = (0..band_count)
        .filter(|&b| extent[b].0 <= extent[b].1)
        .collect();

    // Stack the bands; wrap them into columns when the stack is much
    // taller than wide.
    let width = layer_x.last().copied().unwrap_or(0.)
        + layer_width.last().copied().unwrap_or(0.)
        + 2. * config.group_padding;
    let total: f32 = bands
        .iter()
        .map(|&b| block_height(b) + config.group_gap)
        .sum();
    let columns = if config.aspect > 0. && width > 0. {
        ((config.aspect * total / width).sqrt().round() as usize).clamp(1, bands.len().max(1))
    } else {
        1
    };
    let column_height = total / columns as f32;
    let mut offset = vec![0f32; band_count];
    let mut shift = vec![0f32; band_count];
    let mut groups = vec![None; input.groups];
    let (mut column, mut cursor) = (0usize, 0f32);
    for &band in &bands {
        let height = block_height(band);
        if cursor > 0. && cursor + height / 2. > column_height && column + 1 < columns {
            column += 1;
            cursor = 0.;
        }
        let dx = column as f32 * (width + config.layer_gap);
        shift[band] = dx;
        let (top, _) = extent[band];
        if band == 0 {
            offset[0] = cursor - top;
        } else {
            offset[band] = cursor + inner - top;
            let (x0, x1) = span[band];
            groups[band - 1] = Some(Rect::new(
                dx + x0 - config.group_padding,
                cursor,
                x1 - x0 + 2. * config.group_padding,
                height,
            ));
        }
        cursor += height + config.group_gap;
    }

    let nodes: Vec<Rect> = (0..n)
        .map(|v| {
            let node = &input.nodes[v];
            let band = vertices[v].band;
            Rect::new(
                shift[band] + layer_x[layers[v]],
                y[v] + offset[band],
                node.width,
                node.height,
            )
        })
        .collect();
    let dummy_center = |d: usize| {
        let vertex = &vertices[d];
        Pos::new(
            shift[vertex.band] + layer_x[vertex.layer] + layer_width[vertex.layer] / 2.,
            y[d] + offset[vertex.band] + vertex.height / 2.,
        )
    };

    let chain_of: std::collections::HashMap<(usize, usize), &Vec<usize>> =
        chains.iter().map(|(a, b, c)| ((*a, *b), c)).collect();
    let edges = input
        .edges
        .iter()
        .map(|&(a, b)| {
            if a == b || a >= n || b >= n {
                return None;
            }
            let (ra, rb) = (&nodes[a], &nodes[b]);
            let (la, lb) = (layers[a], layers[b]);
            let mid_y = |r: &Rect| r.y + r.h / 2.;
            let route = if lb > la {
                let mut points = vec![Pos::new(ra.right(), mid_y(ra))];
                if let Some(chain) = chain_of.get(&(a, b)) {
                    points.extend(chain.iter().map(|&d| dummy_center(d)));
                }
                points.push(Pos::new(rb.x, mid_y(rb)));
                route_through(RouteKind::Forward, &points)
            } else if lb < la {
                let points = [Pos::new(ra.x, mid_y(ra)), Pos::new(rb.right(), mid_y(rb))];
                route_through(RouteKind::Back, &points)
            } else {
                let start = Pos::new(ra.right(), mid_y(ra));
                let end = Pos::new(rb.right(), mid_y(rb));
                let bulge = (config.layer_gap * 0.4).max((end.y - start.y).abs() * 0.25);
                let segment = [
                    start,
                    Pos::new(start.x + bulge, start.y),
                    Pos::new(end.x + bulge, end.y),
                    end,
                ];
                finish_route(RouteKind::Flat, vec![segment])
            };
            Some(route)
        })
        .collect();

    let mut bounds: Option<Rect> = None;
    for rect in nodes.iter().chain(groups.iter().flatten()) {
        bounds = Some(bounds.map_or(*rect, |b| b.union(rect)));
    }

    let mut by_layer: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for v in 0..n {
        by_layer[layers[v]].push(v);
    }
    for column in &mut by_layer {
        column.sort_by(|&a, &b| nodes[a].y.total_cmp(&nodes[b].y));
    }

    Layout {
        nodes,
        layers,
        order: by_layer,
        edges,
        groups,
        bounds: bounds.unwrap_or_default(),
    }
}

/// Smooth route through `points` with horizontal tangents at each.
fn route_through(kind: RouteKind, points: &[Pos]) -> EdgeRoute {
    let segments = points
        .windows(2)
        .map(|pair| {
            let (p, q) = (pair[0], pair[1]);
            let bend = (q.x - p.x) / 2.;
            [p, Pos::new(p.x + bend, p.y), Pos::new(q.x - bend, q.y), q]
        })
        .collect();
    finish_route(kind, segments)
}

fn finish_route(kind: RouteKind, segments: Vec<[Pos; 4]>) -> EdgeRoute {
    let bounds = Rect::around(segments.iter().flatten().copied());
    let middle = &segments[segments.len() / 2];
    let midpoint = if segments.len() % 2 == 1 {
        bezier_at(middle, 0.5)
    } else {
        middle[0]
    };
    EdgeRoute {
        kind,
        segments,
        bounds,
        midpoint,
    }
}

/// Layer of every node: hints where given, otherwise from the topology.
fn assign_layers(input: &LayoutInput, unique: &[(usize, usize)]) -> Vec<usize> {
    let n = input.nodes.len();
    let hint = |v: usize| input.nodes[v].layer;

    // Orient edges between hinted nodes by their hints; drop same-layer ones.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in unique {
        match (hint(a), hint(b)) {
            (Some(ha), Some(hb)) if ha == hb => {}
            (Some(ha), Some(hb)) if ha > hb => out[b].push(a),
            _ => out[a].push(b),
        }
    }

    // Break cycles: DFS from sources (and low hints) first; edges to a
    // vertex on the stack are dropped from the acyclic graph.
    let mut indegree = vec![0usize; n];
    for targets in &out {
        for &t in targets {
            indegree[t] += 1;
        }
    }
    let mut roots: Vec<usize> = (0..n).collect();
    roots.sort_by_key(|&v| (indegree[v] > 0, hint(v).unwrap_or(usize::MAX), v));
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &root in &roots {
        if state[root] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        state[root] = 1;
        while let Some(&mut (v, ref mut next)) = stack.last_mut() {
            if let Some(&t) = out[v].get(*next) {
                *next += 1;
                match state[t] {
                    0 => {
                        dag[v].push(t);
                        state[t] = 1;
                        stack.push((t, 0));
                    }
                    1 => {} // back edge: breaks a cycle
                    _ => dag[v].push(t),
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }

    // Topological order (Kahn), ties by index for determinism.
    let mut indegree = vec![0usize; n];
    for targets in &dag {
        for &t in targets {
            indegree[t] += 1;
        }
    }
    let mut ready: std::collections::BinaryHeap<std::cmp::Reverse<usize>> = (0..n)
        .filter(|&v| indegree[v] == 0)
        .map(std::cmp::Reverse)
        .collect();
    let mut topo = Vec::with_capacity(n);
    while let Some(std::cmp::Reverse(v)) = ready.pop() {
        topo.push(v);
        for &t in &dag[v] {
            indegree[t] -= 1;
            if indegree[t] == 0 {
                ready.push(std::cmp::Reverse(t));
            }
        }
    }

    // Longest path, floored by hints.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, targets) in dag.iter().enumerate() {
        for &t in targets {
            preds[t].push(v);
        }
    }
    let mut layer = vec![0usize; n];
    for &v in &topo {
        layer[v] = match hint(v) {
            Some(h) => h,
            None => preds[v].iter().map(|&p| layer[p] + 1).max().unwrap_or(0),
        };
    }
    // Pull unhinted sources right, next to their nearest successor.
    for &v in topo.iter().rev() {
        if hint(v).is_none()
            && preds[v].is_empty()
            && let Some(min) = dag[v].iter().map(|&t| layer[t]).min()
        {
            layer[v] = layer[v].max(min.saturating_sub(1));
        }
    }

    // Drop empty layers.
    let mut used: Vec<usize> = layer.clone();
    used.sort_unstable();
    used.dedup();
    layer
        .iter()
        .map(|l| used.binary_search(l).unwrap_or(0))
        .collect()
}

/// Previous center y of each node, relative to the top of its band in the
/// previous layout (approximated by the topmost previous member). Returns
/// those band tops.
fn set_anchors(input: &LayoutInput, vertices: &mut [Vertex], band_count: usize) -> Vec<f32> {
    let mut base = vec![f32::MAX; band_count];
    if !input.config.stable {
        return base;
    }
    for (v, node) in input.nodes.iter().enumerate() {
        if let Some(previous) = node.previous {
            let b = &mut base[vertices[v].band];
            *b = b.min(previous.y);
        }
    }
    for (v, node) in input.nodes.iter().enumerate() {
        if let Some(previous) = node.previous {
            let band = vertices[v].band;
            vertices[v].anchor = Some(previous.y - base[band] + node.height / 2.);
        }
    }
    base
}

/// Seeds of new vertices: the mean anchor of their anchored neighbors, and
/// along a dummy chain the interpolation between anchored ends.
fn seed_vertices(
    vertices: &mut [Vertex],
    preds: &[Vec<usize>],
    succs: &[Vec<usize>],
    chains: &[(usize, usize, Vec<usize>)],
) {
    for vertex in vertices.iter_mut() {
        vertex.seed = vertex.anchor;
    }
    for v in 0..vertices.len() {
        if vertices[v].seed.is_some() || vertices[v].dummy {
            continue;
        }
        let anchors: Vec<f32> = preds[v]
            .iter()
            .chain(&succs[v])
            .filter_map(|&u| vertices[u].anchor)
            .collect();
        if !anchors.is_empty() {
            vertices[v].seed = Some(anchors.iter().sum::<f32>() / anchors.len() as f32);
        }
    }
    for (a, b, chain) in chains {
        if let (Some(sa), Some(sb)) = (vertices[*a].seed, vertices[*b].seed) {
            let steps = chain.len() as f32 + 1.;
            for (i, &d) in chain.iter().enumerate() {
                if vertices[d].anchor.is_none() {
                    vertices[d].seed = Some(sa + (sb - sa) * (i as f32 + 1.) / steps);
                }
            }
        }
    }
}

/// Order of every layer, minimizing crossings with barycenter sweeps.
fn order_layers(
    vertices: &[Vertex],
    preds: &[Vec<usize>],
    succs: &[Vec<usize>],
    layer_count: usize,
    config: &LayoutConfig,
) -> Vec<Vec<usize>> {
    let mut order: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (v, vertex) in vertices.iter().enumerate() {
        order[vertex.layer].push(v);
    }
    // Initial order: band, then previous position (nodes that were shown
    // before), then the input order. Unanchored nodes go after anchored
    // ones and find their place in the first sweep.
    for layer in &mut order {
        layer.sort_by(|&a, &b| {
            let (va, vb) = (&vertices[a], &vertices[b]);
            va.band
                .cmp(&vb.band)
                .then_with(|| match (va.seed, vb.seed) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then(a.cmp(&b))
        });
    }
    let mut pos = vec![0f32; vertices.len()];
    let reindex = |order: &[Vec<usize>], pos: &mut [f32]| {
        for layer in order {
            for (i, &v) in layer.iter().enumerate() {
                pos[v] = i as f32;
            }
        }
    };
    reindex(&order, &mut pos);
    // Rank of anchored nodes among the initial order, as a pull.
    let anchor_rank: Vec<Option<f32>> = vertices
        .iter()
        .enumerate()
        .map(|(v, vertex)| vertex.anchor.map(|_| pos[v]))
        .collect();

    let mut best = order.clone();
    let initial = total_crossings(&order, succs, &pos);
    // With previous positions, keep the previous order unless the sweeps
    // untangle noticeably more.
    let anchored = vertices.iter().any(|v| v.anchor.is_some());
    let mut best_crossings = if anchored {
        initial - initial / 10
    } else {
        initial
    };
    let weight = 3.;
    for _ in 0..config.sweeps {
        for down in [true, false] {
            let layers: Vec<usize> = if down {
                (1..layer_count).collect()
            } else {
                (0..layer_count.saturating_sub(1)).rev().collect()
            };
            for l in layers {
                let mut keyed: Vec<(usize, f32, usize)> = order[l]
                    .iter()
                    .map(|&v| {
                        let neighbors = if down { &preds[v] } else { &succs[v] };
                        let mut key = if neighbors.is_empty() {
                            pos[v]
                        } else {
                            neighbors.iter().map(|&u| pos[u]).sum::<f32>() / neighbors.len() as f32
                        };
                        if let Some(rank) = anchor_rank[v] {
                            key = (key + weight * rank) / (1. + weight);
                        }
                        (vertices[v].band, key, v)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                order[l] = keyed.into_iter().map(|(_, _, v)| v).collect();
                for (i, &v) in order[l].iter().enumerate() {
                    pos[v] = i as f32;
                }
            }
        }
        let crossings = total_crossings(&order, succs, &pos);
        if crossings < best_crossings {
            best_crossings = crossings;
            best = order.clone();
        }
        if best_crossings == 0 {
            break;
        }
    }
    best
}

/// Crossings between all adjacent layer pairs.
fn total_crossings(order: &[Vec<usize>], succs: &[Vec<usize>], pos: &[f32]) -> usize {
    let mut total = 0;
    for layer in order.iter().take(order.len().saturating_sub(1)) {
        let mut pairs: Vec<(usize, usize)> = layer
            .iter()
            .flat_map(|&v| {
                succs[v]
                    .iter()
                    .map(move |&t| (pos[v] as usize, pos[t] as usize))
            })
            .collect();
        total += crossings(&mut pairs);
    }
    total
}

/// Crossings of edges given as `(upper position, lower position)` pairs.
pub fn crossings(pairs: &mut [(usize, usize)]) -> usize {
    pairs.sort_unstable();
    let size = pairs.iter().map(|p| p.1 + 1).max().unwrap_or(0);
    // Fenwick tree over lower positions: count earlier edges ending below.
    let mut tree = vec![0usize; size + 1];
    let mut count = 0;
    for (seen, &(_, b)) in pairs.iter().enumerate() {
        // Edges seen so far ending at or before b.
        let mut i = b + 1;
        let mut le = 0;
        while i > 0 {
            le += tree[i];
            i &= i - 1;
        }
        count += seen - le;
        let mut i = b + 1;
        while i <= size {
            tree[i] += 1;
            i += i.isolate_lowest_one();
        }
    }
    count
}

/// Top y of every vertex in `band`, relative to the band.
fn place_band(
    band: usize,
    order: &[Vec<usize>],
    vertices: &[Vertex],
    preds: &[Vec<usize>],
    succs: &[Vec<usize>],
    config: &LayoutConfig,
    y: &mut [f32],
) {
    let columns: Vec<Vec<usize>> = order
        .iter()
        .map(|layer| {
            layer
                .iter()
                .copied()
                .filter(|&v| vertices[v].band == band)
                .collect()
        })
        .collect();
    if columns.iter().all(Vec::is_empty) {
        return;
    }
    let gap = |a: usize, b: usize| {
        if vertices[a].dummy || vertices[b].dummy {
            config.node_gap / 2.
        } else {
            config.node_gap
        }
    };
    let stack_height = |column: &[usize]| -> f32 {
        let mut h = 0.;
        for (i, &v) in column.iter().enumerate() {
            h += vertices[v].height;
            if i + 1 < column.len() {
                h += gap(v, column[i + 1]);
            }
        }
        h
    };
    let tallest = columns.iter().map(|c| stack_height(c)).fold(0f32, f32::max);

    // Start: seeded vertices at their previous (or derived) place, the
    // rest stacked and centered against the tallest layer.
    for column in &columns {
        let mut cursor = (tallest - stack_height(column)) / 2.;
        let mut desired = Vec::with_capacity(column.len());
        for (i, &v) in column.iter().enumerate() {
            let vertex = &vertices[v];
            desired.push(match vertex.seed {
                Some(center) => center - vertex.height / 2.,
                None => cursor,
            });
            cursor += vertex.height;
            if i + 1 < column.len() {
                cursor += gap(v, column[i + 1]);
            }
        }
        resolve(column, &desired, vertices, &gap, y);
    }

    // Pull toward neighbors in the band, alternating directions.
    let center = |v: usize, y: &[f32]| y[v] + vertices[v].height / 2.;
    for pass in 0..config.passes {
        let indices: Vec<usize> = if pass % 2 == 0 {
            (0..columns.len()).collect()
        } else {
            (0..columns.len()).rev().collect()
        };
        for l in indices {
            let column = &columns[l];
            if column.is_empty() {
                continue;
            }
            let desired: Vec<f32> = column
                .iter()
                .map(|&v| {
                    let vertex = &vertices[v];
                    let mut sum = 0.;
                    let mut count = 0.;
                    for &u in preds[v].iter().chain(&succs[v]) {
                        if vertices[u].band == band {
                            sum += center(u, y);
                            count += 1.;
                        }
                    }
                    let target = match vertex.anchor {
                        Some(anchor) => anchor,
                        None if count > 0. => sum / count,
                        None => center(v, y),
                    };
                    target - vertex.height / 2.
                })
                .collect();
            resolve(column, &desired, vertices, &gap, y);
        }
    }
}

/// Closest tops to `desired` (least squares) that keep the column's order
/// and gaps: isotonic regression on tops minus their stacked offsets.
fn resolve(
    column: &[usize],
    desired: &[f32],
    vertices: &[Vertex],
    gap: &dyn Fn(usize, usize) -> f32,
    y: &mut [f32],
) {
    let mut offsets = Vec::with_capacity(column.len());
    let mut offset = 0f32;
    for (i, &v) in column.iter().enumerate() {
        offsets.push(offset);
        offset += vertices[v].height;
        if i + 1 < column.len() {
            offset += gap(v, column[i + 1]);
        }
    }
    // Pool adjacent violators: blocks of (mean, weight, length).
    let mut blocks: Vec<(f32, f32, usize)> = Vec::with_capacity(column.len());
    for (i, d) in desired.iter().enumerate() {
        blocks.push((d - offsets[i], 1., 1));
        while blocks.len() > 1 {
            let last = blocks[blocks.len() - 1];
            let prev = blocks[blocks.len() - 2];
            if prev.0 <= last.0 {
                break;
            }
            let weight = prev.1 + last.1;
            let merged = (
                (prev.0 * prev.1 + last.0 * last.1) / weight,
                weight,
                prev.2 + last.2,
            );
            blocks.pop();
            *blocks.last_mut().expect("two blocks") = merged;
        }
    }
    let mut i = 0;
    for (value, _, len) in blocks {
        for _ in 0..len {
            y[column[i]] = value + offsets[i];
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LayoutConfig, LayoutInput, LayoutNode, Pos, Rect, RouteKind, bezier_at, clip_bezier,
        crossings, layout,
    };

    const W: f32 = 200.;
    const H: f32 = 52.;

    fn input(n: usize, edges: &[(usize, usize)]) -> LayoutInput {
        LayoutInput {
            nodes: (0..n).map(|_| LayoutNode::new(W, H)).collect(),
            edges: edges.to_vec(),
            bends: Vec::new(),
            groups: 0,
            config: LayoutConfig::default(),
        }
    }

    fn assert_no_overlaps(rects: &[Rect]) {
        let mut sorted: Vec<&Rect> = rects.iter().collect();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        for (i, a) in sorted.iter().enumerate() {
            for b in &sorted[i + 1..] {
                if b.x >= a.right() {
                    break;
                }
                assert!(!a.intersects(b), "{a:?} overlaps {b:?}");
            }
        }
    }

    /// Deterministic pseudo-random numbers.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % bound
        }
    }

    /// A layered service map: `n` nodes in 5 kinds over `groups` groups.
    fn service_map(n: usize, edges: usize, groups: usize, seed: u64) -> LayoutInput {
        let mut rng = Lcg(seed);
        let mut input = input(0, &[]);
        input.groups = groups;
        for i in 0..n {
            let mut node = LayoutNode::new(W, H);
            node.layer = Some(i % 5);
            node.group = Some(rng.next(groups));
            input.nodes.push(node);
        }
        while input.edges.len() < edges {
            let a = rng.next(n);
            let b = rng.next(n);
            if a % 5 < b % 5 && (rng.next(4) == 0 || input.nodes[a].group == input.nodes[b].group) {
                input.edges.push((a, b));
            }
        }
        input
    }

    #[test]
    fn chain_runs_left_to_right() {
        let result = layout(&input(4, &[(0, 1), (1, 2), (2, 3)]));
        assert_eq!(result.layers, vec![0, 1, 2, 3]);
        for pair in result.nodes.windows(2) {
            assert!(pair[0].right() < pair[1].x);
        }
        for route in result.edges.iter().flatten() {
            assert_eq!(route.kind, RouteKind::Forward);
            assert!(route.start().x < route.end().x);
        }
    }

    #[test]
    fn dag_edges_point_right_and_nodes_do_not_overlap() {
        let mut rng = Lcg(7);
        let n = 120;
        let edges: Vec<(usize, usize)> = (0..300)
            .map(|_| {
                let a = rng.next(n);
                let b = rng.next(n);
                (a.min(b), a.max(b))
            })
            .filter(|(a, b)| a != b)
            .collect();
        let result = layout(&input(n, &edges));
        assert_no_overlaps(&result.nodes);
        for (&(a, b), route) in edges.iter().zip(&result.edges) {
            let route = route.as_ref().expect("route");
            assert_eq!(route.kind, RouteKind::Forward);
            assert!(result.layers[a] < result.layers[b]);
            assert!(result.nodes[a].right() <= result.nodes[b].x);
        }
    }

    #[test]
    fn cycles_do_not_break_the_layout() {
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3), (3, 3), (4, 4)];
        let result = layout(&input(5, &edges));
        assert_no_overlaps(&result.nodes);
        let back = result
            .edges
            .iter()
            .flatten()
            .filter(|r| r.kind == RouteKind::Back)
            .count();
        assert_eq!(back, 1);
        // Self-loops have no route.
        assert!(result.edges[4].is_none() && result.edges[5].is_none());
        assert!(
            result
                .nodes
                .iter()
                .all(|r| r.x.is_finite() && r.y.is_finite())
        );
    }

    #[test]
    fn layer_hints_win_and_empty_layers_drop() {
        let mut input = input(4, &[(3, 0), (0, 1)]);
        input.nodes[0].layer = Some(2);
        input.nodes[1].layer = Some(6);
        input.nodes[2].layer = Some(6);
        // Node 3 has no hint: it lands just left of its successor.
        let result = layout(&input);
        assert_eq!(result.layers, vec![1, 2, 2, 0]);
        assert_no_overlaps(&result.nodes);
    }

    #[test]
    fn same_layer_edges_arc_on_the_right() {
        let mut input = input(2, &[(0, 1)]);
        input.nodes[0].layer = Some(0);
        input.nodes[1].layer = Some(0);
        let result = layout(&input);
        let route = result.edges[0].as_ref().expect("route");
        assert_eq!(route.kind, RouteKind::Flat);
        assert!(route.bounds.right() > result.nodes[0].right());
    }

    #[test]
    fn barycenter_untangles_a_permutation() {
        // Sources 0..4 in order, their targets listed reversed.
        let edges = [(0, 8), (1, 7), (2, 6), (3, 5), (4, 9)];
        let result = layout(&input(10, &edges));
        let pos = |v: usize| result.nodes[v].y;
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        let rank = |v: usize, layer: &[usize]| layer.iter().position(|&u| u == v).unwrap();
        for &(a, b) in &edges {
            pairs.push((rank(a, &result.order[0]), rank(b, &result.order[1])));
        }
        assert_eq!(crossings(&mut pairs), 0);
        // Straight edges: each target level with its source.
        for &(a, b) in &edges {
            assert!((pos(a) - pos(b)).abs() < 1.);
        }
    }

    #[test]
    fn crossing_count() {
        assert_eq!(crossings(&mut [(0, 1), (1, 0)]), 1);
        assert_eq!(crossings(&mut [(0, 0), (1, 1), (2, 2)]), 0);
        assert_eq!(crossings(&mut [(0, 2), (1, 1), (2, 0)]), 3);
        // Shared endpoints do not cross.
        assert_eq!(crossings(&mut [(0, 0), (0, 1), (1, 1)]), 0);
    }

    #[test]
    fn groups_box_their_nodes_without_overlap() {
        let input = service_map(300, 700, 4, 11);
        let result = layout(&input);
        assert_no_overlaps(&result.nodes);
        let boxes: Vec<Rect> = result.groups.iter().map(|g| g.expect("group")).collect();
        for (v, node) in input.nodes.iter().enumerate() {
            let rect = result.nodes[v];
            let group = boxes[node.group.unwrap()];
            assert!(group.contains(Pos::new(rect.x, rect.y)));
            assert!(group.contains(Pos::new(rect.right(), rect.bottom())));
            for (g, other) in boxes.iter().enumerate() {
                if Some(g) != node.group {
                    assert!(!other.intersects(&rect));
                }
            }
        }
        assert_no_overlaps(&boxes);
    }

    #[test]
    fn long_edges_bend_through_their_layers() {
        let result = layout(&input(4, &[(0, 1), (1, 2), (0, 2), (2, 3), (0, 3)]));
        let long = result.edges[4].as_ref().expect("route");
        assert_eq!(long.segments.len(), 3);
        let short = result.edges[0].as_ref().expect("route");
        assert_eq!(short.segments.len(), 1);
    }

    #[test]
    fn previous_positions_keep_nodes_in_place() {
        let mut input = service_map(200, 400, 3, 5);
        let first = layout(&input);
        for (v, node) in input.nodes.iter_mut().enumerate() {
            let r = first.nodes[v];
            node.previous = Some(Pos::new(r.x, r.y));
        }
        input.bends = first
            .edges
            .iter()
            .map(|route| route.as_ref().map(|r| r.bends()))
            .collect();
        // An unchanged graph lays out the same.
        let again = layout(&input);
        let max_shift = |a: &[Rect], b: &[Rect], n: usize| {
            (0..n)
                .map(|v| (a[v].y - b[v].y).abs() + (a[v].x - b[v].x).abs())
                .fold(0f32, f32::max)
        };
        assert_eq!(first.order, again.order);
        let shift = max_shift(&first.nodes, &again.nodes, 200);
        assert!(shift < 1., "unchanged graph moved by {shift}");

        // A new leaf: old nodes keep their order in every layer.
        let mut grown = input.clone();
        let mut leaf = LayoutNode::new(W, H);
        leaf.layer = Some(4);
        leaf.group = Some(1);
        grown.nodes.push(leaf);
        grown.edges.push((3, 200));
        let after = layout(&grown);
        for (column, before) in after.order.iter().zip(&first.order) {
            let kept: Vec<usize> = column.iter().copied().filter(|&v| v < 200).collect();
            assert_eq!(&kept, before);
        }
        let moved = (0..200)
            .filter(|&v| (after.nodes[v].y - first.nodes[v].y).abs() > H)
            .count();
        assert!(moved < 20, "{moved} of 200 nodes moved");
    }

    #[test]
    fn large_graph_lays_out_quickly() {
        let input = service_map(1500, 4000, 12, 3);
        let started = std::time::Instant::now();
        let result = layout(&input);
        let elapsed = started.elapsed();
        assert_eq!(result.nodes.len(), 1500);
        assert_no_overlaps(&result.nodes);
        // Debug builds are several times slower than release.
        assert!(elapsed.as_secs_f32() < 5., "layout took {elapsed:?}");
        // Twelve group bands wrap into columns instead of one tall stack.
        let ratio = result.bounds.w / result.bounds.h;
        assert!((0.5..4.).contains(&ratio), "aspect {ratio}");
    }

    #[test]
    fn clipping_keeps_only_curve_parts_in_view() {
        let long = [
            Pos::new(0., 0.),
            Pos::new(500., 0.),
            Pos::new(500., 10_000.),
            Pos::new(1000., 10_000.),
        ];
        let view = Rect::new(400., 4000., 200., 200.);
        let mut out = Vec::new();
        clip_bezier(&long, &view, &mut out);
        assert!(!out.is_empty() && out.len() < 8, "{} pieces", out.len());
        // The kept pieces join up and contain the curve point in the view.
        let hit = (0..=100)
            .map(|i| bezier_at(&long, i as f32 / 100.))
            .find(|p| view.contains(*p))
            .expect("curve crosses the view");
        assert!(out.iter().any(|piece| {
            (0..=50).any(|i| {
                let p = bezier_at(piece, i as f32 / 50.);
                (p.x - hit.x).abs() < 60. && (p.y - hit.y).abs() < 60.
            })
        }));
        let mut none = Vec::new();
        clip_bezier(&long, &Rect::new(-500., 5000., 100., 100.), &mut none);
        assert!(none.is_empty());
        // A curve inside the view stays whole.
        let mut whole = Vec::new();
        clip_bezier(&long, &Rect::new(-10., -10., 2000., 20_000.), &mut whole);
        assert_eq!(whole, vec![long]);
    }

    #[test]
    fn empty_graph() {
        let mut input = input(0, &[(0, 1)]);
        input.groups = 2;
        let result = layout(&input);
        assert!(result.nodes.is_empty());
        assert_eq!(result.edges, vec![None]);
        assert_eq!(result.groups, vec![None, None]);
    }
}
