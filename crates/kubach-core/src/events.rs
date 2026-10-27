//! Event timeline: `events.k8s.io/v1` and core `v1` Events normalized into
//! one [`TimelineEvent`], with filtering, grouping of repeats and owner-tree
//! collection (Deployment → ReplicaSets → Pods).
//!
//! Both APIs expose the same underlying objects; [`events_kind`] picks
//! `events.k8s.io` when served. Live updates go through the [cache](crate::cache)
//! ([`EventFeed`]), one-shot reads through [`list`].

use std::borrow::Cow;
use std::collections::hash_map::{DefaultHasher, Entry};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

use k8s_openapi::api::core::v1::{self as core, ObjectReference, Pod};
use k8s_openapi::api::events::v1 as events_v1;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};
use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::DynamicObject;

use crate::cache::{Changes, ResourceCache, Subscription, WatchStatus};
use crate::diagnostics::{self, Finding};
use crate::discovery::{self, ApiCatalog, DiscoveryError, ResourceKind};
use crate::plugin::Health;
use crate::plugin::builtin::pod_health_with_events;

#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error("the cluster serves no Event API")]
    NotServed,
    #[error(transparent)]
    List(#[from] Box<DiscoveryError>),
}

/// Severity of an event as reported by Kubernetes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventType {
    Normal,
    Warning,
}

/// Which API an event was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventApi {
    /// `events.k8s.io/v1` (or `v1beta1`).
    Events,
    /// Core `v1`.
    Core,
}

/// The object an event is about (`regarding` / `involvedObject`) or related to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct InvolvedObject {
    pub api_version: Option<String>,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub uid: Option<String>,
    /// E.g. `spec.containers{api}` for container-level events.
    pub field_path: Option<String>,
}

impl InvolvedObject {
    fn from_ref(r: &ObjectReference) -> Self {
        Self {
            api_version: r.api_version.clone(),
            kind: r.kind.clone().unwrap_or_default(),
            namespace: r.namespace.clone(),
            name: r.name.clone().unwrap_or_default(),
            uid: r.uid.clone(),
            field_path: r.field_path.clone(),
        }
    }

    fn to_ref(&self) -> ObjectReference {
        ObjectReference {
            api_version: self.api_version.clone(),
            kind: Some(self.kind.clone()),
            namespace: self.namespace.clone(),
            name: Some(self.name.clone()),
            uid: self.uid.clone(),
            field_path: self.field_path.clone(),
            ..Default::default()
        }
    }
}

/// An event from either API, normalized.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineEvent {
    pub api: EventApi,
    /// Name, namespace and uid of the Event object itself.
    pub name: String,
    pub namespace: Option<String>,
    pub uid: Option<String>,
    /// When it last happened: `series.lastObservedTime`, then
    /// `lastTimestamp`, `eventTime`, `firstTimestamp`, creation time.
    pub time: Option<Timestamp>,
    /// When it first happened (`firstTimestamp`, `eventTime`), else `time`.
    pub first_time: Option<Timestamp>,
    pub type_: EventType,
    pub reason: String,
    /// `note` (events.k8s.io) or `message` (core).
    pub message: String,
    pub regarding: InvolvedObject,
    pub related: Option<InvolvedObject>,
    /// `reportingController` / `reportingComponent`, else `source.component`.
    pub reporting_controller: Option<String>,
    pub reporting_instance: Option<String>,
    pub action: Option<String>,
    /// Occurrences: `series.count`, then the deprecated `count`; at least 1.
    pub count: u32,
}

impl AsRef<TimelineEvent> for TimelineEvent {
    fn as_ref(&self) -> &TimelineEvent {
        self
    }
}

fn event_type(type_: Option<&str>) -> EventType {
    match type_ {
        Some("Warning") => EventType::Warning,
        _ => EventType::Normal,
    }
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.clone().filter(|s| !s.is_empty())
}

