//! Box drawing, block elements, braille and powerline symbols drawn as
//! shapes over the whole cell instead of font glyphs, so that lines and
//! blocks of neighbouring cells meet without gaps whatever the line height.
//!
//! Pure geometry without GPUI: [`draw`] turns a symbol and the cell
//! rectangle (in device pixels, ideally snapped to whole pixels) into
//! rectangles and polygons handed to a [`Canvas`]. Line thickness follows
//! the cell size and every edge is rounded to a whole pixel, so equal cells
//! put equal lines in the same place.

use std::f32::consts::FRAC_PI_2;

/// A point `[x, y]` in device pixels.
pub type Pt = [f32; 2];

/// An axis-aligned rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Receives the shapes of a symbol, all in the symbol's color.
pub trait Canvas {
    /// A filled rectangle; `alpha` scales the color's opacity (shades).
    fn rect(&mut self, rect: Rect, alpha: f32);
    /// A filled polygon (may be concave, never self-intersecting).
    fn polygon(&mut self, points: &[Pt]);
}

/// Whether `c` is drawn by [`draw`] rather than by the font.
pub fn is_builtin(c: char) -> bool {
    matches!(
        c,
        '\u{2500}'..='\u{259F}' | '\u{2800}'..='\u{28FF}' | '\u{E0B0}'..='\u{E0B7}'
    )
}

/// Draws `c` over `cell`. Returns `false` (drawing nothing) for symbols
/// that are left to the font.
pub fn draw(c: char, cell: Rect, canvas: &mut dyn Canvas) -> bool {
    let g = Geometry::new(cell);
    match c as u32 {
        0x2500..=0x257F => g.box_drawing(c, canvas),
        0x2580..=0x259F => g.block(c as u32, canvas),
        0x2800..=0x28FF => g.braille(c as u32 - 0x2800, canvas),
        0xE0B0..=0xE0B7 => g.powerline(c as u32, canvas),
        _ => return false,
    }
    true
}

/// Line weights of a box drawing arm.
const N: u8 = 0;
const L: u8 = 1;
const H: u8 = 2;
const D: u8 = 3;

