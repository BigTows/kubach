//! One open release: loading its history, the summary, the tabs (values,
//! manifest, hooks, notes, history) and the revision picked in the history.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::tab::{Tab as TabItem, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::helm::{Release, ReleaseStatus, Releases};
use kubach_core::runtime;
use kubach_i18n::t;
use kubach_ui::{EmptyState, KeyValueList, Panel, ResourceTable, StatusBadge};

use super::super::helm_release::{
    DiffSource, Tab, default_diff_pair, revision_columns, revision_row, status_tone, tab_text,
    update_text,
};
use super::{Detail, HelmScreen, Load, ViewerKey};

impl HelmScreen {
    /// Reloads the open release when its latest storage object changed
    /// (an upgrade, a rollback or a status update, from here or elsewhere).
    pub(super) fn follow_detail(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let current = self.stored_of(&detail.namespace, &detail.name, cx);
        if current == detail.seen {
            return;
        }
        if current.is_none() {
            // Uninstalled (without history), here or elsewhere.
            self.close_detail(cx);
            return;
        }
        let (namespace, name) = (detail.namespace.clone(), detail.name.clone());
        self.open_release(namespace, name, cx);
    }

    /// Opens (or reloads) the details of a release, keeping the tab and the
    /// picked revision when it is the same release.
    pub(super) fn open_release(&mut self, namespace: String, name: String, cx: &mut Context<Self>) {
        let Some(client) = self
            .cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone())
        else {
            return;
        };
        let task_namespace = namespace.clone();
        let task_name = name.clone();
        let driver = self.releases.read(cx).driver().driver;
        let seen = self.stored_of(&namespace, &name, cx);
        let handle = runtime::spawn(async move {
            Releases::with_driver(client, driver)
                .history(&task_namespace, &task_name)
                .await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = handle.await;
            this.update(cx, |this, cx| {
                if let Some(detail) = &mut this.detail {
                    detail.history = match result {
                        Ok(Ok(history)) => Load::Ready(history),
                        Ok(Err(error)) => Load::Failed(error.to_string()),
                        Err(error) => {
                            Load::Failed(t!("helm-loading-interrupted", error = error.to_string()))
                        }
                    };
                    // A picked revision that is gone falls back to the latest.
                    if detail
                        .revision
                        .is_some_and(|revision| detail.find(revision).is_none())
                    {
                        detail.revision = None;
                    }
                    if detail.diff.is_none_or(|(from, to)| {
                        detail.find(from).is_none() || detail.find(to).is_none()
                    }) {
                        detail.diff = default_diff_pair(detail.revisions(), detail.revision);
                    }
                    detail.viewer_key = None;
                    this.update_history_rows(cx);
                    cx.notify();
                }
            })
            .ok();
        });
        let same = self
            .detail
            .as_ref()
            .is_some_and(|d| d.namespace == namespace && d.name == name);
        if !same {
            // A lookup for another release must not reach this one's dialog.
            self.uninstall_lookup = None;
        }
        if same && let Some(detail) = &mut self.detail {
            detail._task = task;
            detail.seen = seen;
        } else {
            self.detail = Some(Detail {
                namespace,
                name,
                history: Load::Loading,
                revision: None,
                tab: Tab::Values,
                revealed: false,
                diff: None,
                diff_source: DiffSource::default(),
                viewer: None,
                viewer_key: None,
                history_table: None,
                upgrade: None,
                seen,
                _task: task,
            });
        }
        cx.notify();
    }

    pub(super) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self.uninstall_lookup = None;
        if let Some((table, _)) = &self.table {
            table.update(cx, |table, cx| table.clear_selection(cx));
        }
        cx.notify();
    }

    fn update_history_rows(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(table) = &detail.history_table else {
            return;
        };
        let rows = detail.revisions().iter().rev().map(revision_row).collect();
        table.update(cx, |table, cx| table.set_rows(rows, cx));
    }

    /// Creates the viewer and the history table, and loads the shown text
    /// into the viewer when it changed.
    pub(super) fn ensure_detail_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.entity().downgrade();
        let Some(detail) = &mut self.detail else {
            return;
        };
        let viewer = detail
            .viewer
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language("yaml")
                        .line_number(true)
                })
            })
            .clone();
        let mut new_table = false;
        if detail.history_table.is_none() {
            let table = cx.new(|cx| {
                ResourceTable::new(revision_columns(), window, cx).on_select(move |row, _, cx| {
                    let revision = row.key.parse().ok();
                    screen
                        .update(cx, |this, cx| this.pick_revision(revision, cx))
                        .ok();
                })
            });
            detail.history_table = Some(table);
            new_table = true;
        }
        let wanted = detail
            .shown()
            .filter(|_| detail.tab.is_text() && detail.revealed)
            .map(|release| {
                (
                    ViewerKey {
                        tab: detail.tab,
                        revision: release.revision,
                    },
                    tab_text(release, detail.tab),
                )
            });
        if let Some((key, text)) = wanted
            && detail.viewer_key.as_ref() != Some(&key)
        {
            detail.viewer_key = Some(key);
            viewer.update(cx, |viewer, cx| viewer.set_value(text, window, cx));
        }
        if new_table {
            self.update_history_rows(cx);
        }
    }

    fn pick_revision(&mut self, revision: Option<u32>, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail {
            let latest = detail.latest().map(|r| r.revision);
            detail.revision = revision.filter(|&r| Some(r) != latest);
            detail.diff = default_diff_pair(detail.revisions(), revision).or(detail.diff);
            cx.notify();
        }
    }

    fn show_latest(&mut self, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail {
            detail.revision = None;
            if let Some(table) = &detail.history_table {
                table.update(cx, |table, cx| table.clear_selection(cx));
            }
            cx.notify();
        }
    }

    /// A newer version of the chart in the user's repositories (#273), with
    /// an Upgrade button that opens the panel on that version.
    fn render_update(&self, detail: &Detail, cx: &mut Context<Self>) -> Option<AnyElement> {
        let latest = detail.latest()?;
        if detail.revision.is_some()
            || detail.upgrade.is_some()
            || latest.status == ReleaseStatus::Uninstalled
        {
            return None;
        }
        let update = self.chart_update(&latest.chart)?;
        let can_upgrade =
            Self::helm_path(cx).is_some() && !self.action_running() && !latest.status.is_pending();
        let version = update.version.clone();
        let button = Button::new("helm-update-upgrade")
            .outline()
            .xsmall()
            .icon(IconName::ArrowUp)
            .label(t!("helm-upgrade-button"))
            .tooltip(t!(
                "helm-upgrade-to-tooltip",
                version = update.version.clone(),
                repository = update.repository.clone()
            ))
            .disabled(!can_upgrade)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_upgrade(Some(version.clone()), window, cx)
            }));
        let theme = cx.theme();
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.info)
                .bg(theme.group_box)
                .text_sm()
                .text_color(theme.info)
                .child(Icon::new(IconName::ArrowUp).small())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .child(update_text(&update)),
                )
                .child(button)
                .into_any_element(),
        )
    }

    pub(super) fn render_detail(&self, detail: &Detail, cx: &mut Context<Self>) -> AnyElement {
        let actions = self.render_actions(detail, cx);
        let installed = self.render_installed(detail, cx);
        let update = self.render_update(detail, cx);
        let theme = cx.theme();
        let back = Button::new("helm-back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label(t!("helm-back-releases"))
            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx)));
        let title = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(format!("{} / {}", detail.namespace, detail.name));
        let icon = detail.latest().map(|release| {
            let path = release
                .chart
                .icon
                .as_deref()
                .and_then(|url| crate::images::RemoteImages::path(url, cx));
            kubach_ui::ChartIcon::new(release.chart.name.clone())
                .size(px(24.))
                .path(path)
        });
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(back)
            .children(icon)
            .child(title)
            .children(actions);

        let release = match (&detail.history, detail.shown()) {
            (Load::Loading, _) => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .child(EmptyState::new(
                        IconName::LoaderCircle,
                        t!("helm-release-loading"),
                    ))
                    .into_any_element();
            }
            (Load::Failed(error), _) => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .child(
                        EmptyState::new(IconName::CircleX, t!("helm-release-load-error"))
                            .description(error.clone()),
                    )
                    .into_any_element();
            }
            (Load::Ready(_), None) => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .child(EmptyState::new(
                        IconName::Inbox,
                        t!("helm-release-not-found"),
                    ))
                    .into_any_element();
            }
            (Load::Ready(_), Some(release)) => release,
        };

        if let Some((panel, _)) = &detail.upgrade {
            let latest = detail.latest().map(|r| r.revision);
            let base = panel.read(cx).base_revision();
            let moved = (latest != Some(base)).then(|| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(t!("helm-upgrade-release-moved", revision = base))
            });
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .children(moved)
                .child(panel.clone())
                .into_any_element();
        }
        let latest = detail.latest().map(|r| r.revision);
        let revision_label = if Some(release.revision) == latest {
            t!("helm-revision-latest", revision = release.revision)
        } else {
            t!(
                "helm-revision-of",
                revision = release.revision,
                latest = latest.unwrap_or_default()
            )
        };
        let mut summary = KeyValueList::new()
            .row(
                t!("helm-field-status"),
                StatusBadge::new(
                    release.status.as_str().to_owned(),
                    status_tone(&release.status),
                ),
            )
            .row(t!("helm-field-revision"), revision_label)
            .row(t!("helm-field-chart"), release.chart.reference())
            .row(
                t!("helm-field-app-version"),
                release
                    .chart
                    .app_version
                    .clone()
                    .unwrap_or_else(|| "—".into()),
            )
            .row(
                t!("helm-field-updated"),
                or_dash(release.last_deployed.map(|at| at.to_string())),
            )
            .row(
                t!("helm-field-first-deployed"),
                or_dash(release.first_deployed.map(|at| at.to_string())),
            )
            .row(
                t!("helm-field-description"),
                div().whitespace_normal().child(release.description.clone()),
            );
        if !release.labels.is_empty() {
            let labels: Vec<String> = release
                .labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            summary = summary.row(
                t!("helm-field-labels"),
                div().whitespace_normal().child(labels.join(", ")),
            );
        }
        let viewing_old = detail.revision.is_some().then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.warning)
                .child(t!("helm-showing-old-revision", revision = release.revision))
                .child(
                    Button::new("helm-show-latest")
                        .ghost()
                        .xsmall()
                        .label(t!("helm-show-latest"))
                        .on_click(cx.listener(|this, _, _, cx| this.show_latest(cx))),
                )
        });

        let selected = Tab::ALL
            .iter()
            .position(|tab| *tab == detail.tab)
            .unwrap_or_default();
        let tabs = TabBar::new("helm-tabs")
            .underline()
            .small()
            .selected_index(selected)
            .children(Tab::ALL.map(|tab| TabItem::new().label(tab.title())))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(detail) = &mut this.detail {
                    detail.tab = Tab::ALL[*ix];
                    cx.notify();
                }
            }));

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .children(installed)
            .children(update)
            .children(viewing_old)
            .child(Panel::new().child(summary))
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h(px(240.))
                    .flex()
                    .flex_col()
                    .child(self.render_tab(detail, release, cx)),
            )
            .into_any_element()
    }

    fn render_tab(&self, detail: &Detail, release: &Release, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // The context's security settings may forbid revealing (#96).
        let reveal_allowed = self
            .cluster
            .read(cx)
            .active_allows_secret(kubach_core::settings::SecretAction::Reveal);
        if detail.tab.sensitive() && !reveal_allowed {
            return EmptyState::new(IconName::EyeOff, t!("helm-hidden-secrets"))
                .description(t!("helm-secret-values-hidden"))
                .into_any_element();
        }
        if detail.tab.sensitive() && !detail.revealed {
            return EmptyState::new(IconName::EyeOff, t!("helm-hidden-secrets"))
                .description(t!("helm-hidden-secrets-description"))
                .action(
                    Button::new("helm-reveal")
                        .outline()
                        .small()
                        .icon(IconName::Eye)
                        .label(t!("helm-reveal"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(detail) = &mut this.detail {
                                detail.revealed = true;
                                cx.notify();
                            }
                        })),
                )
                .into_any_element();
        }
        let framed = |content: AnyElement| {
            div()
                .flex_1()
                .min_h_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(content)
                .into_any_element()
        };
        match detail.tab {
            tab if tab.is_text() => match &detail.viewer {
                Some(viewer) => framed(
                    Editor::new(viewer)
                        .h_full()
                        .bordered(false)
                        .readonly(true)
                        .into_any_element(),
                ),
                None => div().into_any_element(),
            },
            Tab::Notes if release.notes.trim().is_empty() => {
                EmptyState::new(IconName::FileText, t!("helm-notes-none"))
                    .description(t!("helm-notes-none-description"))
                    .into_any_element()
            }
            Tab::Notes => framed(
                div()
                    .id("helm-notes")
                    .size_full()
                    .overflow_y_scroll()
                    .p_3()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .children(
                        release
                            .notes
                            .lines()
                            .map(|line| div().min_h(px(16.)).child(line.to_owned())),
                    )
                    .into_any_element(),
            ),
            Tab::History => {
                let hint = div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("helm-history-hint"));
                let table = detail
                    .history_table
                    .clone()
                    .map(|table| framed(table.into_any_element()));
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(hint)
                    .children(table)
                    .into_any_element()
            }
            _ => self.render_diff(detail, cx),
        }
    }
}

fn or_dash(text: Option<String>) -> String {
    text.unwrap_or_else(|| "—".into())
}
