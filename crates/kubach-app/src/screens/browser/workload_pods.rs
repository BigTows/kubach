//! "Pods" in the details of a workload (#393): the pods of a Deployment,
//! StatefulSet, DaemonSet, ReplicaSet, Job or CronJob with their status,
//! readiness, restarts, age and node, right under the main block. A click
//! on a pod shows its details in place, the browser's list stays as it is
//! ([`ShowObject`]).
//!
//! Pods (and the ReplicaSets / Jobs between the workload and its pods)
//! come from unfiltered watches of the namespace, the same the
//! "Connections" block holds, so no watch of its own is started next to
//! it. Which pods belong to the workload is decided in core
//! ([`kubach_core::workload_pods`]). A change of another pod of the
//! namespace redraws nothing: the block notifies (and tells the details,
//! for the metrics block) only when its own pods change.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::*;
use k8s_openapi::jiff::Timestamp;
use kubach_core::ResourceKind;
use kubach_core::cache::{ResourceCache, Subscription as Watch, WatchStatus};
use kubach_core::discovery::ApiCatalog;
use kubach_core::plugin::HealthStatus;
use kubach_core::workload_pods::{PodRow, Workload, pod_rows, pods_key, pods_of};
use kubach_i18n::t;
use kubach_ui::{AgeTicker, Panel, StatusDot, format_since, unix_now};
use kube::api::DynamicObject;
use kube::discovery::verbs;

use super::super::activity::watch_changes;
use super::super::{plugin_text, rows};
use super::details::ShowObject;

/// Pods shown before the list scrolls inside the block.
const VISIBLE_PODS: usize = 8;
/// Height of the list once it scrolls.
const LIST_HEIGHT: Pixels = px(360.);
const READY_WIDTH: Pixels = px(40.);
const RESTARTS_WIDTH: Pixels = px(56.);
const AGE_WIDTH: Pixels = px(40.);

/// The block's own pods changed (not on every watch event): the details
/// rebuild what depends on them (the metrics block).
pub(super) struct PodsChanged;

/// The pods of one workload.
pub(super) struct WorkloadPods {
    cache: ResourceCache,
    workload: Workload,
    object: Arc<DynamicObject>,
    /// `None` when the cluster does not serve or let us watch pods.
    pods: Option<Watch>,
    /// ReplicaSets of a Deployment, Jobs of a CronJob.
    intermediate: Option<Watch>,
    /// What `rows` were built from.
    key: Option<Vec<(String, String)>>,
    shown: Vec<Arc<DynamicObject>>,
    rows: Vec<PodRow>,
    status: WatchStatus,
    /// Bumped whenever `shown` changes.
    version: u64,
    ages: AgeTicker,
    _listeners: Vec<Task<()>>,
}

impl EventEmitter<ShowObject> for WorkloadPods {}
impl EventEmitter<PodsChanged> for WorkloadPods {}

/// A watch of all `group`/`kind` objects of `namespace`, joining a running one.
fn watch(
    cache: &ResourceCache,
    catalog: &ApiCatalog,
    group: &str,
    kind: &str,
    namespace: Option<&str>,
) -> Option<Watch> {
    let kind = catalog
        .find(group, kind)
        .filter(|kind| kind.supports(verbs::WATCH))?;
    Some(cache.subscribe(kind, namespace))
}

impl WorkloadPods {
    /// The workload `kind` is, when its details list pods.
    pub fn workload(kind: &ResourceKind) -> Option<Workload> {
        Workload::from_api(&kind.api.group, &kind.api.kind)
    }

