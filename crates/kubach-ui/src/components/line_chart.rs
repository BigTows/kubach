//! A compact time-series line chart: lines over a shared time axis, a
//! zero-based value axis rounded up to a readable maximum, a legend with
//! each line's latest value. Colors are the theme's `chart.*` tokens.
//!
//! Hovering the plot snaps a vertical line to the nearest sample, marks
//! each line there and shows a readout: the time (local clock and how long
//! before the right edge) and every line's and threshold's value (#376).
//! The hovered sample lives in element state; it changes, and notifies the
//! view, only when the cursor reaches another sample or leaves the plot.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::age::format_age;
use crate::measure::{TEXT_XS, mono_text_width};
use crate::tokens::Tone;
use kubach_i18n::t;

/// Opacity of the area under a line, relative to its color.
const AREA_OPACITY: f32 = 0.10;
/// Narrowest value axis; longer labels (`2048.0 MiB`) widen it to fit on
/// one line (#481).
const AXIS_WIDTH: Pixels = px(52.);
/// Gap between the value labels and the plot (`pr_2`).
const AXIS_GAP: Pixels = px(8.);
/// Points further apart than this many typical intervals leave a gap.
const GAP_FACTOR: f64 = 3.0;

type Format = Rc<dyn Fn(f64) -> SharedString>;
/// Points and colors of the lines, shared with the hover listener.
type PlotLines = Rc<Vec<(Vec<(f64, f64)>, Hsla)>>;

/// A horizontal reference line of a [`LineChart`], e.g. a request or a
/// limit, drawn dashed in its tone.
#[derive(Clone, Debug)]
pub struct ChartThreshold {
    pub name: SharedString,
    pub value: f64,
    pub tone: Tone,
}

/// One line of a [`LineChart`]: `(x, y)` points in increasing `x`.
#[derive(Clone, Debug, Default)]
pub struct ChartLine {
    pub name: SharedString,
    pub points: Vec<(f64, f64)>,
    /// Theme color slot, `chart.1` … `chart.5`; by position when `None`.
    pub slot: Option<usize>,
}

impl ChartLine {
    pub fn new(name: impl Into<SharedString>, points: Vec<(f64, f64)>) -> Self {
        Self {
            name: name.into(),
            points,
            slot: None,
        }
    }

    /// Uses theme color `chart.{slot + 1}` (wrapping after 5).
    pub fn slot(mut self, slot: usize) -> Self {
        self.slot = Some(slot);
        self
    }
}

/// Thresholds far above the data (a 10-core node at 200m) would flatten
/// the lines: they are drawn only within this factor of the data's maximum.
const THRESHOLD_SCALE: f64 = 4.0;

/// Whether a threshold at `value` is drawn for data peaking at `data_max`.
/// Off-scale ones stay in the legend, marked with an arrow.
pub fn on_scale(value: f64, data_max: f64) -> bool {
    data_max <= 0.0 || value <= data_max * THRESHOLD_SCALE
}

/// Theme color of slot `ix`: `chart.1` … `chart.5`, wrapping.
pub fn chart_color(ix: usize, cx: &App) -> Hsla {
    let theme = cx.theme();
    [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ][ix % 5]
}

/// The smallest of 1, 2, 2.5, 5 × 10ⁿ at or above `value`; 1 for zero or
/// less, so an idle series still gets an axis.
pub fn nice_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(value.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|&nice| nice >= value * (1.0 - 1e-9))
        .unwrap_or(10.0 * magnitude)
}

/// As [`nice_ceiling`], in steps of a binary unit (KiB, MiB, GiB) for
/// bytes: 1.6 GiB rounds to 2 GiB, not to 2·10⁹ bytes (1907.3 MiB).
pub fn nice_binary_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let power = (value.log2() / 10.0).floor().max(0.0);
    let unit = 1024f64.powf(power);
    nice_ceiling(value / unit) * unit
}

