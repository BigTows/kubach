//! `scale_batch` and `rollout_restart_batch` under a real server loop
//! (#108): a fake API server holds objects and records every write, because
//! what must never be sent matters as much as what comes back.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kubach_core::plugin::BoxFuture;
use kube::{Client, Config};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::McpServer;
use crate::audit::AuditLog;
use crate::batch_tools::{MAX_CONCURRENCY, MAX_OBJECTS};
use crate::confirm::{
    BatchAction, BatchChange, ConfirmationDecision, ConfirmationRequest, Confirmer, RequestKind,
    ScriptedConfirmer,
};
use crate::grants::{Grant, GrantState, Grants};
use crate::identity::Identity;
use crate::remember::{Remembered, RememberedAction};
use crate::rules::Capability;
use crate::scope::{SessionScope, SessionSpec};
use crate::session::Session;
use crate::settings::{Profile, SecurityPolicy};
use crate::testutil::{catalog, permissive};

#[derive(Clone)]
struct Object {
    uid: String,
    replicas: i64,
    /// Matches every label selector of the tests.
    labeled: bool,
}

/// (namespace, plural, name).
type Key = (String, String, String);

#[derive(Default)]
struct Cluster {
    objects: BTreeMap<Key, Object>,
    /// Every PATCH: path and body.
    writes: Vec<(String, Value)>,
    reads: Vec<String>,
    /// Names whose PATCH fails with a 500.
    broken: HashSet<String>,
    patching: usize,
    most_at_once: usize,
    /// How long a PATCH takes.
    delay: Option<Duration>,
}

type Shared = Arc<Mutex<Cluster>>;

fn key(namespace: &str, plural: &str, name: &str) -> Key {
    (namespace.to_owned(), plural.to_owned(), name.to_owned())
}

fn add(cluster: &Shared, namespace: &str, plural: &str, name: &str, replicas: i64) {
    let uid = format!("uid-{namespace}-{name}");
    let object = Object {
        uid,
        replicas,
        labeled: true,
    };
    let key = key(namespace, plural, name);
    cluster.lock().unwrap().objects.insert(key, object);
}

fn kind_of(plural: &str) -> &'static str {
    match plural {
        "deployments" => "Deployment",
        "daemonsets" => "DaemonSet",
        _ => "StatefulSet",
    }
}

fn body_of(key: &Key, object: &Object) -> Value {
    json!({
        "apiVersion": "apps/v1", "kind": kind_of(&key.1),
        "metadata": {"name": key.2, "namespace": key.0, "uid": object.uid},
        "spec": {"replicas": object.replicas},
    })
}

fn failure(line: &str, code: u16) -> (String, Value) {
    (
        line.to_owned(),
        json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
               "message": line, "code": code}),
    )
}

fn answer_get(cluster: &mut Cluster, path: &str) -> (String, Value) {
    cluster.reads.push(path.to_owned());
    let bare = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = bare.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["api", "v1", "namespaces"] => {
            let mut names: Vec<&String> = cluster.objects.keys().map(|key| &key.0).collect();
            names.dedup();
            let items: Vec<Value> = names
                .iter()
                .map(|name| json!({"metadata": {"name": name}}))
                .collect();
            let list = json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadataList",
                              "metadata": {}, "items": items});
            ("200 OK".to_owned(), list)
        }
        ["apis", "apps", "v1", "namespaces", namespace, plural] => {
            let items: Vec<Value> = cluster
                .objects
                .iter()
                .filter(|(key, object)| key.0 == *namespace && key.1 == *plural && object.labeled)
                .map(|(key, object)| body_of(key, object))
                .collect();
            let list =
                json!({"apiVersion": "apps/v1", "kind": "List", "metadata": {}, "items": items});
            ("200 OK".to_owned(), list)
        }
        ["apis", "apps", "v1", "namespaces", namespace, plural, name] => {
            match cluster.objects.get(&key(namespace, plural, name)) {
                Some(object) => (
                    "200 OK".to_owned(),
                    body_of(&key(namespace, plural, name), object),
                ),
                None => failure("404 Not Found", 404),
            }
        }
        _ => failure("404 Not Found", 404),
    }
}

