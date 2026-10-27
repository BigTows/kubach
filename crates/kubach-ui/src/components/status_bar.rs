//! Status bar at the bottom of the main window: small items on the left
//! (connection, namespace, sync) and on the right (port forwards, MCP,
//! version). Items are quiet text until hovered; one that needs attention
//! is tinted with its tone.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{STATUS_BAR_HEIGHT, TINT_OPACITY, Tone};
use crate::{LoadingBar, StatusDot};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type Menu = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// One item of the [`StatusBar`]: an optional status dot or icon and a
/// label. Clickable with [`on_click`](Self::on_click), or opens a menu
/// above it with [`menu`](Self::menu).
pub struct StatusBarItem {
    id: ElementId,
    label: SharedString,
    dot: Option<Tone>,
    icon: Option<Lucide>,
    /// Tints the label and the icon.
    tone: Option<Tone>,
    highlight: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    menu: Option<Menu>,
}

impl StatusBarItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            dot: None,
            icon: None,
            tone: None,
            highlight: false,
            tooltip: None,
            on_click: None,
            menu: None,
        }
    }

    /// Status dot before the label, e.g. the connection state.
    pub fn dot(mut self, tone: Tone) -> Self {
        self.dot = Some(tone);
        self
    }

    /// Any icon of the full Lucide set.
    pub fn icon(mut self, icon: impl Into<Lucide>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Colors the label and the icon with `tone`.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Tinted background: something waits for the user.
    pub fn highlight(mut self, highlight: bool) -> Self {
        self.highlight = highlight;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Menu opened above the item on click.
    pub fn menu(
        mut self,
        menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(menu));
        self
    }

    fn render(self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let tone = if self.highlight {
            self.tone.or(Some(Tone::Warning))
        } else {
            self.tone
        };
        let foreground = tone.map_or(theme.muted_foreground, |tone| tone.color(cx));
        let background = match (self.highlight, tone) {
            (true, Some(tone)) => tone.color(cx).opacity(TINT_OPACITY),
            _ => theme.transparent,
        };
        let content = div()
            .flex()
            .items_center()
            .gap_1p5()
            .when_some(self.dot, |row, tone| row.child(StatusDot::new(tone)))
            .when_some(self.icon, |row, icon| {
                // A loader icon spins: a static one reads as stuck.
                if matches!(icon, Lucide::Loader | Lucide::LoaderCircle) {
                    row.child(
                        crate::spinner()
                            .icon(Icon::new(icon))
                            .with_size(px(12.))
                            .color(foreground),
                    )
                } else {
                    row.child(Icon::new(icon).size_3().text_color(foreground))
                }
            })
            .child(self.label.clone());

        if self.on_click.is_none() && self.menu.is_none() {
            return div()
                .id(self.id)
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.radius)
                .bg(background)
                .text_color(foreground)
                .child(content)
                .when_some(self.tooltip, |item, tooltip| {
                    item.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                            .build(window, cx)
                    })
                })
                .into_any_element();
        }

        let style = ButtonCustomVariant::new(cx)
            .color(background)
            .foreground(foreground)
            .hover(theme.secondary_hover)
            .active(theme.secondary_active);
        let button = Button::new(self.id)
            .custom(style)
            .xsmall()
            .h(STATUS_BAR_HEIGHT - px(4.))
            .text_xs()
            .child(content)
            .when_some(self.tooltip, |button, tooltip| button.tooltip(tooltip))
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            });
        match self.menu {
            Some(menu) => button
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |popup, window, cx| {
                    menu(popup, window, cx)
                })
                .into_any_element(),
            None => button.into_any_element(),
        }
    }
}

/// Bar along the bottom edge of a window with items pinned left and right.
#[derive(IntoElement)]
pub struct StatusBar {
    id: ElementId,
    left: Vec<StatusBarItem>,
    right: Vec<StatusBarItem>,
    loading: bool,
}

impl StatusBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            left: Vec::new(),
            right: Vec::new(),
            loading: false,
        }
    }

    /// A thin [`LoadingBar`] along the top edge while something refreshes
    /// in the background (e.g. a list shown from the cache).
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn left(mut self, item: StatusBarItem) -> Self {
        self.left.push(item);
        self
    }

    pub fn right(mut self, item: StatusBarItem) -> Self {
        self.right.push(item);
        self
    }

    /// Adds `item` on the left when it is `Some`.
    pub fn left_some(self, item: Option<StatusBarItem>) -> Self {
        match item {
            Some(item) => self.left(item),
            None => self,
        }
    }

    /// Adds `item` on the right when it is `Some`.
    pub fn right_some(self, item: Option<StatusBarItem>) -> Self {
        match item {
            Some(item) => self.right(item),
            None => self,
        }
    }
}

impl RenderOnce for StatusBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let side = |items: Vec<StatusBarItem>, cx: &App| {
            div()
                .h_full()
                .flex()
                .items_center()
                .gap_0p5()
                .min_w_0()
                .children(items.into_iter().map(|item| item.render(cx)))
        };
        let loading = self.loading.then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .child(LoadingBar::new(ElementId::NamedChild(
                    std::sync::Arc::new(self.id.clone()),
                    "loading".into(),
                )))
        });
        div()
            .id(self.id)
            .relative()
            .h(STATUS_BAR_HEIGHT)
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_1()
            .py_0p5()
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .overflow_hidden()
            .child(side(self.left, cx))
            .child(side(self.right, cx))
            .children(loading)
    }
}
