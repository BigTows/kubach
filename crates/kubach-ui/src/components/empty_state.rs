use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Centered placeholder for screens with nothing to show yet.
#[derive(IntoElement)]
pub struct EmptyState {
    icon: Lucide,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: impl Into<Lucide>, title: impl Into<SharedString>) -> Self {
        Self {
            icon: icon.into(),
            title: title.into(),
            description: None,
            action: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .py_12()
            .px_6()
            .rounded_lg()
            .border_1()
            .border_dashed()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_10()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    // A loader icon spins: a static one reads as stuck.
                    .child(
                        if matches!(self.icon, Lucide::Loader | Lucide::LoaderCircle) {
                            crate::spinner()
                                .icon(Icon::new(self.icon))
                                .color(theme.muted_foreground)
                                .into_any_element()
                        } else {
                            Icon::new(self.icon)
                                .size_5()
                                .text_color(theme.muted_foreground)
                                .into_any_element()
                        },
                    ),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(self.title),
            )
            .when_some(self.description, |col, description| {
                col.child(
                    div()
                        .max_w(px(360.))
                        .text_sm()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(description),
                )
            })
            .when_some(self.action, |col, action| {
                col.child(div().mt_2().child(action))
            })
    }
}
