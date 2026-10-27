//! Local cache of cluster objects, filled by list+watch, and the change
//! notifications the UI reacts to.
//!
//! The UI never lists objects itself. It [subscribes](ResourceCache::subscribe)
//! to a kind in a namespace (or all namespaces), reads [snapshots](Subscription::snapshot)
//! and waits for [changes](Changes::next):
//!
//! - **Lazy**: a watch runs only while at least one [`Subscription`] for it is
//!   alive. Subscribing to the same kind and namespace twice shares one
//!   watch; dropping the last handle closes it.
//! - **Resilient**: a failed list or watch is retried with backoff on a
//!   fresh `kube::runtime::watcher`, which relists, so recovery is always
//!   visible (the watcher itself resumes silently, #316). A short outage is
//!   retried quietly, keeping the status; a lasting one shows up as
//!   [`WatchStatus::Error`] until it recovers (see [`outage`]).
//! - **Coalesced**: a burst of events (a rollout, the initial list) produces a
//!   single notification, at most one per [`DEBOUNCE`].
//! - **Cheap reads**: snapshots are shared `Arc` slices, sorted by namespace
//!   and name.
//! - **No secret values**: Secrets are [redacted](crate::secrets::mask::redact) on the way in,
//!   so nothing reading the cache can display them.
//! - **Narrowed watches**: a [`WatchFilter`] adds label/field selectors and
//!   can ask for metadata only (e.g. Helm release objects, whose payload is
//!   not needed to notice a change).
//! - **Lingering**: a watch whose last subscription is dropped keeps running
//!   for [`LINGER`] (at most [`MAX_LINGERING`] of them), so switching to
//!   another namespace and back shows the list at once, without a new list
//!   request (#300).
//! - **Borrowed snapshots**: while a namespace's own watch loads, its
//!   snapshot is taken from a loaded watch of all namespaces of the kind,
//!   if one runs.

mod merge;
pub mod outage;
mod store;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use kube::Client;
use kube::api::{ApiResource, DynamicObject, PartialObjectMeta};
use kube::runtime::watcher::{self, Event};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::discovery::{self, ResourceKind};
use crate::runtime::{self, BackgroundTask};
use crate::status_bar::SyncSummary;
use crate::sync::lock;
use outage::Outage;

pub use merge::{MergedChanges, MergedSnapshot, SubscriptionSet, merge};
pub use store::{Change, ObjectStore, Objects, WatchError, WatchStatus};

/// Quiet period after an object change before subscribers are notified, so a
/// burst of events turns into one UI update.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How long a watch keeps running after its last subscription is dropped.
pub const LINGER: Duration = Duration::from_secs(90);

/// Watches kept running without subscribers; the least recently released
/// stops first.
pub const MAX_LINGERING: usize = 8;

/// Items of a watcher stream, as the cache consumes them.
pub type EventStream = BoxStream<'static, Result<Event<DynamicObject>, watcher::Error>>;

type Source = Arc<dyn Fn(&ResourceKind, &WatchKey) -> EventStream + Send + Sync>;

/// Narrows a watch. The default watches every object with its full body.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct WatchFilter {
    /// Label selector, e.g. `owner=helm`.
    pub labels: Option<String>,
    /// Field selector, e.g. `type=helm.sh/release.v1`.
    pub fields: Option<String>,
    /// Only metadata is fetched: cached objects have an empty body.
    pub metadata_only: bool,
}

/// Identity of a watch: a kind (at a version) in a namespace, or in all
/// namespaces when `namespace` is `None`, with its filter. Cluster-scoped
/// kinds never have a namespace.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WatchKey {
    pub group: String,
    pub version: String,
    pub plural: String,
    pub namespace: Option<String>,
    pub filter: WatchFilter,
}

impl WatchKey {
    pub fn new(kind: &ResourceKind, namespace: Option<&str>) -> Self {
        Self::filtered(kind, namespace, WatchFilter::default())
    }

    pub fn filtered(kind: &ResourceKind, namespace: Option<&str>, filter: WatchFilter) -> Self {
        Self {
            group: kind.api.group.clone(),
            version: kind.api.version.clone(),
            plural: kind.api.plural.clone(),
            namespace: namespace.filter(|_| kind.namespaced).map(str::to_owned),
            filter,
        }
    }
}

/// Cache of the objects watched for one connection. Cheap to clone; all
/// clones share the same watches. Dropping every clone and subscription
/// stops all watches.
#[derive(Clone)]
pub struct ResourceCache {
    inner: Arc<CacheInner>,
}

struct CacheInner {
    source: Source,
    watches: Mutex<HashMap<WatchKey, Weak<Watch>>>,
    linger: Duration,
    /// Delay before the first retry of a failed watch.
    retry: Duration,
    /// Watches without subscribers, kept until the deadline; oldest first.
    lingering: Mutex<VecDeque<(Instant, Arc<Watch>)>>,
}

impl CacheInner {
    /// Keeps `watch`, just released by its last subscription, running for
    /// a while.
    fn linger(self: &Arc<Self>, watch: Arc<Watch>) {
        if self.linger.is_zero() {
            return;
        }
        {
            let mut lingering = lock(&self.lingering);
            lingering.retain(|(_, held)| !Arc::ptr_eq(held, &watch));
            lingering.push_back((Instant::now() + self.linger, watch));
            while lingering.len() > MAX_LINGERING {
                lingering.pop_front();
            }
        }
        let inner = Arc::downgrade(self);
        let linger = self.linger;
        // Holds only a weak handle: a closed cache needs no expiry.
        drop(runtime::spawn_detached(async move {
            tokio::time::sleep(linger).await;
            if let Some(inner) = inner.upgrade() {
                inner.expire();
            }
        }));
    }

