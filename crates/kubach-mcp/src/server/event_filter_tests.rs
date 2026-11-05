//! `list_events` filters under a real server loop (#111): what reaches the
//! API server, and what the answer says about what it did not read.

use std::sync::{Arc, Mutex};

use k8s_openapi::jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use super::McpServer;
use super::all_namespaces_tests::{Cluster, EventsReply, args, session};
use crate::confirm::DenyAll;

const NS: &str = "team-a";

fn event(name: &str, namespace: &str, age_seconds: i64) -> Value {
    let time = Timestamp::now() - SignedDuration::from_secs(age_seconds);
    json!({"apiVersion": "v1", "kind": "Event",
           "metadata": {"name": name, "namespace": namespace},
           "involvedObject": {"kind": "Pod", "name": name},
           "reason": "BackOff", "type": "Warning", "message": format!("event {name}"),
           "lastTimestamp": time.to_string()})
}

fn page(items: Vec<Value>, token: &str) -> (String, Value) {
    (
        "200 OK".to_owned(),
        json!({"apiVersion": "v1", "kind": "EventList", "metadata": {"continue": token},
               "items": items}),
    )
}

fn namespace_of(path: &str) -> &str {
    path.trim_start_matches("/api/v1/namespaces/")
        .split('/')
        .next()
        .unwrap_or_default()
}

/// Two pages: the newest event comes last, past an old one and a page break.
fn two_pages(path: &str) -> (String, Value) {
    let namespace = namespace_of(path);
    if path.contains("continue=") {
        page(vec![event("fresh", namespace, 30)], "")
    } else {
        page(vec![event("ancient", namespace, 7200)], "next")
    }
}

fn never_ending(path: &str) -> (String, Value) {
    page(vec![event("fresh", namespace_of(path), 30)], "next")
}

fn token_expires(path: &str) -> (String, Value) {
    if path.contains("continue=") {
        (
            "410 Gone".to_owned(),
            json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                   "reason": "Expired", "message": "the token expired", "code": 410}),
        )
    } else {
        page(vec![event("fresh", namespace_of(path), 30)], "next")
    }
}

async fn call(answer: EventsReply, pinned: &[&str], arguments: Value) -> (String, Vec<String>) {
    let (url, seen) = Cluster::with_events(answer).serve().await;
    let server = McpServer::new(session(&url, pinned, &[], &[]), Arc::new(DenyAll), None);
    let outcome = server.call("agent", "list_events", args(arguments)).await;
    (outcome.text, event_reads(&seen))
}

