//! A column of selectable items beside a page, e.g. the contexts on the
//! per-cluster settings page: a colored mark, a name, a muted second line
//! and a short badge. Scrolls on its own; a header (such as a filter
//! input) stays on top. Filtering is up to the caller.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

use crate::TruncatedText;

type Select = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// Default width of a [`SideList`].
pub const SIDE_LIST_WIDTH: Pixels = px(240.);

/// One entry of a [`SideList`].
#[derive(Clone, Debug, Default)]
pub struct SideListItem {
    /// Passed to the select handler; unique within the list.
    pub key: SharedString,
    pub label: SharedString,
    /// Muted line under the label.
    pub detail: Option<SharedString>,
    /// Small mark left of the label, e.g. the color of a cluster tile.
    pub accent: Option<Hsla>,
    /// Short muted text right of the label.
    pub badge: Option<SharedString>,
    /// Drawn faded, e.g. hidden elsewhere.
    pub muted: bool,
}

impl SideListItem {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn accent(mut self, accent: Hsla) -> Self {
        self.accent = Some(accent);
        self
    }

    pub fn badge(mut self, badge: impl Into<SharedString>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }
}

/// Vertical list of [`SideListItem`]s with one selected.
#[derive(IntoElement)]
pub struct SideList {
    id: SharedString,
    items: Vec<SideListItem>,
    selected: Option<SharedString>,
    header: Option<AnyElement>,
    empty: SharedString,
    width: Pixels,
    on_select: Option<Select>,
}

impl SideList {
    /// `id` prefixes the ids of the items.
    pub fn new(id: impl Into<SharedString>, items: Vec<SideListItem>) -> Self {
        Self {
            id: id.into(),
            items,
            selected: None,
            header: None,
            empty: t!("ui-side-list-empty").into(),
            width: SIDE_LIST_WIDTH,
            on_select: None,
        }
    }

    /// Key of the selected item.
    pub fn selected(mut self, key: Option<impl Into<SharedString>>) -> Self {
        self.selected = key.map(Into::into);
        self
    }

    /// Shown above the items and not scrolled, e.g. a filter input.
    pub fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(header.into_any_element());
        self
    }

    /// Text shown when there are no items.
    pub fn empty(mut self, text: impl Into<SharedString>) -> Self {
        self.empty = text.into();
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    pub fn on_select(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SideList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, hover, active) = (
            theme.foreground,
            theme.muted_foreground,
            theme.list_hover,
            theme.list_active,
        );
        let empty = self.items.is_empty();
        let rows = self.items.into_iter().enumerate().map(|(ix, item)| {
            let selected = self.selected.as_ref() == Some(&item.key);
            let on_select = self.on_select.clone();
            let key = item.key.clone();
            div()
                .id(ElementId::NamedInteger(self.id.clone(), ix as u64))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |row| row.bg(active))
                .when(!selected, |row| row.hover(move |row| row.bg(hover)))
                .when_some(on_select, |row, handler| {
                    row.on_click(move |_, window, cx| handler(&key, window, cx))
                })
                .child(
                    div()
                        .size(px(8.))
                        .flex_shrink_0()
                        .rounded_full()
                        .when_some(item.accent, |mark, color| mark.bg(color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .text_color(if item.muted { muted } else { fg })
                                .child(TruncatedText::new("label", item.label)),
                        )
                        .when_some(item.detail, |col, detail| {
                            col.child(div().text_xs().truncate().text_color(muted).child(detail))
                        }),
                )
                .when_some(item.badge, |row, badge| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child(badge),
                    )
                })
        });
        div()
            .w(self.width)
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .when_some(self.header, |list, header| {
                list.child(div().flex_shrink_0().p_2().pb_0().child(header))
            })
            .child(
                div()
                    .id(SharedString::from(format!("{}-scroll", self.id)))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .when(empty, |list| {
                        list.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(muted)
                                .child(self.empty),
                        )
                    })
                    .children(rows),
            )
    }
}