/// Arms `[up, right, down, left]` of the plain line symbols.
#[rustfmt::skip]
fn box_arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [N, L, N, L], '━' => [N, H, N, H], '│' => [L, N, L, N], '┃' => [H, N, H, N],
        '┌' => [N, L, L, N], '┍' => [N, H, L, N], '┎' => [N, L, H, N], '┏' => [N, H, H, N],
        '┐' => [N, N, L, L], '┑' => [N, N, L, H], '┒' => [N, N, H, L], '┓' => [N, N, H, H],
        '└' => [L, L, N, N], '┕' => [L, H, N, N], '┖' => [H, L, N, N], '┗' => [H, H, N, N],
        '┘' => [L, N, N, L], '┙' => [L, N, N, H], '┚' => [H, N, N, L], '┛' => [H, N, N, H],
        '├' => [L, L, L, N], '┝' => [L, H, L, N], '┞' => [H, L, L, N], '┟' => [L, L, H, N],
        '┠' => [H, L, H, N], '┡' => [H, H, L, N], '┢' => [L, H, H, N], '┣' => [H, H, H, N],
        '┤' => [L, N, L, L], '┥' => [L, N, L, H], '┦' => [H, N, L, L], '┧' => [L, N, H, L],
        '┨' => [H, N, H, L], '┩' => [H, N, L, H], '┪' => [L, N, H, H], '┫' => [H, N, H, H],
        '┬' => [N, L, L, L], '┭' => [N, L, L, H], '┮' => [N, H, L, L], '┯' => [N, H, L, H],
        '┰' => [N, L, H, L], '┱' => [N, L, H, H], '┲' => [N, H, H, L], '┳' => [N, H, H, H],
        '┴' => [L, L, N, L], '┵' => [L, L, N, H], '┶' => [L, H, N, L], '┷' => [L, H, N, H],
        '┸' => [H, L, N, L], '┹' => [H, L, N, H], '┺' => [H, H, N, L], '┻' => [H, H, N, H],
        '┼' => [L, L, L, L], '┽' => [L, L, L, H], '┾' => [L, H, L, L], '┿' => [L, H, L, H],
        '╀' => [H, L, L, L], '╁' => [L, L, H, L], '╂' => [H, L, H, L], '╃' => [H, L, L, H],
        '╄' => [H, H, L, L], '╅' => [L, L, H, H], '╆' => [L, H, H, L], '╇' => [H, H, L, H],
        '╈' => [L, H, H, H], '╉' => [H, L, H, H], '╊' => [H, H, H, L], '╋' => [H, H, H, H],
        '═' => [N, D, N, D], '║' => [D, N, D, N],
        '╒' => [N, D, L, N], '╓' => [N, L, D, N], '╔' => [N, D, D, N],
        '╕' => [N, N, L, D], '╖' => [N, N, D, L], '╗' => [N, N, D, D],
        '╘' => [L, D, N, N], '╙' => [D, L, N, N], '╚' => [D, D, N, N],
        '╛' => [L, N, N, D], '╜' => [D, N, N, L], '╝' => [D, N, N, D],
        '╞' => [L, D, L, N], '╟' => [D, L, D, N], '╠' => [D, D, D, N],
        '╡' => [L, N, L, D], '╢' => [D, N, D, L], '╣' => [D, N, D, D],
        '╤' => [N, D, L, D], '╥' => [N, L, D, L], '╦' => [N, D, D, D],
        '╧' => [L, D, N, D], '╨' => [D, L, N, L], '╩' => [D, D, N, D],
        '╪' => [L, D, L, D], '╫' => [D, L, D, L], '╬' => [D, D, D, D],
        '╴' => [N, N, N, L], '╵' => [L, N, N, N], '╶' => [N, L, N, N], '╷' => [N, N, L, N],
        '╸' => [N, N, N, H], '╹' => [H, N, N, N], '╺' => [N, H, N, N], '╻' => [N, N, H, N],
        '╼' => [N, H, N, L], '╽' => [L, N, H, N], '╾' => [N, L, N, H], '╿' => [H, N, L, N],
        _ => return None,
    })
}

type Span = (f32, f32);

/// `t` wide span centered in `len` from `start`, on whole pixels.
fn span(start: f32, len: f32, t: f32) -> Span {
    let a = start + ((len - t) / 2.0).floor();
    (a, a + t)
}

/// Where the strokes of one axis (vertical or horizontal arms) lie across it.
struct Axis {
    /// The two strokes of a double line.
    double: Option<(Span, Span)>,
    /// The thickest light or heavy stroke.
    single: Span,
    /// Outer extent of the strokes, if the axis has any arm.
    far: Option<Span>,
}

/// A polygon on the stack.
struct Poly {
    points: [Pt; 40],
    len: usize,
}

impl Poly {
    fn new() -> Self {
        Self {
            points: [[0.0; 2]; 40],
            len: 0,
        }
    }

    fn push(&mut self, x: f32, y: f32) {
        if self.len < self.points.len() {
            self.points[self.len] = [x, y];
            self.len += 1;
        }
    }

    fn points(&self) -> &[Pt] {
        &self.points[..self.len]
    }
}

/// Segments of a quarter (arc corners) or half (powerline) curve.
const CURVE_STEPS: usize = 16;

struct Geometry {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    w: f32,
    h: f32,
    light: f32,
    heavy: f32,
}

impl Geometry {
    fn new(cell: Rect) -> Self {
        let light = (cell.w.min(cell.h) / 8.0).round().max(1.0);
        Self {
            x0: cell.x,
            y0: cell.y,
            x1: cell.right(),
            y1: cell.bottom(),
            w: cell.w,
            h: cell.h,
            light,
            heavy: light * 2.0,
        }
    }

    fn thickness(&self, weight: u8) -> f32 {
        match weight {
            H => self.heavy,
            D => self.light * 3.0,
            _ => self.light,
        }
    }

    /// Fills `x0..x1 × y0..y1`, skipping empty rectangles.
    fn fill(canvas: &mut dyn Canvas, x0: f32, y0: f32, x1: f32, y1: f32, alpha: f32) {
        if x1 > x0 && y1 > y0 {
            canvas.rect(Rect::new(x0, y0, x1 - x0, y1 - y0), alpha);
        }
    }