fn count(series: Option<i32>, deprecated: Option<i32>) -> u32 {
    series
        .or(deprecated)
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(1)
        .max(1)
}

fn created(meta: &ObjectMeta) -> Option<Timestamp> {
    meta.creation_timestamp.as_ref().map(|t| t.0)
}

impl TimelineEvent {
    /// Normalizes a core `v1` Event.
    pub fn from_core(e: &core::Event) -> Self {
        let event_time = e.event_time.as_ref().map(|t| t.0);
        let first = e.first_timestamp.as_ref().map(|t| t.0);
        let last_observed = e
            .series
            .as_ref()
            .and_then(|s| s.last_observed_time.as_ref())
            .map(|t| t.0);
        let time = last_observed
            .or(e.last_timestamp.as_ref().map(|t| t.0))
            .or(event_time)
            .or(first)
            .or_else(|| created(&e.metadata));
        Self {
            api: EventApi::Core,
            name: e.metadata.name.clone().unwrap_or_default(),
            namespace: e.metadata.namespace.clone(),
            uid: e.metadata.uid.clone(),
            time,
            first_time: first.or(event_time).or(time),
            type_: event_type(e.type_.as_deref()),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.message.clone().unwrap_or_default(),
            regarding: InvolvedObject::from_ref(&e.involved_object),
            related: e.related.as_ref().map(InvolvedObject::from_ref),
            reporting_controller: non_empty(&e.reporting_component)
                .or_else(|| e.source.as_ref().and_then(|s| non_empty(&s.component))),
            reporting_instance: non_empty(&e.reporting_instance)
                .or_else(|| e.source.as_ref().and_then(|s| non_empty(&s.host))),
            action: non_empty(&e.action),
            count: count(e.series.as_ref().and_then(|s| s.count), e.count),
        }
    }

    /// Normalizes an `events.k8s.io/v1` Event.
    pub fn from_events_v1(e: &events_v1::Event) -> Self {
        let event_time = e.event_time.as_ref().map(|t| t.0);
        let first = e.deprecated_first_timestamp.as_ref().map(|t| t.0);
        let time = e
            .series
            .as_ref()
            .map(|s| s.last_observed_time.0)
            .or(e.deprecated_last_timestamp.as_ref().map(|t| t.0))
            .or(event_time)
            .or(first)
            .or_else(|| created(&e.metadata));
        let source = e.deprecated_source.as_ref();
        Self {
            api: EventApi::Events,
            name: e.metadata.name.clone().unwrap_or_default(),
            namespace: e.metadata.namespace.clone(),
            uid: e.metadata.uid.clone(),
            time,
            first_time: first.or(event_time).or(time),
            type_: event_type(e.type_.as_deref()),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.note.clone().unwrap_or_default(),
            regarding: e
                .regarding
                .as_ref()
                .map(InvolvedObject::from_ref)
                .unwrap_or_default(),
            related: e.related.as_ref().map(InvolvedObject::from_ref),
            reporting_controller: non_empty(&e.reporting_controller)
                .or_else(|| source.and_then(|s| non_empty(&s.component))),
            reporting_instance: non_empty(&e.reporting_instance)
                .or_else(|| source.and_then(|s| non_empty(&s.host))),
            action: non_empty(&e.action),
            count: count(e.series.as_ref().map(|s| s.count), e.deprecated_count),
        }
    }

    /// Normalizes an Event of either API read as a dynamic object. List
    /// items carry no `apiVersion`/`kind`; then the API is told apart by
    /// its required fields. `None` for other kinds or invalid data.
    pub fn from_dynamic(obj: &DynamicObject) -> Option<Self> {
        match obj.types.as_ref() {
            Some(t) if t.kind != "Event" => None,
            Some(t) => Self::decode(obj, &t.api_version),
            None => {
                let data = obj.data.as_object()?;
                if data.contains_key("involvedObject") {
                    Self::decode(obj, "v1")
                } else if data.contains_key("eventTime") || data.contains_key("regarding") {
                    Self::decode(obj, "events.k8s.io/v1")
                } else {
                    None
                }
            }
        }
    }