    /// Stops lingering watches whose time is up. (Dropping a watch only
    /// aborts its task, so it is fine under the lock.)
    fn expire(&self) {
        let now = Instant::now();
        lock(&self.lingering).retain(|(due, _)| *due > now);
    }

    /// Takes `watch` out of the lingering ones (it has a subscriber again).
    fn revive(&self, watch: &Arc<Watch>) {
        lock(&self.lingering).retain(|(_, held)| !Arc::ptr_eq(held, watch));
    }

    fn is_lingering(&self, watch: &Arc<Watch>) -> bool {
        lock(&self.lingering)
            .iter()
            .any(|(_, held)| Arc::ptr_eq(held, watch))
    }
}

impl ResourceCache {
    /// Cache that watches through `client`.
    pub fn new(client: Client) -> Self {
        Self::with_source(move |kind, key| {
            // `any_semantic` lets the API server answer the list from its
            // own cache, which is much cheaper for large clusters.
            let mut config = watcher::Config::default().any_semantic();
            if let Some(labels) = &key.filter.labels {
                config = config.labels(labels);
            }
            if let Some(fields) = &key.filter.fields {
                config = config.fields(fields);
            }
            let namespace = key.namespace.as_deref();
            if key.filter.metadata_only {
                let api: kube::Api<PartialObjectMeta<DynamicObject>> = match namespace {
                    Some(namespace) => {
                        kube::Api::namespaced_with(client.clone(), namespace, &kind.api)
                    }
                    None => kube::Api::all_with(client.clone(), &kind.api),
                };
                // No backoff here: `run` retries on a fresh watcher.
                watcher::watcher(api, config)
                    .map(|item| item.map(|event| map_event(event, meta_object)))
                    .boxed()
            } else {
                let api = discovery::api(&client, kind, namespace);
                // List items come without `kind`; give every object its type.
                let resource = kind.api.clone();
                watcher::watcher(api, config)
                    .map(move |item| item.map(|event| typed_event(event, &resource)))
                    .boxed()
            }
        })
    }

    /// Cache fed by an arbitrary event source, e.g. a scripted stream in
    /// tests. The source is called again for each retry after an error.
    pub fn with_source(
        source: impl Fn(&ResourceKind, &WatchKey) -> EventStream + Send + Sync + 'static,
    ) -> Self {
        Self::build(Arc::new(source), LINGER, outage::RETRY_MIN)
    }

    /// Like [`with_source`](Self::with_source), with released watches kept
    /// for `linger` instead of [`LINGER`] (zero stops them at once).
    pub fn with_source_and_linger(
        source: impl Fn(&ResourceKind, &WatchKey) -> EventStream + Send + Sync + 'static,
        linger: Duration,
    ) -> Self {
        Self::build(Arc::new(source), linger, outage::RETRY_MIN)
    }

