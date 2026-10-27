//! A [`GraphView`](super::GraphView) graph as a standalone SVG (#327):
//! the same boxes, curves, arrowheads, labels and group bands, at the
//! coordinates of the layout, in the colors of a [`SvgPalette`] (taken from
//! the current theme). [`svg_to_png`] rasterizes it.

use std::collections::HashSet;
use std::fmt::Write as _;

use gpui_kit::component::Theme;
use gpui_kit::{Hsla, Rgba, SharedString};

use super::labels::{self, LABEL_FONT, PlacedLabel};
use super::layout::{self, Layout, Pos, Rect};
use super::model::{EdgeStyle, GraphData, Item, Visible};
use super::view::layout_input;
use crate::tokens::{TINT_OPACITY, Tone};

/// Space around the drawing.
const MARGIN: f32 = 24.;
/// Average glyph width in ems, to cut text that would overflow its box.
const GLYPH: f32 = 0.6;
/// Largest PNG side in pixels.
const MAX_PNG_SIDE: f32 = 16_384.;
/// Most PNG pixels (about 400 MB of RGBA).
const MAX_PNG_PIXELS: f32 = 100_000_000.;

/// Colors and font of an exported graph.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgPalette {
    pub background: Hsla,
    pub node: Hsla,
    pub node_border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub edge: Hsla,
    pub group: Hsla,
    pub group_border: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    /// By [`TrafficLevel`](super::TrafficLevel).
    pub traffic: [Hsla; 5],
    /// First choice of font; generic fallbacks follow it.
    pub font: SharedString,
}

impl SvgPalette {
    /// The colors the canvas paints with in `theme`.
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            background: theme.background,
            node: theme.popover,
            node_border: theme.border,
            text: theme.foreground,
            muted: theme.muted_foreground,
            edge: theme.ring,
            group: theme.group_box,
            group_border: theme.border,
            success: theme.success,
            warning: theme.warning,
            danger: theme.danger,
            info: theme.info,
            traffic: crate::theme::fallback_traffic_palette(theme),
            font: theme.font_family.clone(),
        }
    }

    /// The colors the canvas paints with now, the Kubach traffic colors
    /// included.
    pub fn from_app(cx: &gpui_kit::App) -> Self {
        use gpui_kit::component::ActiveTheme as _;
        Self {
            traffic: crate::theme::traffic_palette(cx),
            ..Self::from_theme(cx.theme())
        }
    }

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

/// `#rrggbb` and the alpha of a color.
fn hex(color: Hsla) -> (String, f32) {
    let rgba = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    (
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(rgba.r),
            byte(rgba.g),
            byte(rgba.b)
        ),
        rgba.a.clamp(0., 1.),
    )
}

/// `fill="#…"` with `fill-opacity` when not opaque; `attr` is `fill` or
/// `stroke`.
fn paint(attr: &str, color: Hsla, alpha: f32) -> String {
    let (hex, a) = hex(color);
    let a = a * alpha;
    if a >= 0.999 {
        format!("{attr}=\"{hex}\"")
    } else {
        format!("{attr}=\"{hex}\" {attr}-opacity=\"{a:.3}\"")
    }
}

/// Text for SVG content and attribute values.
pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Not allowed in XML 1.0.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// Approximate width of `text` at `size`.
fn text_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * GLYPH
}

/// `text` cut with an ellipsis to about `width`.
fn fit(text: &str, size: f32, width: f32) -> String {
    let max = (width / (size * GLYPH)).floor().max(0.) as usize;
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    if max <= 1 {
        return String::new();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn n(v: f32) -> String {
    let rounded = (v * 10.).round() / 10.;
    if rounded == rounded.trunc() {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded:.1}")
    }
}

/// The SVG of `data` laid out from scratch with nothing folded: what the
/// canvas shows for this data before the user moves anything.
pub fn graph_svg(data: &GraphData, palette: &SvgPalette) -> String {
    let visible = Visible::build(data, &HashSet::new());
    let layout = layout::layout(&layout_input(data, &visible, None));
    render(data, &visible, &layout, palette)
}

/// Where [`graph_svg`] draws the node boxes and the edge labels of `data`.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphGeometry {
    /// Box of every node (or folded group) drawn.
    pub nodes: Vec<Rect>,
    /// Edge labels that found room, shortened where needed.
    pub labels: Vec<PlacedLabel>,
}

