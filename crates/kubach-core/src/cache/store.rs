//! Watch-event application, independent of any I/O.

use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::DynamicObject;
use kube::runtime::watcher::{self, Event};

use crate::secrets::mask::redact;

/// Objects of one watch, sorted by namespace and name.
pub type Objects = Arc<[Arc<DynamicObject>]>;

/// State of a watch as shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchStatus {
    /// The initial list has not completed yet.
    Loading,
    /// The store mirrors the cluster.
    Ready,
    /// The last list or watch attempt failed; the watcher retries with
    /// backoff and objects from before the error (if any) are kept.
    Error(WatchError),
}

/// A failed list or watch, in a form the UI can explain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchError {
    /// The user may not list or watch this kind here (HTTP 403).
    pub forbidden: bool,
    /// The connection to the API server failed or dropped (no HTTP answer),
    /// as opposed to an error the API server answered with.
    pub connection: bool,
    /// The whole error chain, for details (not for a banner).
    pub message: String,
}

impl WatchError {
    pub fn from_watcher(error: &watcher::Error) -> Self {
        let status = match error {
            watcher::Error::InitialListFailed(kube::Error::Api(status))
            | watcher::Error::WatchStartFailed(kube::Error::Api(status))
            | watcher::Error::WatchFailed(kube::Error::Api(status))
            | watcher::Error::WatchError(status) => Some(status),
            _ => None,
        };
        match status {
            Some(status) if !status.message.is_empty() => Self {
                forbidden: status.is_forbidden(),
                connection: false,
                message: status.message.clone(),
            },
            Some(status) => Self {
                forbidden: status.is_forbidden(),
                connection: false,
                message: error.to_string(),
            },
            None => Self {
                forbidden: false,
                connection: !matches!(error, watcher::Error::NoResourceVersion),
                message: error.to_string(),
            },
        }
    }
}

/// What applying an event changed, so the caller can decide how soon to
/// notify subscribers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing visible (e.g. objects buffered during a relist).
    None,
    /// Objects were added, changed or removed.
    Objects,
    /// The watch status changed (loaded, failed, recovered).
    Status,
}

type ObjectKey = (String, String);

fn key(object: &DynamicObject) -> ObjectKey {
    (
        object.metadata.namespace.clone().unwrap_or_default(),
        object.metadata.name.clone().unwrap_or_default(),
    )
}

/// In-memory mirror of one watch (kind + namespace or all namespaces).
///
/// Follows the reflector contract: objects listed after `Init` are buffered
/// and replace the store atomically on `InitDone`, so a relist after a
/// reconnect never shows a half-empty list and drops objects deleted while
/// the watch was down.
#[derive(Debug)]
pub struct ObjectStore {
    objects: BTreeMap<ObjectKey, Arc<DynamicObject>>,
    relisting: Option<BTreeMap<ObjectKey, Arc<DynamicObject>>>,
    status: WatchStatus,
    /// The watch failed and retries quietly (see [`super::outage`]): the
    /// status still shows what it was before.
    reconnecting: bool,
    redact_secrets: bool,
    snapshot: Option<Objects>,
}

impl ObjectStore {
    /// `redact_secrets` strips values from every object (see
    /// [`redact`]); set it for the core `Secret` kind.
    pub fn new(redact_secrets: bool) -> Self {
        Self {
            objects: BTreeMap::new(),
            relisting: None,
            status: WatchStatus::Loading,
            reconnecting: false,
            redact_secrets,
            snapshot: None,
        }
    }

    pub fn status(&self) -> &WatchStatus {
        &self.status
    }

    /// Whether the watch lost its connection and retries quietly, or shows
    /// an error other than "forbidden".
    pub fn is_reconnecting(&self) -> bool {
        self.reconnecting || matches!(&self.status, WatchStatus::Error(error) if !error.forbidden)
    }

    /// The watch failed; it retries without showing the error yet.
    pub fn set_reconnecting(&mut self) {
        self.reconnecting = true;
    }

