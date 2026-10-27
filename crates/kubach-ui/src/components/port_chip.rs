//! A container port as a chip (`https 10250/TCP`): clickable to forward it,
//! showing the local address once forwarded, inert for ports that cannot
//! be forwarded (UDP, SCTP).

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One port of a container. The owner decides what a click does (start a
/// forward, open the forwarded address) and says so in the tooltip.
#[derive(IntoElement)]
pub struct PortChip {
    id: ElementId,
    label: SharedString,
    forwarded: Option<SharedString>,
    tooltip: Option<SharedString>,
    disabled: bool,
    on_click: Option<OnClick>,
}

impl PortChip {
    /// `label` is the port as shown, e.g. `https 10250/TCP`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            forwarded: None,
            tooltip: None,
            disabled: false,
            on_click: None,
        }
    }

    /// The local address the port is forwarded to, e.g. `127.0.0.1:10250`.
    pub fn forwarded(mut self, address: Option<impl Into<SharedString>>) -> Self {
        self.forwarded = address.map(Into::into);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Not clickable: shown muted, the tooltip (if any) says why.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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

impl RenderOnce for PortChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let forwarded = self.forwarded.is_some();
        let clickable = !self.disabled && self.on_click.is_some();
        let (border, text) = if forwarded {
            (theme.success, theme.foreground)
        } else if self.disabled {
            (theme.border, theme.muted_foreground)
        } else {
            (theme.border, theme.foreground)
        };
        let hover = theme.muted_foreground;
        let tooltip = self.tooltip;
        let on_click = self.on_click.filter(|_| clickable);
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .max_w_full()
            .h(px(22.))
            .items_center()
            .gap_1()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(theme.muted)
            .text_xs()
            .text_color(text)
            .when(clickable, |chip| {
                chip.cursor_pointer()
                    .hover(move |chip| chip.border_color(hover))
            })
            .when_some(tooltip, |chip, tip| {
                chip.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            })
            .when_some(on_click, |chip, handler| {
                chip.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .when(!self.disabled, |chip| {
                chip.child(
                    Icon::new(Lucide::ArrowRightLeft)
                        .xsmall()
                        .text_color(if forwarded {
                            theme.success
                        } else {
                            theme.muted_foreground
                        }),
                )
            })
            .child(div().flex_none().child(self.label))
            .children(self.forwarded.map(|address| {
                div()
                    .flex_none()
                    .text_color(theme.success)
                    .child(format!("→ {address}"))
            }))
    }
}
