//! `allNamespaces` and `fieldSelector` under a real server loop (#107): a
//! fake API server answers per path and records what reached it, because
//! what must never be sent matters as much as what comes back.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use kube::{Client, Config};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::McpServer;
use crate::audit::AuditLog;
use crate::confirm::{Confirmer, DenyAll, RequestKind, ScriptedConfirmer};
use crate::grants::{Grant, GrantState, Grants};
use crate::identity::Identity;
use crate::rules::Capability;
use crate::scope::{SessionScope, SessionSpec};
use crate::session::Session;
use crate::settings::{Profile, SecurityPolicy};
use crate::testutil::{catalog, permissive};

/// What the fake cluster answers for one namespace.
#[derive(Clone)]
enum Reply {
    Pods(&'static [&'static str]),
    /// One pod, and a `continue` token: the limit cut the list.
    Cut,
    Status(&'static str, u16),
}

/// An answer to an event list: the status line and the body.
pub(super) type EventsReply = fn(&str) -> (String, Value);

#[derive(Clone, Default)]
pub(super) struct Cluster {
    /// The namespaces it lists; `None`: listing them is forbidden.
    namespaces: Option<Vec<&'static str>>,
    replies: BTreeMap<&'static str, Reply>,
    /// Answers event lists, given the whole path; default: [`events`].
    events: Option<EventsReply>,
}

fn status(line: &str, code: u16) -> Value {
    json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
           "message": format!("{line}: pods"), "code": code})
}

fn pods(namespace: &str, names: &[&str], token: &str) -> Value {
    let items: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({"apiVersion": "v1", "kind": "Pod",
                   "metadata": {"name": name, "namespace": namespace},
                   "spec": {"containers": [{"name": "app", "image": "nginx"}]}})
        })
        .collect();
    json!({"apiVersion": "v1", "kind": "PodList", "metadata": {"continue": token}, "items": items})
}

fn events(namespace: &str) -> Value {
    let event = |age: &str, name: &str| {
        json!({"apiVersion": "v1", "kind": "Event",
               "metadata": {"name": name, "namespace": namespace},
               "involvedObject": {"kind": "Pod", "name": name},
               "reason": "Failed", "type": "Warning", "message": format!("{namespace} {name}"),
               "lastTimestamp": age})
    };
    let (old, new) = if namespace == "team-a" {
        ("2026-01-01T00:00:00Z", "2026-01-04T00:00:00Z")
    } else {
        ("2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z")
    };
    json!({"apiVersion": "v1", "kind": "EventList", "metadata": {},
           "items": [event(old, "old"), event(new, "new")]})
}

impl Cluster {
    pub(super) fn with_events(events: EventsReply) -> Self {
        Self {
            events: Some(events),
            ..Self::default()
        }
    }

    fn answer(&self, full_path: &str) -> (String, Value) {
        let path = full_path.split('?').next().unwrap_or(full_path);
        let ok = |body| ("200 OK".to_owned(), body);
        if path == "/api/v1/namespaces" {
            return match &self.namespaces {
                Some(names) => {
                    let items: Vec<Value> = names
                        .iter()
                        .map(|name| json!({"metadata": {"name": name}}))
                        .collect();
                    ok(
                        json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadataList",
                              "metadata": {}, "items": items}),
                    )
                }
                None => ("403 Forbidden".to_owned(), status("forbidden", 403)),
            };
        }
        let mut parts = path.trim_start_matches("/api/v1/namespaces/").split('/');
        let namespace = parts.next().unwrap_or_default();
        if parts.next() == Some("events") {
            return match self.events {
                Some(answer) => answer(full_path),
                None => ok(events(namespace)),
            };
        }
        match self.replies.get(namespace) {
            Some(Reply::Pods(names)) => ok(pods(namespace, names, "")),
            Some(Reply::Cut) => ok(pods(namespace, &["only-one"], "more")),
            Some(Reply::Status(line, code)) => ((*line).to_owned(), status(line, *code)),
            None => ok(pods(namespace, &[], "")),
        }
    }

    pub(super) async fn serve(self) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = log.clone();
                let cluster = self.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut reader = BufReader::new(read);
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).await.unwrap_or(0) == 0 || header == "\r\n"
                        {
                            break;
                        }
                    }
                    let path = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    log.lock().unwrap().push(path.clone());
                    let (status, body) = cluster.answer(&path);
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = write.write_all(response.as_bytes()).await;
                });
            }
        });
        (url, seen)
    }
}

