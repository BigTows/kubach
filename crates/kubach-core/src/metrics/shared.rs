//! One metrics-server poll per connection and set of namespaces, shared by
//! everything that shows usage: the Pods / Nodes table and the metrics block
//! of the details read the same samples instead of polling side by side.
//!
//! - **Shared**: [subscribing](MetricsHub::subscribe_with) joins a running
//!   poll whose [scope](PollScope) covers the one asked for (all namespaces
//!   cover any namespace; nodes cover any node), or starts one.
//! - **Lazy**: a poll runs only while a [`PollSubscription`] for it is alive;
//!   dropping the last one stops it.
//! - **Latest wins**: a subscriber gets the latest result right away, then
//!   each new one; a slow reader skips results instead of queueing them.

use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use tokio::sync::watch;

use super::{
    DEFAULT_POLL_INTERVAL, MetricsApi, MetricsError, MetricsIndex, NodeMetrics, PodMetrics,
};
use crate::runtime::{self, BackgroundTask};
use crate::sync::lock;

/// What one poll fetches.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PollScope {
    /// Every node.
    Nodes,
    /// Pods in these namespaces (sorted, no duplicates), or in all
    /// namespaces when `None`.
    Pods(Option<Vec<String>>),
}

impl PollScope {
    /// Pods in `namespaces` (all when `None`), in any order.
    pub fn pods(namespaces: Option<Vec<String>>) -> Self {
        Self::Pods(namespaces.map(|mut namespaces| {
            namespaces.sort();
            namespaces.dedup();
            namespaces
        }))
    }

    /// Whether a poll of `self` has everything a poll of `other` would.
    pub fn covers(&self, other: &PollScope) -> bool {
        match (self, other) {
            (Self::Nodes, Self::Nodes) | (Self::Pods(None), Self::Pods(_)) => true,
            (Self::Pods(Some(mine)), Self::Pods(Some(wanted))) => {
                wanted.iter().all(|namespace| mine.contains(namespace))
            }
            _ => false,
        }
    }
}

/// One successful poll.
#[derive(Debug, Default)]
pub struct Polled {
    /// Pod polls: every pod of the scope.
    pub pods: Vec<PodMetrics>,
    /// Node polls: every node.
    pub nodes: Vec<NodeMetrics>,
    /// Both, by object, for table cells.
    pub index: MetricsIndex,
}

impl Polled {
    pub fn from_pods(pods: Vec<PodMetrics>) -> Self {
        Self {
            index: MetricsIndex::from_pods(pods.iter().cloned()),
            pods,
            nodes: Vec::new(),
        }
    }

    pub fn from_nodes(nodes: Vec<NodeMetrics>) -> Self {
        Self {
            index: MetricsIndex::from_nodes(nodes.iter().cloned()),
            pods: Vec::new(),
            nodes,
        }
    }

    /// Pods of `namespace` (a poll may cover more namespaces).
    pub fn pods_in<'a>(&'a self, namespace: &'a str) -> impl Iterator<Item = &'a PodMetrics> {
        self.pods
            .iter()
            .filter(move |pod| pod.namespace == namespace)
    }

    pub fn node(&self, name: &str) -> Option<&NodeMetrics> {
        self.index.node(name)
    }
}

/// Result of one poll, as every subscriber sees it.
pub type PollResult = Result<Arc<Polled>, Arc<MetricsError>>;

/// Running polls of one connection. Cheap to clone; clones share polls.
#[derive(Clone)]
pub struct MetricsHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    interval: Duration,
    polls: Mutex<Vec<Weak<Poll>>>,
}

