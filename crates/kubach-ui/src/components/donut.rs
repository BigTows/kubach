//! A ring chart against a maximum (#414): CPU or memory used, requested
//! and limited out of allocatable, pods out of the nodes' capacity.
//!
//! Each ring is filled to the share of the maximum its segments take,
//! stacked clockwise from the top; several rings are drawn inside each
//! other (used outside, then requests, then limits). The center shows a big
//! value (a percent) with a caption, under the ring "X / max". The legend
//! lists every segment with its value and share; values never wrap, labels
//! truncate. A share past the maximum fills the ring and is flagged `▲`.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::components::line_chart::chart_color;
use crate::theme::usage_marker_colors;
use crate::tokens::Tone;
use kubach_i18n::t;

type Format = Rc<dyn Fn(f64) -> SharedString>;

/// Diameter of the ring.
pub const DONUT_SIZE: Pixels = px(120.);
const STROKE: f32 = 9.;
const RING_GAP: f32 = 3.;

/// Theme color of a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DonutColor {
    Tone(Tone),
    /// The request mark of usage bars (`usage.request`).
    Request,
    /// The limit mark of usage bars (`usage.limit`).
    Limit,
    /// `chart.{n + 1}`.
    Chart(usize),
}

impl DonutColor {
    fn resolve(self, cx: &App) -> Hsla {
        match self {
            Self::Tone(tone) => tone.color(cx),
            Self::Request => usage_marker_colors(cx).0,
            Self::Limit => usage_marker_colors(cx).1,
            Self::Chart(slot) => chart_color(slot, cx),
        }
    }
}

/// A part of a ring and its legend row.
#[derive(Clone, Debug)]
pub struct DonutSegment {
    pub label: SharedString,
    /// `None`: not known (the legend says "unavailable").
    pub value: Option<f64>,
    pub color: DonutColor,
    /// Appended to the value in the legend, e.g. "3 pods without a limit".
    pub note: Option<SharedString>,
}

impl DonutSegment {
    pub fn new(label: impl Into<SharedString>, value: Option<f64>, color: DonutColor) -> Self {
        Self {
            label: label.into(),
            value,
            color,
            note: None,
        }
    }

    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Start and end shares (0…1) of each segment of a ring, stacked and
/// clamped to the full circle.
pub fn ring_spans(values: &[Option<f64>], max: f64) -> Vec<(f64, f64)> {
    let mut start = 0.0_f64;
    values
        .iter()
        .map(|value| {
            let share = match value {
                Some(value) if max > 0.0 && value.is_finite() => value.max(0.0) / max,
                _ => 0.0,
            };
            let end = (start + share).min(1.0);
            let span = (start, end);
            start = end;
            span
        })
        .collect()
}

/// `"1.2 · 12%"`, `"12 · 150% ▲"`, or the value alone without a maximum.
pub fn donut_value(value: Option<f64>, max: f64, format: &dyn Fn(f64) -> SharedString) -> String {
    let Some(value) = value else {
        return t!("ui-unavailable");
    };
    if max <= 0.0 {
        return format(value).to_string();
    }
    let share = value / max * 100.0;
    let flag = if share > 100.0 { " ▲" } else { "" };
    format!("{} · {share:.0}%{flag}", format(value))
}

#[derive(IntoElement)]
pub struct Donut {
    max: f64,
    rings: Vec<Vec<DonutSegment>>,
    center: SharedString,
    caption: Option<SharedString>,
    footer: Option<SharedString>,
    format: Format,
}

impl Donut {
    /// A ring chart against `max` (allocatable, capacity).
    pub fn new(max: f64) -> Self {
        Self {
            max,
            rings: Vec::new(),
            center: "—".into(),
            caption: None,
            footer: None,
            format: Rc::new(|value| format!("{value:.2}").into()),
        }
    }

    /// One ring of stacked `segments`, inside the rings added before.
    pub fn ring(mut self, segments: impl IntoIterator<Item = DonutSegment>) -> Self {
        self.rings.push(segments.into_iter().collect());
        self
    }

    /// The big value in the middle, e.g. "34%".
    pub fn center(mut self, center: impl Into<SharedString>) -> Self {
        self.center = center.into();
        self
    }