/// Splits `points` where the distance to the next point exceeds
/// [`GAP_FACTOR`] times the smallest interval (missing scrapes).
pub fn segments(points: &[(f64, f64)]) -> Vec<&[(f64, f64)]> {
    let interval = points
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|d| *d > 0.0)
        .fold(f64::INFINITY, f64::min);
    let mut segments = Vec::new();
    let mut start = 0;
    for ix in 1..points.len() {
        if points[ix].0 - points[ix - 1].0 > interval * GAP_FACTOR {
            segments.push(&points[start..ix]);
            start = ix;
        }
    }
    if start < points.len() {
        segments.push(&points[start..]);
    }
    segments
}

/// The point of `points` (in increasing `x`) nearest to `x`.
fn nearest_point(points: &[(f64, f64)], x: f64) -> Option<(f64, f64)> {
    let ix = points.partition_point(|p| p.0 < x);
    let after = points.get(ix).copied();
    let before = ix.checked_sub(1).and_then(|ix| points.get(ix)).copied();
    match (before, after) {
        (Some(b), Some(a)) => Some(if x - b.0 <= a.0 - x { b } else { a }),
        (b, a) => b.or(a),
    }
}

/// The `x` of the sample nearest to `x` over all `lines`: where the hover
/// line snaps.
pub fn nearest_x<'a>(lines: impl IntoIterator<Item = &'a [(f64, f64)]>, x: f64) -> Option<f64> {
    lines
        .into_iter()
        .filter_map(|points| nearest_point(points, x))
        .map(|p| p.0)
        .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
}

/// The value of `points` at `x`: its sample within half of its smallest
/// interval, `None` in a gap or outside the line.
pub fn value_at(points: &[(f64, f64)], x: f64) -> Option<f64> {
    let (at, value) = nearest_point(points, x)?;
    let interval = points
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|d| *d > 0.0)
        .fold(f64::INFINITY, f64::min);
    let tolerance = if interval.is_finite() {
        interval / 2.0
    } else {
        1e-6
    };
    ((at - x).abs() <= tolerance).then_some(value)
}

/// `HH:MM:SS` of Unix time `time` at `offset` seconds from UTC.
pub fn clock(time: f64, offset: i64) -> String {
    let seconds = (time.floor() as i64 + offset).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// How long before `end` (the right edge, "now") `time` is: `now` or `-5m`.
pub fn ago(time: f64, end: f64) -> String {
    let seconds = (end - time).max(0.0).round() as u64;
    if seconds == 0 {
        t!("ui-chart-now")
    } else {
        format!("-{}", format_age(seconds))
    }
}

/// Offset of the system time zone from UTC at `time`, in seconds.
fn local_offset(time: f64) -> i64 {
    jiff::Timestamp::from_second(time as i64)
        .map(|at| i64::from(jiff::tz::TimeZone::system().to_offset(at).seconds()))
        .unwrap_or(0)
}

/// One line of a [`ChartReadout`]: its position in the chart's lines and
/// its value, formatted, or `None` in a gap.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadoutValue {
    pub line: usize,
    pub name: SharedString,
    pub value: Option<SharedString>,
}

/// What the hover readout shows for the sample at `time`.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartReadout {
    pub time: f64,
    /// Local `HH:MM:SS`.
    pub clock: String,
    /// `-5m` or `now`.
    pub ago: String,
    pub values: Vec<ReadoutValue>,
    /// Requests and limits, formatted.
    pub thresholds: Vec<(SharedString, SharedString, Tone)>,
}

/// The readout at `time` for `lines` and `thresholds`, with values in
/// `format` and the clock at `offset` seconds from UTC; `end` is the right
/// edge of the chart ("now"). Lines without points are left out.
pub fn readout(
    lines: &[ChartLine],
    thresholds: &[ChartThreshold],
    time: f64,
    end: f64,
    offset: i64,
    format: &dyn Fn(f64) -> SharedString,
) -> ChartReadout {
    ChartReadout {
        time,
        clock: clock(time, offset),
        ago: ago(time, end),
        values: lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.points.is_empty())
            .map(|(ix, line)| ReadoutValue {
                line: ix,
                name: line.name.clone(),
                value: value_at(&line.points, time).map(format),
            })
            .collect(),
        thresholds: thresholds
            .iter()
            .map(|t| (t.name.clone(), format(t.value), t.tone))
            .collect(),
    }
}