fn event_reads(seen: &Mutex<Vec<String>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|path| path.contains("/events"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn type_and_reason_reach_the_server_as_a_selector() {
    let (text, reads) = call(
        two_pages,
        &[NS],
        json!({"namespace": NS, "type": "Warning", "reason": "BackOff"}),
    )
    .await;
    assert_eq!(reads.len(), 2, "{reads:?}");
    for read in &reads {
        assert!(
            read.contains("fieldSelector=type%3DWarning%2Creason%3DBackOff"),
            "{read}"
        );
    }
    assert!(text.contains("event fresh"), "{text}");
    assert!(!text.contains("Window:"), "{text}");
}

#[tokio::test]
async fn the_window_is_read_to_the_end() {
    let (text, reads) = call(
        two_pages,
        &[NS],
        json!({"namespace": NS, "sinceSeconds": 3600}),
    )
    .await;
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert!(reads[1].contains("continue=next"), "{reads:?}");
    assert!(
        text.starts_with("Window: the last 3600 s, fully covered: all 1 events"),
        "{text}"
    );
    assert!(text.contains("event fresh"), "{text}");
    assert!(!text.contains("event ancient"), "{text}");
}

#[tokio::test]
async fn a_read_stopped_at_its_bound_is_not_covered() {
    let (text, reads) = call(
        never_ending,
        &[NS],
        json!({"namespace": NS, "sinceSeconds": 3600}),
    )
    .await;
    assert_eq!(reads.len(), 10, "{reads:?}");
    assert!(
        text.starts_with("Window: the last 3600 s, NOT fully covered"),
        "{text}"
    );
    assert!(text.contains("at most 5000 events"), "{text}");
}

#[tokio::test]
async fn without_a_window_a_stopped_read_still_says_so() {
    let (text, _) = call(never_ending, &[NS], json!({"namespace": NS})).await;
    assert!(text.contains("newer ones may be missing"), "{text}");
}

#[tokio::test]
async fn an_expired_token_cuts_the_read_and_keeps_what_was_read() {
    let (text, reads) = call(
        token_expires,
        &[NS],
        json!({"namespace": NS, "sinceSeconds": 60}),
    )
    .await;
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert!(text.contains("NOT fully covered"), "{text}");
    assert!(text.contains("event fresh"), "{text}");
}

#[tokio::test]
async fn a_limit_inside_the_window_is_a_cut() {
    fn three(path: &str) -> (String, Value) {
        let namespace = namespace_of(path);
        page(
            vec![
                event("a", namespace, 30),
                event("b", namespace, 20),
                event("c", namespace, 10),
            ],
            "",
        )
    }
    let (text, _) = call(
        three,
        &[NS],
        json!({"namespace": NS, "sinceSeconds": 60, "limit": 2}),
    )
    .await;
    assert!(text.contains("CUT: the newest 2 of 3"), "{text}");
    assert!(!text.contains("fully covered"), "{text}");
    assert!(
        text.contains("event c") && !text.contains("event a"),
        "{text}"
    );
}

#[tokio::test]
async fn all_namespaces_filters_each_and_covers_the_window() {
    let (text, reads) = call(
        two_pages,
        &["team-a", "team-b"],
        json!({"allNamespaces": true, "type": "Warning", "sinceSeconds": 3600}),
    )
    .await;
    assert_eq!(reads.len(), 4, "{reads:?}");
    assert!(
        reads.iter().all(|read| read.contains("type%3DWarning")),
        "{reads:?}"
    );
    assert!(text.starts_with("Coverage: complete"), "{text}");
    assert!(text.contains("fully covered: all 2 events"), "{text}");
    assert!(
        text.contains("team-a\t") && text.contains("team-b\t"),
        "{text}"
    );
}

#[tokio::test]
async fn all_namespaces_names_where_the_read_stopped() {
    let (text, reads) = call(
        never_ending,
        &["team-a", "team-b"],
        json!({"allNamespaces": true, "sinceSeconds": 3600}),
    )
    .await;
    assert_eq!(reads.len(), 20, "{reads:?}");
    assert!(text.starts_with("Coverage: PARTIAL"), "{text}");
    assert!(
        text.contains("newer events may be missing in: team-a, team-b"),
        "{text}"
    );
    assert!(text.contains("NOT fully covered"), "{text}");
}

#[tokio::test]
async fn all_namespaces_with_a_limit_never_says_covered() {
    let (text, _) = call(
        two_pages,
        &["team-a", "team-b"],
        json!({"allNamespaces": true, "sinceSeconds": 3600, "limit": 1}),
    )
    .await;
    assert!(text.starts_with("Coverage: PARTIAL"), "{text}");
    assert!(text.contains("CUT: the newest 1 of 2"), "{text}");
    assert!(!text.contains("fully covered"), "{text}");
}

#[tokio::test]
async fn bad_input_never_reaches_the_server() {
    for arguments in [
        json!({"namespace": NS, "type": "warning"}),
        json!({"namespace": NS, "reason": "BackOff,type=Normal"}),
        json!({"namespace": NS, "reason": "a=b"}),
        json!({"namespace": NS, "sinceSeconds": 0}),
        json!({"namespace": NS, "sinceSeconds": -5}),
        json!({"namespace": NS, "sinceSeconds": 604801}),
        json!({"namespace": NS, "sinceSeconds": 99999999999u64}),
    ] {
        let (text, reads) = call(two_pages, &[NS], arguments.clone()).await;
        assert!(reads.is_empty(), "{arguments}: {reads:?}");
        assert!(!text.contains("event fresh"), "{arguments}: {text}");
    }
}
