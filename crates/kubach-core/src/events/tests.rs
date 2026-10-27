use super::*;
use crate::plugin::HealthStatus;
use kube::api::{ApiResource, GroupVersionKind};
use kube::discovery::{ApiCapabilities, Scope, verbs};
use serde_json::{Value, json};

fn dynamic(value: Value) -> DynamicObject {
    serde_json::from_value(value).unwrap()
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn core_event(name: &str, extra: Value) -> TimelineEvent {
    let mut value = json!({
        "apiVersion": "v1",
        "kind": "Event",
        "metadata": { "name": name, "namespace": "web", "uid": format!("uid-{name}") },
        "involvedObject": { "kind": "Pod", "namespace": "web", "name": "api-1", "uid": "pod-1" },
        "type": "Warning",
        "reason": "BackOff",
        "message": "Back-off restarting failed container",
    });
    merge(&mut value, extra);
    TimelineEvent::from_dynamic(&dynamic(value)).unwrap()
}

fn merge(base: &mut Value, extra: Value) {
    if let (Value::Object(base), Value::Object(extra)) = (base, extra) {
        for (k, v) in extra {
            base.insert(k, v);
        }
    }
}

#[test]
fn normalizes_core_events() {
    let e = core_event(
        "a",
        json!({
            "firstTimestamp": "2026-10-22T10:00:00Z",
            "lastTimestamp": "2026-10-22T10:05:00Z",
            "count": 7,
            "source": { "component": "kubelet", "host": "node-1" },
            "involvedObject": {
                "kind": "Pod", "namespace": "web", "name": "api-1", "uid": "pod-1",
                "fieldPath": "spec.containers{api}"
            }
        }),
    );
    assert_eq!(e.api, EventApi::Core);
    assert_eq!(e.type_, EventType::Warning);
    assert_eq!(e.reason, "BackOff");
    assert_eq!(e.message, "Back-off restarting failed container");
    assert_eq!(e.time, Some(ts("2026-10-22T10:05:00Z")));
    assert_eq!(e.first_time, Some(ts("2026-10-22T10:00:00Z")));
    assert_eq!(e.count, 7);
    assert_eq!(e.reporting_controller.as_deref(), Some("kubelet"));
    assert_eq!(e.reporting_instance.as_deref(), Some("node-1"));
    assert_eq!(e.regarding.kind, "Pod");
    assert_eq!(e.regarding.uid.as_deref(), Some("pod-1"));
    assert_eq!(
        e.regarding.field_path.as_deref(),
        Some("spec.containers{api}")
    );
}

#[test]
fn core_series_wins_over_timestamps() {
    let e = core_event(
        "a",
        json!({
            "eventTime": "2026-10-22T10:00:00.000000Z",
            "series": { "count": 4, "lastObservedTime": "2026-10-22T10:09:00.000000Z" },
            "count": 1,
            "reportingComponent": "kubelet",
        }),
    );
    assert_eq!(e.time, Some(ts("2026-10-22T10:09:00Z")));
    assert_eq!(e.first_time, Some(ts("2026-10-22T10:00:00Z")));
    assert_eq!(e.count, 4);
}

#[test]
fn normalizes_events_k8s_io() {
    let e = TimelineEvent::from_dynamic(&dynamic(json!({
        "apiVersion": "events.k8s.io/v1",
        "kind": "Event",
        "metadata": { "name": "b", "namespace": "web", "uid": "uid-b" },
        "eventTime": "2026-10-22T10:00:00.000000Z",
        "series": { "count": 3, "lastObservedTime": "2026-10-22T10:02:00.000000Z" },
        "type": "Normal",
        "reason": "Pulled",
        "note": "Successfully pulled image",
        "action": "Pulling",
        "regarding": { "kind": "Pod", "namespace": "web", "name": "api-1", "uid": "pod-1" },
        "related": { "kind": "Node", "name": "node-1" },
        "reportingController": "kubelet",
        "reportingInstance": "node-1",
    })))
    .unwrap();
    assert_eq!(e.api, EventApi::Events);
    assert_eq!(e.type_, EventType::Normal);
    assert_eq!(e.message, "Successfully pulled image");
    assert_eq!(e.time, Some(ts("2026-10-22T10:02:00Z")));
    assert_eq!(e.first_time, Some(ts("2026-10-22T10:00:00Z")));
    assert_eq!(e.count, 3);
    assert_eq!(e.related.as_ref().unwrap().kind, "Node");
    assert_eq!(e.reporting_controller.as_deref(), Some("kubelet"));
    assert_eq!(e.action.as_deref(), Some("Pulling"));
}

#[test]
fn events_k8s_io_falls_back_to_deprecated_fields() {
    let e = TimelineEvent::from_dynamic(&dynamic(json!({
        "apiVersion": "events.k8s.io/v1",
        "kind": "Event",
        "metadata": { "name": "c", "namespace": "web" },
        "eventTime": null,
        "deprecatedFirstTimestamp": "2026-10-22T09:00:00Z",
        "deprecatedLastTimestamp": "2026-10-22T09:30:00Z",
        "deprecatedCount": 12,
        "deprecatedSource": { "component": "default-scheduler" },
        "type": "Warning",
        "reason": "FailedScheduling",
        "regarding": { "kind": "Pod", "namespace": "web", "name": "api-1" },
    })))
    .unwrap();
    assert_eq!(e.time, Some(ts("2026-10-22T09:30:00Z")));
    assert_eq!(e.first_time, Some(ts("2026-10-22T09:00:00Z")));
    assert_eq!(e.count, 12);
    assert_eq!(e.reporting_controller.as_deref(), Some("default-scheduler"));
}

#[test]
fn falls_back_to_creation_time_and_count_one() {
    let e = core_event(
        "a",
        json!({ "metadata": { "name": "a", "creationTimestamp": "2026-10-22T08:00:00Z" } }),
    );
    assert_eq!(e.time, Some(ts("2026-10-22T08:00:00Z")));
    assert_eq!(e.count, 1);
}

#[test]
fn decodes_list_items_without_types() {
    let core = dynamic(json!({
        "metadata": { "name": "a" },
        "involvedObject": { "kind": "Pod", "name": "p" },
        "reason": "Started",
    }));
    assert_eq!(
        TimelineEvent::from_dynamic(&core).unwrap().api,
        EventApi::Core
    );
    let new = dynamic(json!({
        "metadata": { "name": "b" },
        "eventTime": "2026-10-22T10:00:00.000000Z",
        "regarding": { "kind": "Pod", "name": "p" },
        "note": "hi",
    }));
    let e = TimelineEvent::from_dynamic(&new).unwrap();
    assert_eq!((e.api, e.message.as_str()), (EventApi::Events, "hi"));
    let pod = dynamic(json!({ "metadata": { "name": "p" }, "spec": {} }));
    assert!(TimelineEvent::from_dynamic(&pod).is_none());
}

#[test]
fn ignores_other_kinds() {
    let pod = dynamic(json!({ "apiVersion": "v1", "kind": "Pod", "metadata": { "name": "p" } }));
    assert!(TimelineEvent::from_dynamic(&pod).is_none());
}

#[test]
fn filters_by_object_namespace_and_type() {
    let warning = core_event("w", json!({}));
    let normal = core_event("n", json!({ "type": "Normal", "reason": "Started" }));
    let other = core_event(
        "o",
        json!({
            "metadata": { "name": "o", "namespace": "db" },
            "involvedObject": { "kind": "Pod", "namespace": "db", "name": "pg-0", "uid": "pod-2" }
        }),
    );
    let all = [warning.clone(), normal.clone(), other.clone()];

    let by_uid = EventFilter::object(ObjectKey {
        uid: Some("pod-1".into()),
        ..ObjectKey::new("Pod", Some("web"), "renamed")
    });
    assert_eq!(timeline(&all, &by_uid).len(), 2);

    let by_name = EventFilter::object(ObjectKey::new("Pod", Some("db"), "pg-0"));
    assert_eq!(timeline(&all, &by_name), vec![other]);

    let warnings = EventFilter {
        namespace: Some("web".into()),
        warnings_only: true,
        ..EventFilter::default()
    };
    assert_eq!(timeline(&all, &warnings), vec![warning]);
}

#[test]
fn timeline_is_sorted_and_deduplicated() {
    let late = core_event("late", json!({ "lastTimestamp": "2026-10-22T11:00:00Z" }));
    let early = core_event("early", json!({ "lastTimestamp": "2026-10-22T09:00:00Z" }));
    let undated = core_event("undated", json!({}));
    let all = [late.clone(), early.clone(), late.clone(), undated.clone()];
    let names: Vec<_> = timeline(&all, &EventFilter::default())
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["undated", "early", "late"]);
}