/// The hovered sample of a chart, in element state.
#[derive(Default)]
struct ChartHover {
    at: Option<HoverAt>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HoverAt {
    /// `x` of the sample.
    x: f64,
    /// Its distance from the left of the plot, and the plot width.
    offset: Pixels,
    width: Pixels,
}

/// A titled line chart with a legend and a value axis.
#[derive(IntoElement)]
pub struct LineChart {
    id: ElementId,
    title: SharedString,
    lines: Vec<ChartLine>,
    thresholds: Vec<ChartThreshold>,
    x_range: Option<(f64, f64)>,
    x_labels: Option<(SharedString, SharedString)>,
    format: Format,
    height: Pixels,
    empty: SharedString,
    binary: bool,
}

impl LineChart {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            lines: Vec::new(),
            thresholds: Vec::new(),
            x_range: None,
            x_labels: None,
            format: Rc::new(|value| format!("{value:.2}").into()),
            height: px(96.),
            empty: t!("ui-chart-no-data").into(),
            binary: false,
        }
    }

    pub fn line(mut self, line: ChartLine) -> Self {
        self.lines.push(line);
        self
    }

    pub fn lines(mut self, lines: impl IntoIterator<Item = ChartLine>) -> Self {
        self.lines.extend(lines);
        self
    }

    /// Adds a dashed reference line at `value` (requests, limits).
    pub fn threshold(mut self, name: impl Into<SharedString>, value: f64, tone: Tone) -> Self {
        if value.is_finite() && value > 0.0 {
            self.thresholds.push(ChartThreshold {
                name: name.into(),
                value,
                tone,
            });
        }
        self
    }

    /// The x span shown; the data's own span by default.
    pub fn x_range(mut self, start: f64, end: f64) -> Self {
        self.x_range = Some((start, end));
        self
    }

    /// Labels under the left and right ends of the x axis (`-1h`, `now`).
    pub fn x_labels(
        mut self,
        start: impl Into<SharedString>,
        end: impl Into<SharedString>,
    ) -> Self {
        self.x_labels = Some((start.into(), end.into()));
        self
    }

    /// Formats values on the axis and in the legend.
    pub fn format(mut self, format: impl Fn(f64) -> SharedString + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }

    /// Rounds the value axis in binary units, for bytes (see
    /// [`nice_binary_ceiling`]).
    pub fn binary(mut self) -> Self {
        self.binary = true;
        self
    }

    /// Height of the plot area.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Shown instead of the plot when no line has points.
    pub fn empty(mut self, text: impl Into<SharedString>) -> Self {
        self.empty = text.into();
        self
    }

    fn x_span(&self) -> Option<(f64, f64)> {
        let (min, max) = self.x_range.unwrap_or_else(|| {
            self.lines
                .iter()
                .flat_map(|line| line.points.iter().map(|p| p.0))
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
                    (lo.min(x), hi.max(x))
                })
        });
        (min.is_finite() && max.is_finite() && max > min).then_some((min, max))
    }
}

impl RenderOnce for LineChart {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors: Vec<Hsla> = self
            .lines
            .iter()
            .enumerate()
            .map(|(ix, line)| chart_color(line.slot.unwrap_or(ix), cx))
            .collect();
        let has_data = self.lines.iter().any(|line| !line.points.is_empty());
        let data_max = self
            .lines
            .iter()
            .flat_map(|line| line.points.iter().map(|p| p.1))
            .fold(0.0, f64::max);
        let peak = self
            .thresholds
            .iter()
            .map(|t| t.value)
            .filter(|&value| on_scale(value, data_max))
            .fold(data_max, f64::max);
        let y_max = if self.binary {
            nice_binary_ceiling(peak)
        } else {
            nice_ceiling(peak)
        };
        let thresholds: Vec<(f64, Hsla)> = self
            .thresholds
            .iter()
            .filter(|t| on_scale(t.value, data_max))
            .map(|t| (t.value, t.tone.color(cx)))
            .collect();
        let x_span = self.x_span();
        let format = self.format.clone();

