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
//! - **Not for every context**: the contexts in
//!   [`disk_cache::uncached`](crate::disk_cache::uncached) (those of the
//!   vault, [ADR 0008](../../../docs/adr/0008-kubeconfig-vault.md)) are
//!   neither stored nor read.
//! - **Off the UI thread**: [`ListWriter`] stores in the background: a list
//!   [`WRITE_DELAY`] after it was first loaded, then, while it keeps
//!   changing, once per [`WRITE_INTERVAL`], the latest list winning.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::cache::Objects;
use crate::cache::merge_objects;
use crate::discovery::ResourceKind;
use crate::disk_cache::Uncached;
use crate::runtime;
use crate::secrets::mask::{LAST_APPLIED, is_core_secret, is_secret_value};

/// Version of the file format; files of any other version are ignored.
pub const FORMAT_VERSION: u32 = 1;

/// Folder of the list cache inside the disk cache.
const FOLDER: &str = "lists";
const EXTENSION: &str = "json.gz";

/// How long [`ListWriter`] waits before storing a list that was not stored
/// lately, so the burst of events after a load is written once.
pub const WRITE_DELAY: Duration = Duration::from_secs(3);

/// The least time between two writes of a list that keeps changing (pods
/// during a rollout). Storing means serialising and compressing the whole
/// list, and the cache is only read when the kind is opened again: a list
/// a minute old is as good there as one three seconds old.
pub const WRITE_INTERVAL: Duration = Duration::from_secs(60);

/// How long a list found over [`Limits::max_entry_bytes`] is not tried
/// again while it has about as many objects (their size may still change),
/// and how long the writer remembers a key nothing is queued for.
const REMEMBER: Duration = Duration::from_secs(10 * 60);