/// The boxes and labels of [`graph_svg`], to check their placement.
pub fn graph_geometry(data: &GraphData) -> GraphGeometry {
    let visible = Visible::build(data, &HashSet::new());
    let layout = layout::layout(&layout_input(data, &visible, None));
    GraphGeometry {
        labels: place(data, &visible, &layout)
            .into_iter()
            .flatten()
            .collect(),
        nodes: layout.nodes,
    }
}

/// Edge labels placed with the export's text width guess.
fn place(data: &GraphData, visible: &Visible, layout: &Layout) -> Vec<Option<PlacedLabel>> {
    labels::place_labels(
        &layout.nodes,
        &layout.edges,
        &labels::edge_texts(data, visible),
        &mut |text| text_width(text, LABEL_FONT),
    )
}

/// The SVG of a laid out graph.
pub(super) fn render(
    data: &GraphData,
    visible: &Visible,
    layout: &Layout,
    palette: &SvgPalette,
) -> String {
    let placed = place(data, visible, layout);
    let bounds = if layout.nodes.is_empty() {
        Rect::new(0., 0., 200., 80.)
    } else {
        placed
            .iter()
            .flatten()
            .fold(layout.bounds, |bounds, label| bounds.union(&label.rect))
    };
    let (x, y) = (bounds.x - MARGIN, bounds.y - MARGIN);
    let (w, h) = (bounds.w + 2. * MARGIN, bounds.h + 2. * MARGIN);
    let font = xml_escape(&format!(
        "{}, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif",
        palette.font
    ));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" \
         viewBox=\"{} {} {} {}\" font-family=\"{font}\">",
        n(w),
        n(h),
        n(x),
        n(y),
        n(w),
        n(h)
    );
    let _ = writeln!(
        out,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" {}/>",
        n(x),
        n(y),
        n(w),
        n(h),
        paint("fill", palette.background, 1.)
    );
    groups(&mut out, visible, layout, palette);
    edges(&mut out, data, visible, layout, palette);
    edge_labels(&mut out, &placed, palette);
    nodes(&mut out, data, visible, layout, palette);
    out.push_str("</svg>\n");
    out
}

fn groups(out: &mut String, visible: &Visible, layout: &Layout, p: &SvgPalette) {
    for (g, rect) in layout.groups.iter().enumerate() {
        let Some(rect) = rect else {
            continue;
        };
        let group = &visible.groups[g];
        let _ = writeln!(
            out,
            "<g class=\"group\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"10\" {} {}/>",
            n(rect.x),
            n(rect.y),
            n(rect.w),
            n(rect.h),
            paint("fill", p.group, 1.),
            paint("stroke", p.group_border, 1.)
        );
        let title = format!("{}  ·  {}", group.title, group.members.len());
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-size=\"11\" font-weight=\"500\" {}>{}</text></g>",
            n(rect.x + 12.),
            n(rect.y + 18.),
            paint("fill", p.muted, 1.),
            xml_escape(&fit(&title, 11., rect.w - 24.))
        );
    }
}