#[test]
fn groups_repeated_events() {
    let a = core_event(
        "a",
        json!({
            "firstTimestamp": "2026-10-22T09:00:00Z",
            "lastTimestamp": "2026-10-22T09:10:00Z",
            "count": 3,
            "involvedObject": {
                "kind": "Pod", "namespace": "web", "name": "api-1", "uid": "pod-1",
                "fieldPath": "spec.containers{api}"
            }
        }),
    );
    let b = core_event(
        "b",
        json!({
            "firstTimestamp": "2026-10-22T09:20:00Z",
            "lastTimestamp": "2026-10-22T09:30:00Z",
            "count": 2,
        }),
    );
    let other = core_event(
        "c",
        json!({ "reason": "Failed", "lastTimestamp": "2026-10-22T09:15:00Z" }),
    );
    let groups = group(&[b.clone(), a, other]);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].latest.reason, "Failed");
    let backoff = &groups[1];
    assert_eq!(backoff.count, 5);
    assert_eq!(backoff.events, 2);
    assert_eq!(backoff.first_time, Some(ts("2026-10-22T09:00:00Z")));
    assert_eq!(backoff.last_time, Some(ts("2026-10-22T09:30:00Z")));
    assert_eq!(*backoff.latest, b);
}

fn owned(kind: &str, name: &str, uid: &str, owner: Option<&str>) -> DynamicObject {
    let mut meta = json!({ "name": name, "namespace": "web", "uid": uid });
    if let Some(owner) = owner {
        meta["ownerReferences"] =
            json!([{ "apiVersion": "v1", "kind": "X", "name": "x", "uid": owner }]);
    }
    dynamic(json!({ "apiVersion": "apps/v1", "kind": kind, "metadata": meta }))
}

