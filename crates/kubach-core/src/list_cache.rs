//! The last list of each kind on disk, shown right away the next time the
//! kind is opened while the watch loads (stale-while-revalidate, #300,
//! [ADR 0012](../../../docs/adr/0012-list-cache.md)).
//!
//! - **Keyed** by context, `(group, kind)` and namespace (or all of them),
//!   like the watches: a selection of several namespaces is put together
//!   from one entry per namespace, so switching namespaces finds them too.
//!   A list is never shown for another cluster or other namespaces.
//! - **Never sensitive**: Secrets are not stored at all (not even
//!   redacted), and every object loses `managedFields` and the
//!   last-applied annotation, which can hold a full copy of the object.
//! - **Bounded**: lists over [`Limits::max_objects`] or
//!   [`Limits::max_entry_bytes`] (compressed) are not stored, and the oldest
//!   entries go once there are more than [`Limits::max_entries`] or they take
//!   more than [`Limits::max_total_bytes`].
//! - **Disposable**: lives in the [disk cache](crate::disk_cache) ("Clear
//!   cache" removes it); a file of another format version, a damaged one or
//!   one for another key is ignored and removed.
//! - **Off the UI thread**: [`ListWriter`] stores in the background, at most
//!   once per [`WRITE_DELAY`] for a key, the latest list winning.

use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::cache::Objects;
use crate::discovery::ResourceKind;
use crate::runtime::{self, BackgroundTask};
use crate::secrets::mask::{LAST_APPLIED, is_core_secret, is_secret_value};

/// Version of the file format; files of any other version are ignored.
pub const FORMAT_VERSION: u32 = 1;

/// Folder of the list cache inside the disk cache.
const FOLDER: &str = "lists";
const EXTENSION: &str = "json.gz";

/// How long [`ListWriter`] waits before storing a list, so a busy watch
/// (pods during a rollout) is written once per period, not per event.
pub const WRITE_DELAY: Duration = Duration::from_secs(3);

/// Size limits of the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Lists with more objects are not stored.
    pub max_objects: usize,
    /// Compressed size of one entry above which it is not stored.
    pub max_entry_bytes: u64,
    /// Entries kept, most recently used first.
    pub max_entries: usize,
    /// Compressed size of all entries.
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_objects: 10_000,
            max_entry_bytes: 8 * 1024 * 1024,
            max_entries: 200,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Identity of a cached list.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ListKey {
    pub context: String,
    pub group: String,
    pub kind: String,
    /// `None` for all namespaces and for cluster-scoped kinds.
    pub namespace: Option<String>,
}

impl ListKey {
    /// Key of `(group, kind)` in `context` in `namespace` (`None`: all
    /// namespaces, or a cluster-scoped kind).
    pub fn new(context: &str, group: &str, kind: &str, namespace: Option<&str>) -> Self {
        Self {
            context: context.to_owned(),
            group: group.to_owned(),
            kind: kind.to_owned(),
            namespace: namespace.map(str::to_owned),
        }
    }

    /// Keys of a selection: one per namespace, or one for all of them.
    pub fn for_selection(
        context: &str,
        group: &str,
        kind: &str,
        namespaces: Option<&[String]>,
    ) -> Vec<Self> {
        match namespaces {
            Some(names) => {
                let mut names = names.to_vec();
                names.sort();
                names.dedup();
                names
                    .iter()
                    .map(|name| Self::new(context, group, kind, Some(name)))
                    .collect()
            }
            None => vec![Self::new(context, group, kind, None)],
        }
    }

    /// The same list for every namespace, where cluster-scoped kinds live.
    pub fn all_namespaces(&self) -> Self {
        Self {
            namespace: None,
            ..self.clone()
        }
    }

    /// Whether lists of this key may be stored: never Secrets.
    pub fn cacheable(&self) -> bool {
        !is_core_secret(&self.group, &self.kind)
    }

    /// File name: a hash, so context and namespace names need no escaping.
    fn file_name(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let digest = Sha256::digest(json);
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}.{EXTENSION}")
    }
}