fn edges(out: &mut String, data: &GraphData, visible: &Visible, layout: &Layout, p: &SvgPalette) {
    for (e, edge) in visible.edges.iter().enumerate() {
        let Some(Some(route)) = layout.edges.get(e) else {
            continue;
        };
        let first = &data.edges[edge.edges[0]];
        let tone = if edge.edges.iter().all(|&i| data.edges[i].tone == first.tone) {
            first.tone
        } else {
            Tone::Neutral
        };
        let traffic = edge
            .edges
            .iter()
            .filter_map(|&i| data.edges[i].traffic)
            .max();
        let color = match (traffic, tone) {
            (Some(level), _) => p.traffic[level.index()],
            (None, Tone::Neutral) => p.edge,
            (None, tone) => p.tone(tone),
        };
        let weight = edge
            .edges
            .iter()
            .filter_map(|&i| data.edges[i].weight)
            .fold(0f32, f32::max);
        let width = ((1. + 3. * weight) * 2.).round() / 2.;
        let mut d = String::new();
        for (ix, [a, b, c, end]) in route.segments.iter().enumerate() {
            if ix == 0 {
                let _ = write!(d, "M{} {}", n(a.x), n(a.y));
            }
            let _ = write!(
                d,
                " C{} {} {} {} {} {}",
                n(b.x),
                n(b.y),
                n(c.x),
                n(c.y),
                n(end.x),
                n(end.y)
            );
        }
        let dash = match first.style {
            EdgeStyle::Solid => String::new(),
            EdgeStyle::Dashed => " stroke-dasharray=\"6 4\"".to_owned(),
            EdgeStyle::Dotted => " stroke-dasharray=\"1.5 3\" stroke-linecap=\"round\"".to_owned(),
            EdgeStyle::LongDash => " stroke-dasharray=\"14 6\"".to_owned(),
        };
        let _ = writeln!(
            out,
            "<path d=\"{d}\" fill=\"none\" {} stroke-width=\"{}\"{dash}/>",
            paint("stroke", color, 1.),
            n(width)
        );
        // Arrowhead.
        let tip = route.end();
        let dir = route.end_direction();
        let size = 6.;
        let back = Pos::new(tip.x - dir.x * size, tip.y - dir.y * size);
        let (nx, ny) = (-dir.y * size * 0.55, dir.x * size * 0.55);
        let _ = writeln!(
            out,
            "<polygon points=\"{},{} {},{} {},{}\" {}/>",
            n(tip.x),
            n(tip.y),
            n(back.x + nx),
            n(back.y + ny),
            n(back.x - nx),
            n(back.y - ny),
            paint("fill", color, 1.)
        );
    }
}

/// Labels above every line and clear of the nodes; a shortened one keeps
/// the full text as its tooltip.
fn edge_labels(out: &mut String, placed: &[Option<PlacedLabel>], p: &SvgPalette) {
    for label in placed.iter().flatten() {
        let rect = label.rect;
        let title = if label.shortened {
            format!("<title>{}</title>", xml_escape(&label.full))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "<g class=\"edge-label\">{title}<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"4\" {} {}/>\
             <text x=\"{}\" y=\"{}\" font-size=\"10\" text-anchor=\"middle\" {}>{}</text></g>",
            n(rect.x),
            n(rect.y),
            n(rect.w),
            n(rect.h),
            paint("fill", p.node, 1.),
            paint("stroke", p.node_border, 1.),
            n(rect.x + rect.w / 2.),
            n(rect.y + rect.h / 2. + LABEL_FONT * 0.35),
            paint("fill", p.muted, 1.),
            xml_escape(&label.text)
        );
    }
}

