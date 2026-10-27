//! "← Topology": a way back to where the user came from, with the target
//! in its tooltip ("Back to Topology (⌘[)"). Used by the workspace's
//! history (#395) and the stack of the details panel (#393).

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::*;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Longest label shown in full; longer ones end with "…" (the tooltip
/// keeps the whole target).
pub const BACK_LABEL_MAX: usize = 32;

/// `label` cut to [`BACK_LABEL_MAX`] characters.
pub fn back_label(label: &str) -> String {
    if label.chars().count() <= BACK_LABEL_MAX {
        return label.to_owned();
    }
    let mut cut: String = label.chars().take(BACK_LABEL_MAX - 1).collect();
    cut.push('…');
    cut
}

/// A small ghost button with an arrow and the target's name.
#[derive(IntoElement)]
pub struct BackButton {
    id: ElementId,
    label: SharedString,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl BackButton {
    /// `label` names where the button goes: "Topology", "Deployment web".
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            tooltip: None,
            on_click: None,
        }
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
}

impl RenderOnce for BackButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut button = Button::new(self.id)
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label(back_label(&self.label));
        if let Some(tooltip) = self.tooltip {
            button = button.tooltip(tooltip);
        }
        if let Some(handler) = self.on_click {
            button = button.on_click(move |event, window, cx| handler(event, window, cx));
        }
        button
    }
}

#[cfg(test)]
mod tests {
    use super::{BACK_LABEL_MAX, back_label};

    #[test]
    fn long_labels_are_cut() {
        assert_eq!(back_label("Topology"), "Topology");
        let long = "Deployment a-very-long-deployment-name-indeed";
        let cut = back_label(long);
        assert_eq!(cut.chars().count(), BACK_LABEL_MAX);
        assert!(cut.ends_with('…'));
    }
}