pub(super) fn session(url: &str, pinned: &[&str], read: &[&str], refused: &[&str]) -> Arc<Session> {
    session_under(&permissive(), url, pinned, read, refused)
}

fn session_under(
    policy: &SecurityPolicy,
    url: &str,
    pinned: &[&str],
    read: &[&str],
    refused: &[&str],
) -> Arc<Session> {
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let spec = SessionSpec {
        context: "minikube".into(),
        server_url: "https://127.0.0.1:8443".into(),
        namespaces: pinned.iter().map(|ns| (*ns).to_owned()).collect(),
        profile: Profile::Deploy,
        ttl: None,
        hints: Vec::new(),
    };
    let scope = SessionScope::new(&spec, policy).unwrap();
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    );
    let mut table = Grants::new();
    for ns in read {
        table.set(Grant::read(*ns), GrantState::Granted);
    }
    for ns in refused {
        table.set(Grant::read(*ns), GrantState::Denied);
    }
    session.set_grants(table);
    session
}

pub(super) fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn every_pod() -> Map<String, Value> {
    args(json!({"kind": "Pod", "allNamespaces": true}))
}

fn questions(confirmer: &ScriptedConfirmer) -> Vec<Vec<String>> {
    confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match request.kind {
            RequestKind::Namespaces { names } => Some(names),
            _ => None,
        })
        .collect()
}

fn reads_of(seen: &Mutex<Vec<String>>, namespace: &str) -> usize {
    let prefix = format!("/api/v1/namespaces/{namespace}/");
    seen.lock()
        .unwrap()
        .iter()
        .filter(|path| path.starts_with(&prefix))
        .count()
}

fn four_teams() -> Cluster {
    Cluster {
        namespaces: Some(vec!["team-a", "team-b", "team-c", "team-d"]),
        replies: BTreeMap::from([
            ("team-a", Reply::Pods(&["a-0"])),
            ("team-b", Reply::Pods(&["b-0"])),
            ("team-c", Reply::Pods(&["c-0"])),
            ("team-d", Reply::Pods(&["d-0"])),
        ]),
        ..Cluster::default()
    }
}

#[tokio::test]
async fn a_refusal_gives_what_is_open_says_what_is_not_and_is_not_asked_twice() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &[], &["team-a"], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);

    for _ in 0..2 {
        let outcome = server.call("agent", "list_resources", every_pod()).await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(
            outcome.text.starts_with("Coverage: PARTIAL"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("1 of 4 known"), "{}", outcome.text);
        assert!(
            outcome
                .text
                .contains("- team-b, team-c, team-d: not opened"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("team-a\ta-0"), "{}", outcome.text);
    }
    assert_eq!(questions(&confirmer), [["team-b", "team-c", "team-d"]]);
    assert_eq!(reads_of(&seen, "team-a"), 2);
    for namespace in ["team-b", "team-c", "team-d"] {
        assert_eq!(reads_of(&seen, namespace), 0, "{namespace}");
        assert!(!session.grants().is_granted(&Grant::read(namespace)));
    }

    // The no is about opening in bulk: one namespace can still be named.
    let named = args(json!({"kind": "Pod", "namespace": "team-b"}));
    let outcome = server.call("agent", "list_resources", named).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("b-0"), "{}", outcome.text);
}

