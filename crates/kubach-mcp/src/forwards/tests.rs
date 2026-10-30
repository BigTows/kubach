//! The port-forward tools (#17) against a fake API server that serves pod
//! `web-0` (or the pods a test gives) and Service `web`. Forwards listen for real, on 127.0.0.1;
//! connecting is not tested here (it needs a cluster, see
//! `tests/port_forward_cluster.rs`).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kube::{Client, Config};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::audit::AuditLog;
use crate::confirm::{DenyAll, RequestKind, ScriptedConfirmer};
use crate::grants::Grant;
use crate::identity::Identity;
use crate::rules::Capability;
use crate::server::McpServer;
use crate::session::Session;
use crate::settings::Profile;
use crate::testutil::{NS, catalog, grants, permissive, scope_with};

fn pod() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-0", "namespace": NS, "labels": {"app": "web"},
                     "creationTimestamp": "2026-01-01T00:00:00Z"},
        "spec": {"containers": [{"name": "app", "image": "nginx",
                                 "ports": [{"name": "http", "containerPort": 8080}]}]},
        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}]},
    })
}

fn service() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web", "namespace": NS},
        "spec": {"selector": {"app": "web"},
                 "ports": [{"name": "http", "port": 80, "targetPort": "http"}]},
    })
}

/// Serves `pod()` and `service()` and records `METHOD path` of each request.
async fn fake_api() -> (String, Arc<Mutex<Vec<String>>>) {
    fake_api_with(vec![pod()]).await
}

/// Serves `pods` (listed for any selector) and `service()`.
async fn fake_api_with(pods: Vec<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let pods = Arc::new(pods);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
            let served = pods.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let mut length = 0usize;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if header == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body).await;
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_owned();
                    let path = parts.next().unwrap_or("").to_owned();
                    log.lock().unwrap().push(format!("{method} {path}"));
                    let pods = format!("/api/v1/namespaces/{NS}/pods");
                    let named = path
                        .strip_prefix(&format!("{pods}/"))
                        .and_then(|name| served.iter().find(|p| p["metadata"]["name"] == name));
                    let (status, body) = if let Some(pod) = named {
                        ("200 OK", pod.to_string())
                    } else if path.starts_with(&format!("{pods}?")) {
                        let list = json!({"apiVersion": "v1", "kind": "PodList",
                                          "metadata": {}, "items": *served});
                        ("200 OK", list.to_string())
                    } else if path == format!("/api/v1/namespaces/{NS}/services/web") {
                        ("200 OK", service().to_string())
                    } else {
                        (
                            "404 Not Found",
                            json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                                   "message": "not found", "reason": "NotFound", "code": 404})
                            .to_string(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if write.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, requests)
}