    /// Strokes across `len` from `start` for arms of weights `a` and `b`.
    fn axis(&self, start: f32, len: f32, a: u8, b: u8) -> Axis {
        let double = (a == D || b == D).then(|| {
            let s = start + ((len - self.light * 3.0) / 2.0).floor();
            (
                (s, s + self.light),
                (s + self.light * 2.0, s + self.light * 3.0),
            )
        });
        let t = [a, b]
            .into_iter()
            .filter(|w| *w == L || *w == H)
            .map(|w| self.thickness(w))
            .fold(self.light, f32::max);
        let single = span(start, len, t);
        let far = (a != N || b != N).then_some(match double {
            Some((first, second)) => (first.0, second.1),
            None => single,
        });
        Axis {
            double,
            single,
            far,
        }
    }

    fn box_drawing(&self, c: char, canvas: &mut dyn Canvas) {
        match c {
            '\u{2504}'..='\u{250B}' | '\u{254C}'..='\u{254F}' => self.dashed(c as u32, canvas),
            '\u{256D}'..='\u{2570}' => self.arc(c as u32, canvas),
            '\u{2571}'..='\u{2573}' => self.diagonal(c as u32, canvas),
            _ => {
                if let Some(arms) = box_arms(c) {
                    self.lines(arms, canvas);
                }
            }
        }
    }

    fn lines(&self, [up, right, down, left]: [u8; 4], canvas: &mut dyn Canvas) {
        let (x0, y0, x1, y1, w, h) = (self.x0, self.y0, self.x1, self.y1, self.w, self.h);
        // Vertical arms lie at x positions, horizontal ones at y positions.
        let v = self.axis(x0, w, up, down);
        let hz = self.axis(y0, h, left, right);
        let mid_x = |weight: u8| span(x0, w, self.thickness(weight));
        let mid_y = |weight: u8| span(y0, h, self.thickness(weight));

        // Left arm: ends where the vertical strokes are.
        match left {
            N => {}
            D => {
                let ((top, bottom), (vl, _)) = (hz.double.unwrap(), v.double.unwrap_or_default());
                let end = |side: u8| match side {
                    D => vl.1,
                    N => v.far.map_or(mid_x(D).1, |f| f.1),
                    _ => v.single.1,
                };
                Self::fill(canvas, x0, top.0, end(up), top.1, 1.0);
                Self::fill(canvas, x0, bottom.0, end(down), bottom.1, 1.0);
            }
            weight => {
                let ys = mid_y(weight);
                let end = v.far.map_or(mid_x(weight).1, |f| f.1);
                Self::fill(canvas, x0, ys.0, end, ys.1, 1.0);
            }
        }
        match right {
            N => {}
            D => {
                let ((top, bottom), (_, vr)) = (hz.double.unwrap(), v.double.unwrap_or_default());
                let start = |side: u8| match side {
                    D => vr.0,
                    N => v.far.map_or(mid_x(D).0, |f| f.0),
                    _ => v.single.0,
                };
                Self::fill(canvas, start(up), top.0, x1, top.1, 1.0);
                Self::fill(canvas, start(down), bottom.0, x1, bottom.1, 1.0);
            }
            weight => {
                let ys = mid_y(weight);
                let start = v.far.map_or(mid_x(weight).0, |f| f.0);
                Self::fill(canvas, start, ys.0, x1, ys.1, 1.0);
            }
        }
        match up {
            N => {}
            D => {
                let ((vl, vr), (top, _)) = (v.double.unwrap(), hz.double.unwrap_or_default());
                let end = |side: u8| match side {
                    D => top.1,
                    N => hz.far.map_or(mid_y(D).1, |f| f.1),
                    _ => hz.single.1,
                };
                Self::fill(canvas, vl.0, y0, vl.1, end(left), 1.0);
                Self::fill(canvas, vr.0, y0, vr.1, end(right), 1.0);
            }
            weight => {
                let xs = mid_x(weight);
                let end = hz.far.map_or(mid_y(weight).1, |f| f.1);
                Self::fill(canvas, xs.0, y0, xs.1, end, 1.0);
            }
        }
        match down {
            N => {}
            D => {
                let ((vl, vr), (_, bottom)) = (v.double.unwrap(), hz.double.unwrap_or_default());
                let start = |side: u8| match side {
                    D => bottom.0,
                    N => hz.far.map_or(mid_y(D).0, |f| f.0),
                    _ => hz.single.0,
                };
                Self::fill(canvas, vl.0, start(left), vl.1, y1, 1.0);
                Self::fill(canvas, vr.0, start(right), vr.1, y1, 1.0);
            }
            weight => {
                let xs = mid_x(weight);
                let start = hz.far.map_or(mid_y(weight).0, |f| f.0);
                Self::fill(canvas, xs.0, start, xs.1, y1, 1.0);
            }
        }
    }

