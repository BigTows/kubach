//! CPU / Memory for the Pods and Nodes tables: from metrics-server when the
//! cluster serves `metrics.k8s.io`, else from the context's Prometheus
//! (#179, ADR 0009). Without either everything degrades to a quiet
//! "metrics unavailable".

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gpui_kit::*;
use kubach_core::ResourceKind;
use kubach_core::metrics::prometheus::{Endpoint, Prometheus};
use kubach_core::metrics::shared::{PollResult, PollScope, PollSubscription};
use kubach_core::metrics::{
    self, DEFAULT_POLL_INTERVAL, MetricsError, MetricsIndex, Polling, format_cpu, format_memory,
};
use kubach_i18n::t;
use kubach_ui::{ResourceCell, ResourceColumn};
use kube::api::DynamicObject;

use super::prometheus_link::{PrometheusStatus, doesnt_answer};

/// Kinds that get usage columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageKind {
    Pods,
    Nodes,
}

impl UsageKind {
    pub fn of(kind: &ResourceKind) -> Option<Self> {
        if !kind.api.group.is_empty() {
            return None;
        }
        match kind.api.kind.as_str() {
            "Pod" => Some(Self::Pods),
            "Node" => Some(Self::Nodes),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub enum MetricsState {
    #[default]
    Loading,
    Ready(MetricsIndex),
    /// metrics-server missing, not answering, or forbidden.
    Unavailable,
}

impl MetricsState {
    pub fn from_result(result: Result<MetricsIndex, MetricsError>) -> Self {
        match result {
            Ok(index) => Self::Ready(index),
            Err(_) => Self::Unavailable,
        }
    }

    /// From a shared metrics-server poll.
    pub fn from_poll(result: &PollResult) -> Self {
        match result {
            Ok(polled) => Self::Ready(polled.index.clone()),
            Err(_) => Self::Unavailable,
        }
    }
}

/// Usage of a table as it changes.
pub type States = BoxStream<'static, MetricsState>;

/// Where the usage columns come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageSource {
    MetricsServer,
    Prometheus,
}

impl UsageSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::MetricsServer => "metrics-server",
            Self::Prometheus => "Prometheus",
        }
    }
}

/// Usage of the listed Pods or Nodes, refreshed by polling.
///
/// A metrics-server feed polls for the life of the view. A Prometheus feed
/// follows the context's Prometheus in the [`PrometheusRegistry`]
/// (#186): see [`UsageFeed::follow`].
///
/// [`PrometheusRegistry`]: super::prometheus_link::PrometheusRegistry
pub struct UsageFeed {
    pub kind: UsageKind,
    /// `None` while looking for Prometheus, or when there is no source.
    pub source: Option<UsageSource>,
    pub state: MetricsState,
    /// Prometheus feeds: the server being polled.
    polling: Option<Endpoint>,
    /// Prometheus feeds: why there is no server to poll (it doesn't answer,
    /// the address is not valid).
    problem: Option<String>,
    pub _task: Option<Task<()>>,
}

impl UsageFeed {
    /// Polls metrics-server with `task`.
    pub fn metrics_server(kind: UsageKind, task: Task<()>) -> Self {
        Self {
            kind,
            source: Some(UsageSource::MetricsServer),
            state: MetricsState::Loading,
            polling: None,
            problem: None,
            _task: Some(task),
        }
    }

    /// Without metrics-server: waits for the context's Prometheus.
    pub fn prometheus(kind: UsageKind) -> Self {
        Self {
            kind,
            source: None,
            state: MetricsState::Loading,
            polling: None,
            problem: None,
            _task: None,
        }
    }

    /// Whether the feed follows the context's Prometheus (there is no
    /// metrics-server).
    pub fn follows_prometheus(&self) -> bool {
        self.source != Some(UsageSource::MetricsServer)
    }

    /// Brings a Prometheus feed in line with the context's Prometheus as
    /// the registry has it now. Returns the server to poll when polling
    /// has to (re)start: another server was picked, or the same one was
    /// connected again ("Detect again"). The caller sets the task. Stops
    /// polling when there is no server (Off, not found, doesn't answer);
    /// does nothing for a metrics-server feed.
    pub fn follow(&mut self, status: &PrometheusStatus) -> Option<Prometheus> {
        let prometheus = status.prometheus();
        let start = self.retarget(
            prometheus.map(Prometheus::endpoint),
            status.pending(),
            status.problem(),
        );
        start.then(|| prometheus.cloned()).flatten()
    }

