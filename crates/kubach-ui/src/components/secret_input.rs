//! Pieces of forms where a person enters sensitive values (#230): a value
//! field whose content is never drawn, a file picker row with a check
//! result, controls of a random value generator, and a block for text that
//! came from someone else and must read as unverified.
//!
//! The components hold no values: callers keep them and pass only what may
//! be shown (a status line, a size).

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::segment_tabs::{SegmentTab, SegmentTabs};
use crate::tokens::{TINT_OPACITY, Tone};
use kubach_i18n::t;

type Handler<T> = Rc<dyn Fn(&T, &mut Window, &mut App)>;

/// One value of a form: a monospace label, a status line saying what the
/// value is without showing it (`typed, 12 characters`, `generated, 32
/// characters, hidden`), action buttons and an optional editor below.
#[derive(IntoElement)]
pub struct SecretField {
    id: ElementId,
    label: SharedString,
    /// A note next to the label, such as "asked by the agent".
    badge: Option<(SharedString, Tone)>,
    status: SharedString,
    tone: Tone,
    struck: bool,
    actions: Vec<AnyElement>,
    body: Option<AnyElement>,
}

impl SecretField {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            badge: None,
            status: SharedString::default(),
            tone: Tone::Neutral,
            struck: false,
            actions: Vec::new(),
            body: None,
        }
    }

    pub fn badge(mut self, text: impl Into<SharedString>, tone: Tone) -> Self {
        self.badge = Some((text.into(), tone));
        self
    }

    pub fn status(mut self, status: impl Into<SharedString>, tone: Tone) -> Self {
        self.status = status.into();
        self.tone = tone;
        self
    }

    /// Draws the label struck through (a key being removed).
    pub fn struck(mut self, struck: bool) -> Self {
        self.struck = struck;
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }
}

impl RenderOnce for SecretField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let tone = self.tone.color(cx);
        let badge = self.badge.map(|(text, tone)| {
            let color = tone.color(cx);
            div()
                .flex_shrink_0()
                .px_1p5()
                .rounded_sm()
                .text_xs()
                .text_color(color)
                .bg(color.opacity(TINT_OPACITY))
                .child(text)
        });
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .text_color(theme.foreground)
                            .when(self.struck, |label| label.line_through())
                            .child(self.label),
                    )
                    .children(badge)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(tone)
                            .child(self.status),
                    )
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
    }
}

/// A row that picks a file: the button, the chosen file and what a check
/// of it found (`server.crt — CN=example.com, until 2027-01-01`).
#[derive(IntoElement)]
pub struct FilePickField {
    id: ElementId,
    chosen: Option<SharedString>,
    detail: Option<(SharedString, Tone)>,
    on_pick: Option<Handler<ClickEvent>>,
    disabled: bool,
}

impl FilePickField {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            chosen: None,
            detail: None,
            on_pick: None,
            disabled: false,
        }
    }

    /// The chosen file's name.
    pub fn chosen(mut self, name: Option<impl Into<SharedString>>) -> Self {
        self.chosen = name.map(Into::into);
        self
    }

    /// What a check of the file found.
    pub fn detail(mut self, detail: impl Into<SharedString>, tone: Tone) -> Self {
        self.detail = Some((detail.into(), tone));
        self
    }

    pub fn on_pick(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for FilePickField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let detail = self.detail.map(|(text, tone)| {
            div()
                .text_xs()
                .whitespace_normal()
                .text_color(tone.color(cx))
                .child(text)
        });
        let on_pick = self.on_pick;
        div()
            .flex()
            .items_start()
            .gap_2()
            .min_w_0()
            .child(
                Button::new(self.id)
                    .outline()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .label(t!("ui-secret-choose-file"))
                    .disabled(self.disabled)
                    .when_some(on_pick, |button, handler| {
                        button.on_click(move |event, window, cx| handler(event, window, cx))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(match self.chosen {
                        Some(name) => div()
                            .text_xs()
                            .text_color(foreground)
                            .whitespace_normal()
                            .child(name),
                        None => div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("ui-secret-no-file")),
                    })
                    .children(detail),
            )
    }
}

/// Length and alphabet of a random value generator, with the button that
/// draws a new value. `charsets` are labels; the caller maps the index.
#[derive(IntoElement)]
pub struct GeneratorControls {
    id: SharedString,
    length: usize,
    min: usize,
    max: usize,
    charsets: Vec<SharedString>,
    charset: usize,
    on_length: Option<Handler<usize>>,
    on_charset: Option<Handler<usize>>,
    on_generate: Option<Handler<ClickEvent>>,
}

