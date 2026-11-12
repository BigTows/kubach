//! Watch-event application, independent of any I/O.

use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::DynamicObject;
use kube::runtime::watcher::{self, Event};

use crate::secrets::mask::{LAST_APPLIED, redact};

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

/// Drops what no reader of the cache uses and what can be a large part of an
/// object: `managedFields` and the last-applied annotation (a full copy of
/// the manifest). The editor and "apply" read the object from the cluster,
/// not from here.
fn trim(object: &mut DynamicObject) {
    object.metadata.managed_fields = None;
    if let Some(annotations) = &mut object.metadata.annotations
        && annotations.remove(LAST_APPLIED).is_some()
        && annotations.is_empty()
    {
        object.metadata.annotations = None;
    }
}

/// Whether `new` is the very state of the object that `old` was built from:
/// the same uid and resource version. Compared on what the server sent, so
/// it holds for objects that were trimmed or redacted on the way in.
fn same_version(old: &DynamicObject, new: &DynamicObject) -> bool {
    let known = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
    known(&new.metadata.uid)
        && known(&new.metadata.resource_version)
        && old.metadata.uid == new.metadata.uid
        && old.metadata.resource_version == new.metadata.resource_version
}

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
/// the watch was down. An object the relist returns unchanged (same uid and
/// resource version) keeps its `Arc`, so the relist does not hold a second
/// copy of it and readers can tell by pointer that it did not change.
///
/// Objects are stored without `managedFields` and the last-applied
/// annotation (see [`trim`]).
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

    /// The object as it is stored: trimmed and, for Secrets, redacted, in
    /// place.
    fn prepare(&self, mut object: DynamicObject) -> Arc<DynamicObject> {
        if self.redact_secrets {
            redact(&mut object);
        }
        trim(&mut object);
        Arc::new(object)
    }

    /// The stored object for `object` of a relist: the one already in the
    /// store if the server returned the same version of it.
    fn relisted(&self, key: &ObjectKey, object: DynamicObject) -> Arc<DynamicObject> {
        match self.objects.get(key) {
            Some(old) if same_version(old, &object) => old.clone(),
            _ => self.prepare(object),
        }
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
                let key = key(&object);
                let object = self.relisted(&key, object);
                self.relisting.get_or_insert_default().insert(key, object);
                return Change::None;
            }
            Event::InitDone => {
                self.objects = self.relisting.take().unwrap_or_default();
                Change::Objects
            }
            Event::Apply(object) => {
                self.objects.insert(key(&object), self.prepare(object));
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

    /// A Deployment as the server sends it, at `version`.
    fn deployment(name: &str, version: &str, replicas: u32) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": name,
                "namespace": "web",
                "uid": format!("uid-{name}"),
                "resourceVersion": version,
                "annotations": {
                    LAST_APPLIED: "{\"spec\":{\"replicas\":1}}",
                    "team": "core"
                },
                "managedFields": [{
                    "manager": "kubectl",
                    "operation": "Apply",
                    "fieldsType": "FieldsV1",
                    "fieldsV1": {"f:spec": {"f:replicas": {}}}
                }]
            },
            "spec": {"replicas": replicas}
        }))
        .unwrap()
    }

    fn secret(version: &str, password: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": "db",
                "namespace": "web",
                "uid": "uid-db",
                "resourceVersion": version,
                "managedFields": [{"manager": "kubectl", "operation": "Update"}]
            },
            "type": "Opaque",
            "data": {"password": password}
        }))
        .unwrap()
    }

    #[test]
    fn managed_fields_and_last_applied_are_dropped() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![deployment("a", "1", 1)]);
        store.apply(Ok(Event::Apply(deployment("b", "2", 1))));

        // Both a listed and a watched object.
        let snapshot = store.snapshot();
        assert_eq!(snapshot.len(), 2);
        for stored in snapshot.iter() {
            assert!(stored.metadata.managed_fields.is_none());
            let annotations = stored.metadata.annotations.as_ref().unwrap();
            assert_eq!(annotations.len(), 1);
            assert_eq!(annotations["team"], "core");
            let text = serde_json::to_string(stored.as_ref()).unwrap();
            assert!(!text.contains("managedFields"), "{text}");
            assert!(!text.contains(LAST_APPLIED), "{text}");
            // The rest is as the server sent it.
            assert_eq!(stored.data["spec"]["replicas"], 1);
            assert!(stored.metadata.uid.is_some());
            assert!(stored.metadata.resource_version.is_some());
        }
    }

    #[test]
    fn last_applied_as_the_only_annotation_leaves_none() {
        let mut object = deployment("a", "1", 1);
        object.metadata.annotations.as_mut().unwrap().remove("team");
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![object, pod("web", "p", "Running")]);
        assert!(store.snapshot()[0].metadata.annotations.is_none());

        // Annotations without last-applied are not touched, even empty ones.
        let mut empty = pod("web", "q", "Running");
        empty.metadata.annotations = Some(Default::default());
        store.apply(Ok(Event::Apply(empty)));
        assert_eq!(
            store.snapshot()[2].metadata.annotations,
            Some(Default::default())
        );
    }

    #[test]
    fn relist_keeps_unchanged_objects_and_replaces_changed_ones() {
        let mut store = ObjectStore::new(false);
        init(
            &mut store,
            vec![
                deployment("changed", "1", 1),
                deployment("deleted", "2", 1),
                deployment("same", "3", 1),
            ],
        );
        let before = store.snapshot();

        // Reconnect: one object changed, one was deleted, one was created.
        store.apply(Ok(Event::Init));
        for object in [
            deployment("changed", "7", 5),
            deployment("created", "8", 1),
            deployment("same", "3", 1),
        ] {
            assert_eq!(store.apply(Ok(Event::InitApply(object))), Change::None);
        }
        // The deleted object stays until the list completes.
        assert!(Arc::ptr_eq(&before, &store.snapshot()));
        assert_eq!(store.apply(Ok(Event::InitDone)), Change::Objects);

        let after = store.snapshot();
        assert_eq!(
            names(&mut store),
            ["web/changed", "web/created", "web/same"]
        );
        assert!(Arc::ptr_eq(&before[2], &after[2]));
        assert!(!Arc::ptr_eq(&before[0], &after[0]));
        assert_eq!(after[0].data["spec"]["replicas"], 5);
        assert_eq!(after[0].metadata.resource_version.as_deref(), Some("7"));
        assert!(after[0].metadata.managed_fields.is_none());
        assert!(after[1].metadata.managed_fields.is_none());
    }

    #[test]
    fn relist_replaces_a_recreated_object_with_the_same_version() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![deployment("a", "1", 1)]);
        let before = store.snapshot();

        let mut recreated = deployment("a", "1", 2);
        recreated.metadata.uid = Some("uid-other".into());
        init(&mut store, vec![recreated]);
        let after = store.snapshot();
        assert!(!Arc::ptr_eq(&before[0], &after[0]));
        assert_eq!(after[0].data["spec"]["replicas"], 2);
    }

    #[test]
    fn relist_does_not_reuse_objects_without_uid_or_version() {
        let mut store = ObjectStore::new(false);
        init(&mut store, vec![pod("web", "a", "Pending")]);
        let before = store.snapshot();
        init(&mut store, vec![pod("web", "a", "Running")]);
        let after = store.snapshot();
        assert!(!Arc::ptr_eq(&before[0], &after[0]));
        assert_eq!(after[0].data["status"]["phase"], "Running");
    }

    #[test]
    fn relist_keeps_an_unchanged_redacted_secret() {
        let mut store = ObjectStore::new(true);
        init(&mut store, vec![secret("1", "aHVudGVyMg==")]);
        let first = store.snapshot();
        assert_eq!(first[0].data["data"]["password"], Value::Null);

        init(&mut store, vec![secret("1", "aHVudGVyMg==")]);
        let second = store.snapshot();
        assert!(Arc::ptr_eq(&first[0], &second[0]));

        init(&mut store, vec![secret("2", "b3RoZXI=")]);
        let third = store.snapshot();
        assert!(!Arc::ptr_eq(&second[0], &third[0]));
        assert_eq!(third[0].data["data"]["password"], Value::Null);
        assert_eq!(third[0].metadata.resource_version.as_deref(), Some("2"));
        assert!(third[0].metadata.managed_fields.is_none());
    }
}