    /// `┄┅┆┇┈┉┊┋╌╍╎╏`: a line split into equal dashes.
    fn dashed(&self, c: u32, canvas: &mut dyn Canvas) {
        // Light horizontal, heavy horizontal, light vertical, heavy vertical.
        let (count, variant) = match c {
            0x2504..=0x2507 => (3.0, c - 0x2504),
            0x2508..=0x250B => (4.0, c - 0x2508),
            _ => (2.0, c - 0x254C),
        };
        let heavy = variant % 2 == 1;
        let vertical = variant >= 2;
        let t = if heavy { self.heavy } else { self.light };
        let (start, len) = if vertical {
            (self.y0, self.h)
        } else {
            (self.x0, self.w)
        };
        let across = if vertical {
            span(self.x0, self.w, t)
        } else {
            span(self.y0, self.h, t)
        };
        let gap = (len / count / 4.0).round().max(1.0);
        let lead = (gap / 2.0).floor();
        for i in 0..count as usize {
            let a = start + (len * i as f32 / count).round() + lead;
            let b = start + (len * (i + 1) as f32 / count).round() - (gap - lead);
            if vertical {
                Self::fill(canvas, across.0, a, across.1, b, 1.0);
            } else {
                Self::fill(canvas, a, across.0, b, across.1, 1.0);
            }
        }
    }

