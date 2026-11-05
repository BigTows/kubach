//! `wait_for_condition` (#109) through the handler, against a fake API that
//! answers each read as the test scripts it.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kube::{Client, Config};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
use rmcp::service::PeerRequestOptions;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::confirm::DenyAll;
use crate::grants::{Grant, GrantState, Grants};
use crate::hosted::HostedSessions;
use crate::identity::Identity;
use crate::rules::Capability;
use crate::settings::SecurityPolicy;
use crate::testutil::{NS, OTHER_NS, catalog, permissive, scope_with};

/// A status and a body; `None` leaves the read hanging.
type Answer = Option<(&'static str, Value)>;
/// Answers read number `n` of `path`.
type Script = Box<dyn Fn(usize, &str) -> Answer + Send + Sync>;

const NOT_FOUND: &str = "404 Not Found";
const OK: &str = "200 OK";

struct Fixture {
    server: McpServer,
    requests: Arc<Mutex<Vec<String>>>,
    audit_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn call(&self, value: Value) -> CallOutcome {
        self.server
            .call("agent", "wait_for_condition", args(value))
            .await
    }

    fn reads(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// The call's own audit line, once it is written.
    async fn audited(&self) -> Value {
        for _ in 0..200 {
            if let Some(line) = std::fs::read_to_string(&self.audit_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|line| line["tool"] == "wait_for_condition")
            {
                return line;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no audit line for the call");
    }
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

async fn fixture(script: Script) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = requests.clone();
    let script = Arc::new(script);
    let count = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (log, script, count) = (log.clone(), script.clone(), count.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if header == "\r\n" {
                            break;
                        }
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    log.lock().unwrap().push(line.trim_end().to_owned());
                    let Some((status, body)) = script(count.fetch_add(1, Ordering::SeqCst), &path)
                    else {
                        std::future::pending::<()>().await;
                        return;
                    };
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if write.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let session = Session::from_parts(
        scope_with(Profile::ReadOnly, &permissive(), &[NS]),
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    );
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let server = McpServer::new(session, Arc::new(DenyAll), Some(audit));
    Fixture {
        server,
        requests,
        audit_path,
        _dir: dir,
    }
}

fn status(code: u16, reason: &str) -> Value {
    json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "reason": reason,
           "code": code, "message": format!("the API said {reason}")})
}

fn not_found() -> Answer {
    Some((NOT_FOUND, status(404, "NotFound")))
}

fn pod(name: &str, ready: &str) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": NS, "uid": name, "labels": {"app": "web"}},
        "spec": {"containers": [{"name": "app", "image": "nginx"}]},
        "status": {"phase": "Running", "conditions": [{
            "type": "Ready", "status": ready, "reason": "ContainersNotReady",
            "message": "waiting for password=hunter2",
        }]},
    })
}

fn pods(items: Vec<Value>) -> Value {
    json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": items})
}

fn by_name(condition: &str, timeout: u64) -> Value {
    json!({"kind": "Pod", "name": "web-0", "namespace": NS, "condition": condition,
           "timeoutSeconds": timeout})
}

fn by_selector(condition: &str, timeout: u64) -> Value {
    json!({"kind": "Pod", "labelSelector": "app=web", "namespace": NS, "condition": condition,
           "timeoutSeconds": timeout})
}

#[tokio::test]
async fn an_object_appears_and_the_wait_only_gets_it() {
    let f = fixture(Box::new(|n, _| match n {
        0 => not_found(),
        _ => Some((OK, pod("web-0", "False"))),
    }))
    .await;
    let outcome = f.call(by_name("exists", 10)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome
            .text
            .starts_with(&format!("Pod {NS}/web-0 exists (")),
        "{}",
        outcome.text
    );
    let read = format!("GET /api/v1/namespaces/{NS}/pods/web-0 HTTP/1.1");
    assert_eq!(f.reads(), [read.clone(), read]);
    let audit = f.audited().await;
    assert_eq!(audit["allowed"], true);
    assert_eq!(audit["result"], "ok");
    assert_eq!(audit["args"]["condition"], "exists");
}