impl GeneratorControls {
    pub fn new(
        id: impl Into<SharedString>,
        length: usize,
        range: std::ops::RangeInclusive<usize>,
    ) -> Self {
        Self {
            id: id.into(),
            length,
            min: *range.start(),
            max: *range.end(),
            charsets: Vec::new(),
            charset: 0,
            on_length: None,
            on_charset: None,
            on_generate: None,
        }
    }

    pub fn charsets(
        mut self,
        labels: impl IntoIterator<Item = impl Into<SharedString>>,
        active: usize,
    ) -> Self {
        self.charsets = labels.into_iter().map(Into::into).collect();
        self.charset = active;
        self
    }

    pub fn on_length(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_length = Some(Rc::new(handler));
        self
    }

    pub fn on_charset(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_charset = Some(Rc::new(handler));
        self
    }

    pub fn on_generate(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_generate = Some(Rc::new(handler));
        self
    }
}

/// The next length when stepping by `delta` (8 → 16 → 24 …) within bounds.
pub fn step_length(length: usize, delta: isize, min: usize, max: usize) -> usize {
    let step = 8;
    let next = if delta < 0 {
        length.saturating_sub(step)
    } else {
        length.saturating_add(step)
    };
    next.clamp(min, max)
}

impl RenderOnce for GeneratorControls {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (length, min, max) = (self.length, self.min, self.max);
        let stepper = |suffix: &'static str, delta: isize, icon: IconName| {
            let handler = self.on_length.clone();
            Button::new(SharedString::from(format!("{}-{suffix}", self.id)))
                .ghost()
                .xsmall()
                .icon(icon)
                .disabled(if delta < 0 {
                    length <= min
                } else {
                    length >= max
                })
                .when_some(handler, |button, handler| {
                    button.on_click(move |_, window, cx| {
                        handler(&step_length(length, delta, min, max), window, cx)
                    })
                })
        };
        let on_charset = self.on_charset.clone();
        let tabs = SegmentTabs::new(
            format!("{}-charset", self.id),
            self.charsets.into_iter().map(SegmentTab::new).collect(),
            self.charset,
        )
        .when_some(on_charset, |tabs, handler| {
            tabs.on_select(move |ix, window, cx| handler(ix, window, cx))
        });
        let on_generate = self.on_generate.clone();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t!("ui-secret-length")),
                    )
                    .child(stepper("shorter", -1, IconName::Minus))
                    .child(
                        div()
                            .w(px(28.))
                            .text_center()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(length.to_string()),
                    )
                    .child(stepper("longer", 1, IconName::Plus)),
            )
            .child(tabs)
            .child(
                Button::new(SharedString::from(format!("{}-generate", self.id)))
                    .outline()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .label(t!("ui-secret-generate"))
                    .when_some(on_generate, |button, handler| {
                        button.on_click(move |event, window, cx| handler(event, window, cx))
                    }),
            )
    }
}

/// Text that came from someone else (an AI agent) and is shown as is: a
/// caption says it is not verified, and the text is set apart so it does
/// not read as Kubach's own words.
#[derive(IntoElement)]
pub struct UntrustedText {
    caption: SharedString,
    text: SharedString,
}

impl UntrustedText {
    pub fn new(caption: impl Into<SharedString>, text: impl Into<SharedString>) -> Self {
        Self {
            caption: caption.into(),
            text: text.into(),
        }
    }
}

impl RenderOnce for UntrustedText {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let warning = theme.warning;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_dashed()
            .border_color(warning.opacity(0.5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(warning)
                    .child(Icon::new(IconName::TriangleAlert).xsmall())
                    .child(self.caption),
            )
            .child(
                div()
                    .max_h(px(120.))
                    .overflow_hidden()
                    .whitespace_normal()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(self.text),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::step_length;

    #[test]
    fn lengths_step_by_eight_within_bounds() {
        assert_eq!(step_length(32, 1, 8, 256), 40);
        assert_eq!(step_length(32, -1, 8, 256), 24);
        assert_eq!(step_length(8, -1, 8, 256), 8);
        assert_eq!(step_length(252, 1, 8, 256), 256);
        assert_eq!(step_length(10, -1, 8, 256), 8);
    }
}