    fn build(source: Source, linger: Duration, retry: Duration) -> Self {
        Self {
            inner: Arc::new(CacheInner {
                source,
                watches: Mutex::new(HashMap::new()),
                linger,
                retry,
                lingering: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// Starts watching `kind` in `namespace` (all namespaces when `None`), or
    /// joins the watch that is already running for them.
    ///
    /// Must be called with the I/O [`runtime`] available (it always is: the
    /// watch is spawned there, not on the caller's executor).
    pub fn subscribe(&self, kind: &ResourceKind, namespace: Option<&str>) -> Subscription {
        self.subscribe_filtered(kind, namespace, WatchFilter::default())
    }

    /// Like [`subscribe`](Self::subscribe), but only for objects matching
    /// `filter`. Watches with different filters are separate.
    pub fn subscribe_filtered(
        &self,
        kind: &ResourceKind,
        namespace: Option<&str>,
        filter: WatchFilter,
    ) -> Subscription {
        let key = WatchKey::filtered(kind, namespace, filter);
        let mut watches = lock(&self.inner.watches);
        if let Some(watch) = watches.get(&key).and_then(Weak::upgrade) {
            drop(watches);
            self.inner.revive(&watch);
            return Subscription {
                watch,
                cache: Arc::downgrade(&self.inner),
            };
        }
        watches.retain(|_, watch| watch.strong_count() > 0);
        // A watch of all namespaces stands in while this one loads.
        let all_namespaces = key.namespace.as_ref().and_then(|_| {
            let all = WatchKey {
                namespace: None,
                ..key.clone()
            };
            watches.get(&all).cloned()
        });

        let redact = crate::secrets::mask::is_core_secret(&kind.api.group, &kind.api.kind);
        let (generation, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            store: Mutex::new(ObjectStore::new(redact)),
            generation,
        });
        let connect = {
            let (source, kind, key) = (Arc::clone(&self.inner.source), kind.clone(), key.clone());
            move || source(&kind, &key)
        };
        // The first stream opens here, so the request starts at once.
        let events = connect();
        let task = runtime::spawn(run(Arc::clone(&shared), events, connect, self.inner.retry));
        let watch = Arc::new(Watch {
            key: key.clone(),
            shared,
            all_namespaces,
            _task: task,
        });
        watches.insert(key, Arc::downgrade(&watch));
        Subscription {
            watch,
            cache: Arc::downgrade(&self.inner),
        }
    }

    /// Watches `kind` in each of `namespaces` (all namespaces when `None`)
    /// with `filter`, as one set. A cluster-scoped kind gets one watch
    /// whatever the namespaces.
    pub fn subscribe_many(
        &self,
        kind: &ResourceKind,
        namespaces: Option<&[String]>,
        filter: WatchFilter,
    ) -> SubscriptionSet {
        let parts = match namespaces {
            Some(namespaces) if kind.namespaced => namespaces
                .iter()
                .map(|namespace| self.subscribe_filtered(kind, Some(namespace), filter.clone()))
                .collect(),
            _ => vec![self.subscribe_filtered(kind, None, filter)],
        };
        SubscriptionSet::new(parts)
    }

    /// Number of objects a running, loaded watch holds, without starting
    /// one; `None` when there is no such watch.
    pub fn loaded_count(&self, kind: &ResourceKind, namespace: Option<&str>) -> Option<usize> {
        let key = WatchKey::new(kind, namespace);
        let watch = lock(&self.inner.watches).get(&key)?.upgrade()?;
        let store = lock(&watch.shared.store);
        (store.status() == &WatchStatus::Ready).then(|| store.len())
    }

    /// Number of `kind` objects in `namespace` that a running, loaded watch
    /// holds, either of that namespace or of all namespaces, without
    /// starting one; `None` when there is no such watch.
    pub fn loaded_count_in(&self, kind: &ResourceKind, namespace: &str) -> Option<usize> {
        if let Some(count) = self.loaded_count(kind, Some(namespace)) {
            return Some(count);
        }
        if !kind.namespaced {
            return None;
        }
        let key = WatchKey::new(kind, None);
        let watch = lock(&self.inner.watches).get(&key)?.upgrade()?;
        let mut store = lock(&watch.shared.store);
        if store.status() != &WatchStatus::Ready {
            return None;
        }
        let count = store
            .snapshot()
            .iter()
            .filter(|object| object.metadata.namespace.as_deref() == Some(namespace))
            .count();
        Some(count)
    }

    /// `kind` objects in `namespace` that a running, loaded watch holds,
    /// either of that namespace or of all namespaces, without starting one;
    /// `None` when there is no such watch.
    pub fn loaded_objects_in(
        &self,
        kind: &ResourceKind,
        namespace: &str,
    ) -> Option<Vec<Arc<DynamicObject>>> {
        let keys = [
            WatchKey::new(kind, Some(namespace)),
            WatchKey::new(kind, None),
        ];
        keys.iter().find_map(|key| {
            let watch = lock(&self.inner.watches).get(key)?.upgrade()?;
            let mut store = lock(&watch.shared.store);
            (store.status() == &WatchStatus::Ready).then(|| {
                store
                    .snapshot()
                    .iter()
                    .filter(|object| object.metadata.namespace.as_deref() == Some(namespace))
                    .cloned()
                    .collect()
            })
        })
    }

    /// Number of watches currently running.
    pub fn active_watches(&self) -> usize {
        lock(&self.inner.watches)
            .values()
            .filter(|watch| watch.strong_count() > 0)
            .count()
    }

    /// Objects held by all running watches (the debug overlay, #291).
    #[cfg(debug_assertions)]
    pub fn cached_objects(&self) -> usize {
        let watches: Vec<Arc<Watch>> = lock(&self.inner.watches)
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        watches
            .iter()
            .map(|watch| lock(&watch.shared.store).len())
            .sum()
    }

    /// How many running watches are still loading or reconnecting, for the
    /// "Syncing…" indicator. Forbidden kinds never finish, so they don't
    /// count; a watch that lost its connection (even before its first list)
    /// counts as reconnecting, not loading, so endless retries don't look
    /// like progress (#316).
    pub fn sync_summary(&self) -> SyncSummary {
        let watches = lock(&self.inner.watches);
        let mut summary = SyncSummary::default();
        let running: Vec<_> = watches.values().filter_map(Weak::upgrade).collect();
        drop(watches);
        // Nobody looks at a lingering watch: it doesn't make the app busy.
        for watch in running.iter().filter(|w| !self.inner.is_lingering(w)) {
            let store = lock(&watch.shared.store);
            if store.is_reconnecting() {
                summary.reconnecting += 1;
            } else if store.status() == &WatchStatus::Loading {
                summary.loading += 1;
            }
        }
        summary
    }

    /// Whether both handles refer to the same cache (i.e. the same connection).
    pub fn same_as(&self, other: &ResourceCache) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// A metadata-only object as a [`DynamicObject`] with an empty body.
fn meta_object(object: PartialObjectMeta<DynamicObject>) -> DynamicObject {
    DynamicObject {
        types: object.types,
        metadata: object.metadata,
        data: serde_json::Value::Object(serde_json::Map::new()),
    }
}

/// `event` with its objects typed from `api` (list items have no `kind`).
fn typed_event(event: Event<DynamicObject>, api: &ApiResource) -> Event<DynamicObject> {
    map_event(event, |mut object| {
        discovery::fill_types(&mut object, api);
        object
    })
}

fn map_event<A, B>(event: Event<A>, f: impl Fn(A) -> B) -> Event<B> {
    match event {
        Event::Apply(object) => Event::Apply(f(object)),
        Event::Delete(object) => Event::Delete(f(object)),
        Event::Init => Event::Init,
        Event::InitApply(object) => Event::InitApply(f(object)),
        Event::InitDone => Event::InitDone,
    }
}

/// State shared between a watch task and its subscribers. Owned by the task
/// and the [`Watch`], never the other way round, so dropping the last
/// subscription aborts the task.
struct Shared {
    store: Mutex<ObjectStore>,
    generation: watch::Sender<u64>,
}

impl Shared {
    fn publish(&self) {
        self.generation.send_modify(|generation| *generation += 1);
    }
}

struct Watch {
    key: WatchKey,
    shared: Arc<Shared>,
    /// The watch of all namespaces (same kind and filter) that was running
    /// when this one of a single namespace started.
    all_namespaces: Option<Weak<Watch>>,
    _task: BackgroundTask,
}

impl Watch {
    /// Objects of this watch's namespace in the all-namespaces watch, while
    /// this one is still loading and that one has loaded.
    fn borrowed(&self) -> Option<Objects> {
        let namespace = self.key.namespace.as_deref()?;
        let all = self.all_namespaces.as_ref()?.upgrade()?;
        let mut store = lock(&all.shared.store);
        (store.status() == &WatchStatus::Ready).then(|| {
            store
                .snapshot()
                .iter()
                .filter(|object| object.metadata.namespace.as_deref() == Some(namespace))
                .cloned()
                .collect()
        })
    }
}

/// What woke the watch task up.
// Lives for one loop turn, moved at once: boxing the item would only add
// an allocation per event.
#[allow(clippy::large_enum_variant)]
enum Wake {
    Item(Option<Result<Event<DynamicObject>, watcher::Error>>),
    /// The debounce period is over.
    Publish,
    /// Time to retry after a failure.
    Retry,
    /// The outage lasted long enough to show its error.
    ShowError,
}

/// Next item of `events`; never resolves while there is no stream.
async fn next_item(
    events: &mut Option<EventStream>,
) -> Option<Result<Event<DynamicObject>, watcher::Error>> {
    match events {
        Some(events) => events.next().await,
        None => std::future::pending().await,
    }
}

/// Resolves at `at`; never without one.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Feeds the store from `events`, then from the streams `connect` opens. A failed stream
/// is dropped and, after a backoff, replaced by a fresh one (which relists):
/// the watcher would resume on its own, but silently, leaving the error up
/// until the next change of an object (#316).
async fn run(
    shared: Arc<Shared>,
    events: EventStream,
    connect: impl Fn() -> EventStream,
    retry: Duration,
) {
    let mut events = Some(events);
    let mut debounce: Option<Instant> = None;
    let mut retry_at: Option<Instant> = None;
    let mut outage: Option<Outage> = None;
    loop {
        let show_at = outage.as_ref().and_then(Outage::show_at);
        let wake = tokio::select! {
            item = next_item(&mut events) => Wake::Item(item),
            () = sleep_until(debounce) => Wake::Publish,
            () = sleep_until(retry_at) => Wake::Retry,
            () = sleep_until(show_at) => Wake::ShowError,
        };
        let item = match wake {
            Wake::Item(Some(item)) => item,
            Wake::Item(None) => break,
            Wake::Publish => {
                debounce = None;
                shared.publish();
                continue;
            }
            Wake::Retry => {
                retry_at = None;
                events = Some(connect());
                continue;
            }
            Wake::ShowError => {
                if let Some(outage) = &mut outage {
                    outage.mark_shown();
                    if lock(&shared.store).fail(outage.error().clone()) == Change::Status {
                        shared.publish();
                    }
                }
                continue;
            }
        };
        let change = match item {
            Err(error) => {
                let error = WatchError::from_watcher(&error);
                let now = Instant::now();
                let outage = match &mut outage {
                    Some(outage) => {
                        outage.fail(error);
                        outage
                    }
                    None => outage.insert(Outage::new(now, error)),
                };
                // Relist on a fresh watcher after the backoff.
                events = None;
                retry_at = Some(now + outage.retry_delay(retry));
                let mut store = lock(&shared.store);
                if outage.is_shown() || outage.is_persistent(now) {
                    outage.mark_shown();
                    store.fail(outage.error().clone())
                } else {
                    store.set_reconnecting();
                    Change::None
                }
            }
            Ok(event) => {
                // A completed list or a live event: connected again.
                if matches!(event, Event::InitDone | Event::Apply(_) | Event::Delete(_)) {
                    outage = None;
                }
                lock(&shared.store).apply(Ok(event))
            }
        };
        match change {
            Change::None => {}
            // Loaded or failed: show it right away.
            Change::Status => {
                debounce = None;
                shared.publish();
            }
            Change::Objects => {
                debounce.get_or_insert_with(|| Instant::now() + DEBOUNCE);
            }
        }
    }
    shared.publish();
}

/// Keeps a watch alive. Clones share it; the watch stops when the last
/// clone is dropped.
#[derive(Clone)]
pub struct Subscription {
    watch: Arc<Watch>,
    cache: Weak<CacheInner>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // The last subscription (the map holds weak handles only): keep the
        // watch running for a while.
        if Arc::strong_count(&self.watch) == 1
            && let Some(cache) = self.cache.upgrade()
        {
            cache.linger(Arc::clone(&self.watch));
        }
    }
}

impl Subscription {
    pub fn key(&self) -> &WatchKey {
        &self.watch.key
    }

    /// Current objects and status. While the watch loads, the objects of
    /// its namespace in a loaded watch of all namespaces, if any, as ready.
    pub fn snapshot(&self) -> Snapshot {
        let generation = *self.watch.shared.generation.borrow();
        let mut store = lock(&self.watch.shared.store);
        if store.status() == &WatchStatus::Loading {
            drop(store);
            if let Some(objects) = self.watch.borrowed() {
                return Snapshot {
                    objects,
                    status: WatchStatus::Ready,
                    generation,
                };
            }
            store = lock(&self.watch.shared.store);
        }
        Snapshot {
            objects: store.snapshot(),
            status: store.status().clone(),
            generation,
        }
    }

    /// Whether the watch lost its connection and retries (quietly or with
    /// the error shown): its objects may be out of date (#347).
    pub fn is_reconnecting(&self) -> bool {
        lock(&self.watch.shared.store).is_reconnecting()
    }

    /// Change notifications. The first [`Changes::next`] resolves right away,
    /// so a `while changes.next().await { read snapshot }` loop also renders
    /// the initial state without races.
    pub fn changes(&self) -> Changes {
        let mut receiver = self.watch.shared.generation.subscribe();
        receiver.mark_changed();
        Changes(receiver)
    }
}

/// What a subscriber reads from the cache.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Sorted by namespace, then name.
    pub objects: Objects,
    pub status: WatchStatus,
    /// Increments with every notification.
    pub generation: u64,
}

/// Stream of change notifications for one watch. Works on any executor.
pub struct Changes(watch::Receiver<u64>);

impl Changes {
    /// Waits for the next (coalesced) change. Returns `false` once the watch
    /// has stopped and no more changes will come.
    pub async fn next(&mut self) -> bool {
        self.0.changed().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc;
    use kube::api::GroupVersionKind;
    use kube::discovery::{ApiCapabilities, Scope, verbs};
    use serde_json::json;

    use super::*;

    /// Objects of the initial list come without `kind` (the API server
    /// leaves it out of list items); the cache types them (#372).
    #[test]
    fn listed_objects_get_their_kind() {
        let api = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Pod"));
        let untyped: DynamicObject =
            serde_json::from_value(json!({"metadata": {"name": "web", "namespace": "shop"}}))
                .unwrap();
        let Event::InitApply(object) = typed_event(Event::InitApply(untyped), &api) else {
            panic!("event kind changed");
        };
        let types = object.types.unwrap();
        assert_eq!(
            (types.api_version.as_str(), types.kind.as_str()),
            ("v1", "Pod")
        );
    }

    type Item = Result<Event<DynamicObject>, watcher::Error>;
    type Senders = Arc<Mutex<Vec<(WatchKey, mpsc::UnboundedSender<Item>)>>>;

    fn kind(kind: &str, plural: &str, scope: Scope) -> ResourceKind {
        let gvk = GroupVersionKind::gvk("", "v1", kind);
        ResourceKind::new(
            ApiResource::from_gvk_with_plural(&gvk, plural),
            &ApiCapabilities {
                scope,
                subresources: Vec::new(),
                operations: vec![verbs::LIST.into(), verbs::WATCH.into()],
            },
        )
    }

    fn pod(name: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": "web"}
        }))
        .unwrap()
    }

