use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

type Toggle = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Bordered card with an optional title row. A titled panel can fold: its
/// owner keeps the state and flips it in the toggle handler.
#[derive(IntoElement)]
pub struct Panel {
    title: Option<SharedString>,
    /// Shown after the title, e.g. a count.
    note: Option<SharedString>,
    toggle: Option<(SharedString, bool, Toggle)>,
    children: Vec<AnyElement>,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            title: None,
            note: None,
            toggle: None,
            children: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Muted text after the title.
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Makes the title row fold the panel: the body shows only when `open`,
    /// and clicking the title calls `handler`.
    pub fn collapsible(
        mut self,
        id: impl Into<SharedString>,
        open: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.toggle = Some((id.into(), open, Rc::new(handler)));
        self
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Panel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Panel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let open = self.toggle.as_ref().is_none_or(|(_, open, _)| *open);
        let header = self.title.map(|title| {
            let row = div()
                .px_4()
                .py_2p5()
                .flex()
                .items_center()
                .gap_1p5()
                .when(open, |row| row.border_b_1().border_color(theme.border))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .when(self.toggle.is_some(), |row| {
                    let icon = if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    };
                    row.child(Icon::new(icon).xsmall().text_color(theme.muted_foreground))
                })
                .child(title)
                .when_some(self.note, |row, note| {
                    row.child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(note),
                    )
                });
            match self.toggle {
                Some((id, _, handler)) => row
                    .id(id)
                    .cursor_pointer()
                    .on_click(move |event, window, cx| handler(event, window, cx))
                    .into_any_element(),
                None => row.into_any_element(),
            }
        });
        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .children(header)
            .when(open, |panel| {
                panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .children(self.children),
                )
            })
    }
}
