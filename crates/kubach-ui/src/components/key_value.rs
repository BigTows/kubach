use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::measure::{TEXT_SM, label_column_width};

/// Two-column list of resource fields: muted key, monospace value.
#[derive(IntoElement)]
pub struct KeyValueList {
    rows: Vec<(SharedString, AnyElement)>,
}

impl KeyValueList {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub fn row(mut self, key: impl Into<SharedString>, value: impl IntoElement) -> Self {
        self.rows.push((key.into(), value.into_any_element()));
        self
    }
}

impl Default for KeyValueList {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for KeyValueList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // As wide as the widest key: 140 px fits English, translations run
        // longer (#444); past 240 px a key wraps.
        let key_width = label_column_width(
            self.rows.iter().map(|(key, _)| key.as_ref()),
            TEXT_SM,
            px(140.),
            px(240.),
            window,
            cx,
        );
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .children(self.rows.into_iter().map(|(key, value)| {
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .w(key_width)
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(key),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(value),
                    )
            }))
    }
}

/// `key=value` chip for labels, annotations and selectors.
#[derive(IntoElement)]
pub struct LabelChip {
    key: SharedString,
    value: SharedString,
}

impl LabelChip {
    pub fn new(key: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

impl RenderOnce for LabelChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_none()
            .h(px(22.))
            .items_center()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .child(div().text_color(theme.muted_foreground).child(self.key))
            .child(div().text_color(theme.muted_foreground).child("="))
            .child(div().text_color(theme.foreground).child(self.value))
    }
}