/// What the cache remembers of a kind, enough to show its list before API
/// discovery has run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedKind {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: String,
    pub namespaced: bool,
}

impl CachedKind {
    pub fn of(kind: &ResourceKind) -> Self {
        Self {
            group: kind.api.group.clone(),
            version: kind.api.version.clone(),
            kind: kind.api.kind.clone(),
            plural: kind.api.plural.clone(),
            namespaced: kind.namespaced,
        }
    }

    /// The kind without verbs: nothing can be done to cached objects until
    /// discovery says what the cluster allows.
    pub fn resource_kind(&self) -> ResourceKind {
        let gvk = GroupVersionKind::gvk(&self.group, &self.version, &self.kind);
        ResourceKind {
            api: ApiResource::from_gvk_with_plural(&gvk, &self.plural),
            namespaced: self.namespaced,
            verbs: Vec::new(),
        }
    }
}

/// A list read back from the cache.
#[derive(Clone, Debug)]
pub struct CachedList {
    pub kind: CachedKind,
    /// When it was stored, Unix seconds.
    pub saved_at: i64,
    /// In the order stored (the cache's: namespace, then name).
    pub objects: Objects,
}

#[derive(Serialize, Deserialize)]
struct File<O> {
    version: u32,
    key: ListKey,
    kind: CachedKind,
    saved_at: i64,
    objects: O,
}

/// A copy of `object` without what the list doesn't need and what must not
/// reach the disk: `managedFields` and the last-applied annotation.
pub fn slim(object: &DynamicObject) -> DynamicObject {
    let mut object = object.clone();
    object.metadata.managed_fields = None;
    if let Some(annotations) = &mut object.metadata.annotations {
        annotations.remove(LAST_APPLIED);
    }
    object
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// The list cache in a folder.
#[derive(Clone, Debug)]
pub struct ListCache {
    dir: PathBuf,
    limits: Limits,
}

impl ListCache {
    pub fn new(dir: impl Into<PathBuf>, limits: Limits) -> Self {
        Self {
            dir: dir.into(),
            limits,
        }
    }

    /// `lists` in the disk cache of the config directory
    /// (`KUBACH_CONFIG_DIR` overrides it).
    pub fn in_config_dir() -> Option<Self> {
        crate::disk_cache::dir().map(|dir| Self::new(dir.join(FOLDER), Limits::default()))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &ListKey) -> PathBuf {
        self.dir.join(key.file_name())
    }

    /// The list stored for `key`, if any and readable. Blocking: call it
    /// off the UI thread. A file that can't be used is removed.
    pub fn load(&self, key: &ListKey) -> Option<CachedList> {
        if !key.cacheable() {
            return None;
        }
        let path = self.path(key);
        let file = fs::File::open(&path).ok()?;
        let mut json = Vec::new();
        let read = GzDecoder::new(file).read_to_end(&mut json);
        let parsed = read
            .ok()
            .and_then(|_| serde_json::from_slice::<File<Vec<DynamicObject>>>(&json).ok())
            .filter(|file| file.version == FORMAT_VERSION && &file.key == key);
        let Some(file) = parsed else {
            fs::remove_file(&path).ok();
            return None;
        };
        // Recently used entries survive pruning.
        if let Ok(handle) = fs::File::options().append(true).open(&path) {
            handle.set_modified(SystemTime::now()).ok();
        }
        let objects = file
            .objects
            .into_iter()
            .filter(|object| !is_object_secret(object))
            .map(Arc::new)
            .collect();
        Some(CachedList {
            kind: file.kind,
            saved_at: file.saved_at,
            objects,
        })
    }

    /// The list of a selection ([`ListKey::for_selection`]): the entries of
    /// every key merged, sorted by namespace and name. `None` unless all of
    /// them are there, so a partial list is never shown as the whole.
    pub fn load_selection(&self, keys: &[ListKey]) -> Option<CachedList> {
        let mut lists = keys
            .iter()
            .map(|key| self.load(key))
            .collect::<Option<Vec<_>>>()?;
        if lists.len() <= 1 {
            return lists.pop();
        }
        let kind = lists[0].kind.clone();
        let saved_at = lists.iter().map(|list| list.saved_at).min().unwrap_or(0);
        let mut objects: Vec<Arc<DynamicObject>> = lists
            .iter()
            .flat_map(|list| list.objects.iter().cloned())
            .collect();
        objects.sort_by(|a, b| {
            let key = |o: &DynamicObject| (o.metadata.namespace.clone(), o.metadata.name.clone());
            key(a).cmp(&key(b))
        });
        Some(CachedList {
            kind,
            saved_at,
            objects: objects.into(),
        })
    }

    /// Stores `objects` as the list of `key`, replacing what was there.
    /// Secrets and lists over the limits are not stored (and an older entry
    /// for the key is removed); returns whether it was written. Blocking.
    pub fn store(&self, key: &ListKey, kind: &CachedKind, objects: &[Arc<DynamicObject>]) -> bool {
        let path = self.path(key);
        if !key.cacheable() || objects.len() > self.limits.max_objects {
            fs::remove_file(&path).ok();
            return false;
        }
        let slimmed: Vec<DynamicObject> = objects
            .iter()
            .filter(|object| !is_object_secret(object))
            .map(|object| slim(object))
            .collect();
        let file = File {
            version: FORMAT_VERSION,
            key: key.clone(),
            kind: kind.clone(),
            saved_at: unix_now(),
            objects: slimmed,
        };
        let Some(bytes) = compress(&file) else {
            return false;
        };
        if bytes.len() as u64 > self.limits.max_entry_bytes {
            fs::remove_file(&path).ok();
            return false;
        }
        if crate::settings::write_atomic(&path, &bytes).is_err() {
            return false;
        }
        self.prune();
        true
    }

    /// Removes the least recently used entries beyond the limits.
    pub fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name.ends_with(EXTENSION)
            })
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                let modified = meta.modified().unwrap_or(UNIX_EPOCH);
                Some((modified, meta.len(), entry.path()))
            })
            .collect();
        files.sort_by_key(|file| std::cmp::Reverse(file.0));
        let mut total = 0u64;
        for (ix, (_, len, path)) in files.into_iter().enumerate() {
            total += len;
            if ix >= self.limits.max_entries || total > self.limits.max_total_bytes {
                fs::remove_file(path).ok();
            }
        }
    }
}

