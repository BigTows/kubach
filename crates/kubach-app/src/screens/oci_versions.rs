//! Versions of a chart typed as an `oci://` reference (#301), shared by the
//! install and upgrade panels: listed from the registry after a pause in
//! typing, picked from a dropdown next to the version field. The field stays
//! editable, so a failed listing never blocks typing a version.

use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::helm::oci;
use kubach_core::runtime;
use kubach_i18n::t;

/// Pause after typing a chart before its versions are listed.
const LIST_DELAY: Duration = Duration::from_millis(500);

pub enum OciState {
    /// Not an `oci://` reference.
    Idle,
    Loading,
    /// Newest first.
    Ready(Vec<String>),
    Failed(String),
}

pub struct OciVersions {
    /// Include pre-releases (`--devel`).
    devel: bool,
    /// The reference the state is for, trimmed.
    chart: String,
    state: OciState,
    _task: Task<()>,
}

impl OciVersions {
    pub fn new(devel: bool) -> Self {
        Self {
            devel,
            chart: String::new(),
            state: OciState::Idle,
            _task: Task::ready(()),
        }
    }

    /// The listed versions, empty unless loaded.
    pub fn versions(&self) -> &[String] {
        match &self.state {
            OciState::Ready(versions) => versions,
            _ => &[],
        }
    }

    /// Lists the versions of `chart` after a pause (a newer chart cancels
    /// the pending listing); anything but an `oci://` reference clears them.
    pub fn set_chart(&mut self, chart: &str, cx: &mut Context<Self>) {
        let chart = chart.trim();
        if chart == self.chart {
            return;
        }
        self.chart = chart.to_owned();
        if oci::parse_reference(chart).is_none() {
            self.state = OciState::Idle;
            self._task = Task::ready(());
            cx.notify();
            return;
        }
        self.state = OciState::Loading;
        let (chart, devel) = (self.chart.clone(), self.devel);
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIST_DELAY).await;
            let listed = chart.clone();
            let handle = runtime::spawn(async move { oci::list_versions(&listed, devel).await });
            let state = match handle.await {
                Ok(Ok(versions)) => OciState::Ready(versions),
                Ok(Err(error)) => OciState::Failed(error.to_string()),
                Err(error) => {
                    OciState::Failed(t!("helm-listing-interrupted", error = error.to_string()))
                }
            };
            this.update(cx, |this, cx| {
                if this.chart == chart {
                    this.state = state;
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    /// The dropdown next to the version field: a spinner while listing,
    /// the versions (labelled by `label`) once listed. `None` for charts
    /// that are not `oci://` references.
    pub fn picker(
        &self,
        id: &'static str,
        current: String,
        label: impl Fn(&str) -> String + 'static,
        on_pick: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Option<AnyElement> {
        let button = Button::new(id)
            .outline()
            .small()
            .icon(IconName::ChevronDown);
        let button = match &self.state {
            OciState::Idle => return None,
            OciState::Loading => button
                .loading(true)
                .disabled(true)
                .tooltip(t!("helm-oci-listing"))
                .into_any_element(),
            OciState::Failed(_) => button
                .disabled(true)
                .tooltip(t!("helm-oci-list-failed"))
                .into_any_element(),
            OciState::Ready(versions) if versions.is_empty() => button
                .disabled(true)
                .tooltip(t!("helm-oci-no-versions"))
                .into_any_element(),
            OciState::Ready(versions) => {
                let versions = versions.clone();
                let on_pick = std::rc::Rc::new(on_pick);
                button
                    .tooltip(t!("helm-oci-pick"))
                    .dropdown_menu(move |menu: PopupMenu, _, _| {
                        let menu = menu.scrollable(true).max_h(px(360.));
                        versions.iter().fold(menu, |menu, version| {
                            let (on_pick, version) = (on_pick.clone(), version.clone());
                            menu.item(
                                PopupMenuItem::new(label(&version))
                                    .checked(version == current)
                                    .on_click(move |_, window, cx| {
                                        on_pick(version.clone(), window, cx)
                                    }),
                            )
                        })
                    })
                    .into_any_element()
            }
        };
        Some(button)
    }

    /// A line under the chart row: why versions could not be listed.
    pub fn note(&self, cx: &App) -> Option<AnyElement> {
        let OciState::Failed(error) = &self.state else {
            return None;
        };
        Some(
            div()
                .text_xs()
                .text_color(cx.theme().warning)
                .whitespace_normal()
                .child(t!("helm-oci-list-error", error = error.clone()))
                .into_any_element(),
        )
    }
}

/// `1.2.3 · pre-release` for pre-releases.
pub fn version_label(version: &str) -> String {
    if oci::is_prerelease(version) {
        format!("{version} · {}", t!("helm-version-prerelease"))
    } else {
        version.to_owned()
    }
}