fn about(name: &str, kind: &str, obj: &str, uid: &str) -> TimelineEvent {
    core_event(
        name,
        json!({ "involvedObject": { "kind": kind, "namespace": "web", "name": obj, "uid": uid } }),
    )
}

#[test]
fn collects_events_of_the_owner_tree() {
    let deploy = owned("Deployment", "api", "d1", None);
    let rs = owned("ReplicaSet", "api-7f", "rs1", Some("d1"));
    let old_rs = owned("ReplicaSet", "api-6c", "rs0", Some("d1"));
    let pod = owned("Pod", "api-7f-x", "p1", Some("rs1"));
    let stranger = owned("Pod", "db-0", "p9", Some("ss1"));
    let objects = [rs.clone(), pod.clone(), old_rs.clone(), stranger.clone()];

    let keys: Vec<_> = owned_tree(&deploy, &objects)
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(keys, ["api", "api-7f", "api-6c", "api-7f-x"]);

    let events = [
        about("e1", "Deployment", "api", "d1"),
        about("e2", "ReplicaSet", "api-7f", "rs1"),
        about("e3", "Pod", "api-7f-x", "p1"),
        about("e4", "Pod", "db-0", "p9"),
    ];
    let tree = tree_timeline(&deploy, &objects, &events, false);
    let names: Vec<_> = tree.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["e1", "e2", "e3"]);

    // A pod's tree is just the pod.
    let tree = tree_timeline(&pod, &objects, &events, false);
    assert_eq!(tree.len(), 1);
}

#[test]
fn owner_cycles_terminate() {
    let a = owned("X", "a", "a", Some("b"));
    let b = owned("X", "b", "b", Some("a"));
    let keys = owned_tree(&a, [&a, &b]);
    assert_eq!(keys.len(), 2);
}

fn pending_pod() -> Pod {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": "api-1", "namespace": "web", "uid": "pod-1" },
        "spec": { "containers": [{ "name": "api", "image": "api:1" }] },
        "status": {
            "phase": "Pending",
            "conditions": [{ "type": "PodScheduled", "status": "True" }],
            "containerStatuses": [{
                "name": "api", "image": "api:1", "imageID": "", "ready": false,
                "restartCount": 0,
                "state": { "waiting": { "reason": "ContainerCreating" } }
            }]
        }
    }))
    .unwrap()
}