/// Entries are pruned once per this many writes, and on the first one.
const PRUNE_EVERY: usize = 8;

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

    /// File name: the [tag](crate::disk_cache::context_tag) of the context
    /// and a hash of the key, so context and namespace names need no
    /// escaping and the files of a context are found by its name.
    fn file_name(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let digest = Sha256::digest(json);
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        crate::disk_cache::file_name(&self.context, &format!("{hex}.{EXTENSION}"))
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

/// A copy of `object` without what must not reach the disk:
/// `managedFields` and the last-applied annotation.
pub fn slim(object: &DynamicObject) -> DynamicObject {
    let mut object = object.clone();
    object.metadata.managed_fields = None;
    if let Some(annotations) = &mut object.metadata.annotations {
        annotations.remove(LAST_APPLIED);
    }
    object
}

/// `object` as it is stored. The resource cache drops `managedFields` and
/// the last-applied annotation itself, so its objects are written as they
/// are, without a copy; one that still has either is copied without them.
fn slimmed(object: &DynamicObject) -> Cow<'_, DynamicObject> {
    let meta = &object.metadata;
    let last_applied = meta
        .annotations
        .as_ref()
        .is_some_and(|annotations| annotations.contains_key(LAST_APPLIED));
    if meta.managed_fields.is_some() || last_applied {
        Cow::Owned(slim(object))
    } else {
        Cow::Borrowed(object)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// What became of a list handed to [`ListCache::try_store`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    Written,
    /// Not a list that is stored: Secrets, a context that is not cached,
    /// more than [`Limits::max_objects`] objects. Costs nothing to find out.
    Refused,
    /// Serialised and compressed to `bytes`, over
    /// [`Limits::max_entry_bytes`]: all the work of a write for nothing.
    TooLarge {
        bytes: u64,
    },
    /// The file could not be written.
    Failed,
}

/// The list cache in a folder. Clones share the list of contexts that are
/// not cached and the count of writes.
#[derive(Clone, Debug)]
pub struct ListCache {
    dir: PathBuf,
    limits: Limits,
    uncached: Uncached,
    writes: Arc<AtomicUsize>,
}

impl ListCache {
    /// A cache in `dir` that stores every context.
    pub fn new(dir: impl Into<PathBuf>, limits: Limits) -> Self {
        Self {
            dir: dir.into(),
            limits,
            uncached: Uncached::default(),
            writes: Arc::default(),
        }
    }

    /// The same cache, neither storing nor reading the contexts in
    /// `uncached`.
    pub fn without(mut self, uncached: Uncached) -> Self {
        self.uncached = uncached;
        self
    }

    /// `lists` in the disk cache of the config directory
    /// (`KUBACH_CONFIG_DIR` overrides it), without the contexts in
    /// [`disk_cache::uncached`](crate::disk_cache::uncached).
    pub fn in_config_dir() -> Option<Self> {
        let dir = crate::disk_cache::dir()?.join(FOLDER);
        Some(Self::new(dir, Limits::default()).without(crate::disk_cache::uncached().clone()))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &ListKey) -> PathBuf {
        self.dir.join(key.file_name())
    }

    /// Whether lists of `key` are stored and read here: not Secrets, and
    /// not a context that is not cached.
    pub fn accepts(&self, key: &ListKey) -> bool {
        key.cacheable() && !self.uncached.contains(&key.context)
    }

    /// The list stored for `key`, if any and readable. Blocking: call it
    /// off the UI thread. A file that can't be used is removed.
    pub fn load(&self, key: &ListKey) -> Option<CachedList> {
        if !self.accepts(key) {
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
        // Each entry was stored sorted and they are of different
        // namespaces: put one after another, not sorted again.
        let parts: Vec<&Objects> = lists.iter().map(|list| &list.objects).collect();
        Some(CachedList {
            kind,
            saved_at,
            objects: merge_objects(&parts),
        })
    }

    /// [`try_store`](Self::try_store); returns whether the list was written.
    pub fn store(&self, key: &ListKey, kind: &CachedKind, objects: &[Arc<DynamicObject>]) -> bool {
        self.try_store(key, kind, objects) == Stored::Written
    }

    /// Stores `objects` as the list of `key`, replacing what was there.
    /// Secrets, contexts that are not cached and lists over the limits are
    /// not stored (and an older entry for the key is removed). The file
    /// never holds `managedFields`, the last-applied annotation or a
    /// Secret. Blocking.
    pub fn try_store(
        &self,
        key: &ListKey,
        kind: &CachedKind,
        objects: &[Arc<DynamicObject>],
    ) -> Stored {
        let path = self.path(key);
        if !self.accepts(key) || objects.len() > self.limits.max_objects {
            fs::remove_file(&path).ok();
            return Stored::Refused;
        }
        let slimmed: Vec<Cow<'_, DynamicObject>> = objects
            .iter()
            .filter(|object| !is_object_secret(object))
            .map(|object| slimmed(object))
            .collect();
        let file = File {
            version: FORMAT_VERSION,
            key: key.clone(),
            kind: kind.clone(),
            saved_at: unix_now(),
            objects: slimmed,
        };
        let Some(bytes) = compress(&file) else {
            return Stored::Failed;
        };
        let size = bytes.len() as u64;
        if size > self.limits.max_entry_bytes {
            fs::remove_file(&path).ok();
            return Stored::TooLarge { bytes: size };
        }
        if crate::settings::write_atomic(&path, &bytes).is_err() {
            return Stored::Failed;
        }
        // Reading the folder and the size of every entry for each write is
        // most of the work of storing a small list; until the next prune
        // the cache is over its limits by `PRUNE_EVERY - 1` entries at most.
        let written = self.writes.fetch_add(1, Ordering::Relaxed);
        if written.is_multiple_of(PRUNE_EVERY) {
            self.prune();
        }
        Stored::Written
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
    let encoder = GzEncoder::new(Vec::new(), Compression::fast());
    // The serialiser writes a few bytes at a time: the compressor gets
    // them in blocks.
    let mut buffered = BufWriter::with_capacity(64 * 1024, encoder);
    serde_json::to_writer(&mut buffered, value).ok()?;
    buffered.into_inner().ok()?.finish().ok()
}

/// A list found over the size limit.
struct Oversize {
    objects: usize,
    bytes: u64,
    at: Instant,
}

/// What the writer knows of a key.
#[derive(Default)]
struct Entry {
    /// The latest list queued, and when it is to be written.
    pending: Option<(CachedKind, Objects, Instant)>,
    /// When the key was last written (or tried).
    tried_at: Option<Instant>,
    /// The list that was written (or tried) last. A watch hands out the
    /// same list until its objects change, and a view queues every list of
    /// a selection when any of them changes: the one already on disk is
    /// not written again. Weak: only its address is compared.
    tried: Option<Weak<[Arc<DynamicObject>]>>,
    oversize: Option<Oversize>,
}

/// When each key is written: the part of [`ListWriter`] that needs no
/// runtime, the clock being passed in.
struct Schedule {
    delay: Duration,
    interval: Duration,
    max_entry_bytes: u64,
    entries: HashMap<ListKey, Entry>,
}

impl Schedule {
    fn new(delay: Duration, interval: Duration, max_entry_bytes: u64) -> Self {
        Self {
            delay,
            interval,
            max_entry_bytes,
            entries: HashMap::new(),
        }
    }

    /// Queues `objects` for `key`, replacing a list queued before. The time
    /// of the write is set by the first list queued since the last write:
    /// `delay` from now, but not sooner than `interval` after that write.
    fn save(&mut self, key: ListKey, kind: CachedKind, objects: Objects, now: Instant) {
        let entry = self.entries.entry(key).or_default();
        let same = entry
            .tried
            .as_ref()
            .is_some_and(|tried| Weak::ptr_eq(tried, &Arc::downgrade(&objects)));
        // A list that was too large is not tried again (serialised and
        // compressed in full, to be thrown away) until, going by the
        // number of objects, it would fit, or `REMEMBER` has passed.
        let too_large = entry.oversize.as_ref().is_some_and(|over| {
            let estimate = over.bytes.saturating_mul(objects.len() as u64) / over.objects as u64;
            estimate > self.max_entry_bytes && now < over.at + REMEMBER
        });
        if same || too_large {
            entry.pending = None;
            return;
        }
        let due = match (&entry.pending, entry.tried_at) {
            (Some((_, _, due)), _) => *due,
            (None, Some(tried_at)) => (tried_at + self.interval).max(now + self.delay),
            (None, None) => now + self.delay,
        };
        entry.pending = Some((kind, objects, due));
    }

    /// Makes every queued list due now.
    fn flush(&mut self, now: Instant) {
        for entry in self.entries.values_mut() {
            if let Some((_, _, due)) = &mut entry.pending {
                *due = now;
            }
        }
    }

    /// When the next write is due.
    fn next_due(&self) -> Option<Instant> {
        let pending = self.entries.values().filter_map(|e| e.pending.as_ref());
        pending.map(|(_, _, due)| *due).min()
    }

    /// The lists to write now. Keys with nothing queued and nothing tried
    /// for `REMEMBER` are forgotten.
    fn take_due(&mut self, now: Instant) -> Vec<(ListKey, CachedKind, Objects)> {
        self.entries.retain(|_, entry| {
            entry.pending.is_some() || entry.tried_at.is_some_and(|at| now < at + REMEMBER)
        });
        let mut due = Vec::new();
        for (key, entry) in &mut self.entries {
            if entry.pending.as_ref().is_some_and(|(_, _, at)| *at <= now)
                && let Some((kind, objects, _)) = entry.pending.take()
            {
                due.push((key.clone(), kind, objects));
            }
        }
        due
    }

    /// Records what became of writing `objects` for `key`.
    fn tried(&mut self, key: &ListKey, objects: &Objects, stored: Stored, now: Instant) {
        let entry = self.entries.entry(key.clone()).or_default();
        entry.tried_at = Some(now);
        entry.tried = Some(Arc::downgrade(objects));
        entry.oversize = match stored {
            Stored::TooLarge { bytes } if !objects.is_empty() => Some(Oversize {
                objects: objects.len(),
                bytes,
                at: now,
            }),
            _ => None,
        };
    }
}

enum Message {
    Save(ListKey, CachedKind, Objects),
    /// Write what is queued now, and tell when it is done.
    Flush(Option<oneshot::Sender<()>>),
}

/// Stores lists in the background: [`save`](Self::save) only queues, the
/// write happens off the UI thread, [`WRITE_DELAY`] after the first list of
/// a key and then not more often than once per [`WRITE_INTERVAL`], with
/// the latest list queued by then. [`flush`](Self::flush) and dropping the
/// writer store what is queued right away.
pub struct ListWriter {
    cache: ListCache,
    queue: mpsc::UnboundedSender<Message>,
}

impl ListWriter {
    /// A writer into `cache` (on the I/O runtime).
    pub fn new(cache: ListCache) -> Self {
        Self::with_timing(cache, WRITE_DELAY, WRITE_INTERVAL)
    }

    pub fn with_timing(cache: ListCache, delay: Duration, interval: Duration) -> Self {
        let (queue, rx) = mpsc::unbounded_channel();
        let schedule = Schedule::new(delay, interval, cache.limits.max_entry_bytes);
        // Detached: the loop ends by itself once the writer is dropped,
        // after storing the lists still queued (those of a screen just
        // closed).
        drop(runtime::spawn_detached(write_loop(
            cache.clone(),
            rx,
            schedule,
        )));
        Self { cache, queue }
    }

    /// Queues `objects` as the list of `key`. Cheap: the slice is shared.
    /// Lists the cache does not take are not even queued.
    pub fn save(&self, key: ListKey, kind: CachedKind, objects: Objects) {
        if self.cache.accepts(&key) {
            self.queue.send(Message::Save(key, kind, objects)).ok();
        }
    }

    /// Stores what is queued now rather than when it is due: the lists
    /// will not be queued again (their view is gone).
    pub fn flush(&self) {
        self.queue.send(Message::Flush(None)).ok();
    }

    /// [`flush`](Self::flush), resolving once the lists are on disk (or at
    /// once if the writer's loop is gone).
    pub fn flushed(&self) -> oneshot::Receiver<()> {
        let (done, wait) = oneshot::channel();
        self.queue.send(Message::Flush(Some(done))).ok();
        wait
    }
}

async fn write_loop(
    cache: ListCache,
    mut rx: mpsc::UnboundedReceiver<Message>,
    mut schedule: Schedule,
) {
    loop {
        let received = match schedule.next_due() {
            Some(due) => tokio::time::timeout_at(due, rx.recv()).await.ok(),
            None => Some(rx.recv().await),
        };
        let mut done = None;
        let mut closed = false;
        match received {
            Some(Some(Message::Save(key, kind, objects))) => {
                schedule.save(key, kind, objects, Instant::now());
                continue;
            }
            Some(Some(Message::Flush(ack))) => {
                schedule.flush(Instant::now());
                done = ack;
            }
            // The writer is gone: what is queued is stored, nothing more
            // will come.
            Some(None) => {
                schedule.flush(Instant::now());
                closed = true;
            }
            // A write is due.
            None => {}
        }
        for (key, kind, objects) in schedule.take_due(Instant::now()) {
            let cache = cache.clone();
            let (stored, key, objects) = runtime::blocking(move || {
                let stored = cache.try_store(&key, &kind, &objects);
                (stored, key, objects)
            })
            .await;
            schedule.tried(&key, &objects, stored, Instant::now());
        }
        if let Some(done) = done {
            done.send(()).ok();
        }
        if closed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::io::Read as _;
    use std::sync::Arc;
    use std::time::Duration;

    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::discovery::{ApiCapabilities, Scope, verbs};
    use serde_json::json;
    use tokio::time::Instant;

    use super::{
        CachedKind, FORMAT_VERSION, Limits, ListCache, ListKey, ListWriter, PRUNE_EVERY, REMEMBER,
        Schedule, Stored, WRITE_DELAY, WRITE_INTERVAL, compress, slim, slimmed,
    };
    use crate::cache::Objects;
    use crate::discovery::ResourceKind;
    use crate::disk_cache::Uncached;
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
        cache.prune();
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
        let writer =
            ListWriter::with_timing(cache.clone(), Duration::from_millis(50), WRITE_INTERVAL);
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

    /// A pod as the resource cache holds it: without `managedFields` and
    /// the last-applied annotation.
    fn trimmed(name: &str) -> Arc<DynamicObject> {
        Arc::new(slim(&pod(name)))
    }

    /// What the file of `key` holds, as text.
    fn stored_text(cache: &ListCache, key: &ListKey) -> String {
        let file = std::fs::File::open(cache.path(key)).unwrap();
        let mut text = String::new();
        flate2::read::GzDecoder::new(file)
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    #[test]
    fn objects_of_the_resource_cache_are_stored_without_a_copy() {
        assert!(matches!(slimmed(&trimmed("a")), Cow::Borrowed(_)));
        assert!(matches!(slimmed(&pod("a")), Cow::Owned(_)));
        let mut only_managed = (*trimmed("a")).clone();
        only_managed.metadata.managed_fields = pod("a").metadata.managed_fields.clone();
        assert!(matches!(slimmed(&only_managed), Cow::Owned(_)));

        // Whichever way, neither reaches the file.
        let (_dir, cache) = cache(Limits::default());
        let objects = [trimmed("a"), pod("b"), Arc::new(only_managed)];
        assert!(cache.store(&key("prod"), &pods_kind(), &objects));
        let text = stored_text(&cache, &key("prod"));
        assert!(text.contains("\"nodeName\":\"n1\""), "{text}");
        assert!(!text.contains("managedFields"), "{text}");
        assert!(!text.contains(LAST_APPLIED), "{text}");
        assert_eq!(cache.load(&key("prod")).unwrap().objects.len(), 3);
    }

    #[test]
    fn a_context_that_is_not_cached_is_neither_stored_nor_read() {
        let (_dir, plain) = cache(Limits::default());
        let uncached = Uncached::default();
        let cache = plain.clone().without(uncached.clone());
        let count = || std::fs::read_dir(cache.dir()).unwrap().count();

        // A list an earlier version stored, before the context was set
        // apart.
        assert!(plain.store(&key("from-vault"), &pods_kind(), &[pod("old")]));
        assert!(cache.store(&key("from-file"), &pods_kind(), &[pod("a")]));
        assert_eq!(count(), 2);
        uncached.set(BTreeSet::from(["from-vault".to_owned()]));

        assert!(!cache.accepts(&key("from-vault")));
        assert!(cache.accepts(&key("from-file")));
        // The file is there, and is not read.
        assert!(cache.path(&key("from-vault")).exists());
        assert!(cache.load(&key("from-vault")).is_none());
        let both = [key("from-file"), key("from-vault")];
        assert!(cache.load_selection(&both).is_none());
        // Nothing is written, and what was there goes.
        let stored = cache.try_store(&key("from-vault"), &pods_kind(), &[pod("a")]);
        assert_eq!(stored, Stored::Refused);
        assert!(!cache.path(&key("from-vault")).exists());
        let all = ListKey::new("from-vault", "", "ConfigMap", None);
        assert_eq!(
            cache.try_store(&all, &pods_kind(), &[pod("a")]),
            Stored::Refused
        );
        assert_eq!(count(), 1);

        // The writer does not queue such lists.
        let writer =
            ListWriter::with_timing(cache.clone(), Duration::from_secs(3600), WRITE_INTERVAL);
        writer.save(key("from-vault"), pods_kind(), Arc::from(vec![pod("a")]));
        writer.save(key("from-file"), pods_kind(), Arc::from(vec![pod("b")]));
        writer.flushed().blocking_recv().unwrap();
        assert_eq!(count(), 1);
        assert!(cache.load(&key("from-vault")).is_none());
        // A context of a kubeconfig file is stored and read as before.
        let list = cache.load(&key("from-file")).unwrap();
        assert_eq!(names(&list.objects), ["b"]);
    }

    #[test]
    fn entries_are_pruned_once_per_several_writes() {
        let limits = Limits {
            max_entries: 1,
            ..Limits::default()
        };
        let (_dir, cache) = cache(limits);
        let count = || std::fs::read_dir(cache.dir()).unwrap().count();
        // The first write prunes, the next ones do not.
        for ix in 0..PRUNE_EVERY {
            assert!(cache.store(&key(&format!("c{ix}")), &pods_kind(), &[pod("a")]));
        }
        assert_eq!(count(), PRUNE_EVERY);
        assert!(cache.store(&key("last"), &pods_kind(), &[pod("a")]));
        assert_eq!(count(), 1);
    }

    fn list(names: &[&str]) -> Objects {
        names.iter().map(|name| trimmed(name)).collect()
    }

    fn schedule() -> Schedule {
        Schedule::new(
            WRITE_DELAY,
            WRITE_INTERVAL,
            Limits::default().max_entry_bytes,
        )
    }

    /// Writes everything due at `now`; returns how many lists it was.
    fn write_due(schedule: &mut Schedule, now: Instant, stored: Stored) -> usize {
        let due = schedule.take_due(now);
        for (key, _, objects) in &due {
            schedule.tried(key, objects, stored, now);
        }
        due.len()
    }

    #[test]
    fn a_changing_list_is_written_once_per_interval() {
        let mut schedule = schedule();
        let start = Instant::now();
        let step = Duration::from_millis(150);
        // A list that changes every 150 ms for five minutes.
        let mut writes = Vec::new();
        for tick in 0..(5 * 60 * 1000 / 150) {
            let now = start + step * tick;
            schedule.save(key("prod"), pods_kind(), list(&["a"]), now);
            if write_due(&mut schedule, now, Stored::Written) > 0 {
                writes.push((now - start).as_secs());
            }
        }
        // Once soon after the first load, then once a minute.
        assert_eq!(writes, [3, 63, 123, 183, 243]);
        let first_minute = writes.iter().filter(|at| **at < 60).count();
        assert_eq!(first_minute, 1);
    }

    #[test]
    fn a_quiet_list_is_written_soon_after_it_changes() {
        let mut schedule = schedule();
        let start = Instant::now();
        schedule.save(key("prod"), pods_kind(), list(&["a"]), start);
        assert_eq!(schedule.next_due(), Some(start + WRITE_DELAY));
        assert_eq!(write_due(&mut schedule, start, Stored::Written), 0);
        // The latest list queued by then is the one written.
        let latest = list(&["a", "b"]);
        schedule.save(key("prod"), pods_kind(), latest.clone(), start);
        let due = schedule.take_due(start + WRITE_DELAY);
        assert_eq!(due.len(), 1);
        assert!(Arc::ptr_eq(&due[0].2, &latest));
        schedule.tried(&key("prod"), &latest, Stored::Written, start + WRITE_DELAY);
        assert_eq!(schedule.next_due(), None);

        // Long after the last write a change waits the short delay only.
        let later = start + WRITE_INTERVAL * 3;
        schedule.save(key("prod"), pods_kind(), list(&["c"]), later);
        assert_eq!(schedule.next_due(), Some(later + WRITE_DELAY));
        // Another key has its own times.
        schedule.save(key("dev"), pods_kind(), list(&["a"]), later);
        assert_eq!(
            write_due(&mut schedule, later + WRITE_DELAY, Stored::Written),
            2
        );
    }

    #[test]
    fn the_list_already_written_is_not_written_again() {
        let mut schedule = schedule();
        let start = Instant::now();
        let same = list(&["a"]);
        schedule.save(key("prod"), pods_kind(), same.clone(), start);
        assert_eq!(
            write_due(&mut schedule, start + WRITE_DELAY, Stored::Written),
            1
        );
        // A view queues its lists on every change of any of them.
        for seconds in [10, 70, 300] {
            let now = start + Duration::from_secs(seconds);
            schedule.save(key("prod"), pods_kind(), same.clone(), now);
            assert_eq!(schedule.next_due(), None);
        }
        // A changed list is queued, and back to the written one is not.
        let now = start + Duration::from_secs(301);
        schedule.save(key("prod"), pods_kind(), list(&["b"]), now);
        assert!(schedule.next_due().is_some());
        schedule.save(key("prod"), pods_kind(), same, now);
        assert_eq!(schedule.next_due(), None);
    }

    #[test]
    fn a_list_over_the_size_limit_is_not_tried_again_until_it_shrinks() {
        let limit = 1000;
        let mut schedule = Schedule::new(WRITE_DELAY, WRITE_INTERVAL, limit);
        let start = Instant::now();
        let names: Vec<String> = (0..100).map(|ix| format!("p{ix}")).collect();
        let of = |count: usize| -> Objects { names[..count].iter().map(|n| trimmed(n)).collect() };
        schedule.save(key("prod"), pods_kind(), of(100), start);
        // 100 objects took twice the limit.
        let over = Stored::TooLarge { bytes: 2 * limit };
        assert_eq!(write_due(&mut schedule, start + WRITE_DELAY, over), 1);

        // About as many objects: not tried, however long it keeps changing.
        let mut now = start + WRITE_DELAY;
        while now < start + REMEMBER {
            schedule.save(key("prod"), pods_kind(), of(99), now);
            schedule.save(key("prod"), pods_kind(), of(51), now);
            assert_eq!(schedule.next_due(), None);
            assert_eq!(write_due(&mut schedule, now, over), 0);
            now += Duration::from_secs(1);
        }
        // Half as many would fit: tried again, and written.
        let now = start + WRITE_INTERVAL * 2;
        schedule.save(key("prod"), pods_kind(), of(50), now);
        assert_eq!(schedule.next_due(), Some(now + WRITE_DELAY));
        assert_eq!(
            write_due(&mut schedule, now + WRITE_DELAY, Stored::Written),
            1
        );
        schedule.save(key("prod"), pods_kind(), of(99), now + WRITE_DELAY);
        assert!(schedule.next_due().is_some());

        // The same size is tried again after a long time: the objects
        // themselves may have become smaller.
        let mut schedule = Schedule::new(WRITE_DELAY, WRITE_INTERVAL, limit);
        schedule.save(key("prod"), pods_kind(), of(100), start);
        assert_eq!(write_due(&mut schedule, start + WRITE_DELAY, over), 1);
        let tried_at = start + WRITE_DELAY;
        schedule.save(key("prod"), pods_kind(), of(99), tried_at + REMEMBER);
        assert_eq!(schedule.next_due(), Some(tried_at + REMEMBER + WRITE_DELAY));
    }

    #[test]
    fn flush_makes_the_queued_lists_due_and_idle_keys_are_forgotten() {
        let mut schedule = schedule();
        let start = Instant::now();
        schedule.save(key("prod"), pods_kind(), list(&["a"]), start);
        schedule.save(key("dev"), pods_kind(), list(&["a"]), start);
        assert_eq!(write_due(&mut schedule, start, Stored::Written), 0);
        schedule.flush(start);
        assert_eq!(write_due(&mut schedule, start, Stored::Written), 2);
        assert_eq!(schedule.entries.len(), 2);
        // Only a key with something queued is kept for long.
        schedule.save(key("dev"), pods_kind(), list(&["b"]), start + REMEMBER);
        assert_eq!(
            write_due(&mut schedule, start + REMEMBER, Stored::Written),
            0
        );
        assert_eq!(schedule.entries.len(), 1);
        assert!(schedule.entries.contains_key(&key("dev")));
    }

    #[test]
    fn writer_stores_what_is_queued_on_flush_and_when_dropped() {
        let (_dir, cache) = cache(Limits::default());
        let hour = Duration::from_secs(3600);
        let writer = ListWriter::with_timing(cache.clone(), hour, hour);
        writer.save(key("prod"), pods_kind(), list(&["a"]));
        writer.flushed().blocking_recv().unwrap();
        assert_eq!(names(&cache.load(&key("prod")).unwrap().objects), ["a"]);
        // Nothing queued: a flush has nothing to wait for.
        writer.flushed().blocking_recv().unwrap();

        writer.save(key("dev"), pods_kind(), list(&["b"]));
        drop(writer);
        for _ in 0..200 {
            if let Some(list) = cache.load(&key("dev")) {
                assert_eq!(names(&list.objects), ["b"]);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the list queued when the writer was dropped was not written");
    }

    /// A pod of a realistic size (about 3 KiB of JSON): two containers
    /// with env, probes and mounts, and their statuses.
    fn big_pod(ix: usize) -> Arc<DynamicObject> {
        let container = |name: &str| {
            json!({
                "name": name,
                "image": format!("registry.example.com/team/{name}:1.2.{ix}"),
                "args": ["--listen=:8080", "--log-level=info"],
                "env": [
                    {"name": "POD_NAME", "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}}},
                    {"name": "DATABASE_URL", "value": "postgres://db.internal:5432/app"},
                    {"name": "FEATURES", "value": "a,b,c,d"}
                ],
                "ports": [{"containerPort": 8080, "name": "http", "protocol": "TCP"}],
                "resources": {
                    "requests": {"cpu": "100m", "memory": "128Mi"},
                    "limits": {"cpu": "1", "memory": "512Mi"}
                },
                "livenessProbe": {"httpGet": {"path": "/healthz", "port": "http"}, "periodSeconds": 10},
                "readinessProbe": {"httpGet": {"path": "/ready", "port": "http"}, "periodSeconds": 5},
                "volumeMounts": [
                    {"name": "config", "mountPath": "/etc/app"},
                    {
                        "name": "kube-api-access",
                        "mountPath": "/var/run/secrets/kubernetes.io/serviceaccount",
                        "readOnly": true
                    }
                ]
            })
        };
        let status = |name: &str| {
            json!({
                "name": name, "ready": true, "restartCount": ix % 7, "started": true,
                "image": format!("registry.example.com/team/{name}:1.2.{ix}"),
                "imageID": format!("registry.example.com/team/{name}@sha256:{ix:064x}"),
                "containerID": format!("containerd://{:064x}", ix * 31),
                "state": {"running": {"startedAt": "2026-11-01T10:00:00Z"}}
            })
        };
        let object = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": format!("web-{ix:05}-{:x}", ix * 7919),
                "namespace": format!("team-{:02}", ix % 40),
                "uid": format!("{ix:08x}-0000-4000-8000-{ix:012x}"),
                "resourceVersion": format!("{}", 1_000_000 + ix),
                "creationTimestamp": "2026-11-01T10:00:00Z",
                "labels": {
                    "app.kubernetes.io/name": "web",
                    "app.kubernetes.io/instance": format!("web-{}", ix % 300),
                    "pod-template-hash": "5d8f7c9b6"
                },
                "annotations": {"prometheus.io/scrape": "true", "prometheus.io/port": "8080"},
                "ownerReferences": [{
                    "apiVersion": "apps/v1", "kind": "ReplicaSet",
                    "name": format!("web-{}-5d8f7c9b6", ix % 300), "uid": "u", "controller": true
                }]
            },
            "spec": {
                "nodeName": format!("node-{}", ix % 50),
                "serviceAccountName": "web",
                "restartPolicy": "Always",
                "containers": [container("app"), container("sidecar")],
                "volumes": [
                    {"name": "config", "configMap": {"name": "web-config"}},
                    {"name": "kube-api-access", "projected": {"sources": [
                        {"serviceAccountToken": {"path": "token", "expirationSeconds": 3607}}
                    ]}}
                ],
                "tolerations": [{
                    "key": "node.kubernetes.io/not-ready", "operator": "Exists",
                    "effect": "NoExecute", "tolerationSeconds": 300
                }]
            },
            "status": {
                "phase": "Running",
                "podIP": format!("10.{}.{}.{}", ix % 200, ix % 250, ix % 199),
                "hostIP": "192.168.1.10",
                "qosClass": "Burstable",
                "startTime": "2026-11-01T10:00:00Z",
                "conditions": [
                    {"type": "Initialized", "status": "True", "lastTransitionTime": "2026-11-01T10:00:00Z"},
                    {"type": "Ready", "status": "True", "lastTransitionTime": "2026-11-01T10:00:05Z"},
                    {"type": "ContainersReady", "status": "True", "lastTransitionTime": "2026-11-01T10:00:05Z"},
                    {"type": "PodScheduled", "status": "True", "lastTransitionTime": "2026-11-01T10:00:00Z"}
                ],
                "containerStatuses": [status("app"), status("sidecar")]
            }
        });
        Arc::new(serde_json::from_value(object).unwrap())
    }

    /// Time of one `store` of 10 000 pods, printed: run with
    /// `cargo test -p kubach-core --lib store_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn store_time_of_ten_thousand_pods() {
        let (dir, cache) = cache(Limits::default());
        let pods: Vec<_> = (0..10_000).map(big_pod).collect();
        let key = ListKey::new("prod", "", "Pod", None);
        let mut times = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            assert!(cache.store(&key, &pods_kind(), &pods));
            times.push(start.elapsed().as_millis());
        }
        let bytes = crate::disk_cache::size(dir.path());
        println!("store of 10 000 pods, ms: {times:?}; {bytes} bytes on disk");
    }
}