    /// Like [`from_dynamic`](Self::from_dynamic), for objects known to be
    /// Events of `kind` (as listed or watched through it).
    pub fn from_dynamic_of(obj: &DynamicObject, kind: &ResourceKind) -> Option<Self> {
        Self::decode(obj, &kind.api.api_version)
    }

    fn decode(obj: &DynamicObject, api_version: &str) -> Option<Self> {
        let value = serde_json::to_value(obj).ok()?;
        if api_version.starts_with("events.k8s.io/") {
            serde_json::from_value(value)
                .ok()
                .map(|e| Self::from_events_v1(&e))
        } else if api_version == "v1" {
            serde_json::from_value(value)
                .ok()
                .map(|e| Self::from_core(&e))
        } else {
            None
        }
    }

    pub fn is_warning(&self) -> bool {
        self.type_ == EventType::Warning
    }

    /// The event as a core `v1` Event, e.g. for [`diagnostics::diagnose`].
    pub fn to_core(&self) -> core::Event {
        core::Event {
            metadata: ObjectMeta {
                name: Some(self.name.clone()),
                namespace: self.namespace.clone(),
                uid: self.uid.clone(),
                ..Default::default()
            },
            involved_object: self.regarding.to_ref(),
            related: self.related.as_ref().map(InvolvedObject::to_ref),
            type_: Some(
                match self.type_ {
                    EventType::Normal => "Normal",
                    EventType::Warning => "Warning",
                }
                .into(),
            ),
            reason: Some(self.reason.clone()),
            message: Some(self.message.clone()),
            first_timestamp: self.first_time.map(Time),
            last_timestamp: self.time.map(Time),
            count: i32::try_from(self.count).ok(),
            reporting_component: self.reporting_controller.clone(),
            reporting_instance: self.reporting_instance.clone(),
            action: self.action.clone(),
            ..Default::default()
        }
    }
}

/// Identifies an object: by uid when both sides have one, else by
/// kind + namespace + name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ObjectKey {
    pub uid: Option<String>,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}

impl ObjectKey {
    pub fn new(kind: &str, namespace: Option<&str>, name: &str) -> Self {
        Self {
            uid: None,
            kind: kind.into(),
            namespace: namespace.map(str::to_owned),
            name: name.into(),
        }
    }

    /// Key of `obj`. List items have no `kind`; use [`of_kind`](Self::of_kind)
    /// to match uid-less events by name for them.
    pub fn of(obj: &DynamicObject) -> Self {
        Self {
            uid: obj.metadata.uid.clone(),
            kind: obj
                .types
                .as_ref()
                .map(|t| t.kind.clone())
                .unwrap_or_default(),
            namespace: obj.metadata.namespace.clone(),
            name: obj.metadata.name.clone().unwrap_or_default(),
        }
    }

    /// Key of `obj`, an object of `kind`.
    pub fn of_kind(obj: &DynamicObject, kind: &ResourceKind) -> Self {
        Self {
            kind: kind.api.kind.clone(),
            ..Self::of(obj)
        }
    }

    pub fn matches(&self, obj: &InvolvedObject) -> bool {
        match (self.uid.as_deref(), obj.uid.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => {
                self.kind == obj.kind
                    && self.name == obj.name
                    && self.namespace.as_deref().filter(|n| !n.is_empty())
                        == obj.namespace.as_deref().filter(|n| !n.is_empty())
            }
        }
    }
}

/// Which events to keep. The default keeps everything.
#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    /// Keep events about any of these objects; empty keeps all.
    pub objects: Vec<ObjectKey>,
    /// Keep events stored in this namespace.
    pub namespace: Option<String>,
    pub warnings_only: bool,
}

impl EventFilter {
    pub fn object(key: ObjectKey) -> Self {
        Self {
            objects: vec![key],
            ..Self::default()
        }
    }