fn is_object_secret(object: &DynamicObject) -> bool {
    object
        .types
        .as_ref()
        .is_some_and(|types| types.kind == "Secret" && !types.api_version.contains('/'))
        || is_secret_value(&object.data)
}

fn compress<T: Serialize>(value: &T) -> Option<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    serde_json::to_writer(&mut encoder, value).ok()?;
    encoder.flush().ok()?;
    encoder.finish().ok()
}

struct Pending {
    kind: CachedKind,
    objects: Objects,
    due: Instant,
}

/// Stores lists in the background: [`save`](Self::save) only queues, the
/// write happens off the UI thread after [`WRITE_DELAY`], with the latest
/// list queued for the key by then. Pending writes are dropped with it.
pub struct ListWriter {
    queue: mpsc::UnboundedSender<(ListKey, CachedKind, Objects)>,
    _task: BackgroundTask,
}

impl ListWriter {
    /// A writer into `cache` (on the I/O runtime).
    pub fn new(cache: ListCache) -> Self {
        Self::with_delay(cache, WRITE_DELAY)
    }

    pub fn with_delay(cache: ListCache, delay: Duration) -> Self {
        let (queue, rx) = mpsc::unbounded_channel();
        let task = runtime::spawn(write_loop(cache, rx, delay));
        Self { queue, _task: task }
    }

    /// Queues `objects` as the list of `key`. Cheap: the slice is shared.
    pub fn save(&self, key: ListKey, kind: CachedKind, objects: Objects) {
        if key.cacheable() {
            self.queue.send((key, kind, objects)).ok();
        }
    }
}