        let legend = self
            .lines
            .iter()
            .zip(&colors)
            .filter(|(line, _)| !line.points.is_empty())
            .map(|(line, color)| {
                let latest = line
                    .points
                    .last()
                    .map(|p| (format)(p.1))
                    .unwrap_or_default();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.)).rounded_full().bg(*color))
                    .when(self.lines.len() > 1, |row| {
                        row.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(line.name.clone()),
                        )
                    })
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(latest),
                    )
            });
        let threshold_legend = self.thresholds.iter().map(|t| {
            let color = t.tone.color(cx);
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .gap(px(1.))
                        .child(div().w(px(3.)).h(px(1.5)).bg(color))
                        .child(div().w(px(3.)).h(px(1.5)).bg(color)),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(t.name.clone()),
                )
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(if on_scale(t.value, data_max) {
                            (format)(t.value)
                        } else {
                            format!("{} ↑", (format)(t.value)).into()
                        }),
                )
        });
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .text_xs()
            .when(!self.title.is_empty(), |header| {
                header.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(self.title.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_3()
                    .gap_y_1()
                    .children(legend)
                    .children(threshold_legend),
            );

        let body = match (has_data, x_span) {
            (true, Some((x0, x1))) => {
                let hover = window.use_keyed_state(
                    ElementId::Name(format!("{}-hover", self.id).into()),
                    cx,
                    |_, _| ChartHover::default(),
                );
                let card = hover.read(cx).at.and_then(|at| {
                    let readout = readout(
                        &self.lines,
                        &self.thresholds,
                        at.x,
                        x1,
                        local_offset(at.x),
                        &*format,
                    );
                    readout_card(&readout, &colors, &theme, cx).map(|card| (at, card))
                });
                let lines: PlotLines = Rc::new(
                    self.lines
                        .into_iter()
                        .zip(colors)
                        .map(|(line, color)| (line.points, color))
                        .collect(),
                );
                let style = PlotStyle {
                    grid: theme.chart_grid,
                    cursor: theme.muted_foreground.opacity(0.6),
                    ring: theme.background,
                };
                let plot = canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |bounds, hitbox, window, cx| {
                        let at = hover.read(cx).at;
                        paint_plot(
                            bounds,
                            &lines,
                            &thresholds,
                            (x0, x1),
                            y_max,
                            style.grid,
                            window,
                        );
                        if let Some(at) = at {
                            paint_cursor(bounds, &lines, at, y_max, style, window);
                        }
                        track_hover(bounds, hitbox, hover, lines, (x0, x1), window);
                    },
                )
                .size_full();
                let tooltip = card.map(|(at, card)| {
                    // Beside the line, on the side with more room; the
                    // window edge snaps it back in.
                    let (anchor, dx) = if at.offset > at.width / 2. {
                        (Anchor::TopRight, px(-8.))
                    } else {
                        (Anchor::TopLeft, px(8.))
                    };
                    div()
                        .absolute()
                        .top(px(2.))
                        .left(at.offset)
                        .w_0()
                        .h_0()
                        .child(
                            deferred(
                                anchored()
                                    .anchor(anchor)
                                    .offset(point(dx, px(0.)))
                                    .snap_to_window_with_margin(px(8.))
                                    .child(card),
                            )
                            .with_priority(1),
                        )
                });
                let axis_label = |text: SharedString| {
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(text)
                };
                // As wide as the longest label: a translated or long unit
                // stays on one line, beside its grid line (#481).
                let (top, zero) = ((format)(y_max), (format)(0.0));
                let widest = mono_text_width(&top, TEXT_XS, window, cx)
                    .max(mono_text_width(&zero, TEXT_XS, window, cx));
                let axis_width = (px(f32::from(widest).ceil()) + AXIS_GAP).max(AXIS_WIDTH);
                let axis = div()
                    .w(axis_width)
                    .flex_none()
                    .h(self.height)
                    .flex()
                    .flex_col()
                    .justify_between()
                    .items_end()
                    .pr(AXIS_GAP)
                    .text_xs()
                    .whitespace_nowrap()
                    .child(axis_label(top))
                    .child(axis_label(zero));
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div().flex().child(axis).child(
                            div()
                                .relative()
                                .flex_1()
                                .min_w_0()
                                .h(self.height)
                                .child(plot)
                                .children(tooltip),
                        ),
                    )
                    .when_some(self.x_labels, |body, (start, end)| {
                        body.child(
                            div()
                                .pl(axis_width)
                                .flex()
                                .justify_between()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(start)
                                .child(end),
                        )
                    })
                    .into_any_element()
            }
            _ => div()
                .h(self.height)
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_dashed()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(self.empty)
                .into_any_element(),
        };

        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .child(header)
            .child(body)
    }
}