    pub fn matches(&self, e: &TimelineEvent) -> bool {
        (!self.warnings_only || e.is_warning())
            && self
                .namespace
                .as_ref()
                .is_none_or(|ns| e.namespace.as_ref() == Some(ns))
            && (self.objects.is_empty() || self.objects.iter().any(|k| k.matches(&e.regarding)))
    }
}

/// Sorts oldest first (events without a time first), ties by name, and
/// drops duplicates of the same Event object (same uid).
pub fn sort_timeline(events: &mut Vec<TimelineEvent>) {
    events.sort_by(|a, b| (a.time, &a.name).cmp(&(b.time, &b.name)));
    let mut seen = HashSet::new();
    events.retain(|e| e.uid.as_ref().is_none_or(|uid| seen.insert(uid.clone())));
}

/// Events matching `filter`, as a sorted timeline.
pub fn timeline<'a>(
    events: impl IntoIterator<Item = &'a TimelineEvent>,
    filter: &EventFilter,
) -> Vec<TimelineEvent> {
    let mut out: Vec<_> = events
        .into_iter()
        .filter(|e| filter.matches(e))
        .cloned()
        .collect();
    sort_timeline(&mut out);
    out
}

/// Repeats of one event: same object, type, reason and message.
#[derive(Clone, Debug, PartialEq)]
pub struct EventGroup {
    /// The most recent occurrence, shared with the [`EventIndex`] it came
    /// from.
    pub latest: Arc<TimelineEvent>,
    pub first_time: Option<Timestamp>,
    pub last_time: Option<Timestamp>,
    /// Sum of the events' counts.
    pub count: u32,
    /// Number of Event objects merged into the group.
    pub events: usize,
    /// Hash of the [`key`](Self::key), computed once.
    key_hash: u64,
}

/// Identity of an [`EventGroup`]: object (without the container), type,
/// reason and message. Stays the same while the group gets new occurrences.
pub type GroupKey = (InvolvedObject, EventType, String, String);

fn group_key(e: &TimelineEvent) -> GroupKey {
    // Container-level events of one object belong together.
    let object = InvolvedObject {
        field_path: None,
        ..e.regarding.clone()
    };
    (object, e.type_, e.reason.clone(), e.message.clone())
}

impl EventGroup {
    pub fn key(&self) -> GroupKey {
        group_key(&self.latest)
    }

    /// The [`key`](Self::key) borrowed, to compare without cloning.
    pub fn key_ref(&self) -> GroupKeyRef<'_> {
        GroupKeyRef::of(&self.latest)
    }

    /// A hash of the [`key`](Self::key), the same for equal keys; cheap.
    pub fn key_hash(&self) -> u64 {
        self.key_hash
    }
}

/// A group in a hash map by its key, hashed once.
struct ByKey<'a>(&'a EventGroup);

impl Hash for ByKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.key_hash);
    }
}

impl PartialEq for ByKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.key_hash == other.0.key_hash && self.0.key_ref() == other.0.key_ref()
    }
}

impl Eq for ByKey<'_> {}

/// [`GroupKey`] borrowed from an event, to group without cloning. Equal
/// exactly when the [`GroupKey`]s are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupKeyRef<'a> {
    api_version: Option<&'a str>,
    kind: &'a str,
    namespace: Option<&'a str>,
    name: &'a str,
    uid: Option<&'a str>,
    type_: EventType,
    reason: &'a str,
    message: &'a str,
}

impl<'a> GroupKeyRef<'a> {
    fn of(e: &'a TimelineEvent) -> Self {
        let r = &e.regarding;
        Self {
            api_version: r.api_version.as_deref(),
            kind: &r.kind,
            namespace: r.namespace.as_deref(),
            name: &r.name,
            uid: r.uid.as_deref(),
            type_: e.type_,
            reason: &e.reason,
            message: &e.message,
        }
    }
}