fn nodes(out: &mut String, data: &GraphData, visible: &Visible, layout: &Layout, p: &SvgPalette) {
    for (item, rect) in layout.nodes.iter().enumerate() {
        let (title, subtitle, tone, badges, folded) = match visible.items[item] {
            Item::Node(ix) => {
                let node = &data.nodes[ix];
                (
                    node.title.to_string(),
                    node.subtitle.as_ref().map(ToString::to_string),
                    node.tone,
                    node.badges.as_slice(),
                    false,
                )
            }
            Item::Group(g) => {
                let group = &visible.groups[g];
                (
                    group.title.to_string(),
                    Some(format!("{} items", group.members.len())),
                    group.tone,
                    &[][..],
                    true,
                )
            }
        };
        out.push_str("<g class=\"node\">");
        if folded {
            let _ = write!(
                out,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"6\" {} {}/>",
                n(rect.x + 4.),
                n(rect.y + 4.),
                n(rect.w),
                n(rect.h),
                paint("fill", p.node, 1.),
                paint("stroke", p.node_border, 1.)
            );
        }
        let _ = write!(
            out,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"6\" {} {}/>",
            n(rect.x),
            n(rect.y),
            n(rect.w),
            n(rect.h),
            paint("fill", p.node, 1.),
            paint("stroke", p.node_border, 1.)
        );
        let pad = 10.;
        let text_x = 12.;
        let _ = write!(
            out,
            "<text x=\"{}\" y=\"{}\" font-size=\"12.5\" font-weight=\"500\" {}>{}</text>",
            n(rect.x + text_x),
            n(rect.y + 20.),
            paint("fill", p.text, 1.),
            xml_escape(&fit(&title, 12.5, rect.w - text_x - pad))
        );
        // Second row: status dot, subtitle, badges from the right.
        let row_y = rect.y + 29.;
        let row_h = 16.;
        let mut right = rect.w - pad;
        for badge in badges.iter().rev() {
            let color = p.tone(badge.tone);
            let w = text_width(&badge.label, 10.) + 10.;
            if right - w < text_x + 40. {
                break;
            }
            right -= w;
            let _ = write!(
                out,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"8\" {}/>\
                 <text x=\"{}\" y=\"{}\" font-size=\"10\" font-weight=\"500\" text-anchor=\"middle\" {}>{}</text>",
                n(rect.x + right),
                n(row_y),
                n(w),
                n(row_h),
                paint("fill", color, TINT_OPACITY),
                n(rect.x + right + w / 2.),
                n(row_y + 11.5),
                paint("fill", color, 1.),
                xml_escape(&badge.label)
            );
            right -= 4.;
        }
        let dot = 6.;
        let _ = write!(
            out,
            "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" {}/>",
            n(rect.x + text_x + dot / 2.),
            n(row_y + row_h / 2.),
            n(dot / 2.),
            paint("fill", p.tone(tone), 1.)
        );
        if let Some(subtitle) = subtitle {
            let sx = text_x + dot + 6.;
            let _ = write!(
                out,
                "<text x=\"{}\" y=\"{}\" font-size=\"11\" {}>{}</text>",
                n(rect.x + sx),
                n(row_y + 12.),
                paint("fill", p.muted, 1.),
                xml_escape(&fit(&subtitle, 11., right - sx - 4.))
            );
        }
        out.push_str("</g>\n");
    }
}