    /// Shows `error` as the status (objects from before it are kept).
    pub fn fail(&mut self, error: WatchError) -> Change {
        self.reconnecting = false;
        if self.set_status(WatchStatus::Error(error)) {
            Change::Status
        } else {
            Change::None
        }
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Sorted objects. Cheap: the slice is cached until the next change.
    pub fn snapshot(&mut self) -> Objects {
        self.snapshot
            .get_or_insert_with(|| self.objects.values().cloned().collect())
            .clone()
    }

    fn prepare(&self, mut object: DynamicObject) -> (ObjectKey, Arc<DynamicObject>) {
        if self.redact_secrets {
            redact(&mut object);
        }
        (key(&object), Arc::new(object))
    }

    fn set_status(&mut self, status: WatchStatus) -> bool {
        let changed = self.status != status;
        self.status = status;
        changed
    }

    /// Applies one item of a watcher stream.
    pub fn apply(&mut self, item: Result<Event<DynamicObject>, watcher::Error>) -> Change {
        let event = match item {
            Ok(event) => event,
            Err(error) => return self.fail(WatchError::from_watcher(&error)),
        };
        let change = match event {
            Event::Init => {
                self.relisting = Some(BTreeMap::new());
                return Change::None;
            }
            Event::InitApply(object) => {
                let (key, object) = self.prepare(object);
                self.relisting.get_or_insert_default().insert(key, object);
                return Change::None;
            }
            Event::InitDone => {
                self.objects = self.relisting.take().unwrap_or_default();
                Change::Objects
            }
            Event::Apply(object) => {
                let (key, object) = self.prepare(object);
                self.objects.insert(key, object);
                Change::Objects
            }
            Event::Delete(object) => {
                self.objects.remove(&key(&object));
                Change::Objects
            }
        };
        self.snapshot = None;
        self.reconnecting = false;
        // A completed list or a live event means the watch is healthy (again).
        if self.set_status(WatchStatus::Ready) {
            return Change::Status;
        }
        change
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::secrets::mask::LAST_APPLIED;

    fn pod(namespace: &str, name: &str, phase: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace},
            "status": {"phase": phase}
        }))
        .unwrap()
    }

    fn names(store: &mut ObjectStore) -> Vec<String> {
        store
            .snapshot()
            .iter()
            .map(|o| {
                format!(
                    "{}/{}",
                    o.metadata.namespace.as_deref().unwrap_or(""),
                    o.metadata.name.as_deref().unwrap_or("")
                )
            })
            .collect()
    }

    fn init(store: &mut ObjectStore, objects: Vec<DynamicObject>) -> Change {
        store.apply(Ok(Event::Init));
        for object in objects {
            assert_eq!(store.apply(Ok(Event::InitApply(object))), Change::None);
        }
        store.apply(Ok(Event::InitDone))
    }

    fn forbidden() -> watcher::Error {
        watcher::Error::InitialListFailed(kube::Error::Api(Box::new(
            serde_json::from_value(json!({
                "status": "Failure",
                "code": 403,
                "reason": "Forbidden",
                "message": "pods is forbidden: User \"bob\" cannot list resource \"pods\""
            }))
            .unwrap(),
        )))
    }

    #[test]
    fn initial_list_is_applied_atomically_and_sorted() {
        let mut store = ObjectStore::new(false);
        assert_eq!(store.status(), &WatchStatus::Loading);
        store.apply(Ok(Event::Init));
        store.apply(Ok(Event::InitApply(pod("web", "b", "Running"))));
        // Nothing is visible until the list completes.
        assert!(store.snapshot().is_empty());
        store.apply(Ok(Event::InitApply(pod("api", "z", "Running"))));
        store.apply(Ok(Event::InitApply(pod("web", "a", "Running"))));
        assert_eq!(store.apply(Ok(Event::InitDone)), Change::Status);
        assert_eq!(store.status(), &WatchStatus::Ready);
        assert_eq!(names(&mut store), ["api/z", "web/a", "web/b"]);
    }

    #[test]
    fn apply_and_delete_update_the_store() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![pod("web", "a", "Pending")]);

        assert_eq!(
            store.apply(Ok(Event::Apply(pod("web", "a", "Running")))),
            Change::Objects
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.snapshot()[0].data["status"]["phase"], "Running");

        store.apply(Ok(Event::Apply(pod("web", "b", "Running"))));
        assert_eq!(names(&mut store), ["web/a", "web/b"]);

        assert_eq!(
            store.apply(Ok(Event::Delete(pod("web", "a", "Running")))),
            Change::Objects
        );
        assert_eq!(names(&mut store), ["web/b"]);
    }

    #[test]
    fn relist_replaces_objects_missed_while_disconnected() {
        let mut store = ObjectStore::new(false);
        init(
            &mut store,
            vec![pod("web", "a", "Running"), pod("web", "b", "Running")],
        );

        // Reconnect: `a` was deleted and `c` created while the watch was down.
        store.apply(Ok(Event::Init));
        store.apply(Ok(Event::InitApply(pod("web", "b", "Running"))));
        // The old list stays visible during the relist.
        assert_eq!(names(&mut store), ["web/a", "web/b"]);
        store.apply(Ok(Event::InitApply(pod("web", "c", "Running"))));
        assert_eq!(store.apply(Ok(Event::InitDone)), Change::Objects);
        assert_eq!(names(&mut store), ["web/b", "web/c"]);
    }

    #[test]
    fn snapshot_is_shared_until_a_change() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![pod("web", "a", "Running")]);
        let first = store.snapshot();
        assert!(Arc::ptr_eq(&first, &store.snapshot()));
        store.apply(Ok(Event::Apply(pod("web", "b", "Running"))));
        let second = store.snapshot();
        assert!(!Arc::ptr_eq(&first, &second));
        // Unchanged objects are shared, not copied.
        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn errors_are_reported_and_cleared_on_recovery() {
        let mut store = ObjectStore::new(false);
        assert_eq!(store.apply(Err(forbidden())), Change::Status);
        let WatchStatus::Error(error) = store.status() else {
            panic!("expected an error, got {:?}", store.status());
        };
        assert!(error.forbidden);
        assert!(error.message.starts_with("pods is forbidden"));
        // Repeated failures (backoff retries) do not re-notify.
        assert_eq!(store.apply(Err(forbidden())), Change::None);

        assert_eq!(
            init(&mut store, vec![pod("web", "a", "Running")]),
            Change::Status
        );
        assert_eq!(store.status(), &WatchStatus::Ready);
    }

    #[test]
    fn errors_keep_previous_objects() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![pod("web", "a", "Running")]);
        store.apply(Err(watcher::Error::NoResourceVersion));
        assert!(matches!(store.status(), WatchStatus::Error(e) if !e.forbidden));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn secrets_are_redacted() {
        let secret: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": "db",
                "namespace": "web",
                "annotations": {
                    LAST_APPLIED: "{\"data\":{\"password\":\"aHVudGVyMg==\"}}",
                    "team": "core"
                }
            },
            "type": "Opaque",
            "data": {"password": "aHVudGVyMg==", "user": "YWRtaW4="},
            "stringData": {"token": "plain"}
        }))
        .unwrap();

        let mut store = ObjectStore::new(true);
        init(&mut store, vec![secret.clone()]);
        store.apply(Ok(Event::Apply(secret)));

        let stored = &store.snapshot()[0];
        let text = serde_json::to_string(stored.as_ref()).unwrap();
        assert!(!text.contains("aHVudGVyMg=="), "{text}");
        assert!(!text.contains("YWRtaW4="), "{text}");
        assert!(!text.contains("plain"), "{text}");
        assert_eq!(stored.data["data"]["password"], Value::Null);
        assert_eq!(stored.data["type"], "Opaque");
        let annotations = stored.metadata.annotations.as_ref().unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations["team"], "core");
    }
}