/// Groups repeated events, ordered by their last occurrence (oldest first).
pub fn group<'a>(events: impl IntoIterator<Item = &'a TimelineEvent>) -> Vec<EventGroup> {
    group_with(events, |e| Arc::new(e.clone()))
}

/// [`group`] for shared events: the groups point at them.
pub fn group_shared<'a>(
    events: impl IntoIterator<Item = &'a Arc<TimelineEvent>>,
) -> Vec<EventGroup> {
    group_with(events, Arc::clone)
}

/// Group being collected: the latest event is shared only at the end.
struct Partial<'a, T> {
    latest: &'a T,
    first_time: Option<Timestamp>,
    last_time: Option<Timestamp>,
    count: u32,
    events: usize,
}

fn group_with<'a, T: AsRef<TimelineEvent> + 'a>(
    events: impl IntoIterator<Item = &'a T>,
    share: impl Fn(&T) -> Arc<TimelineEvent>,
) -> Vec<EventGroup> {
    let mut index: HashMap<GroupKeyRef<'a>, usize> = HashMap::new();
    let mut groups: Vec<Partial<'a, T>> = Vec::new();
    for item in events {
        let e = item.as_ref();
        match index.entry(GroupKeyRef::of(e)) {
            Entry::Occupied(slot) => {
                let g = &mut groups[*slot.get()];
                g.count = g.count.saturating_add(e.count);
                g.events += 1;
                g.first_time = min_time(g.first_time, e.first_time);
                if e.time >= g.last_time {
                    g.last_time = e.time;
                    g.latest = item;
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(groups.len());
                groups.push(Partial {
                    latest: item,
                    first_time: e.first_time,
                    last_time: e.time,
                    count: e.count,
                    events: 1,
                });
            }
        }
    }
    groups.sort_by(|a, b| {
        (a.last_time, &a.latest.as_ref().name).cmp(&(b.last_time, &b.latest.as_ref().name))
    });
    groups
        .into_iter()
        .map(|g| {
            let mut hasher = DefaultHasher::new();
            GroupKeyRef::of(g.latest.as_ref()).hash(&mut hasher);
            EventGroup {
                latest: share(g.latest),
                first_time: g.first_time,
                last_time: g.last_time,
                count: g.count,
                events: g.events,
                key_hash: hasher.finish(),
            }
        })
        .collect()
}

/// Groups of events matching `filter`, most recent first, at most `limit`.
pub fn recent_groups<'a>(
    events: impl IntoIterator<Item = &'a TimelineEvent>,
    filter: &EventFilter,
    limit: usize,
) -> Vec<EventGroup> {
    let mut groups = group(events.into_iter().filter(|e| filter.matches(e)));
    groups.reverse();
    groups.truncate(limit);
    groups
}

/// A live timeline (newest first) after an update; see [`merge_live`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveGroups {
    pub groups: Vec<EventGroup>,
    /// New groups not shown yet because the list is held.
    pub pending: usize,
}

/// Applies a fresh list of groups (newest first, as from [`recent_groups`])
/// to the one on screen.
///
/// Not held, the fresh list is shown as is. Held (the user scrolled into the
/// list), rows must not move under the pointer: shown groups stay in place
/// and only take their new count, time and message; groups that are gone
/// (expired events) are dropped; new groups are not inserted but counted in
/// [`LiveGroups::pending`] until the list is released.
pub fn merge_live(shown: &[EventGroup], fresh: Vec<EventGroup>, hold: bool) -> LiveGroups {
    if !hold || shown.is_empty() {
        return LiveGroups {
            groups: fresh,
            pending: 0,
        };
    }
    // Matched by the keys' hashes, computed with the groups.
    let picks: Vec<usize> = {
        let index: HashMap<ByKey, usize> = fresh
            .iter()
            .enumerate()
            .map(|(ix, g)| (ByKey(g), ix))
            .collect();
        shown
            .iter()
            .filter_map(|g| index.get(&ByKey(g)).copied())
            .collect()
    };
    let mut fresh: Vec<Option<EventGroup>> = fresh.into_iter().map(Some).collect();
    let groups: Vec<EventGroup> = picks
        .into_iter()
        .filter_map(|ix| fresh[ix].take())
        .collect();
    LiveGroups {
        groups,
        pending: fresh.iter().flatten().count(),
    }
}