    /// [`Self::follow`] for the server the registry `offered`, if any;
    /// `true` when polling of it has to start.
    fn retarget(
        &mut self,
        offered: Option<&Endpoint>,
        pending: bool,
        problem: Option<String>,
    ) -> bool {
        if !self.follows_prometheus() {
            return false;
        }
        self.problem = problem;
        match offered {
            Some(endpoint) if self.polling.as_ref() == Some(endpoint) => false,
            Some(endpoint) => {
                self.polling = Some(endpoint.clone());
                self.source = Some(UsageSource::Prometheus);
                self.state = MetricsState::Loading;
                self._task = None;
                true
            }
            None => {
                self.polling = None;
                self.source = None;
                self._task = None;
                self.state = if pending {
                    MetricsState::Loading
                } else {
                    MetricsState::Unavailable
                };
                false
            }
        }
    }

    /// Note after the row count: the source, or why there is none.
    pub fn note(&self) -> Option<String> {
        if let Some(problem) = &self.problem {
            return Some(problem.clone());
        }
        match (&self.state, self.source, &self.polling) {
            // The server answered the probe but stopped answering polls.
            (MetricsState::Unavailable, _, Some(endpoint)) => Some(doesnt_answer(endpoint)),
            (MetricsState::Unavailable, _, None) => Some(t!("metrics-unavailable")),
            (_, Some(source), _) => Some(source.label().to_owned()),
            (_, None, _) => None,
        }
    }
}

/// What the metrics-server poll of `kind` covers: pods in `namespaces`
/// (all when `None`), or nodes.
pub fn poll_scope(kind: UsageKind, namespaces: Option<Vec<String>>) -> PollScope {
    match kind {
        UsageKind::Nodes => PollScope::Nodes,
        UsageKind::Pods => PollScope::pods(namespaces),
    }
}

/// Usage from a shared metrics-server poll (see
/// [`kubach_core::metrics::shared`]).
pub fn metrics_server_states(poll: PollSubscription) -> States {
    poll.results()
        .map(|result| MetricsState::from_poll(&result))
        .boxed()
}

/// Polls usage of `kind` from Prometheus, at the metrics-server interval:
/// two instant queries per poll for all `namespaces` (all when `None`).
pub fn poll_prometheus(
    kind: UsageKind,
    prometheus: Prometheus,
    namespaces: Option<Vec<String>>,
) -> Polling<MetricsIndex> {
    metrics::poll(DEFAULT_POLL_INTERVAL, move || {
        let prometheus = prometheus.clone();
        let namespaces = namespaces.clone();
        async move {
            let result = match kind {
                UsageKind::Nodes => prometheus.node_usage().await,
                UsageKind::Pods => prometheus.pod_usage(namespaces.as_deref()).await,
            };
            result.map_err(|e| MetricsError::Unavailable(e.to_string()))
        }
    })
}

pub fn columns() -> [ResourceColumn; 2] {
    [
        ResourceColumn::new("cpu", super::plugin_text::column_title_of("cpu", "CPU"))
            .sortable()
            .width(px(90.))
            .align_end(),
        ResourceColumn::new(
            "memory",
            super::plugin_text::column_title_of("memory", "Memory"),
        )
        .sortable()
        .width(px(100.))
        .align_end(),
    ]
}

pub fn cells(state: &MetricsState, object: &DynamicObject) -> [ResourceCell; 2] {
    let usage = match state {
        MetricsState::Ready(index) => index.usage_of(object),
        _ => None,
    };
    match usage {
        Some(usage) => [
            ResourceCell::mono(format_cpu(usage.cpu)).sort_by(saturate(usage.cpu)),
            ResourceCell::mono(format_memory(usage.memory)).sort_by(saturate(usage.memory)),
        ],
        None => [
            ResourceCell::text("—").muted().sort_by(-1),
            ResourceCell::text("—").muted().sort_by(-1),
        ],
    }
}