fn answer_patch(cluster: &mut Cluster, path: &str, body: &Value) -> (String, Value) {
    let bare = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = bare.trim_start_matches('/').split('/').collect();
    let [
        "apis",
        "apps",
        "v1",
        "namespaces",
        namespace,
        plural,
        name,
        rest @ ..,
    ] = parts.as_slice()
    else {
        return failure("404 Not Found", 404);
    };
    cluster.writes.push((path.to_owned(), body.clone()));
    if cluster.broken.contains(*name) {
        return failure("500 Internal Server Error", 500);
    }
    let key = key(namespace, plural, name);
    let Some(object) = cluster.objects.get_mut(&key) else {
        return failure("404 Not Found", 404);
    };
    if body.pointer("/metadata/uid").and_then(Value::as_str) != Some(object.uid.as_str()) {
        return failure("409 Conflict", 409);
    }
    if rest == ["scale"] {
        object.replicas = body["spec"]["replicas"].as_i64().unwrap();
        let scale = json!({"apiVersion": "autoscaling/v1", "kind": "Scale",
                           "metadata": {"name": key.2, "namespace": key.0},
                           "spec": {"replicas": object.replicas}});
        return ("200 OK".to_owned(), scale);
    }
    ("200 OK".to_owned(), body_of(&key, object))
}

async fn serve(cluster: Shared) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let cluster = cluster.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).await.unwrap_or(0) == 0 || header == "\r\n" {
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
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or_default().to_owned();
                let path = words.next().unwrap_or_default().to_owned();
                let (status, reply) = if method == "PATCH" {
                    let delay = {
                        let mut cluster = cluster.lock().unwrap();
                        cluster.patching += 1;
                        cluster.most_at_once = cluster.most_at_once.max(cluster.patching);
                        cluster.delay
                    };
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let mut cluster = cluster.lock().unwrap();
                    cluster.patching -= 1;
                    answer_patch(&mut cluster, &path, &body)
                } else {
                    answer_get(&mut cluster.lock().unwrap(), &path)
                };
                let reply = reply.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = write.write_all(response.as_bytes()).await;
            });
        }
    });
    url
}

/// Runs `during` when the batch window opens, as if the world moved on
/// while the user read it, then answers like `answer`.
struct Moving {
    answer: ScriptedConfirmer,
    during: Box<dyn Fn() + Send + Sync>,
}

impl Confirmer for Moving {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        if matches!(request.kind, RequestKind::Batch { .. }) {
            (self.during)();
        }
        self.answer.confirm(request)
    }
}

struct Setup<'a> {
    pinned: &'a [&'a str],
    read: &'a [&'a str],
    refused: &'a [&'a str],
    impersonated: bool,
    profile: Profile,
    policy: SecurityPolicy,
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Self {
            pinned: &[],
            read: &["team-a", "team-b"],
            refused: &[],
            impersonated: false,
            profile: Profile::Deploy,
            policy: permissive(),
        }
    }
}

struct Fixture {
    cluster: Shared,
    server: McpServer,
    session: Arc<Session>,
    audit_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn call(&self, tool: &str, value: Value) -> super::CallOutcome {
        let Value::Object(map) = value else {
            panic!("not an object")
        };
        self.server.call("agent", tool, map).await
    }

    fn writes(&self) -> Vec<(String, Value)> {
        self.cluster.lock().unwrap().writes.clone()
    }

    fn wrote_to(&self, name: &str) -> bool {
        let needle = format!("/{name}");
        self.writes().iter().any(|(path, _)| {
            let bare = path.split('?').next().unwrap_or(path);
            bare.ends_with(&needle) || bare.ends_with(&format!("{needle}/scale"))
        })
    }

    fn replicas(&self, namespace: &str, name: &str) -> i64 {
        self.cluster.lock().unwrap().objects[&key(namespace, "deployments", name)].replicas
    }

    /// The object lines of the audit log, the call's own line left out.
    fn audited(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.audit_path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| {
                line["args"].get("uid").is_some()
                    || line["args"].get("object").is_some()
                    || line["result"] == "left-out"
            })
            .collect()
    }

    fn audit_of(&self, name: &str) -> Value {
        self.audited()
            .into_iter()
            .rev()
            .find(|line| {
                line["args"]["name"] == name
                    || line["args"]["object"]
                        .as_str()
                        .is_some_and(|object| object.ends_with(&format!("/{name}")))
            })
            .unwrap_or_else(|| panic!("no audit line for {name}"))
    }
}