/// Colors of the plot from the theme.
#[derive(Clone, Copy)]
struct PlotStyle {
    grid: Hsla,
    /// The hover line.
    cursor: Hsla,
    /// Around the hover markers, to lift them off the lines.
    ring: Hsla,
}

/// The readout card, `None` when there is nothing to show.
fn readout_card(
    readout: &ChartReadout,
    colors: &[Hsla],
    theme: &gpui_kit::component::Theme,
    cx: &App,
) -> Option<AnyElement> {
    if readout.values.is_empty() {
        return None;
    }
    let mono = theme.mono_font_family.clone();
    let row = |marker: Div, name: SharedString, value: SharedString| {
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(marker)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .pl_2()
                    .font_family(mono.clone())
                    .text_color(theme.popover_foreground)
                    .child(value),
            )
    };
    let values = readout.values.iter().map(|value| {
        let color = colors.get(value.line).copied().unwrap_or(theme.foreground);
        row(
            div().flex_none().size(px(6.)).rounded_full().bg(color),
            value.name.clone(),
            value.value.clone().unwrap_or_else(|| "—".into()),
        )
    });
    let thresholds = readout.thresholds.iter().map(|(name, value, tone)| {
        let color = tone.color(cx);
        row(
            div()
                .flex_none()
                .w(px(6.))
                .flex()
                .gap(px(1.))
                .child(div().w(px(2.5)).h(px(1.5)).bg(color))
                .child(div().w(px(2.5)).h(px(1.5)).bg(color)),
            name.clone(),
            value.clone(),
        )
    });
    Some(
        div()
            .min_w(px(140.))
            .max_w(px(260.))
            .flex()
            .flex_col()
            .gap_0p5()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .text_xs()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .pb_0p5()
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_color(theme.popover_foreground)
                            .child(readout.clock.clone()),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(readout.ago.clone()),
                    ),
            )
            .children(values)
            .children(thresholds)
            .into_any_element(),
    )
}

/// Follows the cursor over the plot: snaps to the nearest sample and
/// notifies only when that sample changes or the cursor leaves.
fn track_hover(
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    hover: Entity<ChartHover>,
    lines: PlotLines,
    (x0, x1): (f64, f64),
    window: &mut Window,
) {
    fn set(hover: &Entity<ChartHover>, at: Option<HoverAt>, cx: &mut App) {
        hover.update(cx, |state, cx| {
            if state.at != at {
                state.at = at;
                cx.notify();
            }
        });
    }
    let left = hover.clone();
    window.on_mouse_event(move |_: &MouseExitEvent, _, _, cx| set(&left, None, cx));
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let width = f32::from(bounds.size.width);
        let at = (hitbox.is_hovered(window) && width > 0.0)
            .then(|| {
                let share = f32::from(event.position.x - bounds.origin.x) / width;
                let x = x0 + f64::from(share.clamp(0.0, 1.0)) * (x1 - x0);
                let near = nearest_x(lines.iter().map(|(points, _)| points.as_slice()), x)?;
                let share = ((near - x0) / (x1 - x0)).clamp(0.0, 1.0) as f32;
                Some(HoverAt {
                    x: near,
                    offset: px(share * width),
                    width: bounds.size.width,
                })
            })
            .flatten();
        set(&hover, at, cx);
    });
}