#[tokio::test]
async fn a_status_condition_is_waited_for_by_name() {
    let f = fixture(Box::new(|n, _| match n {
        0 => Some((OK, pod("web-0", "False"))),
        _ => Some((OK, pod("web-0", "True"))),
    }))
    .await;
    let outcome = f.call(by_name("Ready=True", 10)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("has Ready=True"), "{}", outcome.text);
}

#[tokio::test]
async fn an_object_goes_away() {
    let f = fixture(Box::new(|n, _| match n {
        0 => Some((OK, pod("web-0", "True"))),
        _ => not_found(),
    }))
    .await;
    let outcome = f.call(by_name("deleted", 10)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("is gone ("), "{}", outcome.text);
}

#[tokio::test]
async fn a_selector_is_listed_and_says_what_met_means() {
    let f = fixture(Box::new(|n, _| {
        let second = if n == 0 { "False" } else { "True" };
        Some((OK, pods(vec![pod("web-0", "True"), pod("web-1", second)])))
    }))
    .await;
    let outcome = f.call(by_selector("Ready=True", 10)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("All 2 pods matching"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("Met means: every matching object has Ready=True, and at least one matches."),
        "{}",
        outcome.text
    );
    let reads = f.reads();
    assert!(reads[0].contains("labelSelector=app%3Dweb"), "{reads:?}");
    assert!(reads[0].contains("limit=500"), "{reads:?}");
    assert!(
        reads.iter().all(|read| !read.contains("watch")),
        "{reads:?}"
    );
}

#[tokio::test]
async fn the_timeout_tells_the_final_state() {
    let f = fixture(Box::new(|_, _| Some((OK, pod("web-0", "False"))))).await;
    let outcome = f.call(by_name("Ready=True", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome
            .text
            .starts_with(&format!("Pod {NS}/web-0: Ready=True not met after 1s.")),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("Final state: Ready=False (ContainersNotReady: waiting for password="),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("web-0 Running ready"),
        "{}",
        outcome.text
    );
    assert!(!outcome.text.contains("hunter2"), "{}", outcome.text);
    let audit = f.audited().await;
    assert_eq!(audit["result"], "error");
    assert_eq!(audit["allowed"], true);
}

#[tokio::test]
async fn a_timeout_on_a_selector_names_at_most_ten_objects() {
    let f = fixture(Box::new(|_, _| {
        let items = (0..12).map(|i| pod(&format!("web-{i}"), "False")).collect();
        Some((OK, pods(items)))
    }))
    .await;
    let outcome = f.call(by_selector("Ready=True", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("Met means: every"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("0 of 12 have Ready=True"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("(and 2 more)"), "{}", outcome.text);

    // Nothing matches: not "all of none".
    let f = fixture(Box::new(|_, _| Some((OK, pods(Vec::new()))))).await;
    let outcome = f.call(by_selector("Ready=True", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("Final state: no object matches"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn an_object_that_stays_is_held_up_by_its_finalizers() {
    let f = fixture(Box::new(|_, _| {
        let mut held = pod("web-0", "True");
        held["metadata"]["deletionTimestamp"] = json!("2026-11-05T07:00:00Z");
        held["metadata"]["finalizers"] = json!(["example.com/hold"]);
        Some((OK, held))
    }))
    .await;
    let outcome = f.call(by_name("deleted", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("Final state: still there"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("held by finalizers example.com/hold"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn what_the_policy_refuses_fails_at_once_and_reads_nothing() {
    let f = fixture(Box::new(|_, _| None)).await;
    let refused = [
        json!({"kind": "Secret", "name": "db", "namespace": NS, "condition": "exists"}),
        json!({"kind": "Node", "name": "n1", "condition": "exists"}),
        json!({"kind": "Pod", "name": "web-0", "namespace": OTHER_NS, "condition": "exists"}),
        json!({"kind": "Pod", "labelSelector": "app=web", "namespace": OTHER_NS, "condition": "exists"}),
        json!({"kind": "Pod", "name": "../web", "namespace": NS, "condition": "exists"}),
    ];
    let invalid = [
        json!({"kind": "Pod", "namespace": NS, "condition": "exists"}),
        json!({"kind": "Pod", "name": "a", "labelSelector": "b=c", "namespace": NS, "condition": "exists"}),
        json!({"kind": "Pod", "labelSelector": " ", "namespace": NS, "condition": "exists"}),
        json!({"kind": "Pod", "name": "a", "namespace": NS, "condition": "Ready"}),
        json!({"kind": "Pod", "name": "a", "namespace": NS, "condition": "exists", "timeoutSeconds": 0}),
        json!({"kind": "Pod", "name": "a", "namespace": NS}),
    ];
    let started = Instant::now();
    for call in refused.into_iter().chain(invalid) {
        let outcome = f.call(call.clone()).await;
        assert!(outcome.is_error, "{call}: {}", outcome.text);
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(f.reads().is_empty(), "{:?}", f.reads());
}

#[tokio::test]
async fn a_refusal_of_the_api_server_ends_the_wait_at_once() {
    let f = fixture(Box::new(|_, _| {
        Some(("403 Forbidden", status(403, "Forbidden")))
    }))
    .await;
    let started = Instant::now();
    let outcome = f.call(by_name("exists", 60)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        outcome.text.contains("does not let this session get pods"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audited().await["allowed"], false);
    assert_eq!(f.reads().len(), 1);

    // A selector the API server rejects does not get better either.
    let f = fixture(Box::new(|_, _| {
        Some(("400 Bad Request", status(400, "BadRequest")))
    }))
    .await;
    let started = Instant::now();
    let outcome = f.call(by_selector("exists", 60)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_passing_failure_of_the_api_server_does_not_end_the_wait() {
    let f = fixture(Box::new(|n, _| match n {
        0 => Some(("503 Service Unavailable", status(503, "ServiceUnavailable"))),
        _ => Some((OK, pod("web-0", "True"))),
    }))
    .await;
    let outcome = f.call(by_name("Ready=True", 10)).await;
    assert!(!outcome.is_error, "{}", outcome.text);

    // Until the timeout, which tells the last failure.
    let f = fixture(Box::new(|_, _| {
        Some(("500 Internal Server Error", status(500, "InternalError")))
    }))
    .await;
    let outcome = f.call(by_name("Ready=True", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("no read has finished"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("The last read failed: API error 500"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn a_read_that_hangs_does_not_outlast_the_timeout() {
    let f = fixture(Box::new(|_, _| None)).await;
    let started = Instant::now();
    let outcome = f.call(by_name("exists", 1)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        outcome.text.contains("no read has finished"),
        "{}",
        outcome.text
    );
}

/// The API server knows nothing of a right taken back from a session
/// without its own identity: only the policy can end this wait.
#[tokio::test]
async fn a_right_taken_back_ends_the_wait() {
    let f = Arc::new(fixture(Box::new(|_, _| not_found())).await);
    let wait = {
        let f = f.clone();
        tokio::spawn(async move { f.call(by_name("exists", 60)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut refused = Grants::new();
    refused.set(Grant::read(NS), GrantState::Denied);
    f.server.session().set_grants(refused);
    let outcome = tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("the wait ended")
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert_eq!(f.audited().await["allowed"], false);
}

#[tokio::test]
async fn the_end_of_the_session_ends_the_wait_even_during_a_read() {
    let f = Arc::new(fixture(Box::new(|_, _| None)).await);
    let wait = {
        let f = f.clone();
        tokio::spawn(async move { f.call(by_name("exists", 60)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.server.session().close().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("the wait ended")
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("the session ended after"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audited().await["allowed"], false);
}

#[tokio::test]
async fn a_cancelled_wait_ends_with_its_audit_line() {
    let f = Arc::new(fixture(Box::new(|_, _| Some((OK, pod("web-0", "False"))))).await);
    let cancel = CancellationToken::new();
    let wait = {
        let (f, cancel) = (f.clone(), cancel.clone());
        tokio::spawn(async move {
            f.server
                .call_cancellable(
                    "agent",
                    "wait_for_condition",
                    args(by_name("Ready=True", 60)),
                    None,
                    cancel,
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .expect("the wait ended")
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("was cancelled after"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("Final state: Ready=False"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audited().await["result"], "error");
}

/// rmcp only flags a cancelled call: without the handler passing the flag
/// on, the wait would run to its timeout.
#[tokio::test]
async fn cancelling_over_the_mcp_protocol_stops_the_wait() {
    let f = fixture(Box::new(|_, _| not_found())).await;
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = f.server.clone();
    tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();
    let params = CallToolRequestParams::new("wait_for_condition")
        .with_arguments(args(by_name("exists", 60)));
    let handle = client
        .peer()
        .send_cancellable_request(
            ClientRequest::CallToolRequest(CallToolRequest::new(params)),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.cancel(None).await.unwrap();
    let audit = f.audited().await;
    assert!(
        audit["reason"].as_str().unwrap().contains("was cancelled"),
        "{audit}"
    );
    client.cancel().await.unwrap();
}

/// A relayed call is cancelled by dropping the request.
#[tokio::test]
async fn a_hosted_wait_ends_with_the_request_and_is_audited() {
    let f = fixture(Box::new(|_, _| not_found())).await;
    let hosted = HostedSessions::new();
    let id = f.server.session().id().to_owned();
    let key = hosted.insert(f.server.clone(), 1);
    let relayed = hosted.call(
        &id,
        &key,
        "agent",
        "wait_for_condition",
        args(by_name("exists", 60)),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), relayed)
            .await
            .is_err()
    );
    let audit = f.audited().await;
    assert!(
        audit["reason"].as_str().unwrap().contains("was cancelled"),
        "{audit}"
    );
}

#[tokio::test]
async fn progress_tells_each_new_state_once_and_masked() {
    let f = fixture(Box::new(|n, _| match n {
        0 => not_found(),
        1 | 2 => Some((OK, pod("web-0", "False"))),
        _ => Some((OK, pod("web-0", "True"))),
    }))
    .await;
    let told: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = told.clone();
    let progress: tools::Progress = Arc::new(move |phase: String| sink.lock().unwrap().push(phase));
    let outcome = f
        .server
        .call_with_progress(
            "agent",
            "wait_for_condition",
            args(by_name("Ready=True", 20)),
            Some(progress),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let told = told.lock().unwrap();
    assert_eq!(told.len(), 2, "{told:?}");
    assert_eq!(told[0], format!("Pod {NS}/web-0: not found"));
    assert!(told[1].contains("Ready=False"), "{told:?}");
    assert!(!told[1].contains("hunter2"), "{told:?}");
}

/// The session stays usable: a wait holds no lock while it waits.
#[tokio::test]
async fn other_calls_go_on_during_a_wait() {
    let f = Arc::new(
        fixture(Box::new(|_, path| {
            path.ends_with("/other").then(|| (OK, pod("other", "True")))
        }))
        .await,
    );
    let wait = {
        let f = f.clone();
        tokio::spawn(async move { f.call(by_name("exists", 60)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let other = args(json!({"kind": "Pod", "name": "other", "namespace": NS}));
    let read = f.server.call("agent", "get_resource", other);
    let outcome = tokio::time::timeout(Duration::from_secs(1), read)
        .await
        .expect("not blocked by the wait");
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(!wait.is_finished());
    wait.abort();
}

#[tokio::test]
async fn the_tool_is_offered_read_only_at_the_least_access() {
    let policy = SecurityPolicy::default();
    let client = Client::try_from(Config::new("http://127.0.0.1:9".parse().unwrap())).unwrap();
    let session = Session::from_parts(
        scope_with(Profile::ReadOnly, &policy, &[NS]),
        Identity::ClientOnly { client },
        catalog(),
        HashSet::new(),
    );
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let tools = server.tools();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "wait_for_condition")
        .expect("offered");
    assert_eq!(
        tool.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );
    let description = tool.description.as_deref().unwrap();
    assert!(
        !description.contains("Requires your approval"),
        "{description}"
    );
    assert!(
        description.contains("every matching object"),
        "{description}"
    );
    let selector = &tool.input_schema["properties"]["labelSelector"]["description"];
    assert!(
        selector.as_str().unwrap().contains("at least one"),
        "{selector}"
    );
    assert_eq!(
        tool.input_schema["properties"]["timeoutSeconds"]["maximum"],
        600
    );
}
