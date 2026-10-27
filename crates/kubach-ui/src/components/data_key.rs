//! One key of a ConfigMap or Secret edited on its own (#275): the key name
//! (a label or an input for renaming), a chip with what the value holds
//! (`JSON`, `YAML`), a status, action buttons, the value's editor and a
//! problem line under it (an invalid JSON that can still be saved).
//!
//! The block holds no state: callers pass the editor and the texts.

use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{TINT_OPACITY, Tone};

/// Lines a value editor shows before it scrolls.
pub const DATA_KEY_MAX_LINES: usize = 18;
const DATA_KEY_MIN_LINES: usize = 3;
const LINE_HEIGHT: Pixels = px(20.);
const EDITOR_PADDING: Pixels = px(14.);

/// Height of a value editor holding `lines` lines: it grows with the text
/// from 3 up to [`DATA_KEY_MAX_LINES`] lines.
pub fn data_key_editor_height(lines: usize) -> Pixels {
    let lines = lines.clamp(DATA_KEY_MIN_LINES, DATA_KEY_MAX_LINES);
    LINE_HEIGHT * lines as f32 + EDITOR_PADDING
}

#[derive(IntoElement)]
pub struct DataKeyBlock {
    id: ElementId,
    name: AnyElement,
    kind: Option<SharedString>,
    status: Option<(SharedString, Tone)>,
    actions: Vec<AnyElement>,
    body: Option<AnyElement>,
    problem: Option<(SharedString, Tone)>,
}

impl DataKeyBlock {
    /// A block whose name is any element, such as an input to rename it.
    pub fn new(id: impl Into<ElementId>, name: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            name: name.into_any_element(),
            kind: None,
            status: None,
            actions: Vec::new(),
            body: None,
            problem: None,
        }
    }

    /// A block named by a fixed key, drawn monospace (struck through when
    /// the key is being removed).
    pub fn labeled(
        id: impl Into<ElementId>,
        key: impl Into<SharedString>,
        struck: bool,
        cx: &App,
    ) -> Self {
        let theme = cx.theme();
        let label = div()
            .min_w_0()
            .truncate()
            .font_family(theme.mono_font_family.clone())
            .text_sm()
            .text_color(theme.foreground)
            .when(struck, |label| label.line_through())
            .child(key.into());
        Self::new(id, label)
    }

    /// What the value holds: `JSON`, `YAML`, `Text`.
    pub fn kind(mut self, kind: impl Into<SharedString>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// A short state next to the name: `new`, `changed`, `12 bytes`.
    pub fn status(mut self, status: impl Into<SharedString>, tone: Tone) -> Self {
        self.status = Some((status.into(), tone));
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// The value: a code editor, or a line saying why it isn't shown.
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    /// A problem with the value, shown under it.
    pub fn problem(mut self, problem: Option<impl Into<SharedString>>, tone: Tone) -> Self {
        self.problem = problem.map(|text| (text.into(), tone));
        self
    }
}

impl RenderOnce for DataKeyBlock {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let kind = self.kind.map(|kind| {
            div()
                .flex_shrink_0()
                .px_1p5()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(kind)
        });
        let status = self.status.map(|(text, tone)| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(tone.color(cx))
                .child(text)
        });
        let problem = self.problem.map(|(text, tone)| {
            let color = tone.color(cx);
            div()
                .flex()
                .items_start()
                .gap_1p5()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(color.opacity(TINT_OPACITY))
                .text_xs()
                .text_color(theme.foreground)
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size_3()
                        .flex_shrink_0()
                        .mt_0p5()
                        .text_color(color),
                )
                .child(div().flex_1().min_w_0().whitespace_normal().child(text))
        });
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().flex().child(self.name))
                    .children(kind)
                    .children(status)
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .children(self.actions),
                    ),
            )
            .children(self.body)
            .children(problem)
    }
}

#[cfg(test)]
mod tests {
    use super::{DATA_KEY_MAX_LINES, data_key_editor_height};
    use gpui_kit::px;

    #[test]
    fn editor_grows_with_the_text_within_bounds() {
        assert_eq!(data_key_editor_height(0), data_key_editor_height(3));
        assert!(data_key_editor_height(5) > data_key_editor_height(3));
        assert_eq!(
            data_key_editor_height(500),
            data_key_editor_height(DATA_KEY_MAX_LINES)
        );
        assert_eq!(data_key_editor_height(3), px(74.));
    }
}