    /// `╭╮╯╰`: a quarter circle joining the middle of two cell edges, lined
    /// up with the straight light lines.
    fn arc(&self, c: u32, canvas: &mut dyn Canvas) {
        let (dx, dy) = match c {
            0x256D => (1.0, 1.0),
            0x256E => (-1.0, 1.0),
            0x256F => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let t = self.light;
        let ht = t / 2.0;
        let vs = span(self.x0, self.w, t);
        let hs = span(self.y0, self.h, t);
        let (vmid, hmid) = ((vs.0 + vs.1) / 2.0, (hs.0 + hs.1) / 2.0);
        let edge_x = if dx > 0.0 { self.x1 } else { self.x0 };
        let edge_y = if dy > 0.0 { self.y1 } else { self.y0 };
        let r = (edge_x - vmid).abs().min((edge_y - hmid).abs());
        let (cx, cy) = (vmid + dx * r, hmid + dy * r);
        let mut poly = Poly::new();
        poly.push(vmid - dx * ht, edge_y);
        let rho = r + ht;
        for i in 0..=CURVE_STEPS {
            let phi = FRAC_PI_2 * i as f32 / CURVE_STEPS as f32;
            poly.push(cx - dx * rho * phi.cos(), cy - dy * rho * phi.sin());
        }
        poly.push(edge_x, hmid - dy * ht);
        poly.push(edge_x, hmid + dy * ht);
        let rho = (r - ht).max(0.0);
        for i in (0..=CURVE_STEPS).rev() {
            let phi = FRAC_PI_2 * i as f32 / CURVE_STEPS as f32;
            poly.push(cx - dx * rho * phi.cos(), cy - dy * rho * phi.sin());
        }
        poly.push(vmid + dx * ht, edge_y);
        canvas.polygon(poly.points());
    }

    /// `╱╲╳`: corner-to-corner lines cut to the cell.
    fn diagonal(&self, c: u32, canvas: &mut dyn Canvas) {
        let (x0, y0, x1, y1) = (self.x0, self.y0, self.x1, self.y1);
        // Half the horizontal width of a line `light` thick across.
        let d = self.light * self.w.hypot(self.h) / self.h / 2.0;
        let e = d * self.h / self.w;
        if c != 0x2572 {
            let mut poly = Poly::new();
            for [x, y] in [
                [x1 - d, y0],
                [x1, y0],
                [x1, y0 + e],
                [x0 + d, y1],
                [x0, y1],
                [x0, y1 - e],
            ] {
                poly.push(x, y);
            }
            canvas.polygon(poly.points());
        }
        if c != 0x2571 {
            let mut poly = Poly::new();
            for [x, y] in [
                [x0, y0],
                [x0 + d, y0],
                [x1, y1 - e],
                [x1, y1],
                [x1 - d, y1],
                [x0, y0 + e],
            ] {
                poly.push(x, y);
            }
            canvas.polygon(poly.points());
        }
    }

    /// Edge `k/8` of the way down the cell.
    fn eighth_y(&self, k: f32) -> f32 {
        self.y0 + (self.h * k / 8.0).round()
    }

    /// Edge `k/8` of the way across the cell.
    fn eighth_x(&self, k: f32) -> f32 {
        self.x0 + (self.w * k / 8.0).round()
    }

    fn block(&self, c: u32, canvas: &mut dyn Canvas) {
        let (x0, y0, x1, y1) = (self.x0, self.y0, self.x1, self.y1);
        match c {
            0x2580 => Self::fill(canvas, x0, y0, x1, self.eighth_y(4.0), 1.0),
            0x2581..=0x2588 => {
                let k = (c - 0x2580) as f32;
                Self::fill(canvas, x0, self.eighth_y(8.0 - k), x1, y1, 1.0);
            }
            0x2589..=0x258F => {
                let k = (8 - (c - 0x2588)) as f32;
                Self::fill(canvas, x0, y0, self.eighth_x(k), y1, 1.0);
            }
            0x2590 => Self::fill(canvas, self.eighth_x(4.0), y0, x1, y1, 1.0),
            0x2591..=0x2593 => {
                let alpha = (c - 0x2590) as f32 / 4.0;
                Self::fill(canvas, x0, y0, x1, y1, alpha);
            }
            0x2594 => Self::fill(canvas, x0, y0, x1, self.eighth_y(1.0), 1.0),
            0x2595 => Self::fill(canvas, self.eighth_x(7.0), y0, x1, y1, 1.0),
            _ => {
                // Quadrants: bits upper-left, upper-right, lower-left, lower-right.
                let quadrants: u8 = match c {
                    0x2596 => 0b0100,
                    0x2597 => 0b1000,
                    0x2598 => 0b0001,
                    0x2599 => 0b1101,
                    0x259A => 0b1001,
                    0x259B => 0b0111,
                    0x259C => 0b1011,
                    0x259D => 0b0010,
                    0x259E => 0b0110,
                    _ => 0b1110,
                };
                let (mx, my) = (self.eighth_x(4.0), self.eighth_y(4.0));
                let boxes = [
                    (x0, y0, mx, my),
                    (mx, y0, x1, my),
                    (x0, my, mx, y1),
                    (mx, my, x1, y1),
                ];
                for (i, (a, b, c, d)) in boxes.into_iter().enumerate() {
                    if quadrants & (1 << i) != 0 {
                        Self::fill(canvas, a, b, c, d, 1.0);
                    }
                }
            }
        }
    }

    /// Braille dots on a 2×4 grid; `bits` is the offset from U+2800.
    fn braille(&self, bits: u32, canvas: &mut dyn Canvas) {
        const DOTS: [(f32, f32); 8] = [
            (0.0, 0.0),
            (0.0, 1.0),
            (0.0, 2.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 3.0),
            (1.0, 3.0),
        ];
        let (sub_w, sub_h) = (self.w / 2.0, self.h / 4.0);
        let dot = (sub_w.min(sub_h) * 0.6).round().max(1.0);
        for (i, (col, row)) in DOTS.into_iter().enumerate() {
            if bits & (1 << i) == 0 {
                continue;
            }
            let x = self.x0 + (col * sub_w + (sub_w - dot) / 2.0).round();
            let y = self.y0 + (row * sub_h + (sub_h - dot) / 2.0).round();
            Self::fill(canvas, x, y, x + dot, y + dot, 1.0);
        }
    }

    /// Powerline separators: triangles, chevrons and half circles.
    fn powerline(&self, c: u32, canvas: &mut dyn Canvas) {
        let (x0, y0, x1, y1) = (self.x0, self.y0, self.x1, self.y1);
        let ymid = y0 + self.h / 2.0;
        let mut poly = Poly::new();
        // Mirrors x for the left-pointing shapes.
        let flip = (c - 0xE0B0) & 2 != 0;
        let fx = |x: f32| if flip { x0 + x1 - x } else { x };
        match c {
            0xE0B0 | 0xE0B2 => {
                for [x, y] in [[x0, y0], [x1, ymid], [x0, y1]] {
                    poly.push(fx(x), y);
                }
            }
            0xE0B1 | 0xE0B3 => {
                let dx = (self.light * self.w.hypot(self.h / 2.0) / (self.h / 2.0)).min(self.w);
                for [x, y] in [
                    [x0, y0],
                    [x0 + dx, y0],
                    [x1, ymid],
                    [x0 + dx, y1],
                    [x0, y1],
                    [x1 - dx, ymid],
                ] {
                    poly.push(fx(x), y);
                }
            }
            _ => {
                let (rx, ry) = (self.w, self.h / 2.0);
                let outline = c == 0xE0B5 || c == 0xE0B7;
                let half = |poly: &mut Poly, rx: f32, ry: f32, forward: bool| {
                    for i in 0..=CURVE_STEPS {
                        let i = if forward { i } else { CURVE_STEPS - i };
                        let a = -FRAC_PI_2 + std::f32::consts::PI * i as f32 / CURVE_STEPS as f32;
                        poly.push(fx(x0 + rx * a.cos()), ymid + ry * a.sin());
                    }
                };
                half(&mut poly, rx, ry, true);
                if outline {
                    let t = self.light;
                    half(&mut poly, (rx - t).max(0.0), (ry - t).max(0.0), false);
                }
            }
        }
        canvas.polygon(poly.points());
    }
}

#[cfg(test)]
mod tests {
    use super::{Canvas, Pt, Rect, draw, is_builtin};