/// PNG of an SVG at `scale` (2 for a sharp image on high-density screens),
/// with text in the system fonts. Large drawings are scaled down to stay
/// within a sane image size.
pub fn svg_to_png(svg: &str, scale: f32) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let mut scale = scale.max(0.1);
    scale = scale
        .min(MAX_PNG_SIDE / size.width())
        .min(MAX_PNG_SIDE / size.height())
        .min((MAX_PNG_PIXELS / (size.width() * size.height())).sqrt());
    let width = (size.width() * scale).ceil().max(1.) as u32;
    let height = (size.height() * scale).ceil().max(1.) as u32;
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or_else(|| "image too large".to_owned())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Hsla, hsla};

    use super::{SvgPalette, fit, graph_geometry, graph_svg, svg_to_png, xml_escape};
    use crate::components::graph::labels::tests::assert_clear;
    use crate::components::graph::{EdgeStyle, GraphData, GraphEdge, GraphGroup, GraphNode};
    use crate::tokens::Tone;

    fn palette() -> SvgPalette {
        let gray = |l: f32| -> Hsla { hsla(0., 0., l, 1.) };
        SvgPalette {
            background: gray(0.05),
            node: gray(0.1),
            node_border: gray(0.2),
            text: gray(0.95),
            muted: gray(0.6),
            edge: gray(0.4),
            group: hsla(0., 0., 1., 0.03),
            group_border: gray(0.2),
            success: hsla(0.4, 0.6, 0.5, 1.),
            warning: hsla(0.1, 0.8, 0.5, 1.),
            danger: hsla(0., 0.7, 0.55, 1.),
            info: hsla(0.6, 0.7, 0.55, 1.),
            traffic: [
                hsla(0.6, 1., 0.6, 0.45),
                hsla(0.6, 1., 0.6, 1.),
                hsla(0.75, 0.9, 0.65, 1.),
                hsla(0.1, 0.9, 0.55, 1.),
                hsla(0., 1., 0.65, 1.),
            ],
            font: "Geist".into(),
        }
    }

    fn data() -> GraphData {
        GraphData {
            nodes: vec![
                GraphNode::new("in", "shop <&> \"front\"")
                    .subtitle("Ingress")
                    .group("shop")
                    .layer(0),
                GraphNode::new("svc", "web")
                    .subtitle("Service")
                    .badge("1/2", Tone::Warning)
                    .tone(Tone::Warning)
                    .group("shop")
                    .layer(1),
                GraphNode::new("ext", "203.0.113.7").layer(2),
            ],
            edges: vec![
                GraphEdge::new("e1", "in", "svc")
                    .label("shop.example.com → 80")
                    .weight(0.25),
                GraphEdge::new("e2", "svc", "ext")
                    .style(EdgeStyle::Dashed)
                    .tone(Tone::Danger),
            ],
            groups: vec![GraphGroup::new("shop", "namespace shop")],
        }
    }

    #[test]
    fn svg_has_nodes_edges_groups_and_escapes_text() {
        let svg = graph_svg(&data(), &palette());
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<g class=\"node\">").count(), 3);
        assert_eq!(svg.matches("<path ").count(), 2);
        assert_eq!(svg.matches("<polygon ").count(), 2);
        assert_eq!(svg.matches("<g class=\"group\">").count(), 1);
        assert!(svg.contains("shop &lt;&amp;&gt; &quot;front&quot;"));
        assert!(svg.contains("shop.example.com → 80"));
        assert!(svg.contains("stroke-dasharray=\"6 4\""));
        assert!(svg.contains(">1/2</text>"));
        assert!(!svg.contains("<&>"));
        // Theme colors, not defaults.
        assert!(svg.contains("fill=\"#0d0d0d\""), "{svg}");
    }

    #[test]
    fn traffic_edges_take_traffic_colors_and_long_dashes() {
        use crate::components::graph::TrafficLevel;
        let mut data = data();
        data.edges = vec![
            GraphEdge::new("t1", "in", "svc")
                .weight(1.)
                .traffic(TrafficLevel::Failing),
            GraphEdge::new("t2", "svc", "ext")
                .style(EdgeStyle::LongDash)
                .traffic(TrafficLevel::Low),
        ];
        let p = palette();
        let svg = graph_svg(&data, &p);
        let (failing, _) = super::hex(p.traffic[TrafficLevel::Failing.index()]);
        assert!(svg.contains(&format!("stroke=\"{failing}\"")), "{svg}");
        assert!(svg.contains("stroke-width=\"4\""), "{svg}");
        assert!(svg.contains("stroke-dasharray=\"14 6\""), "{svg}");
    }

    #[test]
    fn labels_stay_clear_of_nodes_and_keep_the_full_text_as_a_tooltip() {
        let mut data = data();
        // Several labels into one Service, as in a traffic layer.
        for (ix, name) in ["a", "b", "c"].into_iter().enumerate() {
            data.nodes
                .push(GraphNode::new(name, name).group("shop").layer(0));
            data.edges.push(
                GraphEdge::new(format!("t{ix}"), name, "svc")
                    .label(format!("{ix}2 req/s · 2.0% err · p95 50 ms")),
            );
        }
        let geometry = graph_geometry(&data);
        let placed: Vec<_> = geometry.labels.iter().cloned().map(Some).collect();
        assert_clear(&geometry.nodes, &placed);
        assert!(geometry.labels.len() >= 3, "{:?}", geometry.labels);
        let svg = graph_svg(&data, &palette());
        let shortened = geometry
            .labels
            .iter()
            .find(|l| l.shortened)
            .expect("a shortened label");
        assert!(svg.contains(&format!(">{}</text>", xml_escape(&shortened.text))));
        assert!(svg.contains(&format!("<title>{}</title>", xml_escape(&shortened.full))));
        // Labels are drawn before the nodes, never under them.
        let last_label = svg.rfind("class=\"edge-label\"").unwrap();
        let first_node = svg.find("class=\"node\"").unwrap();
        assert!(last_label < first_node);
    }

    #[test]
    fn svg_rasterizes_to_png() {
        let png = svg_to_png(&graph_svg(&data(), &palette()), 1.).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn text_is_cut_and_escaped() {
        assert_eq!(fit("abcdefghij", 10., 36.), "abcde…");
        assert_eq!(fit("abc", 10., 36.), "abc");
        assert_eq!(xml_escape("a'b\u{1}"), "a&apos;b");
    }
}
