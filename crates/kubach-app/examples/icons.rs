//! Renders the app icon from `assets/icon/kubach.svg` into the files the
//! platforms use: `kubach.icns` (macOS), `kubach.ico` (Windows),
//! `kubach-512.png` (Linux, and the macOS Dock icon outside a bundle).
//!
//! The cubes sit on a rounded dark tile laid out on Apple's icon grid: a
//! 824 px tile with a 100 px margin on a 1024 px canvas. The Dock draws an
//! icon set at run time over the whole slot like a bundle's, so it gets the
//! same margin and matches the icons next to it.
//!
//! `cargo run -p kubach-app --example icons`

use std::path::Path;

use resvg::tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path as SkPath, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, Transform,
};
use resvg::usvg::{Options, Tree};

/// ICNS entries: OSType and side in pixels, PNG-encoded.
const ICNS: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// ICO entries, PNG-encoded (Windows Vista and later).
const ICO: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// The icon grid, in pixels of a 1024 px canvas.
const GRID: f32 = 1024.;
const TILE_MARGIN: f32 = 100.;
const TILE_RADIUS: f32 = 185.;
/// Width of the cubes on the grid, and their bounds in the SVG.
const CUBES_WIDTH: f32 = 560.;
const CUBES_MIN: (f32, f32) = (36., 54.);
const CUBES_SPAN: (f32, f32) = (440., 442.);

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
    let svg = std::fs::read(dir.join("kubach.svg")).expect("read kubach.svg");
    let tree = Tree::from_data(&svg, &Options::default()).expect("parse kubach.svg");

    std::fs::write(dir.join("kubach-512.png"), render(&tree, 512)).expect("write png");
    std::fs::write(dir.join("kubach.icns"), icns(&tree)).expect("write icns");
    std::fs::write(dir.join("kubach.ico"), ico(&tree)).expect("write ico");
}

/// The icon as a `side` × `side` PNG: the tile, then the cubes on it.
fn render(tree: &Tree, side: u32) -> Vec<u8> {
    let mut pixmap = Pixmap::new(side, side).expect("non-zero size");
    let grid = Transform::from_scale(side as f32 / GRID, side as f32 / GRID);

    let tile = tile_path();
    let paint = Paint {
        shader: LinearGradient::new(
            Point::from_xy(0., TILE_MARGIN),
            Point::from_xy(0., GRID - TILE_MARGIN),
            vec![
                GradientStop::new(0., Color::from_rgba8(0x2e, 0x2e, 0x2e, 0xff)),
                GradientStop::new(1., Color::from_rgba8(0x0a, 0x0a, 0x0a, 0xff)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .expect("two stops"),
        anti_alias: true,
        ..Paint::default()
    };
    pixmap.fill_path(&tile, &paint, FillRule::Winding, grid, None);
    let mut edge = Paint::default();
    edge.set_color_rgba8(0xff, 0xff, 0xff, 0x24);
    edge.anti_alias = true;
    let stroke = Stroke {
        width: 4.,
        ..Stroke::default()
    };
    pixmap.stroke_path(&tile, &edge, &stroke, grid, None);

    // Cubes centred on the tile.
    let scale = CUBES_WIDTH / CUBES_SPAN.0;
    let cubes = Transform::from_translate(
        (GRID - CUBES_SPAN.0 * scale) / 2. - CUBES_MIN.0 * scale,
        (GRID - CUBES_SPAN.1 * scale) / 2. - CUBES_MIN.1 * scale,
    )
    .pre_scale(scale, scale)
    .post_concat(grid);
    resvg::render(tree, cubes, &mut pixmap.as_mut());
    pixmap.encode_png().expect("encode png")
}

/// The rounded tile on the grid.
fn tile_path() -> SkPath {
    // Control point offset of a quarter circle drawn as a cubic.
    const K: f32 = 0.552_284_8;
    let (lo, hi, r) = (TILE_MARGIN, GRID - TILE_MARGIN, TILE_RADIUS);
    let c = r * (1. - K);
    let mut path = PathBuilder::new();
    path.move_to(lo + r, lo);
    path.line_to(hi - r, lo);
    path.cubic_to(hi - c, lo, hi, lo + c, hi, lo + r);
    path.line_to(hi, hi - r);
    path.cubic_to(hi, hi - c, hi - c, hi, hi - r, hi);
    path.line_to(lo + r, hi);
    path.cubic_to(lo + c, hi, lo, hi - c, lo, hi - r);
    path.line_to(lo, lo + r);
    path.cubic_to(lo, lo + c, lo + c, lo, lo + r, lo);
    path.close();
    path.finish().expect("non-empty path")
}

/// Apple icon image: a big-endian header, then type, length and PNG per entry.
fn icns(tree: &Tree) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, side) in ICNS {
        let png = render(tree, side);
        body.extend_from_slice(kind);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&png);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Windows icon: a little-endian directory of PNG images.
fn ico(tree: &Tree) -> Vec<u8> {
    let images: Vec<_> = ICO.iter().map(|&side| (side, render(tree, side))).collect();
    let mut offset = 6 + 16 * images.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (side, png) in &images {
        // 256 is stored as 0.
        let side = (*side % 256) as u8;
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    out
}