#[tokio::test]
async fn one_yes_opens_every_namespace_for_the_session() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &[], &[], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);

    for _ in 0..2 {
        let outcome = server.call("agent", "list_resources", every_pod()).await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(
            outcome
                .text
                .starts_with("Coverage: complete. All 4 namespaces"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("NAMESPACE\t"), "{}", outcome.text);
        assert!(
            outcome.text.contains("(4 pods in 4 namespaces)"),
            "{}",
            outcome.text
        );
    }
    assert_eq!(confirmer.requests().len(), 1);
    assert!(session.grants().is_granted(&Grant::read("team-d")));
    // Never the cluster-wide list.
    let seen = seen.lock().unwrap();
    assert!(
        !seen.iter().any(|path| path.starts_with("/api/v1/pods")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn no_answer_leaves_the_gap_and_asks_again_next_time() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &[], &["team-a"], &[]);
    let server = McpServer::new(session.clone(), Arc::new(DenyAll), None);
    let outcome = server.call("agent", "list_resources", every_pod()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("no answer from the user"),
        "{}",
        outcome.text
    );
    assert_eq!(session.all_namespaces_answer(), None);
    assert_eq!(reads_of(&seen, "team-b"), 0);
}

#[tokio::test]
async fn a_revoked_namespace_is_never_read_and_is_named() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &[], &["team-a"], &["team-b"]);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session, confirmer.clone(), None);
    let outcome = server.call("agent", "list_resources", every_pod()).await;
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("- team-b: the user refused or revoked"),
        "{}",
        outcome.text
    );
    assert_eq!(reads_of(&seen, "team-b"), 0);
    assert_eq!(questions(&confirmer), [["team-c", "team-d"]]);

    let every_event = args(json!({"allNamespaces": true}));
    let outcome = server.call("agent", "list_events", every_event).await;
    assert!(
        outcome.text.contains("- team-b: the user refused"),
        "{}",
        outcome.text
    );
    assert_eq!(reads_of(&seen, "team-b"), 0);
}