async fn fixture(cluster: Shared, confirmer: Arc<dyn Confirmer>, setup: Setup<'_>) -> Fixture {
    let url = serve(cluster.clone()).await;
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let spec = SessionSpec {
        context: "minikube".into(),
        server_url: "https://127.0.0.1:8443".into(),
        namespaces: setup.pinned.iter().map(|ns| (*ns).to_owned()).collect(),
        profile: setup.profile,
        ttl: None,
        hints: Vec::new(),
    };
    let scope = SessionScope::new(&spec, &setup.policy).unwrap();
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let identity = if setup.impersonated {
        Identity::Impersonation {
            client,
            user: "kubach:mcp".into(),
            groups: Vec::new(),
        }
    } else {
        Identity::ClientOnly { client }
    };
    let session = Session::from_parts(scope, identity, catalog(), capabilities);
    let mut table = Grants::new();
    for ns in setup.read {
        table.set(Grant::read(*ns), GrantState::Granted);
        if setup.impersonated {
            table.set(Grant::modify(*ns), GrantState::Granted);
        }
    }
    for ns in setup.refused {
        table.set(Grant::read(*ns), GrantState::Denied);
    }
    session.set_grants(table);
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let server = McpServer::new(session.clone(), confirmer, Some(audit));
    Fixture {
        cluster,
        server,
        session,
        audit_path,
        _dir: dir,
    }
}

/// Deployments `web` (3) and `api` (2) in team-a, `db` (1) in team-b.
fn two_teams() -> Shared {
    let cluster = Shared::default();
    add(&cluster, "team-a", "deployments", "web", 3);
    add(&cluster, "team-a", "deployments", "api", 2);
    add(&cluster, "team-b", "deployments", "db", 1);
    cluster
}

fn batches(confirmer: &ScriptedConfirmer) -> Vec<BatchChange> {
    confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match request.kind {
            RequestKind::Batch { batch } => Some(*batch),
            _ => None,
        })
        .collect()
}

fn listed(batch: &BatchChange) -> Vec<String> {
    batch
        .objects
        .iter()
        .map(|object| {
            let change = object.change().unwrap_or_default();
            format!("{} {change}", object.label()).trim_end().to_owned()
        })
        .collect()
}

const ACROSS: &str = "team-a,team-b";

fn across() -> Vec<&'static str> {
    ACROSS.split(',').collect()
}

#[tokio::test]
async fn one_window_lists_old_and_new_replicas_and_the_answer_can_roll_back() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(two_teams(), confirmer.clone(), Setup::default()).await;

    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "tier=web", "namespaces": across(), "replicas": 0}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);

    assert_eq!(confirmer.requests().len(), 1, "one window");
    let question = &confirmer.requests()[0];
    assert!(!question.allow_remember);
    assert_eq!(question.namespace, "", "two namespaces");
    let shown = batches(&confirmer);
    assert_eq!(shown[0].action, BatchAction::Scale);
    assert_eq!(
        listed(&shown[0]),
        [
            "Deployment team-a/api 2 → 0",
            "Deployment team-a/web 3 → 0",
            "Deployment team-b/db 1 → 0"
        ]
    );

    assert_eq!(f.replicas("team-a", "web"), 0);
    assert_eq!(f.replicas("team-b", "db"), 0);
    assert!(
        outcome.text.starts_with("scale_batch: complete."),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("changed\tDeployment team-a/web\t3\t0\t"),
        "{}",
        outcome.text
    );

    // The answer's rollback is a valid call that restores every count.
    let rollback = outcome
        .text
        .lines()
        .find_map(|line| line.strip_prefix("To roll back, call scale_batch with: "))
        .expect("rollback line");
    let rollback: Value = serde_json::from_str(rollback).unwrap();
    let outcome = f.call("scale_batch", rollback).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(f.replicas("team-a", "web"), 3);
    assert_eq!(f.replicas("team-a", "api"), 2);
    assert_eq!(f.replicas("team-b", "db"), 1);
}