    /// A cache whose watches are fed by the test through channels and stop
    /// as soon as their last subscription goes.
    fn scripted() -> (ResourceCache, Senders) {
        scripted_with_linger(Duration::ZERO)
    }

    fn scripted_with_linger(linger: Duration) -> (ResourceCache, Senders) {
        let senders = Arc::new(Mutex::new(Vec::new()));
        let source: Source = {
            let senders = Arc::clone(&senders);
            Arc::new(move |_kind: &ResourceKind, key: &WatchKey| {
                let (tx, rx) = mpsc::unbounded();
                lock(&senders).push((key.clone(), tx));
                rx.boxed()
            })
        };
        let cache = ResourceCache::build(source, linger, RETRY);
        (cache, senders)
    }

    /// First retry delay of scripted watches.
    const RETRY: Duration = Duration::from_millis(20);

    fn lost_connection() -> watcher::Error {
        watcher::Error::WatchFailed(kube::Error::Service(
            "error reading a body from connection".into(),
        ))
    }

    /// The sender of the `ix`-th stream opened, once it is.
    async fn sender(senders: &Senders, ix: usize) -> mpsc::UnboundedSender<Item> {
        eventually(|| lock(senders).len() > ix).await;
        lock(senders)[ix].1.clone()
    }

    #[tokio::test]
    async fn a_dropped_connection_is_retried_quietly_with_a_relist() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let web = cache.subscribe(&pods, Some("web"));
        let first = sender(&senders, 0).await;
        load(&first, vec![pod_in("web", "a"), pod_in("web", "b")]);
        eventually(|| web.snapshot().status == WatchStatus::Ready).await;

