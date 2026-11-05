//! `cordon_node`, `uncordon_node` and `drain_node` through the MCP handler
//! against a fake API server that keeps the node and its pods.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kube::{Client, Config};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::NODE_USER_IDENTITY;
use crate::access::Access;
use crate::audit::AuditLog;
use crate::confirm::{ConfirmationRequest, RequestKind, ScriptedConfirmer};
use crate::grants::{Grant, GrantState, Grants};
use crate::identity::Identity;
use crate::rules::Capability;
use crate::server::McpServer;
use crate::session::Session;
use crate::settings::{Profile, SecurityPolicy};
use crate::testutil::{NS, OTHER_NS, catalog, permissive, scope_with};

const NODE: &str = "n1";

#[derive(Default)]
struct Cluster {
    requests: Vec<String>,
    cordoned: bool,
    pods: Vec<Value>,
    /// Disruptions the budget of [`budget`] allows, if there is one.
    budget_allows: Option<i32>,
    /// Reaches the node when it gets cordoned.
    late: Option<Value>,
}

fn pod(namespace: &str, name: &str, owner: Option<&str>) -> Value {
    let owners = match owner {
        Some(kind) => json!([{"apiVersion": "apps/v1", "kind": kind, "name": "o", "uid": "o",
                              "controller": true}]),
        None => json!([]),
    };
    json!({"apiVersion": "v1", "kind": "Pod",
           "metadata": {"name": name, "namespace": namespace, "uid": format!("uid-{name}"),
                        "labels": {"app": name}, "ownerReferences": owners},
           "spec": {"nodeName": NODE, "containers": [{"name": "c", "image": "i"}]},
           "status": {"phase": "Running"}})
}

fn with_empty_dir(mut pod: Value) -> Value {
    pod["spec"]["volumes"] = json!([{"name": "scratch", "emptyDir": {}}]);
    pod
}

/// Budget `guard`, over the pod `db-0`.
fn budget(allowed: i32) -> Value {
    json!({"apiVersion": "policy/v1", "kind": "PodDisruptionBudget",
           "metadata": {"name": "guard", "namespace": NS},
           "spec": {"selector": {"matchLabels": {"app": "db-0"}}, "minAvailable": 1},
           "status": {"disruptionsAllowed": allowed, "currentHealthy": 1, "desiredHealthy": 1,
                      "expectedPods": 1}})
}

fn name_of(pod: &Value) -> &str {
    pod["metadata"]["name"].as_str().unwrap()
}

fn respond(
    cluster: &mut Cluster,
    method: &str,
    target: &str,
    body: &str,
) -> (&'static str, String) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let not_found = || {
        let status = json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                            "message": "not found", "reason": "NotFound", "code": 404});
        ("404 Not Found", status.to_string())
    };
    if path == format!("/api/v1/nodes/{NODE}") {
        if method == "PATCH" {
            cluster.cordoned = body.contains("true");
            if cluster.cordoned
                && let Some(late) = cluster.late.take()
            {
                cluster.pods.push(late);
            }
        }
        let node = json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": NODE},
                          "spec": {"unschedulable": cluster.cordoned}});
        return ("200 OK", node.to_string());
    }
    if path == "/api/v1/pods" {
        let selected: Vec<&Value> = cluster
            .pods
            .iter()
            .filter(|pod| match query.split_once("labelSelector=app%3D") {
                Some((_, rest)) => rest.split('&').next() == Some(name_of(pod)),
                None => true,
            })
            .collect();
        let list =
            json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": selected});
        return ("200 OK", list.to_string());
    }
    if method == "GET" && path.ends_with("/poddisruptionbudgets") {
        let items = match cluster.budget_allows {
            Some(allowed) if path.contains(&format!("/namespaces/{NS}/")) => vec![budget(allowed)],
            _ => Vec::new(),
        };
        let list = json!({"apiVersion": "policy/v1", "kind": "PodDisruptionBudgetList",
                          "metadata": {}, "items": items});
        return ("200 OK", list.to_string());
    }
    let Some((_, rest)) = path.split_once("/pods/") else {
        return not_found();
    };
    let (name, sub) = rest.split_once('/').unwrap_or((rest, ""));
    let Some(at) = cluster.pods.iter().position(|pod| name_of(pod) == name) else {
        return not_found();
    };
    match (method, sub) {
        ("GET", "") => ("200 OK", cluster.pods[at].to_string()),
        ("POST", "eviction") => {
            cluster.pods.remove(at);
            let status = json!({"kind": "Status", "apiVersion": "v1", "status": "Success"});
            ("201 Created", status.to_string())
        }
        _ => not_found(),
    }
}