    #[derive(Default)]
    struct Shapes {
        rects: Vec<(Rect, f32)>,
        polygons: Vec<Vec<Pt>>,
    }

    impl Canvas for Shapes {
        fn rect(&mut self, rect: Rect, alpha: f32) {
            self.rects.push((rect, alpha));
        }

        fn polygon(&mut self, points: &[Pt]) {
            self.polygons.push(points.to_vec());
        }
    }

    const CELL: Rect = Rect {
        x: 16.0,
        y: 36.0,
        w: 16.0,
        h: 35.0,
    };

    fn shapes(c: char, cell: Rect) -> Shapes {
        let mut shapes = Shapes::default();
        assert!(draw(c, cell, &mut shapes), "{c:?} is not drawn");
        shapes
    }

    fn supported() -> impl Iterator<Item = char> {
        (0x2500..=0x259F)
            .chain(0x2800..=0x28FF)
            .chain(0xE0B0..=0xE0B7)
            .filter_map(char::from_u32)
    }

    #[test]
    fn knows_which_symbols_it_draws() {
        for c in supported() {
            assert!(is_builtin(c), "{c:?}");
        }
        for c in [
            'a', ' ', '\u{24FF}', '\u{25A0}', '\u{27FF}', '\u{2900}', '\u{E0AF}', '\u{E0B8}',
        ] {
            assert!(!is_builtin(c), "{c:?}");
            assert!(!draw(c, CELL, &mut Shapes::default()));
        }
    }

