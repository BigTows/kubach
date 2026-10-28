//! The release list of the Releases page: the table, the search and the
//! namespace filter.

use gpui_kit::component::input::InputEvent;
use gpui_kit::component::{ActiveTheme as _, IconName};
use gpui_kit::*;
use kubach_core::helm::DriverSource;
use kubach_i18n::t;
use kubach_ui::{EmptyState, FilterBar, ResourceTable, name_matches};

use super::super::helm_release::{release_columns, release_key, release_row};
use super::super::namespace_picker::NamespacePicker;
use super::super::table_sort;
use super::{HelmScreen, Load};

impl HelmScreen {
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.set_value("", window, cx));
        }
    }

    fn show_namespace(&self, cx: &App) -> bool {
        !matches!(self.releases.read(cx).loaded_for(), Some(Some(namespaces)) if namespaces.len() == 1)
    }

    pub(super) fn update_rows(&mut self, cx: &mut Context<Self>) {
        let (Some((table, _)), Load::Ready(listing)) = (&self.table, self.releases.read(cx).list())
        else {
            return;
        };
        let show_namespace = self.show_namespace(cx);
        let rows = listing
            .releases
            .iter()
            .filter(|release| name_matches(&release.name, &self.query))
            .map(|release| {
                let update = self.chart_update(&release.chart);
                release_row(release, show_namespace, update.as_ref())
            })
            .collect();
        table.update(cx, |table, cx| table.set_rows(rows, cx));
    }

    pub(super) fn ensure_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show_namespace = self.show_namespace(cx);
        if self
            .table
            .as_ref()
            .is_some_and(|(_, shown)| *shown == show_namespace)
        {
            return;
        }
        let screen = cx.entity().downgrade();
        let table = cx.new(|cx| {
            ResourceTable::new(release_columns(show_namespace), window, cx).on_select(
                move |row, _, cx| {
                    let key = row.key.to_string();
                    screen
                        .update(cx, |this, cx| this.select_release(&key, cx))
                        .ok();
                },
            )
        });
        table_sort::remember(&table, table_sort::screen("helm.releases"), cx);
        self.table = Some((table, show_namespace));
        self.update_rows(cx);
    }

    fn select_release(&mut self, key: &str, cx: &mut Context<Self>) {
        let Load::Ready(listing) = self.releases.read(cx).list() else {
            return;
        };
        let Some(release) = listing
            .releases
            .iter()
            .find(|r| release_key(&r.namespace, &r.name) == key)
        else {
            return;
        };
        let (namespace, name) = (release.namespace.clone(), release.name.clone());
        self.open_release(namespace, name, cx);
    }

    /// Creates the namespace picker and the search, which need the window.
    pub(super) fn ensure_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_none() {
            let namespaces = self.namespaces.clone();
            self.picker =
                Some(cx.new(|cx| {
                    NamespacePicker::new("helm-namespace-picker", namespaces, window, cx)
                }));
        }
        if self.search.is_none() {
            let search = FilterBar::search_state("", window, cx);
            self._subscriptions
                .extend(crate::i18n::localized_placeholder(
                    &search,
                    || t!("helm-search-releases"),
                    window,
                    cx,
                ));
            self._subscriptions.push(cx.subscribe(
                &search,
                |this, search, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        this.query = search.read(cx).value().to_string();
                        this.update_rows(cx);
                        cx.notify();
                    }
                },
            ));
            self.search = Some(search);
        }
    }

    fn driver_note(&self, cx: &App) -> String {
        let driver = self.releases.read(cx).driver();
        let kind = driver.driver.kind();
        match driver.source {
            DriverSource::Detected => t!("helm-driver-detected", kind = kind),
            DriverSource::Settings => t!("helm-driver-settings", kind = kind),
            DriverSource::Default => t!("helm-driver-default", kind = kind),
        }
    }

    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let releases = self.releases.read(cx);
        let scope = match releases.loaded_for().cloned().flatten() {
            None => t!(
                "helm-releases-scope",
                scope = "all",
                namespace = "",
                namespaces = 0
            ),
            Some(namespaces) if namespaces.len() == 1 => t!(
                "helm-releases-scope",
                scope = "one",
                namespace = namespaces[0].clone(),
                namespaces = 1
            ),
            Some(namespaces) => t!(
                "helm-releases-scope",
                scope = "many",
                namespace = "",
                namespaces = namespaces.len()
            ),
        };
        let listing = match releases.list() {
            Load::Loading => {
                return EmptyState::new(IconName::LoaderCircle, t!("helm-releases-loading"))
                    .into_any_element();
            }
            Load::Failed(error) => {
                return EmptyState::new(IconName::CircleX, t!("helm-releases-list-error"))
                    .description(error.clone())
                    .into_any_element();
            }
            Load::Ready(listing) => listing,
        };
        let failed = (!listing.errors.is_empty()).then(|| {
            let names: Vec<String> = listing
                .errors
                .iter()
                .map(|(namespace, _)| match namespace.as_str() {
                    "" => t!("helm-all-namespaces"),
                    namespace => namespace.to_owned(),
                })
                .collect();
            let all_forbidden = listing.errors.iter().all(|(_, error)| error.is_forbidden());
            let text = if all_forbidden {
                t!(
                    "helm-releases-forbidden",
                    kind = releases.driver().driver.kind(),
                    namespaces = names.join(", ")
                )
            } else {
                t!(
                    "helm-releases-list-failed",
                    namespaces = names.join(", "),
                    error = listing.errors[0].1.to_string()
                )
            };
            let color = if listing.releases.is_empty() {
                theme.danger
            } else {
                theme.warning
            };
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(color)
                .text_sm()
                .text_color(color)
                .whitespace_normal()
                .child(text)
        });
        if listing.releases.is_empty() {
            let empty = if listing.errors.is_empty() {
                EmptyState::new(IconName::Inbox, t!("helm-releases-none")).description(t!(
                    "helm-releases-none-description",
                    scope = scope.clone(),
                    driver = self.driver_note(cx)
                ))
            } else {
                EmptyState::new(IconName::Ban, t!("helm-releases-none-shown"))
            };
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .children(failed)
                .child(empty)
                .into_any_element();
        }
        let Some((table, _)) = &self.table else {
            return div().flex_1().into_any_element();
        };
        let shown = listing
            .releases
            .iter()
            .filter(|release| name_matches(&release.name, &self.query))
            .count();
        if shown == 0 {
            let placeholder = FilterBar::no_matches(
                &t!("helm-noun-releases"),
                &self.query,
                cx.listener(|this, _, window, cx| this.clear_search(window, cx)),
            );
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .children(failed)
                .child(placeholder)
                .into_any_element();
        }
        let count = listing.releases.len();
        let summary = if shown == count {
            t!(
                "helm-releases-summary",
                count = count,
                scope = scope,
                driver = self.driver_note(cx)
            )
        } else {
            t!(
                "helm-releases-summary-filtered",
                shown = shown,
                count = count,
                scope = scope,
                driver = self.driver_note(cx)
            )
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(failed)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(table.clone()),
            )
            .into_any_element()
    }
}