async fn write_loop(
    cache: ListCache,
    mut rx: mpsc::UnboundedReceiver<(ListKey, CachedKind, Objects)>,
    delay: Duration,
) {
    let mut pending: HashMap<ListKey, Pending> = HashMap::new();
    loop {
        let next_due = pending.values().map(|p| p.due).min();
        let received = match next_due {
            Some(due) => tokio::time::timeout_at(due, rx.recv()).await.ok(),
            None => Some(rx.recv().await),
        };
        match received {
            // The writer is gone: nothing more to store.
            Some(None) => break,
            Some(Some((key, kind, objects))) => {
                // The first save sets the time; later ones only replace the
                // list, so a steady stream still gets written.
                let due = pending
                    .get(&key)
                    .map_or_else(|| Instant::now() + delay, |p| p.due);
                pending.insert(key, Pending { kind, objects, due });
            }
            None => {
                let now = Instant::now();
                let due: Vec<ListKey> = pending
                    .iter()
                    .filter(|(_, p)| p.due <= now)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in due {
                    let Some(entry) = pending.remove(&key) else {
                        continue;
                    };
                    let cache = cache.clone();
                    runtime::blocking(move || cache.store(&key, &entry.kind, &entry.objects)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::discovery::{ApiCapabilities, Scope, verbs};
    use serde_json::json;

    use super::{
        CachedKind, FORMAT_VERSION, Limits, ListCache, ListKey, ListWriter, compress, slim,
    };
    use crate::cache::Objects;
    use crate::discovery::ResourceKind;
    use crate::secrets::mask::LAST_APPLIED;

    fn pod(name: &str) -> Arc<DynamicObject> {
        Arc::new(
            serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {
                    "name": name,
                    "namespace": "web",
                    "annotations": {LAST_APPLIED: "{\"spec\":{}}", "team": "core"},
                    "managedFields": [{"manager": "kubectl", "operation": "Apply"}]
                },
                "spec": {"nodeName": "n1"},
                "status": {"phase": "Running"}
            }))
            .unwrap(),
        )
    }

    fn pods_kind() -> CachedKind {
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        CachedKind::of(&ResourceKind::new(
            ApiResource::from_gvk_with_plural(&gvk, "pods"),
            &ApiCapabilities {
                scope: Scope::Namespaced,
                subresources: Vec::new(),
                operations: vec![verbs::LIST.into()],
            },
        ))
    }

    /// Writes `bytes` over `path`, to damage a file.
    fn overwrite(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, bytes)
    }

    fn key(context: &str) -> ListKey {
        ListKey::new(context, "", "Pod", Some("web"))
    }

    fn cache(limits: Limits) -> (tempfile::TempDir, ListCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ListCache::new(dir.path().join("lists"), limits);
        (dir, cache)
    }

    fn names(objects: &Objects) -> Vec<&str> {
        objects
            .iter()
            .map(|o| o.metadata.name.as_deref().unwrap())
            .collect()
    }

    #[test]
    fn keys_sort_namespaces_and_differ_by_context() {
        let names = ["b".to_owned(), "a".to_owned(), "b".to_owned()];
        let keys = ListKey::for_selection("prod", "", "Pod", Some(&names));
        assert_eq!(
            keys,
            [
                ListKey::new("prod", "", "Pod", Some("a")),
                ListKey::new("prod", "", "Pod", Some("b"))
            ]
        );
        let all = ListKey::for_selection("prod", "", "Pod", None);
        assert_eq!(all, [ListKey::new("prod", "", "Pod", None)]);
        let a = &keys[0];
        assert_eq!(
            a.file_name(),
            ListKey::new("prod", "", "Pod", Some("a")).file_name()
        );
        assert_ne!(a.file_name(), keys[1].file_name());
        assert_ne!(a.file_name(), key("dev").file_name());
        assert_ne!(a.file_name(), a.all_namespaces().file_name());
        assert!(a.file_name().ends_with(".json.gz"));
    }

    #[test]
    fn stores_and_loads_a_slimmed_list() {
        let (_dir, cache) = cache(Limits::default());
        let objects = [pod("a"), pod("b")];
        assert!(cache.load(&key("prod")).is_none());
        assert!(cache.store(&key("prod"), &pods_kind(), &objects));

        let list = cache.load(&key("prod")).unwrap();
        assert_eq!(list.kind, pods_kind());
        assert_eq!(names(&list.objects), ["a", "b"]);
        let object = &list.objects[0];
        assert!(object.metadata.managed_fields.is_none());
        let annotations = object.metadata.annotations.as_ref().unwrap();
        assert!(!annotations.contains_key(LAST_APPLIED));
        assert_eq!(annotations["team"], "core");
        assert_eq!(object.data["status"]["phase"], "Running");
        assert!(list.saved_at > 0);
        // Another context or selection has nothing.
        assert!(cache.load(&key("dev")).is_none());
        assert!(cache.load(&key("prod").all_namespaces()).is_none());
        // The kind restored without discovery cannot be acted on.
        let kind = list.kind.resource_kind();
        assert_eq!(kind.api.plural, "pods");
        assert!(kind.namespaced && kind.verbs.is_empty());
    }

    #[test]
    fn selections_are_put_together_from_namespaces() {
        let (_dir, cache) = cache(Limits::default());
        let in_ns = |namespace: &str, name: &str| {
            let mut object = (*pod(name)).clone();
            object.metadata.namespace = Some(namespace.into());
            Arc::new(object)
        };
        let web = ListKey::new("prod", "", "Pod", Some("web"));
        let api = ListKey::new("prod", "", "Pod", Some("api"));
        cache.store(&web, &pods_kind(), &[in_ns("web", "a"), in_ns("web", "c")]);
        cache.store(&api, &pods_kind(), &[in_ns("api", "b")]);

        let names = ["web".to_owned(), "api".to_owned()];
        let keys = ListKey::for_selection("prod", "", "Pod", Some(&names));
        let list = cache.load_selection(&keys).unwrap();
        let shown: Vec<_> = list
            .objects
            .iter()
            .map(|o| {
                format!(
                    "{}/{}",
                    o.metadata.namespace.as_deref().unwrap(),
                    o.metadata.name.as_deref().unwrap()
                )
            })
            .collect();
        assert_eq!(shown, ["api/b", "web/a", "web/c"]);
        // One namespace missing: nothing rather than part of the list.
        let names = ["web".to_owned(), "db".to_owned()];
        let keys = ListKey::for_selection("prod", "", "Pod", Some(&names));
        assert!(cache.load_selection(&keys).is_none());
        assert!(cache.load_selection(&[web]).is_some());
    }

    #[test]
    fn never_stores_secrets() {
        let (_dir, cache) = cache(Limits::default());
        let secret: Arc<DynamicObject> = Arc::new(
            serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {"name": "db", "namespace": "web"},
                "data": {"password": "aHVudGVyMg=="}
            }))
            .unwrap(),
        );
        let secrets = ListKey::new("prod", "", "Secret", None);
        assert!(!secrets.cacheable());
        assert!(!cache.store(&secrets, &pods_kind(), std::slice::from_ref(&secret)));
        assert!(cache.load(&secrets).is_none());
        // A Secret among other objects is left out too.
        assert!(cache.store(&key("prod"), &pods_kind(), &[pod("a"), secret]));
        let list = cache.load(&key("prod")).unwrap();
        assert_eq!(names(&list.objects), ["a"]);
        let all = std::fs::read_dir(cache.dir()).unwrap().count();
        assert_eq!(all, 1);
    }

    #[test]
    fn damaged_or_foreign_files_are_ignored_and_removed() {
        let (_dir, cache) = cache(Limits::default());
        cache.store(&key("prod"), &pods_kind(), &[pod("a")]);
        let path = cache.path(&key("prod"));

        overwrite(&path, b"not gzip").unwrap();
        assert!(cache.load(&key("prod")).is_none());
        assert!(!path.exists());

        // An older format version.
        let old = super::File {
            version: FORMAT_VERSION + 1,
            key: key("prod"),
            kind: pods_kind(),
            saved_at: 1,
            objects: Vec::<DynamicObject>::new(),
        };
        overwrite(&path, &compress(&old).unwrap()).unwrap();
        assert!(cache.load(&key("prod")).is_none());
        assert!(!path.exists());

        // A file of another key under this name.
        let other = super::File {
            version: FORMAT_VERSION,
            key: key("dev"),
            kind: pods_kind(),
            saved_at: 1,
            objects: Vec::<DynamicObject>::new(),
        };
        overwrite(&path, &compress(&other).unwrap()).unwrap();
        assert!(cache.load(&key("prod")).is_none());
    }

    #[test]
    fn lists_over_the_limits_are_not_stored() {
        let limits = Limits {
            max_objects: 2,
            ..Limits::default()
        };
        let (_dir, cache) = cache(limits);
        assert!(cache.store(&key("prod"), &pods_kind(), &[pod("a")]));
        // Too many objects: not stored, and the old list goes as well.
        assert!(!cache.store(&key("prod"), &pods_kind(), &[pod("a"), pod("b"), pod("c")]));
        assert!(cache.load(&key("prod")).is_none());

        let tiny = Limits {
            max_entry_bytes: 16,
            ..Limits::default()
        };
        let (_dir, cache) = super::tests::cache(tiny);
        assert!(!cache.store(&key("prod"), &pods_kind(), &[pod("a")]));
    }

    #[test]
    fn prunes_the_least_recently_used() {
        let limits = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        let (_dir, cache) = cache(limits);
        let set_age = |key: &ListKey, seconds_ago: u64| {
            let file = std::fs::File::options()
                .append(true)
                .open(cache.path(key))
                .unwrap();
            file.set_modified(std::time::SystemTime::now() - Duration::from_secs(seconds_ago))
                .unwrap();
        };
        cache.store(&key("a"), &pods_kind(), &[pod("a")]);
        set_age(&key("a"), 300);
        cache.store(&key("b"), &pods_kind(), &[pod("b")]);
        set_age(&key("b"), 200);
        // Reading `a` makes it recent, so `b` is the oldest.
        assert!(cache.load(&key("a")).is_some());
        cache.store(&key("c"), &pods_kind(), &[pod("c")]);
        assert!(cache.load(&key("a")).is_some());
        assert!(cache.load(&key("b")).is_none());
        assert!(cache.load(&key("c")).is_some());

        // The total size limit prunes as well.
        let small = Limits {
            max_total_bytes: 1,
            ..Limits::default()
        };
        let cache = ListCache::new(cache.dir(), small);
        cache.prune();
        assert_eq!(std::fs::read_dir(cache.dir()).unwrap().count(), 0);
    }

    #[test]
    fn writer_stores_the_latest_list_in_the_background() {
        let (_dir, cache) = cache(Limits::default());
        let writer = ListWriter::with_delay(cache.clone(), Duration::from_millis(50));
        let first: Objects = Arc::from(vec![pod("a")]);
        let second: Objects = Arc::from(vec![pod("a"), pod("b")]);
        writer.save(key("prod"), pods_kind(), first);
        writer.save(key("prod"), pods_kind(), second);
        // Secrets are not even queued.
        writer.save(
            ListKey::new("prod", "", "Secret", None),
            pods_kind(),
            Arc::from(vec![]),
        );
        assert!(cache.load(&key("prod")).is_none());
        for _ in 0..200 {
            if let Some(list) = cache.load(&key("prod")) {
                assert_eq!(names(&list.objects), ["a", "b"]);
                assert_eq!(std::fs::read_dir(cache.dir()).unwrap().count(), 1);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the list was not written");
    }

    #[test]
    fn slim_keeps_the_body() {
        let object = slim(&pod("a"));
        assert_eq!(object.data["spec"]["nodeName"], "n1");
        assert!(object.metadata.managed_fields.is_none());
    }
}