/// Identity of an Event object in an [`EventIndex`]: its uid, else
/// namespace and name (behind a NUL, which no uid contains).
fn event_id(obj: &DynamicObject) -> Cow<'_, str> {
    match &obj.metadata.uid {
        Some(uid) => Cow::Borrowed(uid),
        None => Cow::Owned(format!(
            "\0{}\0{}",
            obj.metadata.namespace.as_deref().unwrap_or_default(),
            obj.metadata.name.as_deref().unwrap_or_default()
        )),
    }
}

/// An object as last seen and what it decoded to (`None`: not an Event).
#[derive(Clone)]
struct Decoded {
    object: Arc<DynamicObject>,
    event: Option<Arc<TimelineEvent>>,
}

impl Decoded {
    /// Whether `obj` is the version decoded: the same object, or the same
    /// `resourceVersion`.
    fn is(&self, obj: &Arc<DynamicObject>) -> bool {
        Arc::ptr_eq(&self.object, obj)
            || self.object.metadata.resource_version.is_some()
                && self.object.metadata.resource_version == obj.metadata.resource_version
    }
}

/// Events of a watch (one Event kind), decoded incrementally.
///
/// [`update`](Self::update) decodes only the objects that are new or have a
/// new `resourceVersion` and forgets the ones that are gone; decoding the
/// JSON is the costly part of a refresh. The timeline and its groups are
/// rebuilt from the decoded events, only when something changed, and read
/// through a cheap [`TimelineView`].
#[derive(Clone, Default)]
pub struct EventIndex {
    decoded: HashMap<String, Decoded>,
    view: TimelineView,
}

impl EventIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Brings the index to `objects`, the current objects of the watch of
    /// `kind`. Returns whether the timeline changed.
    pub fn update(&mut self, objects: &[Arc<DynamicObject>], kind: &ResourceKind) -> bool {
        let mut previous = std::mem::take(&mut self.decoded);
        self.decoded.reserve(objects.len());
        let mut changed = false;
        for obj in objects {
            let id = event_id(obj);
            let (id, decoded) = match previous.remove_entry(id.as_ref()) {
                Some((id, decoded)) if decoded.is(obj) => (id, decoded),
                found => {
                    changed = true;
                    let id = found.map_or_else(|| id.into_owned(), |(id, _)| id);
                    let event = TimelineEvent::from_dynamic_of(obj, kind).map(Arc::new);
                    let object = obj.clone();
                    (id, Decoded { object, event })
                }
            };
            if self.decoded.insert(id, decoded).is_some() {
                // Seen twice (two watches); the last one wins.
                changed = true;
            }
        }
        changed |= !previous.is_empty();
        if changed {
            self.view = TimelineView::new(self.decoded.values().filter_map(|d| d.event.clone()));
        }
        changed
    }

    /// The timeline as of the last update.
    pub fn view(&self) -> &TimelineView {
        &self.view
    }

    /// Events, sorted oldest first.
    pub fn events(&self) -> &[Arc<TimelineEvent>] {
        self.view.events()
    }

    /// Groups of all events, oldest first.
    pub fn groups(&self) -> &[EventGroup] {
        self.view.groups()
    }

    pub fn len(&self) -> usize {
        self.view.len()
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Number of `Warning` events.
    pub fn warnings(&self) -> usize {
        self.view.warnings()
    }

    /// See [`TimelineView::recent_groups`].
    pub fn recent_groups(&self, filter: &EventFilter, limit: usize) -> Vec<EventGroup> {
        self.view.recent_groups(filter, limit)
    }
}