#[test]
fn event_findings_affect_pod_health() {
    let pod = pending_pod();
    let now = ts("2026-10-22T12:00:00Z");
    assert_eq!(pod_health(&pod, &[], now).status, HealthStatus::Progressing);

    let mount = TimelineEvent::from_dynamic(&dynamic(json!({
        "apiVersion": "events.k8s.io/v1",
        "kind": "Event",
        "metadata": { "name": "m", "namespace": "web" },
        "eventTime": "2026-10-22T11:59:00.000000Z",
        "type": "Warning",
        "reason": "FailedMount",
        "note": "MountVolume.SetUp failed for volume \"cfg\" : configmap \"app-config\" not found",
        "regarding": { "kind": "Pod", "namespace": "web", "name": "api-1", "uid": "pod-1" },
    })))
    .unwrap();
    let health = pod_health(&pod, std::slice::from_ref(&mount), now);
    assert_eq!(health.status, HealthStatus::Failed);
    assert_eq!(health.message, "A volume cannot be mounted");
    let findings = diagnose_pod(&pod, &[mount]);
    assert_eq!(
        findings[0].kind,
        crate::diagnostics::FindingKind::VolumeMountFailed
    );
}

fn kind(group: &str) -> (ApiResource, ApiCapabilities) {
    let gvk = GroupVersionKind::gvk(group, "v1", "Event");
    (
        ApiResource::from_gvk_with_plural(&gvk, "events"),
        ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec![verbs::LIST.into(), verbs::WATCH.into()],
        },
    )
}

#[test]
fn prefers_events_k8s_io() {
    let both = ApiCatalog::from_resources([kind(""), kind("events.k8s.io")]);
    assert_eq!(events_kind(&both).unwrap().api.group, "events.k8s.io");
    let core = ApiCatalog::from_resources([kind("")]);
    assert_eq!(events_kind(&core).unwrap().api.group, "");
    assert!(events_kind(&ApiCatalog::default()).is_none());
}

#[test]
fn recent_groups_are_newest_first_filtered_and_limited() {
    let old = core_event("old", json!({ "lastTimestamp": "2026-10-22T09:00:00Z" }));
    let normal = core_event(
        "normal",
        json!({ "type": "Normal", "reason": "Pulled", "lastTimestamp": "2026-10-22T09:30:00Z" }),
    );
    let new = core_event(
        "new",
        json!({ "reason": "Failed", "lastTimestamp": "2026-10-22T10:00:00Z" }),
    );
    let events = [old, normal, new];
    let all = recent_groups(&events, &EventFilter::default(), 10);
    let reasons: Vec<_> = all.iter().map(|g| g.latest.reason.as_str()).collect();
    assert_eq!(reasons, ["Failed", "Pulled", "BackOff"]);

    let warnings = EventFilter {
        warnings_only: true,
        ..EventFilter::default()
    };
    let limited = recent_groups(&events, &warnings, 1);
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].latest.reason, "Failed");
}

#[test]
fn live_updates_keep_rows_in_place_while_held() {
    let at = |name: &str, reason: &str, time: &str, count: u32| {
        core_event(
            name,
            json!({ "reason": reason, "lastTimestamp": time, "count": count }),
        )
    };
    let filter = EventFilter::default();
    let first = [
        at("a", "BackOff", "2026-10-22T09:00:00Z", 1),
        at("b", "Failed", "2026-10-22T09:30:00Z", 1),
    ];
    let shown = merge_live(&[], recent_groups(&first, &filter, 10), true);
    let reasons = |live: &LiveGroups| -> Vec<(String, u32)> {
        live.groups
            .iter()
            .map(|g| (g.latest.reason.clone(), g.count))
            .collect()
    };
    assert_eq!(
        reasons(&shown),
        [("Failed".into(), 1), ("BackOff".into(), 1)]
    );

    // BackOff repeats (it would move to the top), a new group appears and
    // Failed expires.
    let second = [
        at("a", "BackOff", "2026-10-22T10:00:00Z", 5),
        at("c", "Unhealthy", "2026-10-22T09:45:00Z", 1),
    ];
    let fresh = recent_groups(&second, &filter, 10);
    let held = merge_live(&shown.groups, fresh.clone(), true);
    assert_eq!(reasons(&held), [("BackOff".into(), 5)]);
    assert_eq!(held.pending, 1);

    // Released, the list is the fresh one, newest first.
    let released = merge_live(&held.groups, fresh, false);
    assert_eq!(
        reasons(&released),
        [("BackOff".into(), 5), ("Unhealthy".into(), 1)]
    );
    assert_eq!(released.pending, 0);
}