/// The hover line at the sample and a marker on every line there.
fn paint_cursor(
    bounds: Bounds<Pixels>,
    lines: &[(Vec<(f64, f64)>, Hsla)],
    at: HoverAt,
    y_max: f64,
    style: PlotStyle,
    window: &mut Window,
) {
    let x = bounds.origin.x + at.offset;
    window.paint_quad(fill(
        Bounds::new(
            point(x - px(0.5), bounds.origin.y),
            size(px(1.), bounds.size.height),
        ),
        style.cursor,
    ));
    let bottom = bounds.origin.y + bounds.size.height;
    let height = f32::from(bounds.size.height);
    for (points, color) in lines {
        let Some(value) = value_at(points, at.x) else {
            continue;
        };
        let y = bottom - px((value / y_max).clamp(0.0, 1.0) as f32 * height);
        for (diameter, color) in [(px(8.), style.ring), (px(5.), *color)] {
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(x - diameter / 2., y - diameter / 2.),
                        size(diameter, diameter),
                    ),
                    color,
                )
                .corner_radii(diameter / 2.),
            );
        }
    }
}

fn paint_plot(
    bounds: Bounds<Pixels>,
    lines: &[(Vec<(f64, f64)>, Hsla)],
    thresholds: &[(f64, Hsla)],
    (x0, x1): (f64, f64),
    y_max: f64,
    grid: Hsla,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let left = bounds.origin.x;
    let bottom = bounds.origin.y + bounds.size.height;
    let at = |(x, y): (f64, f64)| {
        let fx = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f32;
        let fy = (y / y_max).clamp(0.0, 1.0) as f32;
        point(left + px(fx * width), bottom - px(fy * height))
    };

    // Grid: top, middle and baseline.
    for share in [0.0f32, 0.5, 1.0] {
        let y = bounds.origin.y + px((share * (height - 1.0)).max(0.0));
        window.paint_quad(fill(
            Bounds::new(point(left, y), size(bounds.size.width, px(1.))),
            grid,
        ));
    }

    // Reference lines: dashes of 4px every 7px.
    for (value, color) in thresholds {
        let y = at((x0, *value)).y;
        let mut x = 0.0f32;
        while x < width {
            let dash = 4.0f32.min(width - x);
            window.paint_quad(fill(
                Bounds::new(point(left + px(x), y), size(px(dash), px(1.))),
                color.opacity(0.8),
            ));
            x += 7.0;
        }
    }

    for (points, color) in lines {
        for segment in segments(points) {
            let projected: Vec<Point<Pixels>> = segment.iter().map(|&p| at(p)).collect();
            let (Some(first), Some(last)) = (projected.first(), projected.last()) else {
                continue;
            };
            if projected.len() > 1 {
                let mut area = PathBuilder::fill();
                area.move_to(point(first.x, bottom));
                for p in &projected {
                    area.line_to(*p);
                }
                area.line_to(point(last.x, bottom));
                area.close();
                if let Ok(path) = area.build() {
                    window.paint_path(path, color.opacity(AREA_OPACITY));
                }
                let mut stroke = PathBuilder::stroke(px(1.5));
                stroke.move_to(*first);
                for p in &projected[1..] {
                    stroke.line_to(*p);
                }
                if let Ok(path) = stroke.build() {
                    window.paint_path(path, *color);
                }
            } else {
                // A lone sample: a dot, so it is not lost.
                let dot = px(3.);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(first.x - dot / 2., first.y - dot / 2.),
                            size(dot, dot),
                        ),
                        *color,
                    )
                    .corner_radii(dot / 2.),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::SharedString;

    use super::{
        ChartLine, ChartThreshold, ago, clock, nearest_x, nice_binary_ceiling, nice_ceiling,
        on_scale, readout, segments, value_at,
    };
    use crate::tokens::Tone;

    #[test]
    fn snaps_to_the_nearest_sample() {
        let a = [(0.0, 1.0), (30.0, 2.0), (60.0, 3.0)];
        let b = [(45.0, 9.0)];
        let lines = [&a[..], &b[..]];
        assert_eq!(nearest_x(lines, 10.0), Some(0.0));
        assert_eq!(nearest_x(lines, 20.0), Some(30.0));
        assert_eq!(nearest_x(lines, 44.0), Some(45.0));
        assert_eq!(nearest_x(lines, 500.0), Some(60.0));
        assert_eq!(nearest_x(lines, -5.0), Some(0.0));
        assert_eq!(nearest_x([&[][..]], 1.0), None);
        // A line has a value at its own samples, not in gaps.
        let gappy = [(0.0, 1.0), (30.0, 2.0), (300.0, 5.0)];
        assert_eq!(value_at(&gappy, 30.0), Some(2.0));
        assert_eq!(value_at(&gappy, 40.0), Some(2.0));
        assert_eq!(value_at(&gappy, 150.0), None);
        assert_eq!(value_at(&[(5.0, 1.0)], 5.0), Some(1.0));
        assert_eq!(value_at(&[(5.0, 1.0)], 6.0), None);
    }

    #[test]
    fn formats_the_readout() {
        assert_eq!(clock(3661.0, 0), "01:01:01");
        assert_eq!(clock(3661.0, 3 * 3600), "04:01:01");
        assert_eq!(clock(60.0, -3600), "23:01:00");
        assert_eq!(ago(1000.0, 1000.0), "now");
        assert_eq!(ago(700.0, 1000.0), "-5m");
        assert_eq!(ago(1000.0 - 3.0 * 3600.0, 1000.0), "-3h");

        let lines = [
            ChartLine::new("app", vec![(0.0, 0.25), (30.0, 0.5)]),
            ChartLine::new("sidecar", vec![(0.0, 0.1)]),
            ChartLine::new("empty", vec![]),
        ];
        let thresholds = [ChartThreshold {
            name: "Limit".into(),
            value: 1.0,
            tone: Tone::Warning,
        }];
        let cores = |v: f64| SharedString::from(format!("{:.0}m", v * 1000.0));
        let out = readout(&lines, &thresholds, 30.0, 330.0, 0, &cores);
        assert_eq!(out.clock, "00:00:30");
        assert_eq!(out.ago, "-5m");
        assert_eq!(out.values.len(), 2);
        assert_eq!(out.values[0].value.as_deref(), Some("500m"));
        // The sidecar has no sample there.
        assert_eq!(out.values[1].name.as_ref(), "sidecar");
        assert_eq!(out.values[1].value, None);
        assert_eq!(
            out.thresholds,
            [("Limit".into(), "1000m".into(), Tone::Warning)]
        );
    }

    #[test]
    fn far_thresholds_stay_off_scale() {
        assert!(on_scale(0.3, 0.1));
        assert!(!on_scale(10.0, 0.26));
        // Without data, thresholds set the scale.
        assert!(on_scale(10.0, 0.0));
    }

    #[test]
    fn rounds_the_axis_up() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(f64::NAN), 1.0);
        assert_eq!(nice_ceiling(0.13), 0.2);
        assert_eq!(nice_ceiling(0.25), 0.25);
        assert_eq!(nice_ceiling(3.2), 5.0);
        assert_eq!(nice_ceiling(7.0), 10.0);
        assert_eq!(nice_ceiling(10.0), 10.0);
        assert_eq!(nice_ceiling(512.0 * 1024.0 * 1024.0), 1e9);
    }

    #[test]
    fn rounds_bytes_in_binary_units() {
        const MIB: f64 = 1024.0 * 1024.0;
        const GIB: f64 = 1024.0 * MIB;
        assert_eq!(nice_binary_ceiling(0.0), 1.0);
        assert_eq!(nice_binary_ceiling(1.6 * GIB), 2.0 * GIB);
        assert_eq!(nice_binary_ceiling(366.0 * MIB), 500.0 * MIB);
        assert_eq!(nice_binary_ceiling(1000.0 * MIB), 1000.0 * MIB);
        assert_eq!(nice_binary_ceiling(3.0), 5.0);
    }

    #[test]
    fn splits_lines_at_missing_samples() {
        let points = [
            (0.0, 1.0),
            (30.0, 1.0),
            (60.0, 1.0),
            (300.0, 2.0),
            (330.0, 2.0),
        ];
        let parts = segments(&points);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3);
        assert_eq!(parts[1].len(), 2);
        assert!(segments(&[]).is_empty());
        assert_eq!(segments(&[(1.0, 1.0)]).len(), 1);
    }
}
