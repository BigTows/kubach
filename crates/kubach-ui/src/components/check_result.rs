use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{TINT_OPACITY, Tone};

/// Where a check (e.g. "Test connection") stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// Running; the text says what is being done.
    Running(SharedString),
    Passed {
        title: SharedString,
        detail: Option<SharedString>,
    },
    Failed {
        title: SharedString,
        /// The underlying error, in a monospace font.
        detail: Option<SharedString>,
        /// What to try next.
        hint: Option<SharedString>,
    },
}

/// A tinted box with the outcome of a check: a spinner while it runs, then
/// a title with an optional detail and hint.
#[derive(IntoElement)]
pub struct CheckResult {
    id: ElementId,
    status: CheckStatus,
}

impl CheckResult {
    pub fn new(id: impl Into<ElementId>, status: CheckStatus) -> Self {
        Self {
            id: id.into(),
            status,
        }
    }
}

impl RenderOnce for CheckResult {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tone = match &self.status {
            CheckStatus::Running(_) => Tone::Neutral,
            CheckStatus::Passed { .. } => Tone::Success,
            CheckStatus::Failed { .. } => Tone::Danger,
        };
        let color = tone.color(cx);
        let theme = cx.theme();
        let (muted, foreground, mono) = (
            theme.muted_foreground,
            theme.foreground,
            theme.mono_font_family.clone(),
        );
        let icon = match &self.status {
            CheckStatus::Running(_) => crate::spinner().small().color(color).into_any_element(),
            CheckStatus::Passed { .. } => Icon::new(IconName::CircleCheck)
                .small()
                .text_color(color)
                .into_any_element(),
            CheckStatus::Failed { .. } => Icon::new(IconName::CircleX)
                .small()
                .text_color(color)
                .into_any_element(),
        };
        let (title, detail, hint) = match self.status {
            CheckStatus::Running(text) => (text, None, None),
            CheckStatus::Passed { title, detail } => (title, detail, None),
            CheckStatus::Failed {
                title,
                detail,
                hint,
            } => (title, detail, hint),
        };
        div()
            .id(self.id)
            .w_full()
            .min_w_0()
            .flex()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(color.opacity(0.4))
            .bg(color.opacity(TINT_OPACITY))
            .child(div().flex_shrink_0().pt_0p5().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(foreground)
                            .child(title),
                    )
                    .when_some(hint, |this, hint| {
                        this.child(div().text_xs().text_color(foreground).child(hint))
                    })
                    .when_some(detail, |this, detail| {
                        this.child(
                            div()
                                .max_h(px(96.))
                                .overflow_hidden()
                                .text_xs()
                                .font_family(mono)
                                .text_color(muted)
                                .child(detail),
                        )
                    }),
            )
    }
}
