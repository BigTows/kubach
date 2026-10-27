//! "Traffic" layer of the Topology screen (#326, ADR 0017): what really
//! flows between workloads, from the Prometheus metrics of Istio, Linkerd
//! or Cilium Hubble ([`kubach_core::topology::traffic`]).
//!
//! Cheap by construction:
//! - sources are found from the discovery catalog already in memory, then
//!   confirmed by one tiny query each, once per context and Prometheus
//!   (again only on "Detect again");
//! - polling runs only while the screen is shown with the layer on, for
//!   the shown namespaces and window; hiding the screen, switching the
//!   layer off or changing context drops the poll and the data;
//! - only the last window is kept, never a history.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::metrics::prometheus::{Endpoint, Prometheus};
use kubach_core::metrics::{self, MetricsError};
use kubach_core::runtime;
use kubach_core::topology::traffic::{
    self, NamespaceScope, TrafficPlugin, TrafficSnapshot, TrafficWindow,
};
use kubach_core::topology::{EdgeKind, TopologySnapshot};
use kubach_i18n::t;
use kubach_ui::{SegmentTab, SegmentTabs, TrafficLegend};

use super::TopologyScreen;
use crate::cluster::CatalogState;
use crate::screens::prometheus_link::{PrometheusRegistry, PrometheusStatus};

/// Which sources a context's Prometheus has, found once.
enum Detection {
    /// Probing the candidates.
    Running { endpoint: Endpoint, _task: Task<()> },
    Done {
        endpoint: Endpoint,
        /// Plugins the discovery catalog suggested.
        candidates: Vec<&'static str>,
        /// Those with metrics in Prometheus, in order of preference.
        confirmed: Vec<&'static str>,
    },
}

/// What a running poll asks for; another key restarts it.
#[derive(Clone, PartialEq)]
struct PollKey {
    context: String,
    endpoint: Endpoint,
    plugin: &'static str,
    scope: NamespaceScope,
    window: TrafficWindow,
}

/// Why the layer shows what it shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum LayerState {
    Off,
    /// No Prometheus to ask: the reason.
    NoPrometheus(String),
    Detecting,
    /// Nothing to poll; the plugins discovery suggested (whose metrics
    /// Prometheus doesn't have), if any.
    NoSources(Vec<&'static str>),
    /// Polling `source`; `also` were found too but are not used (one
    /// source at a time: two would count the same requests twice).
    Polling {
        source: &'static str,
        also: Vec<&'static str>,
    },
}

/// State of the traffic layer of the screen.
pub(super) struct TrafficLayer {
    pub on: bool,
    pub window: TrafficWindow,
    pub state: LayerState,
    /// The last window polled; dropped with the poll.
    pub data: Option<Arc<TrafficSnapshot>>,
    /// Why the last poll failed (the previous window stays shown).
    pub error: Option<String>,
    /// By context. Kept while the context stays connected.
    detections: HashMap<String, Detection>,
    poll: Option<(PollKey, Task<()>)>,
}

impl Default for TrafficLayer {
    fn default() -> Self {
        Self {
            on: false,
            window: TrafficWindow::default(),
            state: LayerState::Off,
            data: None,
            error: None,
            detections: HashMap::new(),
            poll: None,
        }
    }
}

impl TrafficLayer {
    /// Stops polling and frees the data; `true` when there was data to
    /// take off the graph.
    fn stop_poll(&mut self) -> bool {
        self.poll = None;
        self.error = None;
        self.data.take().is_some()
    }
}

/// Display name of a plugin id.
fn name(id: &str) -> &'static str {
    traffic::plugin(id).map_or("?", |plugin| plugin.name())
}