#[test]
fn group_keys_ignore_the_container_and_occurrences() {
    let a = core_event(
        "a",
        json!({ "involvedObject": { "kind": "Pod", "namespace": "web", "name": "api-1",
                "uid": "pod-1", "fieldPath": "spec.containers{api}" }, "count": 2 }),
    );
    let b = core_event("b", json!({ "count": 7 }));
    let ga = group(&[a]);
    let gb = group(&[b]);
    assert_eq!(ga[0].key(), gb[0].key());
}

/// Core `v1` Event objects as a watch holds them, for [`EventIndex`].
struct Store {
    objects: std::collections::BTreeMap<(String, String), DynamicObject>,
    version: u64,
}

impl Store {
    fn new() -> Self {
        Self {
            objects: Default::default(),
            version: 0,
        }
    }

    fn put(&mut self, ns: &str, name: &str, body: Value) {
        self.version += 1;
        let mut value = json!({
            "apiVersion": "v1",
            "kind": "Event",
            "metadata": {
                "name": name,
                "namespace": ns,
                "uid": format!("uid-{ns}-{name}"),
                "resourceVersion": self.version.to_string(),
            },
            "involvedObject": {
                "kind": "Pod", "namespace": ns, "name": "api-1", "uid": format!("pod-{ns}")
            },
            "type": "Warning",
            "reason": "BackOff",
            "message": "Back-off restarting failed container",
        });
        merge(&mut value, body);
        self.objects
            .insert((ns.into(), name.into()), dynamic(value));
    }

    fn delete(&mut self, ns: &str, name: &str) {
        self.objects.remove(&(ns.to_owned(), name.to_owned()));
    }

    /// Fresh `Arc`s each time, as after a relist.
    fn snapshot(&self) -> Vec<Arc<DynamicObject>> {
        self.objects.values().cloned().map(Arc::new).collect()
    }
}

fn core_kind() -> ResourceKind {
    let catalog = ApiCatalog::from_resources([kind("")]);
    events_kind(&catalog).unwrap().clone()
}

/// What a refresh computed from scratch gives.
fn full(objects: &[Arc<DynamicObject>], kind: &ResourceKind) -> Vec<TimelineEvent> {
    let mut events: Vec<_> = objects
        .iter()
        .filter_map(|o| TimelineEvent::from_dynamic_of(o, kind))
        .collect();
    sort_timeline(&mut events);
    events
}

fn assert_same(index: &EventIndex, objects: &[Arc<DynamicObject>], kind: &ResourceKind) {
    let expected = full(objects, kind);
    let events: Vec<TimelineEvent> = index.events().iter().map(|e| (**e).clone()).collect();
    assert_eq!(events, expected);
    assert_eq!(index.groups(), group(&expected).as_slice());
    assert_eq!(
        index.warnings(),
        expected.iter().filter(|e| e.is_warning()).count()
    );
    let filters = [
        EventFilter::default(),
        EventFilter {
            warnings_only: true,
            ..EventFilter::default()
        },
        EventFilter::object(ObjectKey::new("Pod", Some("a"), "api-1")),
        EventFilter {
            namespace: Some("b".into()),
            ..EventFilter::default()
        },
    ];
    for filter in &filters {
        for limit in [3, 1000] {
            assert_eq!(
                index.recent_groups(filter, limit),
                recent_groups(&expected, filter, limit),
                "{filter:?} {limit}"
            );
        }
    }
}