impl Default for MetricsHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsHub {
    /// Polls every [`DEFAULT_POLL_INTERVAL`].
    pub fn new() -> Self {
        Self::with_interval(DEFAULT_POLL_INTERVAL)
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            inner: Arc::new(HubInner {
                interval,
                polls: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Joins a running poll that covers `scope` (one of exactly `scope`
    /// first), or starts polling `scope` with `fetch`, now and then every
    /// interval on the I/O [`runtime`]. `fetch` is not used when joining.
    pub fn subscribe_with<F, Fut>(&self, scope: PollScope, fetch: F) -> PollSubscription
    where
        F: Fn(PollScope) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Polled, MetricsError>> + Send + 'static,
    {
        let mut polls = lock(&self.inner.polls);
        polls.retain(|poll| poll.strong_count() > 0);
        let running: Vec<Arc<Poll>> = polls.iter().filter_map(Weak::upgrade).collect();
        let joined = running
            .iter()
            .find(|poll| poll.scope == scope)
            .or_else(|| running.iter().find(|poll| poll.scope.covers(&scope)));
        if let Some(poll) = joined {
            return PollSubscription {
                poll: Arc::clone(poll),
            };
        }

        let (tx, rx) = watch::channel(None);
        let interval = self.inner.interval;
        let task_scope = scope.clone();
        let task = runtime::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                let result = fetch(task_scope.clone())
                    .await
                    .map(Arc::new)
                    .map_err(Arc::new);
                tx.send_replace(Some(result));
            }
        });
        let poll = Arc::new(Poll {
            scope,
            latest: rx,
            _task: task,
        });
        polls.push(Arc::downgrade(&poll));
        PollSubscription { poll }
    }

    /// Number of polls currently running.
    pub fn active_polls(&self) -> usize {
        lock(&self.inner.polls)
            .iter()
            .filter(|poll| poll.strong_count() > 0)
            .count()
    }
}

struct Poll {
    scope: PollScope,
    latest: watch::Receiver<Option<PollResult>>,
    _task: BackgroundTask,
}

/// Keeps a poll running. Clones share it; the poll stops when the last
/// clone (and the last [`results`](Self::results) stream) is dropped.
#[derive(Clone)]
pub struct PollSubscription {
    poll: Arc<Poll>,
}

impl PollSubscription {
    /// What the poll fetches; may cover more than was asked for.
    pub fn scope(&self) -> &PollScope {
        &self.poll.scope
    }

    /// The latest result, if the poll has finished one.
    pub fn latest(&self) -> Option<PollResult> {
        self.poll.latest.borrow().clone()
    }

    /// The latest result right away (when there is one), then each new
    /// one. Keeps the poll running while the stream lives.
    pub fn results(self) -> BoxStream<'static, PollResult> {
        let mut latest = self.poll.latest.clone();
        latest.mark_changed();
        futures::stream::unfold((self, latest), |(subscription, mut latest)| async move {
            loop {
                latest.changed().await.ok()?;
                let result = latest.borrow_and_update().clone();
                if let Some(result) = result {
                    return Some((result, (subscription, latest)));
                }
            }
        })
        .boxed()
    }
}

/// metrics-server of one connection, polled through its [`MetricsHub`].
#[derive(Clone)]
pub struct MetricsServer {
    hub: MetricsHub,
    api: MetricsApi,
}

impl MetricsServer {
    pub fn new(hub: MetricsHub, api: MetricsApi) -> Self {
        Self { hub, api }
    }

    /// Joins or starts the poll of `scope`.
    pub fn subscribe(&self, scope: PollScope) -> PollSubscription {
        let api = self.api.clone();
        self.hub.subscribe_with(scope, move |scope| {
            let api = api.clone();
            async move { fetch(&api, &scope).await }
        })
    }
}

