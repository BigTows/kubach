//! Usage of a resource against what is available (#414): a bar filled to
//! the share used, with marks at the requests and the limits, and a legend
//! with each value in percent and in absolute terms.
//!
//! The fill turns warning at 75 % and danger at 90 %, like the usage meter
//! of the details. Marks past the end (limits overcommitted) stay at the end
//! and are flagged in the legend. Mark colors are the theme's `usage.*`.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::usage_marker_colors;
use crate::tokens::Tone;
use kubach_i18n::t;

type Format = Rc<dyn Fn(f64) -> SharedString>;

/// Share of `value` in `total`, `None` when there is no total.
pub fn usage_share(value: f64, total: f64) -> Option<f64> {
    (total > 0.0 && value.is_finite()).then(|| value.max(0.0) / total)
}

/// Tone of the fill at share `share` of the total.
pub fn usage_tone(share: f64) -> Tone {
    if share >= 0.9 {
        Tone::Danger
    } else if share >= 0.75 {
        Tone::Warning
    } else {
        Tone::Success
    }
}

/// `"1.2 · 12%"`, or the value alone without a total; `▲` past the total.
fn legend_value(value: f64, total: f64, format: &Format) -> String {
    match usage_share(value, total) {
        Some(share) if share > 1.0 => format!("{} · {:.0}% ▲", format(value), share * 100.0),
        Some(share) => format!("{} · {:.0}%", format(value), share * 100.0),
        None => format(value).to_string(),
    }
}

#[derive(IntoElement)]
pub struct UsageBar {
    total: f64,
    total_label: SharedString,
    used: Option<f64>,
    requests: Option<f64>,
    limits: Option<f64>,
    /// Appended to the limits in the legend, e.g. "3 pods unbounded".
    limits_note: Option<SharedString>,
    format: Format,
}

impl UsageBar {
    /// A bar against `total` (allocatable, capacity).
    pub fn new(total: f64) -> Self {
        Self {
            total,
            total_label: t!("ui-usage-allocatable").into(),
            used: None,
            requests: None,
            limits: None,
            limits_note: None,
            format: Rc::new(|value| format!("{value:.2}").into()),
        }
    }

    /// The name of the total in the legend ("Allocatable" by default).
    pub fn total_label(mut self, label: impl Into<SharedString>) -> Self {
        self.total_label = label.into();
        self
    }

    /// The share used fills the bar; without it the bar shows only marks.
    pub fn used(mut self, used: Option<f64>) -> Self {
        self.used = used;
        self
    }

    pub fn requests(mut self, requests: f64) -> Self {
        self.requests = Some(requests);
        self
    }

    pub fn limits(mut self, limits: f64) -> Self {
        self.limits = Some(limits);
        self
    }

    pub fn limits_note(mut self, note: impl Into<SharedString>) -> Self {
        self.limits_note = Some(note.into());
        self
    }

    /// Formats values in the legend.
    pub fn format(mut self, format: impl Fn(f64) -> SharedString + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }
}

impl RenderOnce for UsageBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (request_color, limit_color) = usage_marker_colors(cx);
        let used_share = self.used.and_then(|used| usage_share(used, self.total));
        let fill_color = used_share.map(|share| usage_tone(share).color(cx));
        let theme = cx.theme();
        let mark = |share: Option<f64>, color: Hsla| {
            share.map(|share| {
                div()
                    .absolute()
                    .top(px(-3.))
                    .h(px(12.))
                    .w(px(2.))
                    .left(relative(share.clamp(0.0, 1.0) as f32))
                    .ml(px(-1.))
                    .rounded_sm()
                    .bg(color)
            })
        };
        let bar = div()
            .relative()
            .h(px(6.))
            .w_full()
            .rounded_full()
            .bg(theme.muted)
            .when_some(used_share.zip(fill_color), |bar, (share, color)| {
                bar.child(
                    div()
                        .h_full()
                        .w(relative(share.clamp(0.0, 1.0) as f32))
                        .rounded_full()
                        .bg(color),
                )
            })
            .children(mark(
                self.requests.and_then(|v| usage_share(v, self.total)),
                request_color,
            ))
            .children(mark(
                self.limits.and_then(|v| usage_share(v, self.total)),
                limit_color,
            ));
        let format = self.format.clone();
        let row = |swatch: Option<Hsla>, label: String, value: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .whitespace_nowrap()
                        .text_color(theme.muted_foreground)
                        .child(
                            div()
                                .size(px(8.))
                                .rounded_sm()
                                .when_some(swatch, |dot, color| dot.bg(color))
                                .when(swatch.is_none(), |dot| {
                                    dot.border_1().border_color(theme.border)
                                }),
                        )
                        .child(label.to_owned()),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(value),
                )
        };
        let limits = self.limits.map(|limits| {
            let mut value = legend_value(limits, self.total, &format);
            if let Some(note) = &self.limits_note {
                value = format!("{value} · {note}");
            }
            row(Some(limit_color), t!("ui-usage-limits"), value)
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_xs()
            .child(bar)
            .child(row(
                fill_color,
                t!("ui-usage-used"),
                self.used.map_or_else(
                    || t!("ui-unavailable"),
                    |used| legend_value(used, self.total, &format),
                ),
            ))
            .when_some(self.requests, |legend, requests| {
                legend.child(row(
                    Some(request_color),
                    t!("ui-usage-requests"),
                    legend_value(requests, self.total, &format),
                ))
            })
            .children(limits)
            .child(row(
                None,
                self.total_label.to_string(),
                format(self.total).to_string(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Tone, legend_value, usage_share, usage_tone};

    #[test]
    fn shares_and_tones() {
        assert_eq!(usage_share(2.0, 8.0), Some(0.25));
        assert_eq!(usage_share(1.0, 0.0), None);
        assert_eq!(usage_tone(0.5), Tone::Success);
        assert_eq!(usage_tone(0.8), Tone::Warning);
        assert_eq!(usage_tone(1.2), Tone::Danger);
    }

    #[test]
    fn legend_shows_percent_and_value() {
        let format: super::Format = Rc::new(|v| format!("{v:.1}").into());
        assert_eq!(legend_value(2.0, 8.0, &format), "2.0 · 25%");
        assert_eq!(legend_value(12.0, 8.0, &format), "12.0 · 150% ▲");
        assert_eq!(legend_value(2.0, 0.0, &format), "2.0");
    }
}
