use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{TINT_OPACITY, Tone};

/// Small colored dot, e.g. connection state in the cluster picker.
#[derive(IntoElement)]
pub struct StatusDot {
    color: DotColor,
}

enum DotColor {
    Tone(Tone),
    Custom(Hsla),
}

impl StatusDot {
    pub fn new(tone: Tone) -> Self {
        Self {
            color: DotColor::Tone(tone),
        }
    }

    /// A dot of a color the caller resolved from the theme, e.g. the
    /// hotbar color of a context (#461).
    pub fn color(color: Hsla) -> Self {
        Self {
            color: DotColor::Custom(color),
        }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = match self.color {
            DotColor::Tone(tone) => tone.color(cx),
            DotColor::Custom(color) => color,
        };
        div().size_2().flex_shrink_0().rounded_full().bg(color)
    }
}

/// Pill with a label, tinted by tone. No dot: the whole pill already
/// carries the tone, a dot of the same color would add nothing.
#[derive(IntoElement)]
pub struct StatusBadge {
    label: SharedString,
    tone: Tone,
    shrink: bool,
}

impl StatusBadge {
    pub fn new(label: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            label: label.into(),
            tone,
            shrink: false,
        }
    }

    /// Lets the badge narrow to its container, clipping the label with an
    /// ellipsis (e.g. in a narrow table column, with the full text in a
    /// tooltip).
    pub fn shrink(mut self) -> Self {
        self.shrink = true;
        self
    }
}

impl RenderOnce for StatusBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.tone.color(cx);
        div()
            .flex()
            .map(|badge| {
                if self.shrink {
                    badge.min_w_0().overflow_hidden()
                } else {
                    badge.flex_none()
                }
            })
            .items_center()
            .px_2()
            .h(px(22.))
            .rounded_full()
            .bg(color.opacity(TINT_OPACITY))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .map(|badge| {
                if self.shrink {
                    badge.child(div().min_w_0().truncate().child(self.label))
                } else {
                    badge.child(self.label)
                }
            })
    }
}