#[tokio::test]
async fn a_pinned_session_reads_its_namespaces_only_and_asks_nothing() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &["team-a", "team-b"], &[], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session, confirmer.clone(), None);
    let outcome = server.call("agent", "list_resources", every_pod()).await;
    assert!(
        outcome.text.starts_with("Coverage: complete. All 2"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("pinned to these namespaces"),
        "{}",
        outcome.text
    );
    assert!(confirmer.requests().is_empty());
    assert_eq!(reads_of(&seen, "team-c"), 0);
    let seen = seen.lock().unwrap();
    assert!(
        !seen
            .iter()
            .any(|path| path.starts_with("/api/v1/namespaces?")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn namespaces_nobody_may_list_make_the_answer_partial() {
    let cluster = Cluster {
        namespaces: None,
        replies: BTreeMap::from([("team-a", Reply::Pods(&["a-0"]))]),
        ..Cluster::default()
    };
    let (url, _) = cluster.serve().await;
    let session = session(&url, &[], &["team-a"], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session, confirmer.clone(), None);
    let outcome = server.call("agent", "list_resources", every_pod()).await;
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("Other namespaces may exist"),
        "{}",
        outcome.text
    );
    assert!(confirmer.requests().is_empty());
}

#[tokio::test]
async fn a_failing_namespace_is_a_gap_and_the_others_still_answer() {
    let mut cluster = four_teams();
    cluster
        .replies
        .insert("team-b", Reply::Status("403 Forbidden", 403));
    cluster
        .replies
        .insert("team-c", Reply::Status("500 Internal Server Error", 500));
    let (url, _) = cluster.serve().await;
    let session = session(&url, &[], &["team-a", "team-b", "team-c", "team-d"], &[]);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let outcome = server.call("agent", "list_resources", every_pod()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("2 of 4 known"), "{}", outcome.text);
    assert!(
        outcome.text.contains("- team-b: the API server refused"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("- team-c: not read: API error 500"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("team-d\td-0"), "{}", outcome.text);
}

#[tokio::test]
async fn the_limit_holds_per_namespace_and_a_cut_is_said() {
    let mut cluster = four_teams();
    cluster.replies.insert("team-b", Reply::Cut);
    let (url, seen) = cluster.serve().await;
    let session = session(&url, &[], &["team-a", "team-b", "team-c", "team-d"], &[]);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let call = args(json!({"kind": "Pod", "allNamespaces": true, "limit": 1}));
    let outcome = server.call("agent", "list_resources", call).await;
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("more items exist in: team-b"),
        "{}",
        outcome.text
    );
    let seen = seen.lock().unwrap();
    let reads = seen.iter().filter(|path| path.contains("/pods")).count();
    let limited = seen.iter().filter(|path| path.contains("limit=1")).count();
    assert_eq!((reads, limited), (4, 4), "{seen:?}");
}

#[tokio::test]
async fn events_merge_across_namespaces_newest_kept() {
    let (url, _) = four_teams().serve().await;
    let session = session(&url, &["team-a", "team-b"], &[], &[]);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let call = args(json!({"allNamespaces": true, "limit": 2}));
    let outcome = server.call("agent", "list_events", call).await;
    // The limit left events out: both namespaces had more than is shown.
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("more items exist in: team-a, team-b"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("team-a new"), "{}", outcome.text);
    assert!(outcome.text.contains("team-b new"), "{}", outcome.text);
    assert!(!outcome.text.contains(" old"), "{}", outcome.text);
    assert!(
        outcome.text.contains("the newest 2 of 4 events"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn the_field_selector_goes_to_each_namespace() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &["team-a", "team-b"], &[], &[]);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let call = args(json!({"kind": "Pod", "allNamespaces": true,
                           "fieldSelector": "spec.nodeName=node-1"}));
    let outcome = server.call("agent", "list_resources", call).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let seen = seen.lock().unwrap();
    let selected = seen
        .iter()
        .filter(|path| path.contains("fieldSelector=spec.nodeName%3Dnode-1"))
        .count();
    assert_eq!(selected, 2, "{seen:?}");
}

#[tokio::test]
async fn a_field_the_server_refuses_is_bad_input_not_a_gap() {
    let mut cluster = four_teams();
    cluster
        .replies
        .insert("team-a", Reply::Status("400 Bad Request", 400));
    let (url, _) = cluster.serve().await;
    let session = session(&url, &["team-a", "team-b"], &[], &[]);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    for call in [
        json!({"kind": "Pod", "allNamespaces": true, "fieldSelector": "spec.nope=1"}),
        json!({"kind": "Pod", "namespace": "team-a", "fieldSelector": "spec.nope=1"}),
    ] {
        let outcome = server.call("agent", "list_resources", args(call)).await;
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(
            outcome.text.contains("does not accept fieldSelector"),
            "{}",
            outcome.text
        );
    }
}

#[tokio::test]
async fn nothing_reaches_the_api_for_a_refused_query() {
    let (url, seen) = four_teams().serve().await;
    let session = session(&url, &[], &["team-a"], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session, confirmer.clone(), None);
    for (tool, call) in [
        (
            "list_resources",
            json!({"kind": "Secret", "allNamespaces": true}),
        ),
        (
            "list_resources",
            json!({"kind": "Node", "allNamespaces": true}),
        ),
        (
            "list_resources",
            json!({"kind": "Pod", "allNamespaces": true, "namespace": "team-a"}),
        ),
        (
            "list_resources",
            json!({"kind": "Pod", "allNamespaces": true, "fieldSelector": "nonsense"}),
        ),
        (
            "list_resources",
            json!({"kind": "Pod", "fieldSelector": "metadata.namespace=team-b", "namespace": "team-a"}),
        ),
        (
            "list_events",
            json!({"kind": "Secret", "name": "db", "allNamespaces": true}),
        ),
        (
            "list_events",
            json!({"namespace": "team-a", "allNamespaces": true}),
        ),
    ] {
        let outcome = server.call("agent", tool, args(call.clone())).await;
        assert!(outcome.is_error, "{call}: {}", outcome.text);
    }
    assert!(seen.lock().unwrap().is_empty());
    assert!(confirmer.requests().is_empty());
}

#[tokio::test]
async fn custom_resources_are_asked_about_once_not_per_namespace() {
    let (url, _) = four_teams().serve().await;
    let policy = SecurityPolicy::default();
    let session = session_under(&policy, &url, &[], &["team-a", "team-b"], &[]);
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session, confirmer.clone(), None);
    let call = args(json!({"kind": "Widget", "apiVersion": "example.com/v1",
                           "allNamespaces": true}));
    let outcome = server.call("agent", "list_resources", call).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.requests().len(), 1);
}

#[tokio::test]
async fn the_audit_log_names_what_was_covered() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("audit.jsonl");
    let (url, _) = four_teams().serve().await;
    let session = session(&url, &[], &["team-a"], &["team-b"]);
    let confirmer: Arc<dyn Confirmer> = Arc::new(ScriptedConfirmer::denying());
    let audit = AuditLog::open(&path).unwrap();
    let server = McpServer::new(session, confirmer, Some(Arc::new(audit)));
    server.call("agent", "list_resources", every_pod()).await;
    let text = std::fs::read_to_string(&path).unwrap();
    let line: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(line["change"]["namespaces"], json!(["team-a"]));
    assert_eq!(line["change"]["complete"], false);
    assert_eq!(line["change"]["question"], "refused");
}