#[tokio::test]
async fn every_object_has_its_audit_line_under_its_namespace() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = two_teams();
    cluster.lock().unwrap().broken.insert("api".into());
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "replicas": 5, "objects": [
                {"name": "web", "namespace": "team-a"},
                {"name": "api", "namespace": "team-a"},
                {"name": "db", "namespace": "team-b", "replicas": 1},
            ]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);

    let web = f.audit_of("web");
    assert_eq!(web["tool"], "scale_batch");
    assert_eq!(web["result"], "ok");
    assert_eq!(web["decided_by"], "user");
    assert_eq!(web["args"]["uid"], "uid-team-a-web");
    assert_eq!(web["args"]["originalReplicas"], 3);
    assert_eq!(web["args"]["replicas"], 5);
    assert_eq!(f.audit_of("api")["result"], "500");
    assert_eq!(f.audit_of("db")["result"], "unchanged");
    assert_eq!(f.audited().len(), 3);
}

#[tokio::test]
async fn one_failure_hides_nothing_and_the_answer_says_which_is_which() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = two_teams();
    cluster.lock().unwrap().broken.insert("api".into());
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "rollout_restart_batch",
            json!({"kind": "Deployment", "objects": [
                {"name": "web", "namespace": "team-a"},
                {"name": "api", "namespace": "team-a"},
                {"name": "db", "namespace": "team-b"},
            ]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.starts_with("rollout_restart_batch: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("2 of 3 approved objects changed, 1 failed"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("changed\tDeployment team-a/web"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("failed\tDeployment team-a/api"),
        "{}",
        outcome.text
    );
    assert!(f.wrote_to("db"));
    assert!(!outcome.text.contains("To roll back"), "{}", outcome.text);
    // Restarts patch the object itself, bound to its UID.
    let writes = f.writes();
    let (_, web) = writes
        .iter()
        .find(|(path, _)| path.contains("/deployments/web"))
        .unwrap();
    assert_eq!(web["metadata"]["uid"], "uid-team-a-web");
    assert!(
        web.pointer("/spec/template/metadata/annotations/kubectl.kubernetes.io~1restartedAt")
            .is_some()
    );
}

#[tokio::test]
async fn nothing_changed_at_all_is_an_error_that_still_names_each_object() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = two_teams();
    cluster.lock().unwrap().broken.insert("db".into());
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "rollout_restart_batch",
            json!({"kind": "Deployment", "namespace": "team-b", "objects": [{"name": "db"}]}),
        )
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("failed\tDeployment team-b/db"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn an_object_appearing_after_the_answer_is_reported_and_not_touched() {
    let cluster = two_teams();
    let late = cluster.clone();
    let confirmer = Arc::new(Moving {
        answer: ScriptedConfirmer::approving(),
        during: Box::new(move || add(&late, "team-a", "deployments", "late", 4)),
    });
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "tier=web", "namespace": "team-a", "replicas": 0}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(f.replicas("team-a", "web"), 0);
    assert_eq!(f.replicas("team-a", "late"), 4, "never approved");
    assert!(!f.wrote_to("late"));
    assert!(
        outcome.text.starts_with("scale_batch: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("Appeared after the user's answer, not touched: Deployment team-a/late"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audit_of("late")["result"], "not-touched");
}

#[tokio::test]
async fn an_object_replaced_after_the_answer_is_not_touched() {
    let cluster = two_teams();
    let moved = cluster.clone();
    let confirmer = Arc::new(Moving {
        answer: ScriptedConfirmer::approving(),
        during: Box::new(move || {
            let key = key("team-a", "deployments", "web");
            let mut cluster = moved.lock().unwrap();
            cluster.objects.get_mut(&key).unwrap().uid = "someone-elses".into();
        }),
    });
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "namespace": "team-a", "replicas": 0,
                   "objects": [{"name": "web"}, {"name": "api"}]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(f.replicas("team-a", "web"), 3);
    assert!(!f.wrote_to("web"));
    assert_eq!(f.replicas("team-a", "api"), 0);
    assert!(
        outcome.text.contains("not-touched\tDeployment team-a/web"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("replaced"), "{}", outcome.text);
}

#[tokio::test]
async fn the_original_replicas_are_those_right_before_the_change() {
    let cluster = two_teams();
    let moved = cluster.clone();
    let confirmer = Arc::new(Moving {
        answer: ScriptedConfirmer::approving(),
        during: Box::new(move || {
            let key = key("team-a", "deployments", "web");
            moved
                .lock()
                .unwrap()
                .objects
                .get_mut(&key)
                .unwrap()
                .replicas = 7;
        }),
    });
    let f = fixture(cluster, confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "namespace": "team-a", "replicas": 0,
                   "objects": [{"name": "web"}]}),
        )
        .await;
    assert!(
        outcome
            .text
            .contains("changed\tDeployment team-a/web\t7\t0"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains(r#""replicas":7"#), "{}", outcome.text);
}

#[tokio::test]
async fn rights_gone_while_the_window_was_open_stop_the_change() {
    let cluster = two_teams();
    let session: Arc<Mutex<Option<Arc<Session>>>> = Arc::default();
    let revoked = session.clone();
    let confirmer = Arc::new(Moving {
        answer: ScriptedConfirmer::approving(),
        during: Box::new(move || {
            let mut table = Grants::new();
            table.set(Grant::read("team-a"), GrantState::Denied);
            table.set(Grant::read("team-b"), GrantState::Granted);
            revoked.lock().unwrap().as_ref().unwrap().set_grants(table);
        }),
    });
    let f = fixture(cluster, confirmer, Setup::default()).await;
    *session.lock().unwrap() = Some(f.session.clone());
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "replicas": 0, "objects": [
                {"name": "web", "namespace": "team-a"},
                {"name": "db", "namespace": "team-b"},
            ]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(!f.wrote_to("web"));
    assert!(f.wrote_to("db"));
    assert!(
        outcome.text.contains("not-touched\tDeployment team-a/web"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audit_of("web")["result"], "not-touched");
}

#[tokio::test]
async fn a_refused_namespace_is_a_named_gap_in_the_window_and_the_answer() {
    let cluster = two_teams();
    add(&cluster, "team-c", "deployments", "cache", 2);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let setup = Setup {
        refused: &["team-c"],
        ..Setup::default()
    };
    let f = fixture(cluster.clone(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "tier=web", "replicas": 0,
                   "namespaces": ["team-a", "team-c"]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let reads = cluster.lock().unwrap().reads.clone();
    assert!(
        !reads.iter().any(|path| path.contains("team-c")),
        "{reads:?}"
    );
    assert_eq!(confirmer.requests().len(), 1);
    let shown = &batches(&confirmer)[0];
    assert_eq!(shown.objects.len(), 2);
    assert!(
        shown.left_out[0].contains("namespaces team-c:"),
        "{shown:?}"
    );
    assert!(
        outcome.text.starts_with("scale_batch: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("- namespaces team-c:"),
        "{}",
        outcome.text
    );
    assert_eq!(f.replicas("team-a", "web"), 0);
    let gap = f
        .audited()
        .into_iter()
        .find(|line| line["args"]["namespace"] == "team-c")
        .expect("the gap is audited");
    assert_eq!(gap["result"], "left-out");
}

#[tokio::test]
async fn objects_outside_a_pinned_session_are_named_and_the_rest_goes_on() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let setup = Setup {
        pinned: &["team-a"],
        read: &["team-a"],
        ..Setup::default()
    };
    let f = fixture(two_teams(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "replicas": 0, "objects": [
                {"name": "web", "namespace": "team-a"},
                {"name": "db", "namespace": "team-b"},
            ]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(!f.wrote_to("db"));
    assert_eq!(f.replicas("team-a", "web"), 0);
    assert!(
        outcome.text.contains("Deployment team-b/db: ")
            && outcome.text.contains("outside this session"),
        "{}",
        outcome.text
    );
    assert_eq!(batches(&confirmer)[0].objects.len(), 1);
    assert_eq!(f.audit_of("db")["decided_by"], "policy");
}

#[tokio::test]
async fn several_unopened_namespaces_are_asked_about_once_and_a_no_leaves_gaps() {
    let cluster = two_teams();
    add(&cluster, "team-c", "deployments", "cache", 2);
    add(&cluster, "team-d", "deployments", "queue", 2);
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let setup = Setup {
        read: &["team-a"],
        ..Setup::default()
    };
    let f = fixture(cluster.clone(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "tier=web", "replicas": 0,
                   "namespaces": ["team-a", "team-c", "team-d"]}),
        )
        .await;
    // The no was to the first question: the batch window still asks about
    // team-a, and that no ends the call without a write.
    assert!(outcome.is_error, "{}", outcome.text);
    let kinds: Vec<bool> = confirmer
        .requests()
        .iter()
        .map(|request| matches!(request.kind, RequestKind::Namespaces { .. }))
        .collect();
    assert_eq!(kinds, [true, false], "one about namespaces, one batch");
    let RequestKind::Namespaces { names } = &confirmer.requests()[0].kind else {
        panic!("not about namespaces");
    };
    assert_eq!(names, &["team-c", "team-d"]);
    assert!(batches(&confirmer)[0].left_out[0].contains("team-c, team-d"));
    let reads = cluster.lock().unwrap().reads.clone();
    assert!(
        !reads.iter().any(|path| path.contains("team-c")),
        "{reads:?}"
    );
    assert!(f.writes().is_empty());
    assert!(!f.session.grants().is_granted(&Grant::read("team-c")));
}

#[tokio::test]
async fn every_namespace_is_one_question_then_one_window() {
    let cluster = two_teams();
    add(&cluster, "team-c", "deployments", "cache", 2);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let setup = Setup {
        read: &[],
        ..Setup::default()
    };
    let f = fixture(cluster, confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "rollout_restart_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "allNamespaces": true}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let requests = confirmer.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(matches!(requests[0].kind, RequestKind::Namespaces { .. }));
    assert_eq!(batches(&confirmer)[0].objects.len(), 4);
    assert_eq!(f.writes().len(), 4);
    assert!(
        outcome.text.starts_with("rollout_restart_batch: complete."),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn one_unopened_namespace_opens_as_in_a_single_call() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let setup = Setup {
        read: &[],
        ..Setup::default()
    };
    let f = fixture(two_teams(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "namespace": "team-b", "replicas": 0,
                   "objects": [{"name": "db"}]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.requests().len(), 1, "only the batch");
    assert_eq!(f.replicas("team-b", "db"), 0);
}

#[tokio::test]
async fn a_rejection_touches_nothing_and_every_object_is_audited() {
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let f = fixture(two_teams(), confirmer.clone(), Setup::default()).await;
    let outcome = f
        .call(
            "rollout_restart_batch",
            json!({"kind": "Deployment", "labelSelector": "tier=web", "namespace": "team-a"}),
        )
        .await;
    assert!(outcome.is_error);
    assert!(f.writes().is_empty());
    assert_eq!(f.audit_of("web")["result"], "rejected");
    assert_eq!(f.audit_of("web")["decided_by"], "user");
    assert_eq!(f.audit_of("api")["result"], "rejected");
}

#[tokio::test]
async fn what_will_not_change_is_not_in_the_window() {
    let cluster = two_teams();
    add(&cluster, "team-a", "daemonsets", "agent", 0);
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(cluster, confirmer.clone(), Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"namespace": "team-a", "replicas": 3, "objects": [
                {"kind": "Deployment", "name": "web"},
                {"kind": "Deployment", "name": "api"},
                {"kind": "Deployment", "name": "missing"},
                {"kind": "DaemonSet", "name": "agent"},
            ]}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(
        listed(&batches(&confirmer)[0]),
        ["Deployment team-a/api 2 → 3"]
    );
    assert!(!f.wrote_to("web"), "already at 3");
    assert!(
        outcome
            .text
            .contains("unchanged\tDeployment team-a/web\t3\t3"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("Deployment team-a/missing: API error 404"),
        "{}",
        outcome.text
    );
    assert!(
        outcome
            .text
            .contains("DaemonSet team-a/agent: DaemonSet does not support"),
        "{}",
        outcome.text
    );
    assert!(batches(&confirmer)[0].left_out.len() == 2);
}

#[tokio::test]
async fn nothing_to_change_asks_nothing() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = Shared::default();
    let f = fixture(cluster, confirmer.clone(), Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 1}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("nothing to change"),
        "{}",
        outcome.text
    );
    assert!(confirmer.requests().is_empty());
}

#[tokio::test]
async fn a_batch_is_never_cut_to_fit() {
    let cluster = Shared::default();
    for ix in 0..=MAX_OBJECTS {
        add(&cluster, "team-a", "deployments", &format!("d-{ix}"), 1);
    }
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(cluster, confirmer.clone(), Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 0}),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("more than"), "{}", outcome.text);
    assert!(confirmer.requests().is_empty());
    assert!(f.writes().is_empty());
}

#[tokio::test]
async fn the_most_restarts_at_once_is_what_the_agent_asked_for() {
    let cluster = Shared::default();
    for ix in 0..6 {
        add(&cluster, "team-a", "deployments", &format!("d-{ix}"), 1);
    }
    cluster.lock().unwrap().delay = Some(Duration::from_millis(40));
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(cluster.clone(), confirmer, Setup::default()).await;
    let outcome = f
        .call(
            "rollout_restart_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "concurrency": 2}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(f.writes().len(), 6);
    assert_eq!(cluster.lock().unwrap().most_at_once, 2);
}

#[tokio::test]
async fn a_read_only_launch_changes_nothing_and_reads_nothing() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = two_teams();
    let setup = Setup {
        profile: Profile::ReadOnly,
        ..Setup::default()
    };
    let f = fixture(cluster.clone(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 0}),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("Profile"), "{}", outcome.text);
    assert!(confirmer.requests().is_empty());
    assert!(cluster.lock().unwrap().reads.is_empty());
}

#[tokio::test]
async fn access_is_asked_for_once_however_many_namespaces() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let policy = SecurityPolicy::with_minimum(crate::access::Access::NONE);
    let setup = Setup {
        policy,
        ..Setup::default()
    };
    let f = fixture(two_teams(), confirmer.clone(), setup).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"kind": "Deployment", "labelSelector": "a=b", "namespaces": across(), "replicas": 0}),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let requests = confirmer.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(matches!(requests[0].kind, RequestKind::Access { .. }));
    assert!(matches!(requests[1].kind, RequestKind::Batch { .. }));
}

fn remembered(action: RememberedAction, namespace: &str) -> Remembered {
    Remembered {
        action,
        namespace: namespace.to_owned(),
    }
}

#[tokio::test]
async fn only_the_single_calls_own_remembered_answer_skips_the_window() {
    let confirmer = Arc::new(ScriptedConfirmer::approving_for_session());
    let setup = Setup {
        impersonated: true,
        ..Setup::default()
    };
    let f = fixture(two_teams(), confirmer.clone(), setup).await;
    let in_a =
        json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 0});

    // A window, whatever the confirmer would tick: nothing is remembered.
    let outcome = f.call("scale_batch", in_a.clone()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.requests().len(), 1);
    assert!(!confirmer.requests()[0].allow_remember);
    assert!(f.session.remembered().is_empty());

    // "Don't ask again" about this change here: the batch runs like the
    // single call would, and says so.
    let epoch = f.session.remember_epoch();
    let scale = remembered(RememberedAction::Scale, "team-a");
    assert!(f.session.remember(scale, epoch));
    let again =
        json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 1});
    let outcome = f.call("scale_batch", again).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.requests().len(), 1, "no new window");
    assert!(
        outcome
            .text
            .starts_with("Approved by the user's earlier choice"),
        "{}",
        outcome.text
    );
    assert_eq!(f.audit_of("web")["decided_by"], "remembered");

    // Not the same change.
    let restart = json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a"});
    f.call("rollout_restart_batch", restart).await;
    assert_eq!(
        confirmer.requests().len(),
        2,
        "restarts were not remembered"
    );

    // Not one namespace: another is in the batch.
    let across = json!({"kind": "Deployment", "labelSelector": "a=b", "namespaces": across(), "replicas": 2});
    f.call("scale_batch", across).await;
    assert_eq!(confirmer.requests().len(), 3, "team-b was not remembered");
}

