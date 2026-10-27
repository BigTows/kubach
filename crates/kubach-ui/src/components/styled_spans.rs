//! Styled ranges of program output (ANSI SGR colors and attributes) drawn
//! with the theme's ANSI palette, and deterministic layering of highlights.
//!
//! The parser lives in `kubach-core` (`kubach_core::ansi`); this module only
//! knows how to draw what it produced.

use std::ops::Range;

use gpui_kit::{
    FontStyle, FontWeight, HighlightStyle, Hsla, Rgba, StrikethroughStyle, UnderlineStyle, px,
};

/// A color of a [`SpanStyle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanColor {
    /// One of the 16 ANSI palette colors (0–7 normal, 8–15 bright), taken
    /// from the theme.
    Named(u8),
    /// An entry of the 256-color table (16–231 color cube, 232–255 grays).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl SpanColor {
    /// The color drawn for `self`; `palette` is the theme's ANSI palette
    /// ([`crate::theme::ansi_palette`]).
    pub fn resolve(self, palette: &[Hsla; 16]) -> Hsla {
        match self {
            SpanColor::Named(index) => palette[index as usize % 16],
            SpanColor::Indexed(index) if index < 16 => palette[index as usize],
            SpanColor::Indexed(index) if index < 232 => {
                // xterm's 6×6×6 cube.
                let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
                let n = index - 16;
                rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
            }
            SpanColor::Indexed(index) => {
                let gray = 8 + (index - 232) * 10;
                rgb(gray, gray, gray)
            }
            SpanColor::Rgb(r, g, b) => rgb(r, g, b),
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a: 1.,
    }
    .into()
}

/// Attributes of a styled range; the default draws text unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    pub foreground: Option<SpanColor>,
    pub background: Option<SpanColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl SpanStyle {
    pub fn highlight(&self, palette: &[Hsla; 16]) -> HighlightStyle {
        HighlightStyle {
            color: self.foreground.map(|color| color.resolve(palette)),
            background_color: self.background.map(|color| color.resolve(palette)),
            font_weight: self.bold.then_some(FontWeight::BOLD),
            font_style: self.italic.then_some(FontStyle::Italic),
            underline: self.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                ..UnderlineStyle::default()
            }),
            strikethrough: self.strikethrough.then_some(StrikethroughStyle {
                thickness: px(1.),
                ..StrikethroughStyle::default()
            }),
            fade_out: self.dim.then_some(0.4),
        }
    }
}

/// A styled byte range of a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledSpan {
    pub range: Range<usize>,
    pub style: SpanStyle,
}

/// Highlights of `spans`, shifted by `offset` bytes (text drawn before the
/// line, such as a source tag).
pub fn span_highlights(
    spans: &[StyledSpan],
    offset: usize,
    palette: &[Hsla; 16],
) -> impl Iterator<Item = (Range<usize>, HighlightStyle)> {
    spans.iter().map(move |span| {
        (
            span.range.start + offset..span.range.end + offset,
            span.style.highlight(palette),
        )
    })
}

/// Merges possibly overlapping highlights into ordered, non-overlapping
/// ones for [`StyledText::with_highlights`](gpui_kit::StyledText). Where
/// highlights overlap, later ones win field by field, so pass them from
/// lowest to highest priority (e.g. colors, then search hits, then the
/// selection).
pub fn layer_highlights(
    layers: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let layers: Vec<_> = layers
        .into_iter()
        .filter(|(range, _)| !range.is_empty())
        .collect();
    let mut bounds: Vec<usize> = layers
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut merged: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in bounds.windows(2) {
        let segment = pair[0]..pair[1];
        let mut covering = layers
            .iter()
            .filter(|(range, _)| range.start <= segment.start && segment.end <= range.end)
            .map(|(_, style)| *style)
            .peekable();
        if covering.peek().is_none() {
            continue;
        }
        let style = covering.fold(HighlightStyle::default(), |acc, style| acc.highlight(style));
        match merged.last_mut() {
            Some((range, last)) if range.end == segment.start && *last == style => {
                range.end = segment.end;
            }
            _ => merged.push((segment, style)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use gpui_kit::{HighlightStyle, Hsla, hsla};

    use super::{SpanColor, SpanStyle, layer_highlights};

    fn palette() -> [Hsla; 16] {
        std::array::from_fn(|ix| hsla(ix as f32 / 16., 0.5, 0.5, 1.))
    }

    #[test]
    fn named_and_low_indexed_colors_come_from_the_palette() {
        let palette = palette();
        assert_eq!(SpanColor::Named(3).resolve(&palette), palette[3]);
        assert_eq!(SpanColor::Indexed(9).resolve(&palette), palette[9]);
    }

    #[test]
    fn cube_and_grays_follow_xterm() {
        let palette = palette();
        let white = SpanColor::Indexed(231).resolve(&palette);
        assert!((white.l - 1.).abs() < 1e-3);
        let black = SpanColor::Indexed(16).resolve(&palette);
        assert!(black.l.abs() < 1e-3);
        let gray = SpanColor::Indexed(244).resolve(&palette);
        assert!((gray.l - 128. / 255.).abs() < 1e-2);
        assert_eq!(
            SpanColor::Indexed(196).resolve(&palette),
            SpanColor::Rgb(255, 0, 0).resolve(&palette)
        );
    }

    #[test]
    fn style_maps_attributes() {
        let style = SpanStyle {
            foreground: Some(SpanColor::Named(1)),
            bold: true,
            dim: true,
            ..SpanStyle::default()
        }
        .highlight(&palette());
        assert_eq!(style.color, Some(palette()[1]));
        assert!(style.font_weight.is_some());
        assert!(style.fade_out.is_some());
        assert!(style.underline.is_none());
    }

    #[test]
    fn layers_merge_with_later_winning() {
        let red = hsla(0., 1., 0.5, 1.);
        let blue = hsla(0.6, 1., 0.5, 1.);
        let fg = HighlightStyle {
            color: Some(red),
            ..HighlightStyle::default()
        };
        let bg = HighlightStyle {
            background_color: Some(blue),
            ..HighlightStyle::default()
        };
        let both = HighlightStyle {
            color: Some(red),
            background_color: Some(blue),
            ..HighlightStyle::default()
        };
        let merged = layer_highlights(vec![(0..6, fg), (4..8, bg), (2..2, bg)]);
        assert_eq!(merged, vec![(0..4, fg), (4..6, both), (6..8, bg)]);

        let over = HighlightStyle {
            color: Some(blue),
            ..HighlightStyle::default()
        };
        let merged = layer_highlights(vec![(0..4, fg), (0..4, over)]);
        assert_eq!(merged, vec![(0..4, over)]);
        assert!(layer_highlights(Vec::new()).is_empty());
    }
}
