//! Compact segmented tabs inside a panel (CPU / Memory / Network /
//! Filesystem). Tabs without data are muted and say why in a tooltip, but
//! can still be picked; disabled tabs (a chart range with no data, #376)
//! look the same and cannot.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

type Select = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

/// One tab of [`SegmentTabs`].
#[derive(Clone, Debug, Default)]
pub struct SegmentTab {
    pub label: SharedString,
    /// Drawn faded (no data).
    pub muted: bool,
    /// Faded and not selectable.
    pub disabled: bool,
    pub tooltip: Option<SharedString>,
}

impl SegmentTab {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// Fades the tab and ignores clicks on it.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

#[derive(IntoElement)]
pub struct SegmentTabs {
    id: SharedString,
    tabs: Vec<SegmentTab>,
    active: usize,
    on_select: Option<Select>,
}

impl SegmentTabs {
    pub fn new(id: impl Into<SharedString>, tabs: Vec<SegmentTab>, active: usize) -> Self {
        Self {
            id: id.into(),
            tabs,
            active,
            on_select: None,
        }
    }

    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SegmentTabs {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let id = self.id;
        let tabs = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .bg(theme.muted)
            .children(self.tabs.into_iter().enumerate().map(|(ix, tab)| {
                let active = ix == self.active;
                let on_select = self.on_select.clone().filter(|_| !tab.disabled);
                let foreground = match (active, tab.muted || tab.disabled) {
                    (true, _) => theme.foreground,
                    (false, false) => theme.muted_foreground,
                    (false, true) => theme.muted_foreground.opacity(0.5),
                };
                div()
                    .id((id.clone(), ix))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(foreground)
                    .when(!tab.disabled, |tab| tab.cursor_pointer())
                    .when(active, |tab| tab.bg(theme.background).shadow_xs())
                    .when(!active && !tab.disabled, |tab| {
                        tab.hover(|tab| tab.text_color(theme.foreground))
                    })
                    .when_some(on_select, |tab, handler| {
                        tab.on_click(move |_, window, cx| handler(&ix, window, cx))
                    })
                    .when_some(tab.tooltip, |tab, tooltip| {
                        tab.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    })
                    .child(tab.label)
            }));
        // A column stretches its children: the outer row takes the width,
        // the backdrop stays as wide as its tabs.
        div().flex().flex_none().child(tabs)
    }
}