/// Sorted events and their groups, as an [`EventIndex`] last built them.
/// Cheap to clone; can be read while the index is updated elsewhere.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineView {
    /// Sorted oldest first, as [`sort_timeline`] orders a store snapshot.
    events: Arc<[Arc<TimelineEvent>]>,
    /// All groups, oldest first, as [`group`] makes them.
    groups: Arc<[EventGroup]>,
    warnings: usize,
}

impl TimelineView {
    fn new(events: impl IntoIterator<Item = Arc<TimelineEvent>>) -> Self {
        let mut events: Vec<_> = events.into_iter().collect();
        // Store snapshots are sorted by namespace and name, which
        // `sort_timeline` keeps for ties; ids are unique, so are uids.
        events.sort_by(|a, b| {
            (a.time, &a.name, &a.namespace, &a.uid).cmp(&(b.time, &b.name, &b.namespace, &b.uid))
        });
        Self {
            warnings: events.iter().filter(|e| e.is_warning()).count(),
            groups: group_shared(&events).into(),
            events: events.into(),
        }
    }

    /// Events, sorted oldest first.
    pub fn events(&self) -> &[Arc<TimelineEvent>] {
        &self.events
    }

    /// Groups of all events, oldest first.
    pub fn groups(&self) -> &[EventGroup] {
        &self.groups
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Number of `Warning` events.
    pub fn warnings(&self) -> usize {
        self.warnings
    }

    /// Like [`recent_groups`] over [`events`](Self::events): groups matching
    /// `filter`, most recent first, at most `limit`. Without a namespace in
    /// the filter, the groups are taken from the ready ones: the type and
    /// the object an event is about are part of the group key, so the filter
    /// keeps or drops whole groups.
    pub fn recent_groups(&self, filter: &EventFilter, limit: usize) -> Vec<EventGroup> {
        if filter.namespace.is_some() {
            return recent_groups(self.events.iter().map(AsRef::as_ref), filter, limit);
        }
        self.groups
            .iter()
            .rev()
            .filter(|g| filter.matches(&g.latest))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn min_time(a: Option<Timestamp>, b: Option<Timestamp>) -> Option<Timestamp> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `root` and every object it owns, transitively, among `objects` (matched
/// through `metadata.ownerReferences` uids).
pub fn owned_tree<'a>(
    root: &DynamicObject,
    objects: impl IntoIterator<Item = &'a DynamicObject>,
) -> Vec<ObjectKey> {
    let mut children: HashMap<&str, Vec<&DynamicObject>> = HashMap::new();
    for obj in objects {
        for owner in obj.metadata.owner_references.iter().flatten() {
            children.entry(owner.uid.as_str()).or_default().push(obj);
        }
    }
    let mut out = vec![ObjectKey::of(root)];
    let mut queued: HashSet<&str> = root.metadata.uid.as_deref().into_iter().collect();
    let mut queue: VecDeque<&str> = queued.iter().copied().collect();
    while let Some(uid) = queue.pop_front() {
        for child in children.get(uid).into_iter().flatten() {
            // Objects without a uid cannot own anything; take them as leaves.
            if let Some(child_uid) = child.metadata.uid.as_deref() {
                if !queued.insert(child_uid) {
                    continue;
                }
                queue.push_back(child_uid);
            }
            out.push(ObjectKey::of(child));
        }
    }
    out
}

/// Timeline of `root` and the objects it owns (e.g. Deployment →
/// ReplicaSets → Pods), given candidate `objects` and `events`.
pub fn tree_timeline<'a>(
    root: &DynamicObject,
    objects: impl IntoIterator<Item = &'a DynamicObject>,
    events: impl IntoIterator<Item = &'a TimelineEvent>,
    warnings_only: bool,
) -> Vec<TimelineEvent> {
    let filter = EventFilter {
        objects: owned_tree(root, objects),
        namespace: None,
        warnings_only,
    };
    timeline(events, &filter)
}

/// Diagnoses `pod` including findings from its events.
pub fn diagnose_pod(pod: &Pod, events: &[TimelineEvent]) -> Vec<Finding> {
    diagnostics::diagnose(pod, &core_events(events))
}

/// Pod health as the Pod plugin computes it, plus event-based findings
/// (`FailedMount`, `FailedCreatePodSandBox`).
pub fn pod_health(pod: &Pod, events: &[TimelineEvent], now: Timestamp) -> Health {
    pod_health_with_events(pod, &core_events(events), now)
}

fn core_events(events: &[TimelineEvent]) -> Vec<core::Event> {
    events.iter().map(TimelineEvent::to_core).collect()
}

/// The Event kind to read: `events.k8s.io` when served, else core `v1`.
pub fn events_kind(catalog: &ApiCatalog) -> Option<&ResourceKind> {
    catalog
        .find("events.k8s.io", "Event")
        .or_else(|| catalog.find("", "Event"))
}

/// Lists events in `namespace` (all when `None`) once, as a sorted timeline.
pub async fn list(
    client: &Client,
    catalog: &ApiCatalog,
    namespace: Option<&str>,
) -> Result<Vec<TimelineEvent>, EventsError> {
    let kind = events_kind(catalog).ok_or(EventsError::NotServed)?;
    let objects = discovery::list(client, kind, namespace)
        .await
        .map_err(Box::new)?;
    let mut events: Vec<_> = objects
        .iter()
        .filter_map(|o| TimelineEvent::from_dynamic_of(o, kind))
        .collect();
    sort_timeline(&mut events);
    Ok(events)
}

/// Live events of a namespace (or all namespaces), backed by a
/// [`ResourceCache`] subscription; the watch stops when the feed is dropped.
/// Clones share one [`EventIndex`], so each change is decoded once.
#[derive(Clone)]
pub struct EventFeed {
    subscription: Subscription,
    kind: ResourceKind,
    index: Arc<Mutex<EventIndex>>,
}

/// Events of an [`EventFeed`] at one point in time.
#[derive(Clone, Debug)]
pub struct EventsSnapshot {
    /// Sorted oldest first.
    pub events: Vec<TimelineEvent>,
    pub status: WatchStatus,
    pub generation: u64,
}

/// Groups of an [`EventFeed`] at one point in time.
#[derive(Clone, Debug)]
pub struct GroupsSnapshot {
    /// Most recent first; see [`EventIndex::recent_groups`].
    pub groups: Vec<EventGroup>,
    pub status: WatchStatus,
    pub generation: u64,
}

impl EventFeed {
    pub fn subscribe(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        namespace: Option<&str>,
    ) -> Result<Self, EventsError> {
        let kind = events_kind(catalog).ok_or(EventsError::NotServed)?;
        Ok(Self {
            subscription: cache.subscribe(kind, namespace),
            kind: kind.clone(),
            index: Arc::default(),
        })
    }

    /// Updates the shared index to the subscription and reads it.
    fn read<R>(&self, f: impl FnOnce(&EventIndex) -> R) -> (R, WatchStatus, u64) {
        let snapshot = self.subscription.snapshot();
        let mut index = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        index.update(&snapshot.objects, &self.kind);
        (f(&index), snapshot.status, snapshot.generation)
    }

    pub fn snapshot(&self) -> EventsSnapshot {
        let (events, status, generation) =
            self.read(|index| index.events().iter().map(|e| (**e).clone()).collect());
        EventsSnapshot {
            events,
            status,
            generation,
        }
    }

    /// Groups matching `filter`, most recent first, at most `limit`.
    pub fn recent_groups(&self, filter: &EventFilter, limit: usize) -> GroupsSnapshot {
        let (groups, status, generation) = self.read(|index| index.recent_groups(filter, limit));
        GroupsSnapshot {
            groups,
            status,
            generation,
        }
    }

    /// See [`Subscription::changes`].
    pub fn changes(&self) -> Changes {
        self.subscription.changes()
    }
}

#[cfg(test)]
mod tests;
