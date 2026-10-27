use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::components::status::StatusDot;
use crate::tokens::{TINT_OPACITY, Tone};

/// A row of tinted counters, e.g. "2 errors · 1 out of sync · 14 healthy"
/// above a list. Zero counts are left out unless [`StatusCounts::keep_zero`].
#[derive(IntoElement, Default)]
pub struct StatusCounts {
    items: Vec<(usize, SharedString, Tone)>,
    keep_zero: bool,
}

impl StatusCounts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a counter: `count` and its label, colored by `tone`.
    pub fn item(mut self, count: usize, label: impl Into<SharedString>, tone: Tone) -> Self {
        self.items.push((count, label.into(), tone));
        self
    }

    /// Shows counters that are zero too (muted).
    pub fn keep_zero(mut self) -> Self {
        self.keep_zero = true;
        self
    }

    /// Whether anything would be shown.
    pub fn is_empty(&self) -> bool {
        !self.keep_zero && self.items.iter().all(|(count, _, _)| *count == 0)
    }
}

impl RenderOnce for StatusCounts {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let keep_zero = self.keep_zero;
        div().flex().flex_wrap().items_center().gap_2().children(
            self.items
                .into_iter()
                .filter(|(count, _, _)| keep_zero || *count > 0)
                .map(|(count, label, tone)| {
                    let (tone, color) = if count == 0 {
                        (Tone::Neutral, muted)
                    } else {
                        (tone, tone.color(cx))
                    };
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .h(px(22.))
                        .rounded_md()
                        .bg(color.opacity(TINT_OPACITY))
                        .text_xs()
                        .text_color(color)
                        .child(StatusDot::new(tone))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(count.to_string()),
                        )
                        .child(label)
                }),
        )
    }
}