async fn fetch(api: &MetricsApi, scope: &PollScope) -> Result<Polled, MetricsError> {
    match scope {
        PollScope::Nodes => api.nodes().await.map(Polled::from_nodes),
        PollScope::Pods(None) => api.pods(None).await.map(Polled::from_pods),
        PollScope::Pods(Some(namespaces)) => {
            let mut pods = Vec::new();
            for namespace in namespaces {
                pods.extend(api.pods(Some(namespace)).await?);
            }
            Ok(Polled::from_pods(pods))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn pod(namespace: &str, name: &str) -> PodMetrics {
        PodMetrics {
            name: name.into(),
            namespace: namespace.into(),
            timestamp: None,
            window: None,
            containers: Vec::new(),
        }
    }

    /// A fetch that counts its calls and returns one pod per namespace.
    fn counting(
        calls: &Arc<AtomicUsize>,
    ) -> impl Fn(PollScope) -> futures::future::Ready<Result<Polled, MetricsError>> + Send + 'static
    {
        let calls = Arc::clone(calls);
        move |scope| {
            calls.fetch_add(1, Ordering::SeqCst);
            let pods = match scope {
                PollScope::Pods(Some(namespaces)) => {
                    namespaces.iter().map(|ns| pod(ns, "p")).collect()
                }
                _ => vec![pod("web", "p"), pod("api", "p")],
            };
            futures::future::ready(Ok(Polled::from_pods(pods)))
        }
    }

    fn names(namespaces: &[&str]) -> Option<Vec<String>> {
        Some(namespaces.iter().map(|ns| (*ns).to_owned()).collect())
    }

    #[test]
    fn scopes_cover_their_parts() {
        let all = PollScope::pods(None);
        let two = PollScope::pods(names(&["web", "api"]));
        let web = PollScope::pods(names(&["web"]));
        assert_eq!(two, PollScope::pods(names(&["api", "web", "api"])));
        assert!(all.covers(&two) && all.covers(&web));
        assert!(two.covers(&web) && !web.covers(&two));
        assert!(!two.covers(&all));
        assert!(PollScope::Nodes.covers(&PollScope::Nodes));
        assert!(!all.covers(&PollScope::Nodes) && !PollScope::Nodes.covers(&web));
    }

    #[tokio::test]
    async fn covered_scopes_share_one_poll_until_the_last_is_dropped() {
        let hub = MetricsHub::with_interval(Duration::from_secs(3600));
        let calls = Arc::new(AtomicUsize::new(0));

        // The table polls two namespaces...
        let table = hub.subscribe_with(PollScope::pods(names(&["web", "api"])), counting(&calls));
        // ...and the details of a pod in one of them join it.
        let details = hub.subscribe_with(PollScope::pods(names(&["web"])), counting(&calls));
        assert_eq!(hub.active_polls(), 1);
        assert_eq!(details.scope(), table.scope());

        let mut results = details.clone().results();
        let first = results.next().await.unwrap().unwrap();
        assert_eq!(first.pods_in("web").count(), 1);
        assert_eq!(first.pods_in("api").count(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(table.latest().is_some());

        // Another namespace is not covered: a poll of its own.
        let other = hub.subscribe_with(PollScope::pods(names(&["queue"])), counting(&calls));
        assert_eq!(hub.active_polls(), 2);
        drop(other);
        assert_eq!(hub.active_polls(), 1);

        // The poll lives while anything holds it: here the details' stream.
        drop((table, details));
        assert_eq!(hub.active_polls(), 1);
        drop(results);
        assert_eq!(hub.active_polls(), 0);

        // Subscribing again starts a fresh poll.
        let _again = hub.subscribe_with(PollScope::pods(names(&["web"])), counting(&calls));
        assert_eq!(hub.active_polls(), 1);
    }

    #[tokio::test]
    async fn subscribers_get_the_latest_then_each_new_result() {
        let hub = MetricsHub::with_interval(Duration::from_millis(20));
        let calls = Arc::new(AtomicUsize::new(0));
        let table = hub.subscribe_with(PollScope::Nodes, {
            let calls = Arc::clone(&calls);
            move |_| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                futures::future::ready(if call == 1 {
                    Err(MetricsError::Unavailable("timeout".into()))
                } else {
                    Ok(Polled::from_nodes(Vec::new()))
                })
            }
        });
        let mut results = table.clone().results();
        assert!(results.next().await.unwrap().is_ok());
        // Errors reach subscribers too, and polling goes on after them.
        assert!(results.next().await.unwrap().is_err());
        assert!(results.next().await.unwrap().is_ok());

        // A late subscriber gets the latest result right away, without a
        // fetch of its own.
        let before = calls.load(Ordering::SeqCst);
        let late = hub.subscribe_with(
            PollScope::Nodes,
            |_| -> futures::future::Ready<Result<Polled, MetricsError>> {
                panic!("joined polls don't fetch")
            },
        );
        let mut late = late.results();
        assert!(late.next().await.is_some());
        assert!(calls.load(Ordering::SeqCst) - before <= 1);
        assert_eq!(hub.active_polls(), 1);
    }
}