struct FakeApi {
    url: String,
    cluster: Arc<Mutex<Cluster>>,
}

/// Answers one connection's requests until it closes.
async fn serve(stream: tokio::net::TcpStream, cluster: Arc<Mutex<Cluster>>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).await.unwrap_or(0) == 0 {
            return;
        }
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let mut parts = head.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default().to_owned();
        let body = String::from_utf8_lossy(&body).into_owned();
        let (status, text) = {
            let mut cluster = cluster.lock().unwrap();
            cluster.requests.push(format!("{method} {target} {body}"));
            respond(&mut cluster, &method, &target, &body)
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{text}",
            text.len()
        );
        if write.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

impl FakeApi {
    async fn start(cluster: Cluster) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cluster = Arc::new(Mutex::new(cluster));
        let shared = cluster.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { url, cluster }
    }

    fn requests(&self) -> Vec<String> {
        self.cluster.lock().unwrap().requests.clone()
    }

    fn writes(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| !r.starts_with("GET "))
            .collect()
    }

    fn evictions(&self) -> Vec<String> {
        self.writes()
            .into_iter()
            .filter(|r| r.contains("/eviction"))
            .collect()
    }
}

struct Fixture {
    api: FakeApi,
    server: McpServer,
    confirmer: Arc<ScriptedConfirmer>,
    audit_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn fixture_with(
    cluster: Cluster,
    policy: SecurityPolicy,
    confirmer: ScriptedConfirmer,
    namespaces: &[&str],
) -> Fixture {
    let api = FakeApi::start(cluster).await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::Deploy, &policy, namespaces);
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    );
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let confirmer = Arc::new(confirmer);
    let server = McpServer::new(session, confirmer.clone(), Some(audit));
    Fixture {
        api,
        server,
        confirmer,
        audit_path,
        _dir: dir,
    }
}

async fn fixture(cluster: Cluster) -> Fixture {
    fixture_with(cluster, permissive(), ScriptedConfirmer::approving(), &[]).await
}