    pub fn new(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        workload: Workload,
        object: Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> Self {
        let namespace = object.metadata.namespace.as_deref();
        let pods = watch(cache, catalog, "", "Pod", namespace);
        let intermediate = workload
            .intermediate()
            .and_then(|(group, kind)| watch(cache, catalog, group, kind, namespace));
        let listeners = pods
            .iter()
            .chain(&intermediate)
            .map(|watch| watch_changes(watch.changes(), cx, |this: &mut Self, cx| this.refresh(cx)))
            .collect();
        let status = match &pods {
            Some(_) => WatchStatus::Loading,
            None => WatchStatus::Error(kubach_core::cache::WatchError {
                forbidden: false,
                connection: false,
                message: t!("details-not-served", kind = "Pods"),
            }),
        };
        Self {
            cache: cache.clone(),
            workload,
            object,
            pods,
            intermediate,
            key: None,
            shown: Vec::new(),
            rows: Vec::new(),
            status,
            version: 0,
            ages: AgeTicker::default(),
            _listeners: listeners,
        }
    }

    /// Whether this block can follow `object` (a `workload`) of `cache`.
    pub fn follows(
        &self,
        cache: &ResourceCache,
        workload: Workload,
        object: &DynamicObject,
    ) -> bool {
        self.cache.same_as(cache)
            && self.workload == workload
            && self.object.metadata.uid == object.metadata.uid
            && self.object.metadata.name == object.metadata.name
            && self.object.metadata.namespace == object.metadata.namespace
    }

    /// A newer version of the same object (its selector may have changed).
    pub fn set_object(&mut self, object: Arc<DynamicObject>, cx: &mut Context<Self>) {
        if self.object.metadata.resource_version == object.metadata.resource_version {
            return;
        }
        self.object = object;
        self.refresh(cx);
    }

    /// The pods shown, and a version that changes with them.
    pub fn pods(&self) -> (&[Arc<DynamicObject>], u64) {
        (&self.shown, self.version)
    }

    /// Picks the workload's pods again; redraws only when they changed.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(pods) = &self.pods else {
            return;
        };
        let snapshot = pods.snapshot();
        let intermediates = self.intermediate.as_ref().map(Watch::snapshot);
        // ReplicaSets / Jobs not listed (yet): the selector stands in.
        let listed = intermediates
            .as_ref()
            .filter(|s| s.status == WatchStatus::Ready)
            .map(|s| &s.objects[..]);
        let shown = pods_of(self.workload, &self.object, listed, &snapshot.objects);
        let key = pods_key(&shown);
        let status_changed = self.status != snapshot.status;
        self.status = snapshot.status;
        if self.key.as_ref() == Some(&key) {
            if status_changed {
                cx.notify();
            }
            return;
        }
        self.rows = pod_rows(&shown, Timestamp::now());
        self.ages
            .track(self.rows.iter().filter_map(|row| row.created).max(), cx);
        self.key = Some(key);
        self.shown = shown;
        self.version += 1;
        cx.emit(PodsChanged);
        cx.notify();
    }

    fn render_row(&self, ix: usize, row: &PodRow, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tone = rows::tone(row.health);
        let status_color = match row.health {
            HealthStatus::Failed => theme.danger,
            HealthStatus::Progressing | HealthStatus::Degraded => theme.warning,
            HealthStatus::Healthy | HealthStatus::Unknown => theme.muted_foreground,
        };
        let restarts_color = if row.restarts > 0 {
            theme.warning
        } else {
            theme.muted_foreground
        };
        let muted = theme.muted_foreground;
        let object = self.shown.get(ix).cloned();
        let age = row
            .created
            .map(|created| format_since(created, now))
            .unwrap_or_default();
        let cell = |width: Pixels| div().flex_none().w(width).text_right().text_xs();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(StatusDot::new(tone))
                    .child(
                        div().flex_1().min_w_0().overflow_hidden().child(
                            Button::new(("workload-pod", ix))
                                .link()
                                .small()
                                .label(row.name.clone())
                                .tooltip(t!("details-open", kind = "Pod", name = row.name.as_str()))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    if let Some(object) = &object {
                                        cx.emit(ShowObject {
                                            group: String::new(),
                                            kind: "Pod".into(),
                                            object: object.clone(),
                                        });
                                    }
                                })),
                        ),
                    )
                    .child(
                        cell(READY_WIDTH)
                            .text_color(theme.foreground)
                            .child(row.ready_text()),
                    )
                    .child(
                        cell(RESTARTS_WIDTH)
                            .text_color(restarts_color)
                            .child(row.restarts.to_string()),
                    )
                    .child(cell(AGE_WIDTH).text_color(muted).child(age)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_4()
                    .min_w_0()
                    .text_xs()
                    .child(
                        div()
                            .flex_none()
                            .text_color(status_color)
                            .child(row.status.clone()),
                    )
                    .child(div().flex_none().text_color(muted).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(match &row.node {
                                Some(node) => node.clone(),
                                None => t!("pods-not-scheduled"),
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for WorkloadPods {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let note = |text: String| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let body = if self.rows.is_empty() {
            match &self.status {
                WatchStatus::Loading => note(t!("details-loading")),
                WatchStatus::Error(error) => {
                    note(t!("details-unavailable", error = error.message.as_str()))
                }
                WatchStatus::Ready => note(t!("pods-none")),
            }
        } else {
            let now = unix_now();
            let rows: Vec<AnyElement> = self
                .rows
                .iter()
                .enumerate()
                .map(|(ix, row)| self.render_row(ix, row, now, cx))
                .collect();
            let header = |width: Pixels, id: &str, fallback: &str| {
                div()
                    .flex_none()
                    .w(width)
                    .text_right()
                    .child(plugin_text::column_title_of(id, fallback))
            };
            let header = div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .flex_1()
                        .pl_4()
                        .child(plugin_text::column_title_of("name", "Name")),
                )
                .child(header(READY_WIDTH, "ready", "Ready"))
                .child(header(RESTARTS_WIDTH, "restarts", "Restarts"))
                .child(header(AGE_WIDTH, "age", "Age"));
            let list = div().flex().flex_col().gap_2().children(rows);
            let list = if self.rows.len() > VISIBLE_PODS {
                // A long list scrolls inside the block; the wheel over it
                // does not scroll the details around it (#392).
                div()
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("workload-pods-list")
                            .max_h(LIST_HEIGHT)
                            .overflow_y_scroll()
                            .child(list),
                    )
                    .into_any_element()
            } else {
                list.into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(header)
                .child(list)
                .into_any_element()
        };
        let mut panel = Panel::new().title("Pods");
        if !self.rows.is_empty() {
            panel = panel.note(self.rows.len().to_string());
        }
        panel.child(body)
    }
}
