//! Debug overlay of the app's own metrics (#291): a small translucent card
//! of labelled values in sections (Frames, Process, Kubach). Debug builds
//! only; the owner formats the values and refreshes the card.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::Tone;

/// Width of the card: wide enough for "Frame time 12.3 / 4.5 / 67.8 ms".
pub const DEBUG_HUD_WIDTH: Pixels = px(248.);

/// Opacity of the card background, so the screen under it stays visible.
const BACKGROUND_OPACITY: f32 = 0.86;

struct Row {
    label: SharedString,
    value: SharedString,
    tone: Option<Tone>,
}

struct Section {
    title: SharedString,
    rows: Vec<Row>,
}

/// Translucent card with sections of `label … value` rows in a monospace
/// font, and an optional muted footer (e.g. the shortcut that hides it).
#[derive(IntoElement, Default)]
pub struct DebugHud {
    sections: Vec<Section>,
    footer: Option<SharedString>,
}

impl DebugHud {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a section; the following rows go into it.
    pub fn section(mut self, title: impl Into<SharedString>) -> Self {
        self.sections.push(Section {
            title: title.into(),
            rows: Vec::new(),
        });
        self
    }

    pub fn row(self, label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        self.push(label.into(), value.into(), None)
    }

    /// A row whose value is colored with `tone` (a slow frame, high CPU).
    pub fn toned_row(
        self,
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
        tone: Tone,
    ) -> Self {
        self.push(label.into(), value.into(), Some(tone))
    }

    pub fn footer(mut self, footer: impl Into<SharedString>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    fn push(mut self, label: SharedString, value: SharedString, tone: Option<Tone>) -> Self {
        if self.sections.is_empty() {
            self = self.section("");
        }
        if let Some(section) = self.sections.last_mut() {
            section.rows.push(Row { label, value, tone });
        }
        self
    }
}

impl RenderOnce for DebugHud {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let sections = self.sections.into_iter().map(|section| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .when(!section.title.is_empty(), |column| {
                    column.child(
                        div()
                            .pb_0p5()
                            .text_color(theme.muted_foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child(section.title.to_uppercase()),
                    )
                })
                .children(section.rows.into_iter().map(|row| {
                    let value = row.tone.map_or(theme.foreground, |tone| tone.color(cx));
                    div()
                        .flex()
                        .justify_between()
                        .gap_3()
                        .child(div().text_color(theme.muted_foreground).child(row.label))
                        .child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(value)
                                .child(row.value),
                        )
                }))
        });
        div()
            .w(DEBUG_HUD_WIDTH)
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover.opacity(BACKGROUND_OPACITY))
            .shadow_lg()
            .text_xs()
            .children(sections)
            .when_some(self.footer, |card, footer| {
                card.child(
                    div()
                        .pt_1()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_color(theme.muted_foreground)
                        .child(footer),
                )
            })
    }
}