#[tokio::test]
async fn a_client_only_namespace_is_never_remembered_and_the_window_says_so() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(two_teams(), confirmer.clone(), Setup::default()).await;
    let epoch = f.session.remember_epoch();
    let scale = remembered(RememberedAction::Scale, "team-a");
    assert!(f.session.remember(scale, epoch));
    let call =
        json!({"kind": "Deployment", "labelSelector": "a=b", "namespace": "team-a", "replicas": 0});
    let outcome = f.call("scale_batch", call).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.requests().len(), 1);
    assert!(!confirmer.requests()[0].mode.server_enforced());
}

#[tokio::test]
async fn wrong_arguments_never_reach_the_cluster() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let cluster = two_teams();
    let f = fixture(cluster.clone(), confirmer.clone(), Setup::default()).await;
    let too_many: Vec<Value> = (0..=MAX_OBJECTS)
        .map(|ix| json!({"name": format!("d-{ix}")}))
        .collect();
    let scale = [
        json!({"kind": "Deployment", "namespace": "team-a", "replicas": 1}),
        json!({"kind": "Deployment", "namespace": "team-a", "replicas": 1, "labelSelector": "a=b", "objects": [{"name": "web"}]}),
        json!({"kind": "Deployment", "namespace": "team-a", "replicas": 1, "labelSelector": " "}),
        json!({"namespace": "team-a", "replicas": 1, "labelSelector": "a=b"}),
        json!({"kind": "Deployment", "namespace": "team-a", "labelSelector": "a=b"}),
        json!({"kind": "Deployment", "namespace": "team-a", "objects": [{"name": "web"}]}),
        json!({"kind": "Deployment", "namespace": "team-a", "replicas": 1, "objects": []}),
        json!({"kind": "Deployment", "namespace": "team-a", "replicas": 1, "objects": too_many}),
        json!({"kind": "Deployment", "replicas": 1, "labelSelector": "a=b", "namespace": "team-a", "namespaces": ["team-b"]}),
        json!({"kind": "Deployment", "replicas": 1, "labelSelector": "a=b", "namespaces": []}),
        json!({"kind": "Deployment", "replicas": 1, "labelSelector": "a=b", "namespace": "team-a", "allNamespaces": true}),
        json!({"kind": "Deployment", "replicas": 1, "namespaces": ["team-a"], "objects": [{"name": "web"}]}),
        json!({"kind": "Deployment", "replicas": 1, "namespace": "team-a", "concurrency": 2, "objects": [{"name": "web"}]}),
        json!({"kind": "Deployment", "replicas": 1, "namespace": "team-a", "nope": 1, "objects": [{"name": "web"}]}),
    ];
    for args in scale {
        let outcome = f.call("scale_batch", args.clone()).await;
        assert!(outcome.is_error, "{args}");
        assert!(
            outcome.text.contains("InvalidInput"),
            "{args}: {}",
            outcome.text
        );
    }
    let restart = [
        json!({"kind": "Deployment", "namespace": "team-a", "objects": [{"name": "web"}], "replicas": 1}),
        json!({"kind": "Deployment", "namespace": "team-a", "objects": [{"name": "web", "replicas": 1}]}),
        json!({"kind": "Deployment", "namespace": "team-a", "objects": [{"name": "web"}], "concurrency": 0}),
        json!({"kind": "Deployment", "namespace": "team-a", "objects": [{"name": "web"}], "concurrency": MAX_CONCURRENCY + 1}),
    ];
    for args in restart {
        let outcome = f.call("rollout_restart_batch", args.clone()).await;
        assert!(outcome.is_error, "{args}");
        assert!(
            outcome.text.contains("InvalidInput"),
            "{args}: {}",
            outcome.text
        );
    }
    assert!(confirmer.requests().is_empty());
    assert!(cluster.lock().unwrap().reads.is_empty());
}

#[tokio::test]
async fn an_object_listed_twice_is_refused_before_the_window() {
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let f = fixture(two_teams(), confirmer.clone(), Setup::default()).await;
    let outcome = f
        .call(
            "scale_batch",
            json!({"namespace": "team-a", "objects": [
                {"kind": "Deployment", "name": "web", "replicas": 1},
                {"kind": "deployments", "name": "web", "replicas": 5},
            ]}),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("listed twice"), "{}", outcome.text);
    assert!(confirmer.requests().is_empty());
}

#[test]
fn the_tools_are_changes_and_need_the_deploy_level() {
    for name in ["scale_batch", "rollout_restart_batch"] {
        assert!(super::confirms_each_call(name), "{name}");
        let spec = super::catalog().into_iter().find(|spec| spec.name == name);
        assert!(spec.is_some_and(|spec| !spec.read_only), "{name}");
    }
}
