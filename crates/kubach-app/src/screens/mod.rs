//! Screens of the main window, beyond the workspace shell.

mod access;
mod activity;
mod browser;
mod column_order;
mod events;
mod gitops;
mod helm;
mod helm_charts;
mod helm_install;
mod helm_release;
mod helm_upgrade;
mod kinds;
mod kustomize;
mod managed_by;
mod metrics;
mod namespace_form;
mod namespace_picker;
mod namespaces;
mod oci_versions;
mod overview;
mod panel_width;
pub(crate) mod plugin_text;
mod prometheus_link;
mod rbac;
mod rows;
mod secret;
mod security;
mod topology;
mod usage;

pub use access::{AccessPage, AccessScreen, Reveal};
pub use activity::set_active;
pub use browser::{BrowserPlace, ResourceBrowser};
pub use events::EventsScreen;
pub use gitops::GitOpsScreen;
pub use helm::{HelmPage, HelmScreen, ShowHelmPage};
pub use kinds::{BrowseSection, KindEntry, custom_groups, find as find_kind};
pub use kustomize::KustomizeScreen;
pub use namespace_picker::Namespaces;
pub use overview::OverviewScreen;
pub use prometheus_link::{PrometheusRegistry, PrometheusStatus, confirm as confirm_prometheus};
pub use topology::TopologyScreen;

use gpui_kit::{App, Entity, IntoElement, Render, StyleRefinement, Styled as _, actions};

actions!(
    navigation,
    [
        /// Back to where a jump came from (#395): `⌘[` on macOS, `Alt+←`
        /// elsewhere. The details panel goes back in its own stack first
        /// while it has focus.
        GoBack
    ]
);

/// `entity` embedded as a cached view filling its parent (#355, #363): it
/// re-renders only when it notifies (or the window refreshes, as on a theme
/// change); other redraws reuse its last frame. So it must notify, or
/// observe, for everything its render reads.
///
/// Reading it here keeps its notifications reaching the window: GPUI tracks
/// what a cached view reads only on a frame that renders it, and leaves out
/// entities already read earlier in that frame, such as one created during
/// the parent's render. Without this read such a view would never redraw.
pub fn cached<T: Render>(entity: &Entity<T>, cx: &App) -> impl IntoElement + use<T> {
    entity.read(cx);
    entity
        .clone()
        .cached(StyleRefinement::default().size_full())
}
