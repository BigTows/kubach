use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::TruncatedText;
use crate::tokens::ROW_HEIGHT;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Sidebar navigation row: icon, label and an optional trailing count.
#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    icon: Icon,
    label: SharedString,
    count: Option<usize>,
    selected: bool,
    nested: bool,
    on_click: Option<ClickHandler>,
}

impl NavItem {
    pub fn new(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            label: label.into(),
            count: None,
            selected: false,
            nested: false,
            on_click: None,
        }
    }

    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Indented under the item above it, as a sub-page.
    pub fn nested(mut self, nested: bool) -> Self {
        self.nested = nested;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NavItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, bg) = if self.selected {
            (theme.sidebar_accent_foreground, theme.sidebar_accent)
        } else {
            (theme.sidebar_foreground, theme.transparent)
        };
        let hover = theme.list_hover;
        let muted = theme.muted_foreground;

        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap_2()
            .h(ROW_HEIGHT)
            .px_2()
            .when(self.nested, |row| row.pl(px(28.)))
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(fg)
            .bg(bg)
            .when(!self.selected, |row| row.hover(move |row| row.bg(hover)))
            .when_some(self.on_click, |row, handler| {
                row.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(self.icon.size_4())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(TruncatedText::new("label", self.label)),
            )
            .when_some(self.count, |row, count| {
                row.child(div().text_xs().text_color(muted).child(count.to_string()))
            })
    }
}