    /// Small text under the center value, e.g. "used".
    pub fn caption(mut self, caption: impl Into<SharedString>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// Under the ring, e.g. "1.2 / 10 cores".
    pub fn footer(mut self, footer: impl Into<SharedString>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Formats values in the legend.
    pub fn format(mut self, format: impl Fn(f64) -> SharedString + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }
}

/// Points of an arc of a circle around `center` from share `from` to `to`
/// (clockwise from the top).
fn arc(center: Point<Pixels>, radius: f32, from: f64, to: f64) -> Vec<Point<Pixels>> {
    let steps = (((to - from) * 96.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|ix| {
            let share = from + (to - from) * ix as f64 / steps as f64;
            let angle = share as f32 * TAU - FRAC_PI_2;
            point(
                center.x + px(radius * angle.cos()),
                center.y + px(radius * angle.sin()),
            )
        })
        .collect()
}

fn stroke(window: &mut Window, points: &[Point<Pixels>], color: Hsla) {
    let Some((first, rest)) = points.split_first() else {
        return;
    };
    let mut path = PathBuilder::stroke(px(STROKE));
    path.move_to(*first);
    for p in rest {
        path.line_to(*p);
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

impl RenderOnce for Donut {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let track = theme.muted;
        let rings: Vec<Vec<((f64, f64), Hsla)>> = self
            .rings
            .iter()
            .map(|ring| {
                let values: Vec<Option<f64>> = ring.iter().map(|s| s.value).collect();
                ring_spans(&values, self.max)
                    .into_iter()
                    .zip(ring.iter().map(|s| s.color.resolve(cx)))
                    .collect()
            })
            .collect();
        let plot = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let center = bounds.center();
                let outer = f32::from(bounds.size.width.min(bounds.size.height)) / 2.0;
                for (ix, ring) in rings.iter().enumerate() {
                    let radius = outer - STROKE / 2.0 - ix as f32 * (STROKE + RING_GAP);
                    if radius <= STROKE / 2.0 {
                        break;
                    }
                    stroke(window, &arc(center, radius, 0.0, 1.0), track);
                    for ((from, to), color) in ring {
                        if to > from {
                            stroke(window, &arc(center, radius, *from, *to), *color);
                        }
                    }
                }
            },
        )
        .size_full();
        let ring = div()
            .relative()
            .flex_none()
            .size(DONUT_SIZE)
            .child(plot)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_family(theme.mono_font_family.clone())
                            .whitespace_nowrap()
                            .text_color(theme.foreground)
                            .child(self.center),
                    )
                    .when_some(self.caption, |center, caption| {
                        center.child(
                            div()
                                .text_xs()
                                .whitespace_nowrap()
                                .text_color(theme.muted_foreground)
                                .child(caption),
                        )
                    }),
            );
        let format = self.format.clone();
        let legend = self.rings.iter().flatten().map(|segment| {
            let mut value = donut_value(segment.value, self.max, &*format);
            if let Some(note) = &segment.note {
                value = format!("{value} · {note}");
            }
            let color = segment.color.resolve(cx);
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .child(div().flex_none().size(px(8.)).rounded_sm().bg(color))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(theme.muted_foreground)
                        .child(segment.label.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(value),
                )
        });
        div()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(ring)
                    .when_some(self.footer, |column, footer| {
                        column.child(
                            div()
                                .text_xs()
                                .whitespace_nowrap()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(footer),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .text_xs()
                    .children(legend),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::SharedString;

    use super::{donut_value, ring_spans};

    #[test]
    fn segments_stack_and_stop_at_the_full_circle() {
        assert_eq!(
            ring_spans(&[Some(2.0), None, Some(4.0)], 8.0),
            [(0.0, 0.25), (0.25, 0.25), (0.25, 0.75)]
        );
        assert_eq!(ring_spans(&[Some(6.0), Some(6.0)], 8.0)[1], (0.75, 1.0));
        assert_eq!(ring_spans(&[Some(1.0)], 0.0), [(0.0, 0.0)]);
    }

    #[test]
    fn legend_values_show_the_share() {
        let format = |v: f64| SharedString::from(format!("{v:.1}"));
        assert_eq!(donut_value(Some(2.0), 8.0, &format), "2.0 · 25%");
        assert_eq!(donut_value(Some(12.0), 8.0, &format), "12.0 · 150% ▲");
        assert_eq!(donut_value(Some(2.0), 0.0, &format), "2.0");
        assert_eq!(donut_value(None, 8.0, &format), "unavailable");
    }
}