fn names(ids: &[&'static str]) -> String {
    ids.iter().map(|id| name(id)).collect::<Vec<_>>().join(", ")
}

/// Why `status` gives no server, for the note.
fn no_prometheus(status: &PrometheusStatus) -> String {
    if let Some(problem) = status.problem() {
        return problem;
    }
    match status {
        PrometheusStatus::Off => t!("traffic-prometheus-off"),
        PrometheusStatus::Detecting | PrometheusStatus::Connecting(_) => {
            t!("traffic-prometheus-looking")
        }
        PrometheusStatus::NotFound => t!("traffic-prometheus-not-found"),
        PrometheusStatus::NoAddress => t!("traffic-prometheus-no-address"),
        _ => t!("traffic-prometheus-unavailable"),
    }
}

/// The graph of `base` with `data` laid over it.
pub(super) fn overlay(base: &TopologySnapshot, data: &TrafficSnapshot) -> TopologySnapshot {
    TopologySnapshot {
        graph: traffic::apply(base.graph.clone(), data),
        loading: base.loading,
        errors: base.errors.clone(),
        unavailable: base.unavailable.clone(),
        policies: base.policies.clone(),
    }
}

impl TopologyScreen {
    /// Brings the traffic layer in line with the screen: polls only while
    /// shown with the layer on and a source confirmed; otherwise stops and
    /// frees the data.
    pub(super) fn sync_traffic(&mut self, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let context = cluster.active().map(|context| context.name.clone());
        let catalog = match cluster.catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        // A reconnected context is detected again.
        self.traffic
            .detections
            .retain(|name, _| cluster.connection_of(name).is_some());
        let scope = self
            .live
            .as_ref()
            .map(|live| NamespaceScope::of(live.namespaces.as_deref()));
        let (true, true, Some(context), Some(catalog), Some(scope)) =
            (self.active, self.traffic.on, context, catalog, scope)
        else {
            let had_data = self.traffic.stop_poll();
            self.traffic.state = LayerState::Off;
            if had_data {
                self.show(cx);
            }
            return;
        };
        let status = PrometheusRegistry::try_global(cx)
            .map(|registry| registry.update(cx, |registry, cx| registry.ensure(&context, cx)));
        let Some(prometheus) = status.as_ref().and_then(|s| s.prometheus()).cloned() else {
            let had_data = self.traffic.stop_poll();
            self.traffic.state = LayerState::NoPrometheus(
                status
                    .as_ref()
                    .map_or_else(|| t!("traffic-prometheus-unavailable"), no_prometheus),
            );
            if had_data {
                self.show(cx);
            }
            return;
        };
        let endpoint = prometheus.endpoint().clone();
        let detection =
            self.traffic
                .detections
                .get(&context)
                .filter(|detection| match detection {
                    Detection::Running { endpoint: e, .. }
                    | Detection::Done { endpoint: e, .. } => *e == endpoint,
                });
        let (candidates, confirmed) = match detection {
            Some(Detection::Running { .. }) => {
                self.traffic.state = LayerState::Detecting;
                return;
            }
            Some(Detection::Done {
                candidates,
                confirmed,
                ..
            }) => (candidates.clone(), confirmed.clone()),
            None => {
                let groups = catalog.kinds().iter().map(|kind| kind.api.group.as_str());
                let candidates = traffic::candidates(groups);
                self.start_detection(context, prometheus, candidates, cx);
                return;
            }
        };
        let Some((&plugin, also)) = confirmed.split_first() else {
            let had_data = self.traffic.stop_poll();
            self.traffic.state = LayerState::NoSources(candidates);
            if had_data {
                self.show(cx);
            }
            return;
        };
        self.traffic.state = LayerState::Polling {
            source: name(plugin),
            also: also.to_vec(),
        };
        let key = PollKey {
            context,
            endpoint,
            plugin,
            scope,
            window: self.traffic.window,
        };
        if self.traffic.poll.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        // Another window, namespaces or source: the old numbers don't
        // apply any more.
        if self.traffic.stop_poll() {
            self.show(cx);
        }
        self.start_poll(key, prometheus, cx);
    }

    /// Confirms `candidates` in `prometheus` (one small query each), then
    /// syncs again.
    fn start_detection(
        &mut self,
        context: String,
        prometheus: Prometheus,
        candidates: Vec<&'static dyn TrafficPlugin>,
        cx: &mut Context<Self>,
    ) {
        let endpoint = prometheus.endpoint().clone();
        let ids: Vec<&'static str> = candidates.iter().map(|plugin| plugin.id()).collect();
        if candidates.is_empty() {
            // Nothing in discovery: no request at all.
            self.traffic.detections.insert(
                context,
                Detection::Done {
                    endpoint,
                    candidates: ids,
                    confirmed: Vec::new(),
                },
            );
            self.sync_traffic(cx);
            return;
        }
        let handle =
            runtime::spawn(async move { traffic::confirm(&prometheus, &candidates).await });
        let name = context.clone();
        let probed = endpoint.clone();
        let task = cx.spawn(async move |this, cx| {
            let confirmed = handle.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                let current = matches!(
                    this.traffic.detections.get(&name),
                    Some(Detection::Running { endpoint, .. }) if *endpoint == probed
                );
                if current {
                    this.traffic.detections.insert(
                        name,
                        Detection::Done {
                            endpoint: probed,
                            candidates: ids,
                            confirmed,
                        },
                    );
                    this.sync_traffic(cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.traffic.detections.insert(
            context,
            Detection::Running {
                endpoint,
                _task: task,
            },
        );
        self.traffic.state = LayerState::Detecting;
    }

    fn start_poll(&mut self, key: PollKey, prometheus: Prometheus, cx: &mut Context<Self>) {
        let Some(plugin) = traffic::plugin(key.plugin) else {
            return;
        };
        let scope = key.scope.clone();
        let window = key.window;
        let mut polling = metrics::poll(window.poll_interval(), move || {
            let prometheus = prometheus.clone();
            let scope = scope.clone();
            async move {
                traffic::fetch(&prometheus, plugin, &scope, window)
                    .await
                    .map_err(|error| MetricsError::Unavailable(error.to_string()))
            }
        });
        let task = cx.spawn(async move |this, cx| {
            while let Some(result) = polling.next().await {
                let updated = this.update(cx, |this, cx| {
                    match result {
                        Ok(snapshot) => {
                            this.traffic.error = None;
                            this.traffic.data = Some(Arc::new(snapshot));
                            this.show(cx);
                        }
                        Err(error) => this.traffic.error = Some(error.to_string()),
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        self.traffic.poll = Some((key, task));
    }

    /// "Detect again": forgets what was found for the context and asks
    /// Prometheus anew.
    fn detect_traffic_again(&mut self, cx: &mut Context<Self>) {
        if let Some(context) = self.context.clone() {
            self.traffic.detections.remove(&context);
        }
        self.sync_traffic(cx);
        cx.notify();
    }

    /// The toolbar button that switches the layer.
    pub(super) fn traffic_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("topology-traffic")
            .small()
            .outline()
            .icon(Lucide::Activity)
            .label(t!("traffic-button"))
            .selected(self.traffic.on)
            .tooltip(t!("traffic-button-tooltip"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.traffic.on = !this.traffic.on;
                this.sync_traffic(cx);
                cx.notify();
            }))
    }

    /// What the traffic layer shows, while it is on: the active source
    /// with the legend and the window, or why there is no traffic.
    pub(super) fn render_traffic_note(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.traffic.on {
            return None;
        }
        let theme = cx.theme().clone();
        let text = |text: String| {
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(text)
        };
        let detect_again = || {
            Button::new("topology-traffic-detect")
                .xsmall()
                .ghost()
                .label(t!("prometheus-detect-again"))
                .tooltip(t!("traffic-detect-again-tooltip"))
                .on_click(cx.listener(|this, _, _, cx| this.detect_traffic_again(cx)))
        };
        let mut row = div()
            .id("topology-traffic-note")
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                gpui_kit::component::Icon::new(Lucide::Activity)
                    .xsmall()
                    .text_color(theme.muted_foreground),
            );
        match &self.traffic.state {
            LayerState::Off => return None,
            LayerState::NoPrometheus(reason) => {
                row = row.child(text(t!(
                    "traffic-needs-prometheus",
                    reason = reason.clone()
                )));
            }
            LayerState::Detecting => {
                row = row.child(text(t!("traffic-detecting")));
            }
            LayerState::NoSources(candidates) => {
                let message = if candidates.is_empty() {
                    t!("traffic-no-sources")
                } else {
                    t!(
                        "traffic-no-metrics",
                        sources = names(candidates),
                        count = candidates.len()
                    )
                };
                row = row.child(text(message)).child(detect_again());
            }
            LayerState::Polling { source, also } => {
                let window = self.traffic.window;
                let tabs = SegmentTabs::new(
                    "topology-traffic-window",
                    TrafficWindow::ALL
                        .iter()
                        .map(|window| {
                            SegmentTab::new(window.label())
                                .tooltip(t!("traffic-window-tooltip", window = window.label()))
                        })
                        .collect(),
                    TrafficWindow::ALL
                        .iter()
                        .position(|w| *w == window)
                        .unwrap_or(1),
                )
                .on_select(cx.listener(|this, ix: &usize, _, cx| {
                    this.traffic.window = TrafficWindow::ALL[*ix];
                    this.sync_traffic(cx);
                    cx.notify();
                }));
                let data = self.traffic.data.as_ref();
                let flows = data.map(|data| data.flows.len());
                let unmapped = self.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.graph.edges().iter().any(|edge| {
                        edge.kind == EdgeKind::Traffic
                            && edge.attrs.traffic.as_ref().is_some_and(|t| t.unmapped)
                    })
                });
                let truncated = data.is_some_and(|data| data.truncated);
                let lead = match flows {
                    None => t!("traffic-lead-loading", source = *source),
                    Some(0) => t!("traffic-lead-none", source = *source),
                    Some(count) if truncated => {
                        t!("traffic-lead-busiest", source = *source, count = count)
                    }
                    Some(count) => t!("traffic-lead", source = *source, count = count),
                };
                row = row
                    .child(tabs)
                    .child(TrafficLegend::new().source(lead).unmapped(unmapped));
                if !also.is_empty() {
                    row = row.child(text(t!("traffic-also-found", sources = names(also))));
                }
                if let Some(error) = &self.traffic.error {
                    row = row.child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .whitespace_normal()
                            .child(t!("traffic-poll-failed", error = error.clone())),
                    );
                }
            }
        }
        Some(row.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::prometheus::Endpoint;

    use super::{LayerState, TrafficLayer, names, no_prometheus};
    use crate::screens::prometheus_link::PrometheusStatus;

    #[test]
    fn stopping_frees_the_window() {
        let mut layer = TrafficLayer::default();
        assert_eq!(layer.state, LayerState::Off);
        assert!(!layer.stop_poll());
        layer.error = Some("timeout".into());
        assert!(!layer.stop_poll());
        assert!(layer.error.is_none());
    }

    #[test]
    fn notes_name_sources_and_prometheus_problems() {
        assert_eq!(names(&["istio", "hubble"]), "Istio, Cilium Hubble");
        assert_eq!(
            no_prometheus(&PrometheusStatus::NotFound),
            "no Prometheus found in the cluster"
        );
        let endpoint = Endpoint::Service {
            namespace: "monitoring".into(),
            name: "p".into(),
            port: "web".into(),
            https: false,
        };
        assert_eq!(
            no_prometheus(&PrometheusStatus::Unreachable(endpoint)),
            "Prometheus at monitoring/p:web doesn't answer"
        );
    }
}