#[test]
fn index_decodes_only_changed_events() {
    let kind = core_kind();
    let mut store = Store::new();
    store.put(
        "a",
        "e1",
        json!({ "lastTimestamp": "2026-10-22T09:00:00Z", "count": 1 }),
    );
    store.put(
        "a",
        "e2",
        json!({ "reason": "Failed", "lastTimestamp": "2026-10-22T09:10:00Z" }),
    );
    let mut index = EventIndex::new();
    assert!(index.update(&store.snapshot(), &kind));
    assert_same(&index, &store.snapshot(), &kind);
    let before: Vec<_> = index.events().to_vec();

    // Same versions in new objects: nothing is decoded again.
    assert!(!index.update(&store.snapshot(), &kind));
    for (a, b) in before.iter().zip(index.events()) {
        assert!(Arc::ptr_eq(a, b));
    }

    // The count of an event changes: its group follows.
    store.put(
        "a",
        "e1",
        json!({ "lastTimestamp": "2026-10-22T09:20:00Z", "count": 6 }),
    );
    assert!(index.update(&store.snapshot(), &kind));
    assert_same(&index, &store.snapshot(), &kind);
    let backoff = index.groups().last().unwrap();
    assert_eq!(
        (backoff.latest.reason.as_str(), backoff.count),
        ("BackOff", 6)
    );
    let failed = index.events().iter().find(|e| e.name == "e2").unwrap();
    assert!(before.iter().any(|e| Arc::ptr_eq(e, failed)));

    // Deleted events and their groups go away.
    store.delete("a", "e1");
    assert!(index.update(&store.snapshot(), &kind));
    assert_same(&index, &store.snapshot(), &kind);
    assert_eq!(index.len(), 1);
    assert_eq!(index.groups().len(), 1);
    store.delete("a", "e2");
    assert!(index.update(&store.snapshot(), &kind));
    assert!(index.is_empty() && index.groups().is_empty());
}

#[test]
fn index_skips_objects_that_are_no_events() {
    let kind = core_kind();
    let broken = Arc::new(dynamic(json!({
        "apiVersion": "v1",
        "kind": "Event",
        "metadata": { "name": "x", "namespace": "a", "uid": "x", "resourceVersion": "1" },
        "involvedObject": "not an object",
    })));
    let mut index = EventIndex::new();
    assert!(index.update(std::slice::from_ref(&broken), &kind));
    assert!(index.is_empty());
    assert!(!index.update(&[broken], &kind));
}

/// Incremental updates over random adds, updates and deletes give what a
/// full rebuild gives.
#[test]
fn index_matches_full_rebuild_on_random_changes() {
    let kind = core_kind();
    for seed in 1..=40u64 {
        // xorshift64*, deterministic per seed.
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |n: u64| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
        };
        let mut store = Store::new();
        let mut index = EventIndex::new();
        for _ in 0..60 {
            let ns = ["a", "b"][next(2) as usize];
            let name = format!("e{}", next(12));
            if next(5) == 0 {
                store.delete(ns, &name);
            } else {
                // Few distinct times and objects: ties and shared groups.
                let type_ = ["Normal", "Warning"][next(2) as usize];
                let reason = ["BackOff", "Failed", "Pulled"][next(3) as usize];
                let message = ["m1", "m2"][next(2) as usize];
                let count = next(4) + 1;
                let object = format!("api-{}", next(2));
                let field_path = ["spec.containers{api}", ""][next(2) as usize];
                let mut body = json!({
                    "type": type_,
                    "reason": reason,
                    "message": message,
                    "count": count,
                    "involvedObject": {
                        "kind": "Pod", "namespace": ns, "name": object, "fieldPath": field_path,
                    },
                });
                if next(4) != 0 {
                    body["lastTimestamp"] = json!(format!("2026-10-22T09:0{}:00Z", next(4)));
                    body["firstTimestamp"] = json!(format!("2026-10-22T08:0{}:00Z", next(4)));
                }
                store.put(ns, &name, body);
            }
            let objects = store.snapshot();
            index.update(&objects, &kind);
            assert_same(&index, &objects, &kind);
            // A fresh index agrees as well.
            let mut fresh = EventIndex::new();
            fresh.update(&objects, &kind);
            assert_eq!(fresh.events(), index.events());
            assert_eq!(fresh.groups(), index.groups());
        }
    }
}