struct Fixture {
    server: McpServer,
    client: Client,
    confirmer: Arc<ScriptedConfirmer>,
    requests: Arc<Mutex<Vec<String>>>,
    audit_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// A debug-level session reading `NS`, with every RBAC right.
async fn fixture(approve: bool) -> Fixture {
    fixture_with(approve, vec![pod()]).await
}

/// [`fixture`] with the API serving `pods`.
async fn fixture_with(approve: bool, pods: Vec<Value>) -> Fixture {
    let (url, requests) = fake_api_with(pods).await;
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::Debug, &permissive(), &[NS]);
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly {
            client: client.clone(),
        },
        catalog(),
        capabilities,
    );
    session.set_grants(grants(&[Grant::read(NS)]));
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let confirmer = Arc::new(if approve {
        ScriptedConfirmer::approving()
    } else {
        ScriptedConfirmer::denying()
    });
    let server = McpServer::new(session, confirmer.clone(), Some(audit));
    Fixture {
        server,
        client,
        confirmer,
        requests,
        audit_path,
        _dir: dir,
    }
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn audit_lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Starts a forward through the tool and returns its id and address.
async fn start(f: &Fixture, target: &str, port: Value) -> (u64, SocketAddr) {
    let outcome = f
        .server
        .call(
            "agent",
            "port_forward_start",
            args(json!({"target": target, "port": port})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let json_start = outcome.text.find('{').unwrap();
    let result: Value = serde_json::from_str(&outcome.text[json_start..]).unwrap();
    (
        result["id"].as_u64().unwrap(),
        result["address"].as_str().unwrap().parse().unwrap(),
    )
}

/// Waits until nothing listens on `addr` any more.
async fn closed(addr: SocketAddr) -> bool {
    for _ in 0..100 {
        if TcpStream::connect(addr).await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Waits for an audit line of `tool` with `by`.
async fn stop_audited(path: &std::path::Path, by: &str) -> bool {
    for _ in 0..100 {
        if audit_lines(path)
            .iter()
            .any(|line| line["tool"] == "port_forward_stop" && line["args"]["by"] == by)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn a_confirmed_forward_listens_on_loopback_and_ends_with_the_session() {
    let f = fixture(true).await;
    let (id, addr) = start(&f, "service/web", json!("http")).await;
    assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(TcpStream::connect(addr).await.is_ok());

    // The user saw namespace, target, ports and session.
    let asked = f.confirmer.requests();
    let question = asked
        .iter()
        .find(|r| matches!(r.kind, RequestKind::PortForward { .. }))
        .expect("the forward was confirmed");
    assert_eq!(
        question.kind,
        RequestKind::PortForward {
            target: "service/web".into(),
            port: "http".into(),
            local_port: None,
        }
    );
    assert_eq!(question.namespace, NS);
    assert!(question.details.contains(f.server.session().id()));
    assert!(question.details.contains("127.0.0.1"));

    // The manager has it, marked as the agent's, on the Service port.
    let manager = f.server.session().forwards().manager();
    let info = manager.get(id).unwrap();
    assert_eq!(info.source, "svc/web");
    assert_eq!(info.source_port, 80);
    assert_eq!((info.pod.as_str(), info.remote_port), ("web-0", 8080));
    let agent = info.agent.unwrap();
    assert_eq!(agent.client, "agent");
    assert_eq!(agent.session, f.server.session().id());

    let listed = f
        .server
        .call("agent", "port_forward_list", Map::new())
        .await;
    assert!(listed.text.contains(&addr.to_string()), "{}", listed.text);
    assert!(f.server.session().grants().is_forwarding(NS));

    // The session's end stops it and frees the port.
    f.server.session().close().await.unwrap();
    assert!(manager.list().is_empty());
    assert!(closed(addr).await);
    assert!(stop_audited(&f.audit_path, "session-ended").await);
    let start_line = audit_lines(&f.audit_path)
        .into_iter()
        .find(|line| line["tool"] == "port_forward_start")
        .unwrap();
    assert_eq!(start_line["decided_by"], "user");
    assert_eq!(start_line["change"]["address"], addr.to_string());
}

#[tokio::test]
async fn a_rejected_forward_opens_nothing() {
    let f = fixture(false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "port_forward_start",
            args(json!({"target": "pod/web-0", "port": 8080})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(f.server.session().forwards().manager().list().is_empty());
    // The target was read to check it (#60), nothing more.
    let requests = f.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .all(|r| r.starts_with("GET ") && !r.contains("portforward")),
        "{requests:?}"
    );
    assert!(!f.server.session().grants().is_forwarding(NS));
}

// ------------------------------------- targets that cannot work (#60)

/// A pod `name` of the Service with `status`.
fn pod_with(name: &str, status: Value) -> Value {
    let mut pod = pod();
    pod["metadata"]["name"] = json!(name);
    pod["status"] = status;
    pod
}

fn crash_looping(name: &str) -> Value {
    pod_with(
        name,
        json!({
            "phase": "Running",
            "conditions": [
                {"type": "Ready", "status": "False", "reason": "ContainersNotReady"},
                {"type": "PodScheduled", "status": "True"},
            ],
            "containerStatuses": [{
                "name": "app", "image": "nginx", "imageID": "", "ready": false,
                "restartCount": 7,
                "state": {"waiting": {"reason": "CrashLoopBackOff",
                                      "message": "back-off 5m0s restarting failed container"}},
            }],
        }),
    )
}

fn unschedulable(name: &str) -> Value {
    pod_with(
        name,
        json!({
            "phase": "Pending",
            "conditions": [{"type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                            "message": "0/3 nodes are available: 3 Insufficient cpu."}],
        }),
    )
}

fn running_unready(name: &str) -> Value {
    pod_with(
        name,
        json!({
            "phase": "Running",
            "conditions": [{"type": "Ready", "status": "False"}],
            "containerStatuses": [{
                "name": "app", "image": "nginx", "imageID": "", "ready": false,
                "restartCount": 0, "started": true,
                "state": {"running": {"startedAt": "2026-01-01T00:00:00Z"}},
            }],
        }),
    )
}

/// Calls `port_forward_start` on `pods`; returns the error text, after
/// checking the user was not asked and nothing was forwarded.
async fn refused(pods: Vec<Value>, target: &str, port: Value) -> String {
    let f = fixture_with(true, pods).await;
    let outcome = f
        .server
        .call(
            "agent",
            "port_forward_start",
            args(json!({"target": target, "port": port})),
        )
        .await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        !f.confirmer
            .requests()
            .iter()
            .any(|r| matches!(r.kind, RequestKind::PortForward { .. })),
        "the user was asked: {}",
        outcome.text
    );
    assert!(f.server.session().forwards().manager().list().is_empty());
    assert!(!f.server.session().grants().is_forwarding(NS));
    let requests = f.requests.lock().unwrap().clone();
    assert!(
        requests.iter().all(|r| !r.contains("portforward")),
        "{requests:?}"
    );
    outcome.text
}

#[tokio::test]
async fn a_crash_looping_pod_is_refused_with_its_reason() {
    let text = refused(vec![crash_looping("web-0")], "pod/web-0", json!(8080)).await;
    for part in [
        "pod web-0 is not ready",
        "phase Running",
        "CrashLoopBackOff",
        "0/1 containers ready",
        "container app: waiting",
        "back-off 5m0s",
        "7 restarts",
        "user was not asked",
    ] {
        assert!(text.contains(part), "{part}: {text}");
    }
}

#[tokio::test]
async fn a_pending_pod_is_refused_with_why_it_is_not_scheduled() {
    let text = refused(vec![unschedulable("web-0")], "pod/web-0", json!(8080)).await;
    assert!(text.contains("phase Pending"), "{text}");
    assert!(text.contains("PodScheduled is False"), "{text}");
    assert!(text.contains("Insufficient cpu"), "{text}");
}

#[tokio::test]
async fn a_running_pod_that_is_not_ready_is_refused() {
    let text = refused(vec![running_unready("web-0")], "pod/web-0", json!(8080)).await;
    assert!(text.contains("phase Running"), "{text}");
    assert!(
        text.contains("container app: running, but not ready"),
        "{text}"
    );
}

#[tokio::test]
async fn a_service_without_ready_pods_is_refused_with_its_pods() {
    let pods = vec![crash_looping("web-1"), unschedulable("web-2")];
    let text = refused(pods, "service/web", json!("http")).await;
    assert!(
        text.contains("none of the 2 pods of svc/web is ready"),
        "{text}"
    );
    assert!(text.contains("no ready endpoints"), "{text}");
    assert!(
        text.contains("pod web-1") && text.contains("CrashLoopBackOff"),
        "{text}"
    );
    assert!(
        text.contains("pod web-2") && text.contains("Pending"),
        "{text}"
    );

    let text = refused(Vec::new(), "service/web", json!(80)).await;
    assert!(text.contains("no pods match svc/web"), "{text}");
    assert!(text.contains("selector app=web"), "{text}");
}

#[tokio::test]
async fn a_port_that_is_not_there_is_refused_with_the_ports_that_are() {
    let text = refused(vec![pod()], "service/web", json!(8080)).await;
    assert!(text.contains("service web has no port 8080"), "{text}");
    assert!(text.contains("svc/web has TCP ports 80 (http)"), "{text}");

    let text = refused(vec![pod()], "pod/web-0", json!("metrics")).await;
    assert!(text.contains("no TCP port named metrics"), "{text}");
    assert!(
        text.contains("pod/web-0 has TCP ports 8080 (http)"),
        "{text}"
    );
}

#[test]
fn a_ready_pod_has_no_problem() {
    let pod: k8s_openapi::api::core::v1::Pod = serde_json::from_value(pod()).unwrap();
    assert_eq!(super::check::pod_problem(&pod), None);
    let pod: k8s_openapi::api::core::v1::Pod =
        serde_json::from_value(crash_looping("web-0")).unwrap();
    assert!(super::check::pod_problem(&pod).is_some());
}

#[tokio::test]
async fn below_the_debug_level_the_user_is_asked_for_access_first() {
    let (url, requests) = fake_api().await;
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::ReadOnly, &permissive(), &[NS]);
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    );
    session.set_grants(grants(&[Grant::read(NS)]));
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let outcome = server
        .call(
            "agent",
            "port_forward_start",
            args(json!({"target": "pod/web-0", "port": 8080})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("debug"), "{}", outcome.text);
    assert!(requests.lock().unwrap().is_empty());
    assert!(server.session().forwards().manager().list().is_empty());
}

#[tokio::test]
async fn the_user_stops_a_forward_in_kubach_and_the_session_notices() {
    let f = fixture(true).await;
    let (id, addr) = start(&f, "pod/web-0", json!(8080)).await;
    // The "Port forwards" screen stops it in the shared manager.
    assert!(f.server.session().forwards().manager().stop(id));
    assert!(closed(addr).await);
    assert!(stop_audited(&f.audit_path, "user").await);
    for _ in 0..100 {
        if !f.server.session().grants().is_forwarding(NS) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!f.server.session().grants().is_forwarding(NS));
    let listed = f
        .server
        .call("agent", "port_forward_list", Map::new())
        .await;
    assert!(listed.text.contains("No port-forwards"), "{}", listed.text);
}

#[tokio::test]
async fn revoking_the_namespace_stops_its_forwards() {
    let f = fixture(true).await;
    let (_, addr) = start(&f, "pod/web-0", json!(8080)).await;
    f.server.session().revoke(&Grant::read(NS)).await.unwrap();
    assert!(f.server.session().forwards().manager().list().is_empty());
    assert!(closed(addr).await);
    assert!(stop_audited(&f.audit_path, "revoked").await);
}

#[tokio::test]
async fn the_agent_stops_only_its_own_forwards() {
    let f = fixture(true).await;
    let manager = f.server.session().forwards().manager();
    // The user's own forward in the same manager.
    let source = kubach_core::streams::fetch_source(&f.client, "Pod", NS, "web-0")
        .await
        .unwrap();
    let users = manager.start(&f.client, &source, 8080, 0).await.unwrap();
    assert!(users.agent.is_none());

    let (id, _) = start(&f, "pod/web-0", json!(8080)).await;
    let listed = f
        .server
        .call("agent", "port_forward_list", Map::new())
        .await;
    assert!(!listed.text.contains(&users.local_addr.to_string()));

    let refused = f
        .server
        .call("agent", "port_forward_stop", args(json!({"id": users.id})))
        .await;
    assert!(refused.is_error);
    assert!(manager.get(users.id).is_some());

    let stopped = f
        .server
        .call("agent", "port_forward_stop", args(json!({"id": id})))
        .await;
    assert!(!stopped.is_error, "{}", stopped.text);
    assert!(manager.get(id).is_none());

    // The session's end leaves the user's forward alone.
    f.server.session().close().await.unwrap();
    assert!(manager.get(users.id).is_some());
}

#[tokio::test]
async fn a_session_keeps_a_bounded_number_of_forwards() {
    let f = fixture(true).await;
    for _ in 0..super::MAX_FORWARDS_PER_SESSION {
        start(&f, "pod/web-0", json!(8080)).await;
    }
    let outcome = f
        .server
        .call(
            "agent",
            "port_forward_start",
            args(json!({"target": "pod/web-0", "port": 8080})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("port_forward_stop"),
        "{}",
        outcome.text
    );
    assert_eq!(
        f.server.session().forwards().count(),
        super::MAX_FORWARDS_PER_SESSION
    );
}

// ------------------------------------------- the agent disconnects (#34)

const TOKEN: &str = "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt";

/// Serves `f`'s session as the app does, the agents gone after `grace`.
async fn endpoint(f: &Fixture, grace: Duration) -> crate::transport::HttpEndpoint {
    let shared = crate::shared::SharedServer::new();
    shared.set(Some(f.server.clone()));
    crate::transport::serve_shared_with_grace(
        shared,
        crate::transport::HttpOptions {
            port: 0,
            token: TOKEN.into(),
            bridge: None,
        },
        grace,
    )
    .await
    .unwrap()
}

/// Sends one MCP HTTP request; returns the open connection, the status
/// and the response head (the body is left unread).
async fn send(
    port: u16,
    method: &str,
    session: Option<&str>,
    accept: &str,
    body: &str,
) -> (TcpStream, u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let session = session
        .map(|id| format!("mcp-session-id: {id}\r\nmcp-protocol-version: 2025-06-18\r\n"))
        .unwrap_or_default();
    let head = format!(
        "{method} {} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nauthorization: Bearer {TOKEN}\r\n\
         content-type: application/json\r\naccept: {accept}\r\n{session}\
         content-length: {}\r\n\r\n{body}",
        crate::transport::MCP_PATH,
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        response.push_str(&line);
    }
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    (reader.into_inner(), status, response)
}

/// Initializes an MCP session over HTTP; returns its id.
async fn initialize(port: u16) -> String {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "agent", "version": "1"}},
    })
    .to_string();
    let both = "application/json, text/event-stream";
    let (_, status, head) = send(port, "POST", None, both, &body).await;
    assert_eq!(status, 200, "{head}");
    let id = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_owned())
        })
        .expect("a session id");
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let (_, status, head) = send(port, "POST", Some(&id), both, &initialized.to_string()).await;
    assert_eq!(status, 202, "{head}");
    id
}

/// Whether forward `id` is still running after a while.
async fn still_running(f: &Fixture, id: u64) -> bool {
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.server.session().forwards().manager().get(id).is_some()
}

/// Waits up to `limit` until nothing listens on `addr` any more.
async fn closed_within(addr: SocketAddr, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn closing_the_mcp_session_stops_the_agents_forwards_at_once() {
    let f = fixture(true).await;
    let endpoint = endpoint(&f, Duration::from_secs(3600)).await;
    let port = endpoint.addr.port();
    let session = initialize(port).await;
    let (id, addr) = start(&f, "service/web", json!("http")).await;
    assert!(still_running(&f, id).await);

    let (_, status, head) = send(port, "DELETE", Some(&session), "*/*", "").await;
    assert!((200..300).contains(&status), "{head}");
    assert!(closed(addr).await);
    assert!(stop_audited(&f.audit_path, "disconnected").await);
    // The Role lost pods/portforward; the session itself goes on.
    assert!(!f.server.session().grants().is_forwarding(NS));
    assert!(f.server.session().is_active());

    // An agent connecting again works in the same session.
    initialize(port).await;
    let (again, _) = start(&f, "pod/web-0", json!(8080)).await;
    assert!(still_running(&f, again).await);
    endpoint.shutdown().await;
}

#[tokio::test]
async fn one_client_leaving_keeps_the_forwards_of_another() {
    let f = fixture(true).await;
    let endpoint = endpoint(&f, Duration::from_secs(3600)).await;
    let port = endpoint.addr.port();
    let first = initialize(port).await;
    let _second = initialize(port).await;
    let (id, _) = start(&f, "service/web", json!("http")).await;
    let (_, status, _) = send(port, "DELETE", Some(&first), "*/*", "").await;
    assert!((200..300).contains(&status));
    assert!(still_running(&f, id).await);
    endpoint.shutdown().await;
}

#[tokio::test]
async fn an_open_stream_keeps_forwards_and_its_loss_ends_them_after_the_grace() {
    let f = fixture(true).await;
    let grace = Duration::from_millis(400);
    let endpoint = endpoint(&f, grace).await;
    let port = endpoint.addr.port();
    let session = initialize(port).await;
    // The client's GET stream, as MCP clients keep it.
    let (stream, status, head) = send(port, "GET", Some(&session), "text/event-stream", "").await;
    assert_eq!(status, 200, "{head}");
    let (id, addr) = start(&f, "service/web", json!("http")).await;
    tokio::time::sleep(grace * 3).await;
    assert!(
        still_running(&f, id).await,
        "an open stream is a connected agent"
    );

    // The client is gone without closing its session.
    drop(stream);
    assert!(closed_within(addr, Duration::from_secs(30)).await);
    assert!(stop_audited(&f.audit_path, "disconnected").await);
    assert!(f.server.session().is_active());
    endpoint.shutdown().await;
}

#[tokio::test]
async fn a_silent_agent_loses_its_forwards_after_the_grace() {
    let f = fixture(true).await;
    let grace = Duration::from_millis(500);
    let endpoint = endpoint(&f, grace).await;
    let port = endpoint.addr.port();
    initialize(port).await;
    let (id, addr) = start(&f, "service/web", json!("http")).await;
    // Within the grace period nothing happens.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(f.server.session().forwards().manager().get(id).is_some());
    assert!(closed_within(addr, Duration::from_secs(10)).await);
    assert!(stop_audited(&f.audit_path, "disconnected").await);
    endpoint.shutdown().await;
}