        first.unbounded_send(Err(lost_connection())).unwrap();
        // The status stays, the old stream is dropped; the status bar says
        // "reconnecting", without a progress bar.
        eventually(|| first.is_closed()).await;
        let snapshot = web.snapshot();
        assert_eq!(snapshot.status, WatchStatus::Ready);
        assert_eq!(snapshot.objects.len(), 2);
        assert_eq!(
            cache.sync_summary(),
            SyncSummary {
                loading: 0,
                reconnecting: 1
            }
        );

        // A fresh watch relists; objects deleted meanwhile go.
        let second = sender(&senders, 1).await;
        load(&second, vec![pod_in("web", "a")]);
        eventually(|| web.snapshot().objects.len() == 1).await;
        assert_eq!(web.snapshot().status, WatchStatus::Ready);
        assert!(!cache.sync_summary().is_syncing());
    }

    #[tokio::test]
    async fn a_lasting_outage_shows_its_error_until_it_recovers() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let web = cache.subscribe(&pods, Some("web"));
        let first = sender(&senders, 0).await;
        load(&first, vec![pod_in("web", "a")]);
        eventually(|| web.snapshot().status == WatchStatus::Ready).await;

        for ix in 0..outage::ERROR_AFTER_FAILURES as usize {
            assert_eq!(web.snapshot().status, WatchStatus::Ready);
            sender(&senders, ix)
                .await
                .unbounded_send(Err(lost_connection()))
                .unwrap();
            eventually(|| lock(&senders)[ix].1.is_closed()).await;
        }
        let snapshot = web.snapshot();
        let WatchStatus::Error(error) = &snapshot.status else {
            panic!("no error: {:?}", snapshot.status);
        };
        assert!(error.connection);
        assert!(!error.forbidden);
        assert!(error.message.contains("error reading a body"));
        // Cached objects stay; still retrying, but not "loading".
        assert_eq!(snapshot.objects.len(), 1);
        assert_eq!(cache.sync_summary().loading, 0);
        assert_eq!(cache.sync_summary().reconnecting, 1);

        let last = sender(&senders, outage::ERROR_AFTER_FAILURES as usize).await;
        load(&last, vec![pod_in("web", "a")]);
        eventually(|| web.snapshot().status == WatchStatus::Ready).await;
        assert!(!cache.sync_summary().is_syncing());
    }

    #[tokio::test]
    async fn forbidden_is_shown_at_once_and_is_not_syncing() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let web = cache.subscribe(&pods, Some("web"));
        let forbidden = watcher::Error::InitialListFailed(kube::Error::Api(Box::new(
            serde_json::from_value(json!({
                "status": "Failure",
                "code": 403,
                "reason": "Forbidden",
                "message": "pods is forbidden"
            }))
            .unwrap(),
        )));
        sender(&senders, 0)
            .await
            .unbounded_send(Err(forbidden))
            .unwrap();
        eventually(|| matches!(web.snapshot().status, WatchStatus::Error(_))).await;
        let WatchStatus::Error(error) = web.snapshot().status else {
            unreachable!();
        };
        assert!(error.forbidden);
        assert!(!error.connection);
        assert!(!cache.sync_summary().is_syncing());
    }

    fn load(tx: &mpsc::UnboundedSender<Item>, objects: Vec<DynamicObject>) {
        tx.unbounded_send(Ok(Event::Init)).unwrap();
        for object in objects {
            tx.unbounded_send(Ok(Event::InitApply(object))).unwrap();
        }
        tx.unbounded_send(Ok(Event::InitDone)).unwrap();
    }

    fn pod_in(namespace: &str, name: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn released_watches_linger_and_are_reused() {
        let (cache, senders) = scripted_with_linger(Duration::from_millis(300));
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let web = cache.subscribe(&pods, Some("web"));
        let tx = lock(&senders)[0].1.clone();
        load(&tx, vec![pod_in("web", "a")]);
        eventually(|| web.snapshot().status == WatchStatus::Ready).await;

        // Switching to another namespace and back: no new list.
        drop(web);
        assert_eq!(cache.active_watches(), 1);
        assert!(!cache.sync_summary().is_syncing());
        let _api = cache.subscribe(&pods, Some("api"));
        let again = cache.subscribe(&pods, Some("web"));
        assert_eq!(lock(&senders).len(), 2);
        let snapshot = again.snapshot();
        assert_eq!(snapshot.status, WatchStatus::Ready);
        assert_eq!(snapshot.objects.len(), 1);

        // Released for good: stops once the time is up.
        drop(again);
        assert!(!tx.is_closed());
        eventually(|| tx.is_closed()).await;
        assert_eq!(cache.active_watches(), 1);
    }

    #[tokio::test]
    async fn only_a_few_watches_linger() {
        let (cache, senders) = scripted_with_linger(Duration::from_secs(60));
        let pods = kind("Pod", "pods", Scope::Namespaced);
        for ix in 0..MAX_LINGERING + 2 {
            drop(cache.subscribe(&pods, Some(&format!("ns{ix}"))));
        }
        assert_eq!(cache.active_watches(), MAX_LINGERING);
        // The first released went first.
        let first = lock(&senders)[0].1.clone();
        eventually(|| first.is_closed()).await;
        let last = lock(&senders).last().unwrap().1.clone();
        assert!(!last.is_closed());
    }

    #[tokio::test]
    async fn a_namespace_borrows_from_the_all_namespaces_watch_while_loading() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let all = cache.subscribe(&pods, None);
        let all_tx = lock(&senders)[0].1.clone();
        load(
            &all_tx,
            vec![pod_in("web", "a"), pod_in("api", "b"), pod_in("web", "c")],
        );
        eventually(|| all.snapshot().status == WatchStatus::Ready).await;

        let web = cache.subscribe(&pods, Some("web"));
        let snapshot = web.snapshot();
        assert_eq!(snapshot.status, WatchStatus::Ready);
        let names: Vec<_> = snapshot
            .objects
            .iter()
            .map(|o| o.metadata.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["a", "c"]);

        // Its own list takes over once loaded.
        let web_tx = lock(&senders)[1].1.clone();
        load(&web_tx, vec![pod_in("web", "a")]);
        eventually(|| web.snapshot().objects.len() == 1).await;

        // Without a loaded all-namespaces watch it just loads.
        drop(all);
        let api = cache.subscribe(&pods, Some("api"));
        assert_eq!(api.snapshot().status, WatchStatus::Loading);
    }

    async fn eventually(mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not reached");
    }

    #[tokio::test]
    async fn subscriptions_share_a_watch_until_the_last_is_dropped() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);

        let first = cache.subscribe(&pods, Some("web"));
        let second = cache.subscribe(&pods, Some("web"));
        assert_eq!(cache.active_watches(), 1);
        assert_eq!(lock(&senders).len(), 1);

        let other = cache.subscribe(&pods, None);
        assert_eq!(cache.active_watches(), 2);
        assert_eq!(other.key().namespace, None);

        drop(first);
        assert_eq!(cache.active_watches(), 2);
        let tx = lock(&senders)[0].1.clone();
        assert!(!tx.is_closed());

        drop(second);
        assert_eq!(cache.active_watches(), 1);
        // The aborted task drops the stream, closing the connection.
        eventually(|| tx.is_closed()).await;

        // Subscribing again starts a fresh watch.
        let _again = cache.subscribe(&pods, Some("web"));
        assert_eq!(lock(&senders).len(), 3);
        drop(other);
        assert_eq!(cache.active_watches(), 1);
    }

    #[tokio::test]
    async fn filtered_watches_are_separate() {
        let (cache, senders) = scripted();
        let secrets = kind("Secret", "secrets", Scope::Namespaced);
        let filter = WatchFilter {
            labels: Some("owner=helm".into()),
            fields: None,
            metadata_only: true,
        };
        let plain = cache.subscribe(&secrets, Some("web"));
        let helm = cache.subscribe_filtered(&secrets, Some("web"), filter.clone());
        let again = cache.subscribe_filtered(&secrets, Some("web"), filter.clone());
        assert_eq!(cache.active_watches(), 2);
        assert_eq!(helm.key(), again.key());
        assert_ne!(plain.key(), helm.key());
        let keys: Vec<_> = lock(&senders).iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(keys[1].filter, filter);
        assert_eq!(keys[0].filter, WatchFilter::default());
    }

    #[test]
    fn metadata_objects_have_an_empty_body() {
        let meta: PartialObjectMeta<DynamicObject> = serde_json::from_value(json!({
            "apiVersion": "meta.k8s.io/v1",
            "kind": "PartialObjectMetadata",
            "metadata": {"name": "a", "namespace": "web", "labels": {"owner": "helm"}}
        }))
        .unwrap();
        let event = map_event(Event::Apply(meta), meta_object);
        let Event::Apply(object) = event else {
            panic!("not an apply");
        };
        assert_eq!(object.metadata.name.as_deref(), Some("a"));
        assert_eq!(object.data, json!({}));
    }

    #[tokio::test]
    async fn cluster_scoped_kinds_ignore_the_namespace() {
        let (cache, _senders) = scripted();
        let nodes = kind("Node", "nodes", Scope::Cluster);
        let a = cache.subscribe(&nodes, Some("web"));
        let _b = cache.subscribe(&nodes, None);
        assert_eq!(a.key().namespace, None);
        assert_eq!(cache.active_watches(), 1);
    }

    #[cfg(debug_assertions)]
    #[tokio::test]
    async fn counts_objects_of_running_watches() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let web = cache.subscribe(&pods, Some("web"));
        let all = cache.subscribe(&pods, None);
        assert_eq!(cache.cached_objects(), 0);
        for (ix, (_, tx)) in lock(&senders).iter().enumerate() {
            tx.unbounded_send(Ok(Event::Init)).unwrap();
            for name in ["a", "b", "c"].iter().take(ix + 1) {
                tx.unbounded_send(Ok(Event::InitApply(pod(name)))).unwrap();
            }
            tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        }
        eventually(|| cache.cached_objects() == 3).await;
        // A stopped watch no longer counts.
        drop(all);
        assert_eq!(cache.cached_objects(), 1);
        drop(web);
        assert_eq!(cache.cached_objects(), 0);
    }

    #[tokio::test]
    async fn notifies_once_per_burst() {
        let (cache, senders) = scripted();
        let subscription = cache.subscribe(&kind("Pod", "pods", Scope::Namespaced), None);
        let mut changes = subscription.changes();
        // The first notification is immediate and shows the loading state.
        assert!(changes.next().await);
        assert_eq!(subscription.snapshot().status, WatchStatus::Loading);

        let tx = lock(&senders)[0].1.clone();
        tx.unbounded_send(Ok(Event::Init)).unwrap();
        tx.unbounded_send(Ok(Event::InitApply(pod("a")))).unwrap();
        tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        assert!(changes.next().await);
        let snapshot = subscription.snapshot();
        assert_eq!(snapshot.status, WatchStatus::Ready);
        assert_eq!(snapshot.objects.len(), 1);

        let before = snapshot.generation;
        for ix in 0..50 {
            tx.unbounded_send(Ok(Event::Apply(pod(&format!("p{ix}")))))
                .unwrap();
        }
        assert!(changes.next().await);
        tokio::time::sleep(DEBOUNCE * 3).await;
        let snapshot = subscription.snapshot();
        assert_eq!(snapshot.objects.len(), 51);
        assert_eq!(snapshot.generation, before + 1);
    }

    #[tokio::test]
    async fn sets_merge_watches_and_count_loaded_ones() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let set = SubscriptionSet::new(vec![
            cache.subscribe(&pods, Some("web")),
            cache.subscribe(&pods, Some("api")),
        ]);
        let mut changes = set.changes();
        assert!(changes.next().await);
        assert_eq!(set.snapshot().status, WatchStatus::Loading);
        assert_eq!(cache.loaded_count(&pods, Some("web")), None);

        let tx = lock(&senders)[0].1.clone();
        tx.unbounded_send(Ok(Event::Init)).unwrap();
        tx.unbounded_send(Ok(Event::InitApply(pod("a")))).unwrap();
        tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        eventually(|| cache.loaded_count(&pods, Some("web")) == Some(1)).await;
        assert!(changes.next().await);
        assert_eq!(set.snapshot().objects.len(), 1);
        // Nothing is started just to count.
        assert_eq!(cache.loaded_count(&pods, None), None);
        assert_eq!(cache.active_watches(), 2);

        // A watch of all namespaces counts the objects of one.
        assert_eq!(cache.loaded_count_in(&pods, "web"), Some(1));
        assert_eq!(cache.loaded_count_in(&pods, "db"), None);
        let all = cache.subscribe(&pods, None);
        let tx = lock(&senders)[2].1.clone();
        tx.unbounded_send(Ok(Event::Init)).unwrap();
        tx.unbounded_send(Ok(Event::InitApply(pod("a")))).unwrap();
        tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        eventually(|| cache.loaded_count_in(&pods, "db") == Some(0)).await;
        assert_eq!(cache.loaded_count(&pods, None), Some(1));
        drop(all);
    }

    #[tokio::test]
    async fn subscribe_many_watches_each_namespace_or_all() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        let filter = WatchFilter {
            labels: Some("app=web".into()),
            ..WatchFilter::default()
        };
        let namespaces = ["web".to_owned(), "api".to_owned()];
        let set = cache.subscribe_many(&pods, Some(&namespaces), filter.clone());
        let keys: Vec<_> = set
            .subscriptions()
            .iter()
            .map(|part| part.key().clone())
            .collect();
        assert_eq!(
            keys.iter()
                .map(|key| key.namespace.as_deref())
                .collect::<Vec<_>>(),
            [Some("web"), Some("api")]
        );
        assert!(keys.iter().all(|key| key.filter == filter));
        // The same namespaces again share the watches.
        let again = cache.subscribe_many(&pods, Some(&namespaces[..1]), filter.clone());
        assert_eq!(again.subscriptions()[0].key(), &keys[0]);
        assert_eq!(cache.active_watches(), 2);
        assert_eq!(lock(&senders).len(), 2);

        // `None`: one watch over all namespaces.
        let all = cache.subscribe_many(&pods, None, WatchFilter::default());
        assert_eq!(all.subscriptions().len(), 1);
        assert_eq!(all.subscriptions()[0].key().namespace, None);

        // Cluster-scoped kinds get one watch whatever the namespaces.
        let nodes = kind("Node", "nodes", Scope::Cluster);
        let set_nodes = cache.subscribe_many(&nodes, Some(&namespaces), WatchFilter::default());
        assert_eq!(set_nodes.subscriptions().len(), 1);
        assert_eq!(set_nodes.subscriptions()[0].key().namespace, None);

        // An empty selection watches nothing.
        let none = cache.subscribe_many(&pods, Some(&[]), WatchFilter::default());
        assert!(none.subscriptions().is_empty());

        drop((set, again));
        assert_eq!(cache.active_watches(), 2);
    }

    #[tokio::test]
    async fn sync_summary_follows_loading_and_reconnects() {
        let (cache, senders) = scripted();
        let pods = kind("Pod", "pods", Scope::Namespaced);
        assert!(!cache.sync_summary().is_syncing());

        let web = cache.subscribe(&pods, Some("web"));
        let _api = cache.subscribe(&pods, Some("api"));
        assert_eq!(
            cache.sync_summary(),
            SyncSummary {
                loading: 2,
                reconnecting: 0
            }
        );

        let (web_tx, api_tx) = {
            let senders = lock(&senders);
            (senders[0].1.clone(), senders[1].1.clone())
        };
        for tx in [&web_tx, &api_tx] {
            tx.unbounded_send(Ok(Event::Init)).unwrap();
            tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        }
        eventually(|| !cache.sync_summary().is_syncing()).await;

        // A dropped connection shows as reconnecting until the relist on
        // a fresh watch.
        web_tx
            .unbounded_send(Err(watcher::Error::NoResourceVersion))
            .unwrap();
        eventually(|| cache.sync_summary().reconnecting == 1).await;
        let retry_tx = sender(&senders, 2).await;
        retry_tx.unbounded_send(Ok(Event::Init)).unwrap();
        retry_tx.unbounded_send(Ok(Event::InitDone)).unwrap();
        eventually(|| !cache.sync_summary().is_syncing()).await;

        // Dropped watches no longer count.
        let nodes = cache.subscribe(&kind("Node", "nodes", Scope::Cluster), None);
        assert_eq!(cache.sync_summary().loading, 1);
        drop(web);
        drop(nodes);
        assert!(!cache.sync_summary().is_syncing());
    }

    #[tokio::test]
    async fn changes_end_when_the_watch_stops() {
        let (cache, _senders) = scripted();
        let subscription = cache.subscribe(&kind("Pod", "pods", Scope::Namespaced), None);
        let mut changes = subscription.changes();
        assert!(changes.next().await);
        drop(subscription);
        assert!(!changes.next().await);
    }
}
