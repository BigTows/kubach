//! The Diff tab of a release: what changed between two revisions.

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::*;
use kubach_i18n::t;
use kubach_ui::{DiffView, EmptyState};

use super::super::helm_release::DiffSource;
use super::{Detail, HelmScreen};

impl HelmScreen {
    pub(super) fn render_diff(&self, detail: &Detail, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some((from, to)) = detail.diff else {
            return EmptyState::new(IconName::Inbox, t!("helm-diff-nothing"))
                .description(t!("helm-diff-single-revision"))
                .into_any_element();
        };
        let screen = cx.entity().downgrade();
        let revisions: Vec<u32> = detail.revisions().iter().map(|r| r.revision).collect();
        let picker = |id: &'static str, label: String, current: u32, is_from: bool| {
            let (screen, revisions) = (screen.clone(), revisions.clone());
            Button::new(id)
                .outline()
                .xsmall()
                .label(label)
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    revisions.iter().rev().fold(menu, |menu, &revision| {
                        let screen = screen.clone();
                        menu.item(
                            PopupMenuItem::new(t!("helm-revision", revision = revision))
                                .checked(revision == current)
                                .on_click(move |_, _, cx| {
                                    screen
                                        .update(cx, |this, cx| {
                                            if let Some(detail) = &mut this.detail
                                                && let Some((from, to)) = &mut detail.diff
                                            {
                                                *(if is_from { from } else { to }) = revision;
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                }),
                        )
                    })
                })
        };
        let source_picker = {
            let screen = screen.clone();
            let current = detail.diff_source;
            Button::new("helm-diff-source")
                .outline()
                .xsmall()
                .label(current.title())
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    DiffSource::ALL.iter().fold(menu, |menu, &source| {
                        let screen = screen.clone();
                        menu.item(
                            PopupMenuItem::new(source.title())
                                .checked(source == current)
                                .on_click(move |_, _, cx| {
                                    screen
                                        .update(cx, |this, cx| {
                                            if let Some(detail) = &mut this.detail {
                                                detail.diff_source = source;
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                }),
                        )
                    })
                })
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(source_picker)
            .child(t!("helm-diff-from"))
            .child(picker(
                "helm-diff-from",
                t!("helm-revision", revision = from),
                from,
                true,
            ))
            .child(t!("helm-diff-to"))
            .child(picker(
                "helm-diff-to",
                t!("helm-revision", revision = to),
                to,
                false,
            ));
        let (Some(old), Some(new)) = (detail.find(from), detail.find(to)) else {
            return toolbar.into_any_element();
        };
        let (old, new) = (detail.diff_source.text(old), detail.diff_source.text(new));
        let body = if old == new {
            EmptyState::new(IconName::Check, t!("helm-diff-none"))
                .description(t!(
                    "helm-diff-identical",
                    source = detail.diff_source.title(),
                    from = from,
                    to = to
                ))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(DiffView::new(
                    "helm-diff",
                    crate::editor::diff_view_lines(&old, &new),
                ))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }
}
