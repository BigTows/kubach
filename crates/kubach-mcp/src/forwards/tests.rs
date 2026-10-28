//! The port-forward tools (#17) against a fake API server that serves pod
//! `web-0` and Service `web`. Forwards listen for real, on 127.0.0.1;
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
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
                    let (status, body) = if path == format!("{pods}/web-0") {
                        ("200 OK", pod().to_string())
                    } else if path.starts_with(&format!("{pods}?")) {
                        let list = json!({"apiVersion": "v1", "kind": "PodList",
                                          "metadata": {}, "items": [pod()]});
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
    let (url, requests) = fake_api().await;
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
async fn a_rejected_forward_reads_and_opens_nothing() {
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
    assert!(f.requests.lock().unwrap().is_empty());
    assert!(!f.server.session().grants().is_forwarding(NS));
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
