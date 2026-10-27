use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::TruncatedText;

const STRIP_HEIGHT: Pixels = px(36.);
const TAB_MAX_WIDTH: Pixels = px(240.);
const ACCENT_HEIGHT: Pixels = px(2.);

type TabHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// One tab of a [`TabStrip`].
#[derive(Clone, Default)]
pub struct TabItem {
    pub title: SharedString,
    pub icon: Option<IconName>,
    /// Small chip after the title, e.g. the cluster a tab belongs to.
    pub badge: Option<SharedString>,
    /// Shows a dot for unsaved changes.
    pub dirty: bool,
    pub closable: bool,
    /// Color the tab is marked with: a strip along its top edge and a
    /// tinted icon, e.g. the color of the cluster the tab shows.
    pub accent: Option<Hsla>,
}

impl TabItem {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn badge(mut self, badge: impl Into<SharedString>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    pub fn accent(mut self, accent: impl Into<Option<Hsla>>) -> Self {
        self.accent = accent.into();
        self
    }
}

/// Document tabs above a content area: click selects, the × or a
/// middle-click closes a closable tab.
#[derive(IntoElement)]
pub struct TabStrip {
    id: ElementId,
    items: Vec<TabItem>,
    active: usize,
    on_select: Option<TabHandler>,
    on_close: Option<TabHandler>,
}

impl TabStrip {
    pub fn new(id: impl Into<ElementId>, items: Vec<TabItem>, active: usize) -> Self {
        Self {
            id: id.into(),
            items,
            active,
            on_select: None,
            on_close: None,
        }
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    pub fn on_close(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TabStrip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let tabs = self.items.into_iter().enumerate().map(|(ix, item)| {
            let active = ix == self.active;
            let on_select = self.on_select.clone();
            let on_close = self.on_close.clone().filter(|_| item.closable);
            let middle_close = on_close.clone();
            let accent = item.accent;
            div()
                .id(ix)
                .relative()
                .h_full()
                .max_w(TAB_MAX_WIDTH)
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1p5()
                .pl_3()
                .when(!item.closable, |tab| tab.pr_3())
                .when(item.closable, |tab| tab.pr_1p5())
                .border_r_1()
                .border_color(theme.border)
                .text_sm()
                .cursor_pointer()
                .when(active, |tab| {
                    tab.bg(theme.tab_active)
                        .text_color(theme.tab_active_foreground)
                })
                .when(!active, |tab| {
                    tab.text_color(theme.tab_foreground)
                        .hover(|tab| tab.bg(theme.muted))
                })
                .when_some(on_select, |tab, handler| {
                    tab.on_click(move |_, window, cx| handler(ix, window, cx))
                })
                .when_some(middle_close, |tab, handler| {
                    tab.on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        cx.stop_propagation();
                        handler(ix, window, cx)
                    })
                })
                .when_some(accent, |tab, color| {
                    tab.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(ACCENT_HEIGHT)
                            .bg(color),
                    )
                })
                .when_some(item.icon, |tab, icon| {
                    tab.child(
                        Icon::new(icon)
                            .size_3p5()
                            .flex_shrink_0()
                            .when_some(accent, |icon, color| icon.text_color(color)),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .child(TruncatedText::new("title", item.title)),
                )
                .when_some(item.badge, |tab, badge| {
                    tab.child(
                        div()
                            .flex_shrink_0()
                            .px_1p5()
                            .rounded_sm()
                            .bg(theme.muted)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(badge),
                    )
                })
                .when(item.dirty, |tab| {
                    tab.child(
                        div()
                            .flex_shrink_0()
                            .size(px(6.))
                            .rounded_full()
                            .bg(theme.warning),
                    )
                })
                .when_some(on_close, |tab, handler| {
                    tab.child(
                        div()
                            .id(("close", ix))
                            .flex_shrink_0()
                            .p_0p5()
                            .rounded_sm()
                            .text_color(theme.muted_foreground)
                            .hover(|close| close.bg(theme.muted).text_color(theme.foreground))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                handler(ix, window, cx)
                            })
                            .child(Icon::new(IconName::Close).xsmall()),
                    )
                })
        });
        div()
            .id(self.id)
            .h(STRIP_HEIGHT)
            .flex_shrink_0()
            .flex()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.tab_bar)
            .children(tabs)
    }
}