    #[test]
    fn every_symbol_stays_inside_its_cell() {
        let cells = [
            CELL,
            Rect::new(0.0, 0.0, 8.0, 18.0),
            Rect::new(7.0, 17.0, 7.0, 17.0),
            Rect::new(100.0, 50.0, 23.0, 53.0),
        ];
        for cell in cells {
            for c in supported() {
                let shapes = shapes(c, cell);
                let empty = c == '\u{2800}';
                assert_eq!(
                    shapes.rects.is_empty() && shapes.polygons.is_empty(),
                    empty,
                    "{c:?} in {cell:?}"
                );
                let inside = |x: f32, y: f32| {
                    x >= cell.x - 1e-3
                        && x <= cell.right() + 1e-3
                        && y >= cell.y - 1e-3
                        && y <= cell.bottom() + 1e-3
                };
                for (rect, alpha) in &shapes.rects {
                    assert!(rect.w > 0.0 && rect.h > 0.0, "{c:?} empty rect");
                    assert!(*alpha > 0.0 && *alpha <= 1.0);
                    assert!(inside(rect.x, rect.y), "{c:?} {rect:?} in {cell:?}");
                    assert!(
                        inside(rect.right(), rect.bottom()),
                        "{c:?} {rect:?} in {cell:?}"
                    );
                }
                for polygon in &shapes.polygons {
                    assert!(polygon.len() >= 3, "{c:?}");
                    for &[x, y] in polygon {
                        assert!(inside(x, y), "{c:?} ({x}, {y}) in {cell:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn edges_are_on_whole_pixels() {
        for c in ('\u{2500}'..='\u{2570}').chain('\u{2574}'..='\u{259F}') {
            for (rect, _) in shapes(c, CELL).rects {
                for v in [rect.x, rect.y, rect.w, rect.h] {
                    assert_eq!(v, v.round(), "{c:?} {rect:?}");
                }
            }
        }
    }

    /// Horizontal y-span of a light line, vertical x-span of a light line.
    fn light_spans() -> ((f32, f32), (f32, f32)) {
        let h = shapes('─', CELL).rects[0].0;
        let v = shapes('│', CELL).rects[0].0;
        ((h.y, h.bottom()), (v.x, v.right()))
    }

    #[test]
    fn lines_cross_the_whole_cell_and_line_up() {
        let (hy, vx) = light_spans();
        let covered = |c: char| {
            let rects = shapes(c, CELL).rects;
            let x0 = rects.iter().map(|r| r.0.x).fold(f32::MAX, f32::min);
            let x1 = rects.iter().map(|r| r.0.right()).fold(f32::MIN, f32::max);
            let y0 = rects.iter().map(|r| r.0.y).fold(f32::MAX, f32::min);
            let y1 = rects.iter().map(|r| r.0.bottom()).fold(f32::MIN, f32::max);
            (x0, x1, y0, y1)
        };
        assert_eq!(covered('─'), (CELL.x, CELL.right(), hy.0, hy.1));
        assert_eq!(covered('│'), (vx.0, vx.1, CELL.y, CELL.bottom()));
        assert_eq!(covered('┼'), (CELL.x, CELL.right(), CELL.y, CELL.bottom()));
        // Corners and tees keep the light lines where the straight ones are.
        for c in ['┌', '┐', '└', '┘', '├', '┤', '┬', '┴'] {
            for (rect, _) in shapes(c, CELL).rects {
                let horizontal = (rect.y, rect.bottom()) == hy;
                let vertical = (rect.x, rect.right()) == vx;
                assert!(horizontal || vertical, "{c:?} {rect:?}");
            }
        }
        // A heavy line is thicker and centered over the light one.
        let heavy = shapes('━', CELL).rects[0].0;
        assert!(heavy.h > hy.1 - hy.0);
        assert!(heavy.y <= hy.0 && heavy.bottom() >= hy.1);
    }

    #[test]
    fn double_lines_have_two_strokes() {
        assert_eq!(shapes('═', CELL).rects.len(), 4);
        assert_eq!(shapes('║', CELL).rects.len(), 4);
        let cross = shapes('╬', CELL).rects;
        assert_eq!(cross.len(), 8);
        // The middle of `╬` stays empty.
        let center = (CELL.x + CELL.w / 2.0, CELL.y + CELL.h / 2.0);
        assert!(!cross.iter().any(|(r, _)| {
            r.x < center.0 && r.right() > center.0 && r.y < center.1 && r.bottom() > center.1
        }));
        assert_eq!(shapes('┄', CELL).rects.len(), 3);
        assert_eq!(shapes('┊', CELL).rects.len(), 4);
        assert_eq!(shapes('╍', CELL).rects.len(), 2);
    }

    #[test]
    fn rounded_corners_meet_straight_lines() {
        let (hy, vx) = light_spans();
        let arc = &shapes('╭', CELL).polygons[0];
        // Starts on the bottom edge where `│` is, ends on the right edge where `─` is.
        assert!(arc.contains(&[vx.0, CELL.bottom()]) && arc.contains(&[vx.1, CELL.bottom()]));
        assert!(arc.contains(&[CELL.right(), hy.0]) && arc.contains(&[CELL.right(), hy.1]));
        let arc = &shapes('╯', CELL).polygons[0];
        assert!(arc.contains(&[vx.0, CELL.y]) && arc.contains(&[vx.1, CELL.y]));
        assert!(arc.contains(&[CELL.x, hy.0]) && arc.contains(&[CELL.x, hy.1]));
        assert_eq!(shapes('╳', CELL).polygons.len(), 2);
    }

    #[test]
    fn blocks_split_the_cell_without_gaps() {
        let full = shapes('█', CELL).rects;
        assert_eq!(full, vec![(CELL, 1.0)]);
        let upper = shapes('▀', CELL).rects[0].0;
        let lower = shapes('▄', CELL).rects[0].0;
        assert_eq!(upper.y, CELL.y);
        assert_eq!(upper.bottom(), lower.y);
        assert_eq!(lower.bottom(), CELL.bottom());
        let left = shapes('▌', CELL).rects[0].0;
        let right = shapes('▐', CELL).rects[0].0;
        assert_eq!(left.right(), right.x);
        let eighth = shapes('▁', CELL).rects[0].0;
        assert_eq!(eighth.bottom(), CELL.bottom());
        assert!(eighth.h < lower.h);
        assert_eq!(shapes('▏', CELL).rects[0].0.x, CELL.x);
        assert_eq!(shapes('▙', CELL).rects.len(), 3);
        assert_eq!(shapes('▚', CELL).rects.len(), 2);
        let alphas: Vec<f32> = ['░', '▒', '▓']
            .into_iter()
            .map(|c| shapes(c, CELL).rects[0].1)
            .collect();
        assert_eq!(alphas, vec![0.25, 0.5, 0.75]);
    }

    #[test]
    fn braille_dots_follow_the_bits() {
        assert_eq!(shapes('⣿', CELL).rects.len(), 8);
        assert!(shapes('⠀', CELL).rects.is_empty());
        let dot = shapes('⠁', CELL).rects[0].0;
        assert!(dot.right() <= CELL.x + CELL.w / 2.0 && dot.bottom() <= CELL.y + CELL.h / 4.0);
        let dot = shapes('⢀', CELL).rects[0].0;
        assert!(dot.x >= CELL.x + CELL.w / 2.0 && dot.y >= CELL.y + CELL.h * 3.0 / 4.0);
    }

    #[test]
    fn powerline_arrows_span_the_cell() {
        let mid = CELL.y + CELL.h / 2.0;
        let right = &shapes('\u{E0B0}', CELL).polygons[0];
        assert_eq!(
            right,
            &vec![
                [CELL.x, CELL.y],
                [CELL.right(), mid],
                [CELL.x, CELL.bottom()]
            ]
        );
        let left = &shapes('\u{E0B2}', CELL).polygons[0];
        assert!(left.contains(&[CELL.x, mid]) && left.contains(&[CELL.right(), CELL.y]));
        for c in [
            '\u{E0B1}', '\u{E0B3}', '\u{E0B4}', '\u{E0B5}', '\u{E0B6}', '\u{E0B7}',
        ] {
            assert_eq!(shapes(c, CELL).polygons.len(), 1, "{c:?}");
        }
    }
}
