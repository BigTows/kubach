use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::*;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// An outline button with a chevron that opens a menu of alternatives: the
/// main part runs the default choice, the chevron picks another one (e.g.
/// "Shell" into the default container, or into a chosen one). Small size, to
/// sit in a row of small outline buttons.
#[derive(IntoElement)]
pub struct SplitButton {
    id: ElementId,
    label: SharedString,
    icon: Option<gpui_kit::assets::IconName>,
    tooltip: Option<SharedString>,
    menu_tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    menu: Option<MenuBuilder>,
}

impl SplitButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            tooltip: None,
            menu_tooltip: None,
            on_click: None,
            menu: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<gpui_kit::assets::IconName>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Tooltip of the main part.
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Tooltip of the chevron.
    pub fn menu_tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.menu_tooltip = Some(tooltip.into());
        self
    }

    /// Runs the default choice.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Fills the chevron's menu.
    pub fn menu(
        mut self,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(builder));
        self
    }
}

impl RenderOnce for SplitButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let mut main = Button::new(ElementId::from((self.id.clone(), "main")))
            .ghost()
            .small();
        // An empty label leaves an icon-only button; the tooltip names it.
        if !self.label.is_empty() {
            main = main.label(self.label);
        }
        if let Some(icon) = self.icon {
            main = main.icon(icon);
        }
        if let Some(tooltip) = self.tooltip {
            main = main.tooltip(tooltip);
        }
        if let Some(handler) = self.on_click {
            main = main.on_click(move |event, window, cx| handler(event, window, cx));
        }
        let mut chevron = Button::new(ElementId::from((self.id.clone(), "menu")))
            .ghost()
            .small()
            .icon(IconName::ChevronDown);
        if let Some(tooltip) = self.menu_tooltip {
            chevron = chevron.tooltip(tooltip);
        }
        let menu = self.menu;
        div()
            .id(self.id)
            .h_6()
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(main)
            .child(div().w_px().h_4().flex_shrink_0().bg(theme.border))
            .child(chevron.dropdown_menu(move |popup, window, cx| match &menu {
                Some(menu) => menu(popup, window, cx),
                None => popup,
            }))
    }
}