fn saturate(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::prometheus::Endpoint;
    use kubach_core::metrics::{MetricsError, MetricsIndex, NodeMetrics, Usage};
    use kube::api::DynamicObject;
    use serde_json::json;

    use std::sync::Arc;

    use kubach_core::metrics::shared::{PollScope, Polled};

    use super::{MetricsState, UsageFeed, UsageKind, UsageSource, cells, poll_scope};
    use crate::screens::prometheus_link::PrometheusStatus;

    fn service(name: &str) -> Endpoint {
        Endpoint::Service {
            namespace: "monitoring".into(),
            name: name.into(),
            port: "web".into(),
            https: false,
        }
    }

    #[test]
    fn metrics_server_feeds_ignore_prometheus() {
        let mut feed = UsageFeed::prometheus(UsageKind::Pods);
        feed.source = Some(UsageSource::MetricsServer);
        assert!(!feed.follows_prometheus());
        assert!(!feed.retarget(Some(&service("p")), false, None));
        assert!(feed.follow(&PrometheusStatus::Off).is_none());
        assert_eq!(feed.source, Some(UsageSource::MetricsServer));
        assert!(!matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(feed.note().as_deref(), Some("metrics-server"));
        feed.state = MetricsState::Unavailable;
        assert_eq!(feed.note().as_deref(), Some("metrics unavailable"));
    }

    #[test]
    fn prometheus_feeds_follow_the_registry() {
        let mut feed = UsageFeed::prometheus(UsageKind::Pods);
        assert!(feed.follows_prometheus());
        // Searching: wait quietly.
        assert!(!feed.retarget(None, true, None));
        assert!(!matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(feed.note(), None);
        // Nothing found: unavailable.
        assert!(!feed.retarget(None, false, None));
        assert!(matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(feed.note().as_deref(), Some("metrics unavailable"));
        // Found later (Detect again, an address in the settings): start.
        assert!(feed.retarget(Some(&service("p")), false, None));
        assert_eq!(feed.source, Some(UsageSource::Prometheus));
        assert!(!matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(feed.note().as_deref(), Some("Prometheus"));
        // The same server on the next render: keep polling.
        assert!(!feed.retarget(Some(&service("p")), false, None));
        // Another server picked: restart.
        assert!(feed.retarget(Some(&service("q")), false, None));
        // Detect again reconnects the same server: stop while connecting,
        // then start again.
        assert!(!feed.retarget(None, true, None));
        assert_eq!(feed.source, None);
        assert!(feed.retarget(Some(&service("q")), false, None));
        // Off: stop.
        assert!(feed.follow(&PrometheusStatus::Off).is_none());
        assert_eq!(feed.source, None);
        assert!(feed._task.is_none());
        assert!(matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(feed.note().as_deref(), Some("metrics unavailable"));
    }

    #[test]
    fn notes_tell_why_prometheus_gives_nothing() {
        let mut feed = UsageFeed::prometheus(UsageKind::Nodes);
        let unreachable = PrometheusStatus::Unreachable(service("p"));
        assert!(feed.follow(&unreachable).is_none());
        assert!(matches!(feed.state, MetricsState::Unavailable));
        assert_eq!(
            feed.note().as_deref(),
            Some("Prometheus at monitoring/p:web doesn't answer")
        );
        let invalid = PrometheusStatus::Invalid("nope".into());
        assert!(feed.follow(&invalid).is_none());
        assert_eq!(
            feed.note().as_deref(),
            Some("Prometheus address nope is not valid")
        );
        // A server that answered the probe stops answering polls.
        assert!(feed.retarget(Some(&service("p")), false, None));
        let failed = Err(MetricsError::Unavailable("timeout".into()));
        feed.state = MetricsState::from_result(failed);
        assert_eq!(
            feed.note().as_deref(),
            Some("Prometheus at monitoring/p:web doesn't answer")
        );
        // It answers again.
        feed.state = MetricsState::Ready(MetricsIndex::default());
        assert_eq!(feed.note().as_deref(), Some("Prometheus"));
    }

    #[test]
    fn tables_poll_their_namespaces_through_the_shared_poll() {
        let names = |list: &[&str]| Some(list.iter().map(|n| (*n).to_owned()).collect());
        assert_eq!(
            poll_scope(UsageKind::Nodes, names(&["web"])),
            PollScope::Nodes
        );
        assert_eq!(
            poll_scope(UsageKind::Pods, names(&["web", "api"])),
            PollScope::Pods(names(&["api", "web"]))
        );
        assert_eq!(poll_scope(UsageKind::Pods, None), PollScope::Pods(None));
        let ok = Ok(Arc::new(Polled::from_nodes(Vec::new())));
        assert!(matches!(
            MetricsState::from_poll(&ok),
            MetricsState::Ready(_)
        ));
        let failed = Err(Arc::new(MetricsError::Unavailable("timeout".into())));
        assert!(matches!(
            MetricsState::from_poll(&failed),
            MetricsState::Unavailable
        ));
    }

    #[test]
    fn cells_show_usage_or_a_muted_dash() {
        let node: DynamicObject = serde_json::from_value(
            json!({ "apiVersion": "v1", "kind": "Node", "metadata": { "name": "n1" } }),
        )
        .unwrap();
        let index = MetricsIndex::from_nodes([NodeMetrics {
            name: "n1".into(),
            timestamp: None,
            window: None,
            usage: Usage {
                cpu: 250_000_000,
                memory: 512 << 20,
            },
        }]);
        let [cpu, memory] = cells(&MetricsState::Ready(index), &node);
        assert_eq!(cpu.label().as_ref(), "0.250");
        assert_eq!(memory.label().as_ref(), "512.0 MiB");
        let [cpu, _] = cells(&MetricsState::Unavailable, &node);
        assert_eq!(cpu.label().as_ref(), "—");
    }
}
