//! A counter card of a dashboard (#414): a label, a big value, a caption
//! and whatever goes under it (status counts, a usage bar). A scope note in
//! the corner says what the numbers cover, e.g. "whole cluster".

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::Tone;

/// Narrowest a card gets before the row wraps.
pub const STAT_CARD_MIN_WIDTH: Pixels = px(300.);

#[derive(IntoElement)]
pub struct StatCard {
    label: SharedString,
    value: SharedString,
    /// Colors the value; the foreground when `None`.
    tone: Option<Tone>,
    caption: Option<SharedString>,
    scope: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl StatCard {
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            tone: None,
            caption: None,
            scope: None,
            children: Vec::new(),
        }
    }

    /// Colors the value (e.g. danger when nodes are not ready).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Muted text next to the value: `of 3 ready`, `of allocatable`.
    pub fn caption(mut self, caption: impl Into<SharedString>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// What the numbers cover, in the top right corner.
    pub fn scope(mut self, scope: impl Into<SharedString>) -> Self {
        self.scope = Some(scope.into());
        self
    }
}

impl ParentElement for StatCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for StatCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let value_color = self
            .tone
            .map_or(cx.theme().foreground, |tone| tone.color(cx));
        let theme = cx.theme();
        div()
            .flex_1()
            .min_w(STAT_CARD_MIN_WIDTH)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(self.label),
                    )
                    .when_some(self.scope, |row, scope| {
                        row.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .text_color(theme.muted_foreground)
                                .child(scope),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_family(theme.mono_font_family.clone())
                            .text_color(value_color)
                            .child(self.value),
                    )
                    .when_some(self.caption, |row, caption| {
                        row.child(
                            div()
                                .min_w_0()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(caption),
                        )
                    }),
            )
            .children(self.children)
    }
}
