use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// One line of text cut with "…" where it doesn't fit, with the whole text
/// as a tooltip while it is cut (#444): translations run longer than the
/// English the layout was drawn for. Takes the text style of its parent.
///
/// Whether the text is cut is known after layout, so the tooltip appears
/// from the next frame on; the id keeps that between frames and must be
/// unique among its siblings.
#[derive(IntoElement)]
pub struct TruncatedText {
    id: ElementId,
    text: SharedString,
}

impl TruncatedText {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

impl RenderOnce for TruncatedText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clipped = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let tooltip = (*clipped.read(cx)).then(|| self.text.clone());
        let text = self.text.clone();
        div()
            .min_w_0()
            .truncate()
            .on_children_prepainted(move |bounds, window, cx| {
                let Some(shown) = bounds.first() else {
                    return;
                };
                // The text style of this element is in effect here.
                let style = window.text_style();
                let size = style.font_size.to_pixels(window.rem_size());
                let full = window
                    .text_system()
                    .shape_line(
                        text.replace('\n', " ").into(),
                        size,
                        &[style.to_run(text.len())],
                        None,
                    )
                    .width;
                let cut = full > shown.size.width + px(0.5);
                clipped.update(cx, |clipped, cx| {
                    if *clipped != cut {
                        *clipped = cut;
                        cx.notify();
                    }
                });
            })
            .child(self.text)
            .id(self.id)
            .when_some(tooltip, |text, tooltip| {
                text.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
    }
}