/// A pod of every kind a drain tells apart.
fn node_pods() -> Vec<Value> {
    vec![
        pod(NS, "web-0", Some("ReplicaSet")),
        pod(NS, "bare-0", None),
        pod(NS, "agent-0", Some("DaemonSet")),
        pod(OTHER_NS, "other-0", Some("ReplicaSet")),
    ]
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn audit_lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn calls(f: &Fixture) -> Vec<ConfirmationRequest> {
    f.confirmer
        .requests()
        .into_iter()
        .filter(|r| r.kind == RequestKind::Call)
        .collect()
}

#[tokio::test]
async fn cordon_names_the_node_runs_as_the_user_and_is_audited() {
    let f = fixture(Cluster::default()).await;
    let outcome = f
        .server
        .call("agent", "cordon_node", args(json!({"name": NODE})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let [window] = calls(&f).try_into().unwrap();
    assert!(window.summary.contains(NODE), "{}", window.summary);
    assert!(window.summary.contains("kubeconfig user"));
    // The tick of its own, no "don't ask again".
    assert!(!window.dangerous.is_empty());
    assert!(!window.allow_remember);
    assert!(window.namespace.is_empty());
    let writes = f.api.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(
        writes[0].starts_with(&format!("PATCH /api/v1/nodes/{NODE}?")),
        "{writes:?}"
    );
    assert!(writes[0].contains("fieldManager=kubach-mcp"), "{writes:?}");
    assert!(writes[0].contains(r#"{"spec":{"unschedulable":true}}"#));
    let lines = audit_lines(&f.audit_path);
    let entry = lines.last().unwrap();
    assert_eq!(entry["tool"], "cordon_node");
    assert_eq!(entry["decided_by"], "user");
    assert_eq!(entry["identity"], NODE_USER_IDENTITY);
    assert_eq!(entry["change"]["node"], NODE);

    // Nothing to do: nobody is asked, nothing is written.
    let again = f
        .server
        .call("agent", "cordon_node", args(json!({"name": NODE})))
        .await;
    assert!(again.text.contains("already"), "{}", again.text);
    assert_eq!(calls(&f).len(), 1);

    let outcome = f
        .server
        .call("agent", "uncordon_node", args(json!({"name": NODE})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(calls(&f).len(), 2);
    assert!(f.api.writes()[1].contains(r#"{"spec":{"unschedulable":null}}"#));
}

#[tokio::test]
async fn a_no_writes_nothing() {
    let f = fixture_with(
        Cluster {
            pods: vec![pod(NS, "web-0", Some("ReplicaSet"))],
            ..Cluster::default()
        },
        permissive(),
        ScriptedConfirmer::denying(),
        &[],
    )
    .await;
    for tool in ["cordon_node", "drain_node"] {
        let outcome = f
            .server
            .call("agent", tool, args(json!({"name": NODE})))
            .await;
        assert!(outcome.is_error, "{}", outcome.text);
    }
    assert_eq!(calls(&f).len(), 2);
    assert_eq!(f.api.writes(), Vec::<String>::new());
}

#[tokio::test]
async fn node_changes_are_never_remembered() {
    let f = fixture_with(
        Cluster::default(),
        permissive(),
        ScriptedConfirmer::approving_for_session(),
        &[],
    )
    .await;
    for tool in ["cordon_node", "uncordon_node", "cordon_node"] {
        let outcome = f
            .server
            .call("agent", tool, args(json!({"name": NODE})))
            .await;
        assert!(!outcome.is_error, "{}", outcome.text);
    }
    assert_eq!(calls(&f).len(), 3);
    assert!(f.server.session().remembered().is_empty());
}

#[tokio::test]
async fn the_window_lists_every_pod_and_only_those_are_evicted() {
    let mut pods = node_pods();
    pods.push(with_empty_dir(pod(NS, "cache-0", Some("ReplicaSet"))));
    let mut failed = pod(NS, "job-0", None);
    failed["status"]["phase"] = json!("Failed");
    pods.push(failed);
    let f = fixture(Cluster {
        pods,
        ..Cluster::default()
    })
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "drain_node",
            args(
                json!({"name": NODE, "ignoreDaemonsets": true, "deleteEmptyDirData": true,
                        "timeoutSeconds": 5}),
            ),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let [window] = calls(&f).try_into().unwrap();
    assert!(window.summary.contains(NODE), "{}", window.summary);
    assert!(
        window.summary.contains("evict 3 pods"),
        "{}",
        window.summary
    );
    for pod in ["web-0", "cache-0", "other-0", "bare-0", "agent-0", "job-0"] {
        assert!(window.details.contains(pod), "{pod}: {}", window.details);
    }
    assert!(!window.allow_remember);
    // Red and green in the window are for diffs.
    assert!(
        !window
            .details
            .lines()
            .any(|l| l.starts_with('-') || l.starts_with('+')),
        "{}",
        window.details
    );
    assert!(
        window
            .dangerous
            .iter()
            .any(|d| d.path.ends_with("cache-0") && d.message.contains("emptyDir"))
    );

    let writes = f.api.writes();
    assert!(writes[0].starts_with("PATCH "), "{writes:?}");
    // The Eviction API only: no DELETE, whatever the pod.
    assert!(
        !writes.iter().any(|w| w.starts_with("DELETE ")),
        "{writes:?}"
    );
    let evicted = f.api.evictions();
    assert_eq!(evicted.len(), 3, "{evicted:?}");
    for kept in ["bare-0", "agent-0", "job-0"] {
        assert!(!evicted.iter().any(|e| e.contains(kept)), "{evicted:?}");
    }
    let text = &outcome.text;
    assert!(text.contains("3 of 3 pods are gone"), "{text}");
    assert!(text.contains("Left in place"), "{text}");
    assert!(text.contains("bare-0"), "{text}");
    assert!(text.contains("not complete"), "{text}");

    let lines = audit_lines(&f.audit_path);
    let entry = lines.last().unwrap();
    assert_eq!(entry["identity"], NODE_USER_IDENTITY);
    assert_eq!(entry["decided_by"], "user");
    assert_eq!(entry["change"]["evicted"].as_array().unwrap().len(), 3);
    assert_eq!(entry["change"]["leftInPlace"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_preview_changes_nothing_and_says_what_a_drain_would_do() {
    let mut pods = node_pods();
    pods.push(pod(NS, "db-0", Some("StatefulSet")));
    let f = fixture_with(
        Cluster {
            pods,
            budget_allows: Some(0),
            ..Cluster::default()
        },
        // Reading nodes is all a preview needs.
        SecurityPolicy::with_minimum(Access {
            nodes: true,
            ..Access::NONE
        }),
        ScriptedConfirmer::denying(),
        &[],
    )
    .await;
    let mut grants = Grants::new();
    grants.set(Grant::read(OTHER_NS), GrantState::Denied);
    f.server.session().set_grants(grants);
    let outcome = f
        .server
        .call(
            "agent",
            "drain_node",
            args(json!({"name": NODE, "ignoreDaemonsets": true, "preview": true})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let text = &outcome.text;
    let after = |title: &str, pod: &str| {
        let at = text
            .find(title)
            .unwrap_or_else(|| panic!("{title}: {text}"));
        assert!(text[at..].contains(pod), "{pod}: {text}");
    };
    after("To evict (1):", "web-0");
    after("Blocked by a PodDisruptionBudget", "db-0");
    assert!(text.contains("PodDisruptionBudget guard"), "{text}");
    after("No controller", "bare-0");
    after("Left on the node", "agent-0");
    // Taken back from the session: counted, not named.
    assert!(!text.contains("other-0"), "{text}");
    assert!(text.contains("may not read: 1 (To evict)"), "{text}");
    assert!(f.confirmer.requests().is_empty());
    assert_eq!(f.api.writes(), Vec::<String>::new());
}

#[tokio::test]
async fn a_pod_blocked_by_a_budget_stays_and_the_drain_says_so() {
    let f = fixture(Cluster {
        pods: vec![pod(NS, "db-0", Some("StatefulSet"))],
        budget_allows: Some(0),
        ..Cluster::default()
    })
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "drain_node",
            args(json!({"name": NODE, "timeoutSeconds": 1})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(f.api.evictions().is_empty(), "{:?}", f.api.writes());
    let text = &outcome.text;
    assert!(text.contains("0 of 1 pods are gone"), "{text}");
    assert!(text.contains("Not evicted: "), "{text}");
    assert!(text.contains("guard"), "{text}");
    assert!(text.contains("stays cordoned"), "{text}");
    let [window] = calls(&f).try_into().unwrap();
    assert!(window.details.contains("db-0"), "{}", window.details);
    assert!(window.details.contains("Blocked by"), "{}", window.details);
    let lines = audit_lines(&f.audit_path);
    let failed = &lines.last().unwrap()["change"]["failed"];
    assert_eq!(failed.as_array().unwrap().len(), 1, "{failed}");
}

#[tokio::test]
async fn a_pod_the_user_did_not_see_is_not_evicted() {
    let f = fixture(Cluster {
        pods: vec![pod(NS, "web-0", Some("ReplicaSet"))],
        late: Some(pod(NS, "late-0", Some("ReplicaSet"))),
        ..Cluster::default()
    })
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "drain_node",
            args(json!({"name": NODE, "timeoutSeconds": 5})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let evicted = f.api.evictions();
    assert_eq!(evicted.len(), 1, "{evicted:?}");
    assert!(evicted[0].contains("web-0"), "{evicted:?}");
    let text = &outcome.text;
    assert!(text.contains("after the user approved"), "{text}");
    assert!(text.contains("late-0"), "{text}");
    let lines = audit_lines(&f.audit_path);
    assert_eq!(
        lines.last().unwrap()["change"]["notApproved"],
        json!([format!("{NS}/late-0")])
    );
}

#[tokio::test]
async fn pods_that_stop_the_drain_change_nothing_and_ask_nobody() {
    let f = fixture(Cluster {
        pods: vec![
            pod(NS, "agent-0", Some("DaemonSet")),
            with_empty_dir(pod(NS, "cache-0", Some("ReplicaSet"))),
            pod(NS, "web-0", Some("ReplicaSet")),
        ],
        ..Cluster::default()
    })
    .await;
    let outcome = f
        .server
        .call("agent", "drain_node", args(json!({"name": NODE})))
        .await;
    assert!(outcome.is_error, "{}", outcome.text);
    let text = &outcome.text;
    assert!(text.contains("ignoreDaemonsets"), "{text}");
    assert!(text.contains("agent-0"), "{text}");
    assert!(text.contains("cache-0"), "{text}");
    assert!(f.confirmer.requests().is_empty());
    assert_eq!(f.api.writes(), Vec::<String>::new());
}

#[tokio::test]
async fn a_selector_narrows_the_drain_to_its_pods() {
    let f = fixture(Cluster {
        pods: node_pods(),
        ..Cluster::default()
    })
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "drain_node",
            args(json!({"name": NODE, "podSelector": "app=web-0", "timeoutSeconds": 5})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let [window] = calls(&f).try_into().unwrap();
    assert!(
        window.details.contains("labels app=web-0"),
        "{}",
        window.details
    );
    assert!(!window.details.contains("bare-0"), "{}", window.details);
    assert_eq!(f.api.evictions().len(), 1);
    assert!(outcome.text.contains("Drained."), "{}", outcome.text);
}

#[tokio::test]
async fn there_is_no_force_and_bad_arguments_reach_no_api() {
    let f = fixture(Cluster {
        pods: node_pods(),
        ..Cluster::default()
    })
    .await;
    for call in [
        json!({"name": NODE, "force": true}),
        json!({"name": NODE, "podSelector": " "}),
        json!({"name": NODE, "podSelector": "a\nb"}),
        json!({"name": NODE, "timeoutSeconds": 0}),
        json!({"name": NODE, "timeoutSeconds": 3601}),
        json!({"name": "../secrets"}),
    ] {
        let outcome = f.server.call("agent", "drain_node", args(call)).await;
        assert!(outcome.is_error, "{}", outcome.text);
    }
    assert_eq!(f.api.requests(), Vec::<String>::new());
    assert!(f.confirmer.requests().is_empty());
    let tools = f.server.tools();
    let drain = tools.iter().find(|t| t.name == "drain_node").unwrap();
    let schema = serde_json::to_value(drain.input_schema.as_ref()).unwrap();
    assert!(schema["properties"].get("force").is_none(), "{schema}");
}

#[tokio::test]
async fn a_pinned_session_previews_but_never_writes_on_a_node() {
    let f = fixture_with(
        Cluster {
            pods: node_pods(),
            ..Cluster::default()
        },
        permissive(),
        ScriptedConfirmer::approving(),
        &[NS],
    )
    .await;
    for (tool, call) in [
        ("cordon_node", json!({"name": NODE})),
        ("uncordon_node", json!({"name": NODE})),
        (
            "drain_node",
            json!({"name": NODE, "ignoreDaemonsets": true}),
        ),
    ] {
        let outcome = f.server.call("agent", tool, args(call)).await;
        assert!(outcome.is_error, "{}", outcome.text);
        let text = &outcome.text;
        assert!(text.contains("limited to namespace"), "{text}");
    }
    assert_eq!(f.api.requests(), Vec::<String>::new());
    assert!(f.confirmer.requests().is_empty());
    let names: Vec<String> = f
        .server
        .tools()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n.ends_with("cordon_node")),
        "{names:?}"
    );
    assert!(names.iter().any(|n| n == "drain_node"), "{names:?}");

    let preview = f
        .server
        .call(
            "agent",
            "drain_node",
            args(json!({"name": NODE, "ignoreDaemonsets": true, "preview": true})),
        )
        .await;
    assert!(!preview.is_error, "{}", preview.text);
    assert!(preview.text.contains("web-0"), "{}", preview.text);
    assert!(!preview.text.contains("other-0"), "{}", preview.text);
    assert_eq!(f.api.writes(), Vec::<String>::new());
}

#[tokio::test]
async fn beyond_the_minimum_the_user_is_asked_for_access_first() {
    let f = fixture_with(
        Cluster::default(),
        SecurityPolicy::with_minimum(Access::NONE),
        ScriptedConfirmer::approving(),
        &[],
    )
    .await;
    let outcome = f
        .server
        .call("agent", "cordon_node", args(json!({"name": NODE})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let RequestKind::Access { raise, .. } = &requests[0].kind else {
        panic!("{:?}", requests[0].kind);
    };
    assert_eq!(raise.profile, Profile::Deploy);
    assert!(raise.nodes);
    // Access approves no change: the call has its own window.
    assert_eq!(requests[1].kind, RequestKind::Call);
    let lines = audit_lines(&f.audit_path);
    assert!(lines.iter().any(|l| l["tool"] == "access:cordon_node"));

    // Refused access: no window for the call, nothing reaches the API.
    let f = fixture_with(
        Cluster::default(),
        SecurityPolicy::with_minimum(Access::NONE),
        ScriptedConfirmer::denying(),
        &[],
    )
    .await;
    let outcome = f
        .server
        .call("agent", "cordon_node", args(json!({"name": NODE})))
        .await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert_eq!(f.confirmer.requests().len(), 1);
    assert_eq!(f.api.requests(), Vec::<String>::new());
}

#[tokio::test]
async fn a_drain_stops_with_the_session() {
    let f = fixture(Cluster {
        pods: vec![pod(NS, "db-0", Some("StatefulSet"))],
        budget_allows: Some(0),
        ..Cluster::default()
    })
    .await;
    let started = Instant::now();
    let drain = f.server.call(
        "agent",
        "drain_node",
        args(json!({"name": NODE, "timeoutSeconds": 120})),
    );
    let close = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        f.server.session().close().await.unwrap();
    };
    let (outcome, ()) = tokio::join!(drain, close);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(outcome.text.contains("Not evicted"), "{}", outcome.text);
    assert!(f.api.evictions().is_empty());
}
