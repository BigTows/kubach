//! End-to-end tests of the MCP handler against a fake API server that
//! records every request: denied calls must not reach it at all.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use kube::{Client, Config};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;
use crate::access::Access;
use crate::confirm::{DenyAll, RequestKind, ScriptedConfirmer};
use crate::grants::{Grant, GrantState, Grants};
use crate::identity::Identity;
use crate::rules::Capability;
use crate::secret_write::{SecretInput, SecretOp, SecretRequest};
use crate::session::Session;
use crate::settings::SecurityPolicy;
use crate::testutil::{NS, OTHER_NS, catalog, grants, permissive, scope_with};

/// Records `METHOD path` (and the body) of every request. Serves one
/// Deployment `web` in the session namespace and echoes it for PATCH, the
/// Secret `db`, a pod using it, and accepts Secret writes; 404 otherwise.
struct FakeApi {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
}

/// Pod `web-0`, which reads `db`'s `password`.
fn pod_list() -> Value {
    json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": [{
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-0", "namespace": NS},
        "spec": {"containers": [{"name": "app", "image": "nginx", "env": [
            {"name": "DB_PASSWORD", "valueFrom": {"secretKeyRef": {"name": "db", "key": "password"}}}
        ]}]},
    }]})
}

/// Pod `web-0` of Deployment `web` (through ReplicaSet `web-5d`), using
/// ConfigMap `app-config`.
fn owned_pod() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-0", "namespace": NS, "ownerReferences": [
            {"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "web-5d", "uid": "3", "controller": true}
        ]},
        "spec": {"containers": [{"name": "app", "image": "nginx",
                                 "envFrom": [{"configMapRef": {"name": "app-config"}}]}]},
    })
}

/// Pod `name` with label `app`, owned by StatefulSet `db` (uid 7) when
/// `owned`; an init container `init` and the container `db`.
fn labeled_pod(name: &str, app: &str, owned: bool) -> Value {
    let owners = if owned {
        json!([{"apiVersion": "apps/v1", "kind": "StatefulSet", "name": "db", "uid": "7", "controller": true}])
    } else {
        json!([])
    };
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": NS, "labels": {"app": app},
                     "ownerReferences": owners},
        "spec": {"initContainers": [{"name": "init", "image": "busybox"}],
                 "containers": [{"name": "db", "image": "postgres"}]},
    })
}

/// Pods selected by `app=db` (`db-0`, `db-1` of StatefulSet `db` and the
/// stray `db-other`) or `app=many` (12 pods), for the log tests.
fn selected_pods(path: &str) -> Option<Value> {
    let items: Vec<Value> = if path.contains("app%3Ddb") || path.contains("app=db") {
        vec![
            labeled_pod("db-other", "db", false),
            labeled_pod("db-1", "db", true),
            labeled_pod("db-0", "db", true),
        ]
    } else if path.contains("app%3Dmany") || path.contains("app=many") {
        (0..12)
            .map(|i| labeled_pod(&format!("many-{i:02}"), "many", false))
            .collect()
    } else {
        return None;
    };
    Some(json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": items}))
}

/// Logs of the pods of [`selected_pods`]: each line names its stream, one
/// carries a password for masking.
fn selected_pod_logs(path: &str) -> String {
    let pod = path.split('/').nth_back(1).unwrap_or_default();
    let container = path
        .split(['?', '&'])
        .find_map(|part| part.strip_prefix("container="))
        .unwrap_or("default");
    format!("hello from {pod}/{container}\npassword=hunter2\nERROR {pod} failed\n")
}

/// Logs of pod `levels-0`: every format `minLevel` knows, a line masking
/// breaks the JSON of, and lines without a level.
const LEVELED_LOGS: &str = r#"level=info msg=started
level=error msg="disk full" password=hunter2
{"level":"warn","token_count":5,"msg":"slow"}
{"level":50,"msg":"boom"}
{"level":30,"msg":"level=error in a message"}
E1103 12:00:00.000000       1 main.go:1] failed
I1103 12:00:00.000000       1 main.go:2] ok
    at main.go:3
plain error text
"#;

/// Pods for `debug_container` (#64): `distroless-0` takes any debug
/// container, `restricted-0` only one with a restricted security context,
/// `locked-0` none (Pod Security refuses), `pending-0` is not running. Once
/// added, the container `dbg` runs.
fn debug_pod(method: &str, path: &str, body: &[u8]) -> Option<(&'static str, String)> {
    let rest = path.strip_prefix(&format!("/api/v1/namespaces/{NS}/pods/"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    let (name, sub) = match rest.split_once('/') {
        Some((name, sub)) => (name, Some(sub)),
        None => (rest, None),
    };
    if !["distroless-0", "restricted-0", "locked-0", "pending-0"].contains(&name) {
        return None;
    }
    let phase = if name == "pending-0" {
        "Pending"
    } else {
        "Running"
    };
    let pod = json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": NS},
        "spec": {"containers": [{"name": "app", "image": "gcr.io/distroless/static-debian12",
                                 "securityContext": {"runAsUser": 472}}]},
        "status": {"phase": phase, "ephemeralContainerStatuses": [
            {"name": "dbg", "image": "busybox", "imageID": "", "ready": false, "restartCount": 0,
             "state": {"running": {}}}
        ]},
    });
    let forbidden = || {
        let message = format!(
            "pods \"{name}\" is forbidden: violates PodSecurity \"restricted:latest\": \
             runAsNonRoot != true"
        );
        Some((
            "403 Forbidden",
            json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                   "message": message, "reason": "Forbidden", "code": 403})
            .to_string(),
        ))
    };
    match (method, sub) {
        ("GET", None) => Some(("200 OK", pod.to_string())),
        ("PATCH", Some("ephemeralcontainers")) => {
            let body = String::from_utf8_lossy(body);
            match name {
                "locked-0" => forbidden(),
                "restricted-0" if !body.contains("runAsNonRoot") => forbidden(),
                _ => Some(("200 OK", pod.to_string())),
            }
        }
        _ => None,
    }
}

fn replica_set() -> Value {
    json!({
        "apiVersion": "apps/v1", "kind": "ReplicaSet",
        "metadata": {"name": "web-5d", "namespace": NS, "ownerReferences": [
            {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web", "uid": "1", "controller": true}
        ]},
        "spec": {"selector": {"matchLabels": {"app": "web"}}},
    })
}

fn deployment() -> Value {
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": NS, "uid": "1", "resourceVersion": "1",
                     "annotations": {"kubectl.kubernetes.io/last-applied-configuration": "{\"secret\":1}"}},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "web"}},
                 "template": {"metadata": {"labels": {"app": "web"}},
                              "spec": {"containers": [{"name": "web", "image": "nginx"}]}}},
        "status": {"replicas": 1, "readyReplicas": 1},
    })
}

/// Secret `db` with `password: hunter2` and `copy: hunter2`.
fn secret() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {"name": "db", "namespace": NS, "uid": "2", "resourceVersion": "1"},
        "data": {"password": "aHVudGVyMg==", "copy": "aHVudGVyMg==", "url": "cG9zdGdyZXM6Ly9hcHA6aHVudGVyMkBkYjo1NDMyL3Nob3A="},
    })
}

/// Helm's storage Secret of revision 2 of release `web` (#22): values with
/// a password, a token under an innocent name and a list of keys.
fn helm_storage() -> Value {
    use base64::Engine as _;
    let release = json!({
        "name": "web", "namespace": NS, "version": 2,
        "info": {"status": "deployed", "description": "Upgrade complete"},
        "chart": {"metadata": {"name": "podinfo", "version": "6.15.0"}},
        "config": {
            "replicaCount": 2,
            "auth": {"password": "hunter2-in-values", "user": "app"},
            "extra": {"webhook": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJrdWJhY2gifQ.c2lnbmF0dXJlLXZhbHVl"},
            "apiKeys": ["first-key-in-values"],
        },
        "manifest": "---\nkind: Deployment\n",
    });
    let payload = kubach_core::helm::encode_release(&release);
    json!({
        "apiVersion": "v1", "kind": "Secret", "type": "helm.sh/release.v1",
        "metadata": {"name": "sh.helm.release.v1.web.v2", "namespace": NS,
                     "labels": {"owner": "helm", "name": "web", "version": "2"}},
        "data": {"release": base64::engine::general_purpose::STANDARD.encode(payload)},
    })
}

/// What release `infra` has: an object no temporary ServiceAccount may
/// touch, next to a namespaced one.
const INFRA_MANIFEST: &str = "---\napiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\n\
     metadata:\n  name: infra-reader\nrules: []\n---\napiVersion: v1\nkind: ConfigMap\n\
     metadata:\n  name: infra\n";

fn infra_release(revision: u32) -> Value {
    json!({
        "name": "infra", "namespace": NS, "version": revision,
        "info": {"status": "deployed", "description": "Upgrade complete"},
        "chart": {"metadata": {"name": "infra", "version": "1.0.0"}},
        "config": {},
        "manifest": INFRA_MANIFEST,
    })
}

/// Revision `revision` of release `infra` in its storage Secret.
fn infra_storage(revision: u32) -> Value {
    use base64::Engine as _;
    let payload = kubach_core::helm::encode_release(&infra_release(revision));
    json!({
        "apiVersion": "v1", "kind": "Secret", "type": "helm.sh/release.v1",
        "metadata": {"name": format!("sh.helm.release.v1.infra.v{revision}"), "namespace": NS,
                     "labels": {"owner": "helm", "name": "infra",
                                "version": revision.to_string()}},
        "data": {"release": base64::engine::general_purpose::STANDARD.encode(payload)},
    })
}

/// The labels of `infra`'s storage: revisions 1 and 2.
fn infra_metadata() -> Value {
    let item = |revision: u32| {
        json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadata",
               "metadata": {"name": format!("sh.helm.release.v1.infra.v{revision}"),
                            "namespace": NS,
                            "labels": {"owner": "helm", "name": "infra",
                                       "version": revision.to_string(),
                                       "status": "deployed"}}})
    };
    json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadataList",
           "metadata": {}, "items": [item(1), item(2)]})
}

/// Lists the topology tools read (#327): Deployment `web` with its
/// ReplicaSet, Service `web` (80 → 8080) selecting it, ConfigMap
/// `app-config`, Ingress `web` and a default-deny NetworkPolicy.
fn topology_list(path: &str) -> Option<Value> {
    let path = path.split('?').next().unwrap_or(path);
    let list = |kind: &str, items: Vec<Value>| {
        Some(
            json!({"apiVersion": "v1", "kind": format!("{kind}List"), "metadata": {}, "items": items}),
        )
    };
    let mut web = deployment();
    web["spec"]["template"]["spec"]["containers"][0]["ports"] =
        json!([{"name": "http", "containerPort": 8080}]);
    web["spec"]["template"]["spec"]["containers"][0]["envFrom"] =
        json!([{"configMapRef": {"name": "app-config"}}]);
    match path
        .strip_prefix(&format!("/namespaces/{NS}/"))
        .or_else(|| {
            path.strip_prefix("/api/v1")
                .or_else(|| path.strip_prefix("/apis/apps/v1"))
                .or_else(|| path.strip_prefix("/apis/networking.k8s.io/v1"))
                .and_then(|rest| rest.strip_prefix(&format!("/namespaces/{NS}/")))
        })? {
        "deployments" => list("Deployment", vec![web]),
        "replicasets" => list("ReplicaSet", vec![replica_set()]),
        "services" => list(
            "Service",
            vec![json!({"apiVersion": "v1", "kind": "Service",
                "metadata": {"name": "web", "namespace": NS},
                "spec": {"selector": {"app": "web"}, "clusterIP": "10.0.0.1",
                         "ports": [{"name": "http", "port": 80, "targetPort": 8080}]}})],
        ),
        "configmaps" => list(
            "ConfigMap",
            vec![json!({"apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "app-config", "namespace": NS},
                "data": {"password": "hunter2"}})],
        ),
        "ingresses" => list(
            "Ingress",
            vec![
                json!({"apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
                "metadata": {"name": "web", "namespace": NS},
                "spec": {"rules": [{"host": "web.example.com", "http": {"paths": [
                    {"path": "/", "pathType": "Prefix",
                     "backend": {"service": {"name": "web", "port": {"number": 80}}}}]}}]}}),
            ],
        ),
        "networkpolicies" => list(
            "NetworkPolicy",
            vec![
                json!({"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "default-deny", "namespace": NS},
                "spec": {"podSelector": {}, "policyTypes": ["Ingress"]}}),
            ],
        ),
        _ => None,
    }
}

impl FakeApi {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let body_log = Arc::clone(&bodies);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = Arc::clone(&log);
                let body_log = Arc::clone(&body_log);
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
                        body_log.lock().unwrap().push(format!(
                            "{method} {path}\n{}",
                            String::from_utf8_lossy(&body)
                        ));
                        let target = format!("/apis/apps/v1/namespaces/{NS}/deployments/web");
                        let secrets = format!("/api/v1/namespaces/{NS}/secrets");
                        let pods = format!("/api/v1/namespaces/{NS}/pods");
                        let storage = format!("{secrets}/sh.helm.release.v1.web.v");
                        let infra = format!("{secrets}/sh.helm.release.v1.infra.v");
                        let (status, body) = if method == "GET" && path == format!("{storage}2") {
                            ("200 OK", helm_storage().to_string())
                        } else if method == "GET" && path.starts_with(&infra) {
                            let revision = path[infra.len()..].parse().unwrap_or(0);
                            ("200 OK", infra_storage(revision).to_string())
                        } else if method == "GET"
                            && path.starts_with(&format!("{secrets}?"))
                            && path.contains("name%3Dinfra")
                        {
                            ("200 OK", infra_metadata().to_string())
                        } else if method == "GET" && path == format!("{storage}9") {
                            (
                                "403 Forbidden",
                                json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                                       "message": "secrets is forbidden", "reason": "Forbidden",
                                       "code": 403})
                                .to_string(),
                            )
                        } else if (method == "GET" || method == "PATCH" || method == "DELETE")
                            && path.starts_with(&format!("{secrets}/db"))
                            && !path.starts_with(&format!("{secrets}/db-"))
                        {
                            ("200 OK", secret().to_string())
                        } else if method == "POST" && path.starts_with(&secrets) {
                            ("201 Created", String::from_utf8_lossy(&body).into_owned())
                        } else if let Some(list) = (method == "GET"
                            && path.starts_with(&format!("{pods}?")))
                        .then(|| selected_pods(&path))
                        .flatten()
                        {
                            ("200 OK", list.to_string())
                        } else if method == "GET"
                            && (path == pods || path.starts_with(&format!("{pods}?")))
                        {
                            ("200 OK", pod_list().to_string())
                        } else if method == "GET"
                            && (path.starts_with(&format!("{pods}/db-"))
                                || path.starts_with(&format!("{pods}/many-")))
                        {
                            let rest = &path[pods.len() + 1..];
                            if rest.contains("/log") {
                                ("200 OK", selected_pod_logs(&path))
                            } else {
                                let owned = rest == "db-0" || rest == "db-1";
                                let app = if owned { "db" } else { "many" };
                                ("200 OK", labeled_pod(rest, app, owned).to_string())
                            }
                        } else if method == "GET"
                            && path == format!("/apis/apps/v1/namespaces/{NS}/statefulsets/db")
                        {
                            let db = json!({"apiVersion": "apps/v1", "kind": "StatefulSet",
                                "metadata": {"name": "db", "namespace": NS, "uid": "7"},
                                "spec": {"selector": {"matchLabels": {"app": "db"}}}});
                            ("200 OK", db.to_string())
                        } else if method == "GET"
                            && path.starts_with(&format!("{pods}/levels-0/log"))
                        {
                            ("200 OK", LEVELED_LOGS.to_owned())
                        } else if method == "GET" && path.starts_with(&format!("{pods}/web-0/log"))
                        {
                            ("200 OK", "starting\nready\n".to_owned())
                        } else if let Some(reply) = debug_pod(&method, &path, &body) {
                            reply
                        } else if method == "GET" && path == format!("{pods}/web-0") {
                            ("200 OK", owned_pod().to_string())
                        } else if method == "GET"
                            && path == format!("/apis/apps/v1/namespaces/{NS}/replicasets/web-5d")
                        {
                            ("200 OK", replica_set().to_string())
                        } else if method == "GET"
                            && (path == secrets || path.starts_with(&format!("{secrets}?")))
                        {
                            let list = json!({"apiVersion": "v1", "kind": "SecretList",
                                              "metadata": {}, "items": [secret()]});
                            ("200 OK", list.to_string())
                        } else if path.starts_with(&format!("{target}/scale")) {
                            let scale = json!({"apiVersion": "autoscaling/v1", "kind": "Scale",
                                               "metadata": {"name": "web", "namespace": NS},
                                               "spec": {"replicas": 3}});
                            ("200 OK", scale.to_string())
                        } else if path.starts_with(&target)
                            && (method == "GET" || method == "PATCH")
                        {
                            ("200 OK", deployment().to_string())
                        } else if path
                            .starts_with(&format!("/apis/example.com/v1/namespaces/{NS}/widgets/w"))
                            && (method == "GET" || method == "DELETE")
                        {
                            let widget = json!({"apiVersion": "example.com/v1", "kind": "Widget",
                                                "metadata": {"name": "w", "namespace": NS}});
                            ("200 OK", widget.to_string())
                        } else if method == "GET" && path == "/apis/example.com/v1" {
                            // Discovery for the generic plugin's delete.
                            let resources = json!({"kind": "APIResourceList",
                                "groupVersion": "example.com/v1",
                                "resources": [{"name": "widgets", "singularName": "widget",
                                    "namespaced": true, "kind": "Widget",
                                    "verbs": ["get", "delete"]}]});
                            ("200 OK", resources.to_string())
                        } else if let Some(list) =
                            (method == "GET").then(|| topology_list(&path)).flatten()
                        {
                            ("200 OK", list.to_string())
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
        Self {
            url,
            requests,
            bodies,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// `METHOD path` and body of every request that changed something.
    fn writes(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.starts_with("GET "))
            .cloned()
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

/// A client-only session where reading the namespace is already granted,
/// for the checks below the rights on request.
async fn fixture(profile: Profile, policy: SecurityPolicy, approve: bool) -> Fixture {
    let f = session_fixture(profile, policy, approve, false).await;
    f.server.session().set_grants(grants(&[Grant::read(NS)]));
    f
}

/// A session without any right yet (#227). `impersonated`: the API server
/// enforces the boundary too, so `modify(ns)` is remembered.
async fn session_fixture(
    profile: Profile,
    policy: SecurityPolicy,
    approve: bool,
    impersonated: bool,
) -> Fixture {
    let confirmer = if approve {
        ScriptedConfirmer::approving()
    } else {
        ScriptedConfirmer::denying()
    };
    fixture_with(profile, policy, confirmer, impersonated).await
}

async fn fixture_with(
    profile: Profile,
    policy: SecurityPolicy,
    confirmer: ScriptedConfirmer,
    impersonated: bool,
) -> Fixture {
    let api = FakeApi::start().await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let scope = scope_with(profile, &policy, &[NS]);
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    let identity = if impersonated {
        Identity::Impersonation {
            client,
            user: "kubach:mcp".into(),
            groups: Vec::new(),
        }
    } else {
        Identity::ClientOnly { client }
    };
    let session = Session::from_parts(scope, identity, catalog(), capabilities);
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
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Escape attempts through every tool and argument. None may produce an
/// API request.
fn escape_attempts() -> Vec<(&'static str, Value)> {
    let mut attempts = Vec::new();
    for ns in [OTHER_NS, "kube-system", "", "*", "all"] {
        attempts.extend([
            ("list_resources", json!({"kind": "Pod", "namespace": ns})),
            ("get_resource", json!({"kind": "Pod", "name": "web-0", "namespace": ns})),
            ("describe_resource", json!({"kind": "Deployment", "name": "web", "namespace": ns})),
            ("list_events", json!({"namespace": ns})),
            ("get_logs", json!({"pod": "web-0", "namespace": ns})),
            ("get_logs", json!({"kind": "StatefulSet", "name": "db", "namespace": ns})),
            ("get_logs", json!({"labelSelector": "app=db", "namespace": ns})),
            ("top_pods", json!({"namespace": ns})),
            ("exec", json!({"pod": "web-0", "namespace": ns, "command": ["id"]})),
            ("debug_container", json!({"pod": "distroless-0", "namespace": ns, "targetContainer": "app"})),
            ("port_forward_start", json!({"target": "pod/web-0", "port": 8080, "namespace": ns})),
            ("scale", json!({"kind": "Deployment", "name": "web", "namespace": ns, "replicas": 5})),
            ("rollout_restart", json!({"kind": "Deployment", "name": "web", "namespace": ns})),
            ("delete_resource", json!({"kind": "Deployment", "name": "web", "namespace": ns})),
            ("apply_manifest", json!({"yaml": "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: c}\n", "namespace": ns})),
            ("request_secret_input", json!({"name": "db", "mode": "create", "namespace": ns})),
            ("request_secret_input", json!({"name": "db", "mode": "update", "namespace": ns})),
            ("copy_secret", json!({"name": "db", "namespace": ns, "toName": "db2"})),
            ("copy_secret", json!({"name": "db", "toNamespace": ns})),
            ("delete_secret", json!({"name": "db", "namespace": ns})),
            ("get_topology", json!({"namespace": ns})),
            ("get_topology", json!({"namespaces": [NS, ns]})),
            ("get_topology", json!({"resource": {"kind": "Deployment", "name": "web", "namespace": ns}})),
            ("can_reach", json!({"from": {"kind": "Deployment", "name": "web", "namespace": ns},
                                 "to": {"kind": "Deployment", "name": "web"}, "port": 80})),
            ("can_reach", json!({"from": {"kind": "Deployment", "name": "web"},
                                 "to": {"kind": "Service", "name": "web", "namespace": ns}, "port": 80})),
        ]);
    }
    attempts.extend([
        ("list_resources", json!({"kind": "Pod", "allNamespaces": true})),
        ("list_resources", json!({"kind": "Pod", "fieldSelector": "metadata.namespace=kube-system"})),
        ("list_resources", json!({"kind": "Namespace"})),
        ("list_resources", json!({"kind": "nodes"})),
        ("list_resources", json!({"kind": "ClusterRoleBinding"})),
        ("list_resources", json!({"kind": "Secret"})),
        ("list_resources", json!({"kind": "pods/log"})),
        ("list_resources", json!({"kind": "Pod", "apiVersion": "v1/namespaces/kube-system"})),
        ("get_resource", json!({"kind": "Secret", "name": "db"})),
        ("get_resource", json!({"kind": "Pod", "name": "../../kube-system/pods/x"})),
        ("get_resource", json!({"kind": "Pod", "name": "x", "namespace": NS, "extra": 1})),
        ("get_logs", json!({"pod": "web-0", "container": "../../x"})),
        ("get_logs", json!({})),
        ("get_logs", json!({"pod": "web-0", "labelSelector": "app=db"})),
        ("get_logs", json!({"kind": "Secret", "name": "db"})),
        ("get_logs", json!({"kind": "ConfigMap", "name": "app-config"})),
        ("get_logs", json!({"kind": "StatefulSet"})),
        ("get_logs", json!({"kind": "StatefulSet", "name": "../db"})),
        ("get_logs", json!({"labelSelector": "app=db\n"})),
        ("get_logs", json!({"pod": "web-0", "allContainers": true, "container": "app"})),
        ("get_logs", json!({"pod": "web-0", "filter": "(", "regex": true})),
        ("get_logs", json!({"pod": "web-0", "filter": ""})),
        ("get_logs", json!({"pod": "web-0", "minLevel": "loud"})),
        ("get_logs", json!({"pod": "web-0", "minLevel": ""})),
        ("list_events", json!({"kind": "Secret", "name": "db"})),
        ("scale", json!({"kind": "Deployment", "name": "web/../../x", "replicas": 1})),
        ("delete_resource", json!({"kind": "RoleBinding", "name": "x"})),
        ("delete_resource", json!({"kind": "ServiceAccount", "name": "x"})),
        ("delete_resource", json!({"kind": "Namespace", "name": "kubach-test-mcp-other"})),
        ("apply_manifest", json!({"yaml": format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: c\n  namespace: {OTHER_NS}\n")})),
        ("apply_manifest", json!({"yaml": "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: evil\n"})),
        ("apply_manifest", json!({"yaml": "apiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: x\nroleRef: {apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: cluster-admin}\n"})),
        ("apply_manifest", json!({"yaml": "apiVersion: v1\nkind: Secret\nmetadata:\n  name: s\n"})),
        ("apply_manifest", json!({"yaml": format!("apiVersion: v1\nkind: List\nitems:\n- apiVersion: v1\n  kind: ConfigMap\n  metadata: {{name: c, namespace: {OTHER_NS}}}\n")})),
        ("apply_manifest", json!({"yaml": format!("apiVersion: apps/v1\nkind: Deployment\nmetadata: {{name: web}}\nspec:\n  template:\n    metadata: {{namespace: {OTHER_NS}}}\n")})),
        ("request_secret_input", json!({"name": "db", "mode": "create", "value": "hunter2"})),
        ("request_secret_input", json!({"name": "db", "mode": "copy"})),
        ("request_secret_input", json!({"name": "db", "mode": "create", "type": "kubernetes.io/service-account-token"})),
        ("request_secret_input", json!({"name": "../x", "mode": "create"})),
        ("copy_secret", json!({"name": "db"})),
        ("delete_secret", json!({"name": "db/../x"})),
        ("get_topology", json!({"resource": {"kind": "Deployment", "name": "../x"}})),
        ("get_topology", json!({"depth": 0})),
        ("get_topology", json!({"allNamespaces": true})),
        ("can_reach", json!({"from": {"address": "10.0.0.1"}, "to": {"address": "10.0.0.2"}, "port": 80})),
        ("can_reach", json!({"from": {"kind": "Deployment", "name": "web"}, "to": {"kind": "Deployment", "name": "web"}, "port": 70000})),
        ("no_such_tool", json!({})),
    ]);
    attempts
}

#[tokio::test]
async fn escape_attempts_never_reach_the_api() {
    let f = fixture(Profile::Deploy, permissive(), true).await;
    for (tool, arguments) in escape_attempts() {
        let outcome = f.server.call("agent", tool, args(arguments.clone())).await;
        assert!(
            outcome.is_error,
            "{tool} {arguments} was allowed: {}",
            outcome.text
        );
        assert!(
            outcome.text.starts_with("denied by policy"),
            "{tool}: {}",
            outcome.text
        );
    }
    assert_eq!(f.api.requests(), Vec::<String>::new());
    assert!(
        f.confirmer.requests().is_empty(),
        "nothing should reach the user either"
    );
    let lines = audit_lines(&f.audit_path);
    assert_eq!(lines.len(), escape_attempts().len());
    assert!(
        lines
            .iter()
            .all(|l| l["decided_by"] == "policy" && l["allowed"] == false)
    );
}

#[tokio::test]
async fn reads_stay_in_the_session_namespace() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_resource",
            args(json!({"kind": "deployments", "name": "web"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("name: web"));
    assert!(!outcome.text.contains("last-applied-configuration"));
    assert_eq!(
        f.api.requests(),
        vec![format!("GET /apis/apps/v1/namespaces/{NS}/deployments/web")]
    );
}

#[tokio::test]
async fn rejected_confirmation_prevents_the_change() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "scale",
            args(json!({"kind": "Deployment", "name": "web", "replicas": 3})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("rejected"), "{}", outcome.text);
    // Only the read of the object; no PATCH.
    assert!(f.api.requests().iter().all(|r| r.starts_with("GET ")));
    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].namespace, NS);
    assert!(
        requests[0].summary.contains("3 replicas"),
        "{}",
        requests[0].summary
    );
    // Client-only sessions never offer "remember".
    assert!(!requests[0].allow_remember);
    let audit = audit_lines(&f.audit_path);
    assert_eq!(audit[0]["decided_by"], "user");
    assert_eq!(audit[0]["result"], "rejected");
    assert_eq!(audit[0]["identity"], "user");
    assert_eq!(audit[0]["client"], "agent");
}

#[tokio::test]
async fn approved_change_runs_in_the_session_namespace() {
    let f = fixture(Profile::Deploy, permissive(), true).await;
    let outcome = f
        .server
        .call(
            "agent",
            "scale",
            args(json!({"kind": "Deployment", "name": "web", "replicas": 3})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let requests = f.api.requests();
    assert!(
        requests.iter().any(|r| r.starts_with("PATCH ")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|r| r.contains(&format!("/namespaces/{NS}/"))),
        "{requests:?}"
    );
    assert_eq!(audit_lines(&f.audit_path)[0]["decided_by"], "user");
}

#[tokio::test]
async fn kinds_without_a_plugin_can_be_deleted() {
    let f = fixture(Profile::Deploy, permissive(), true).await;
    let outcome = f
        .server
        .call(
            "agent",
            "delete_resource",
            args(json!({"kind": "Widget", "apiVersion": "example.com/v1", "name": "w"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(f.api.requests().last().unwrap().starts_with(&format!(
        "DELETE /apis/example.com/v1/namespaces/{NS}/widgets/w"
    )));
    assert_eq!(
        f.confirmer.requests()[0].summary,
        format!("Delete Widget {NS}/w")
    );
}

#[tokio::test]
async fn default_confirmer_denies() {
    let api = FakeApi::start().await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::Deploy, &permissive(), &[NS]);
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        Capability::ALL.into_iter().collect(),
    );
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let outcome = server
        .call(
            "agent",
            "rollout_restart",
            args(json!({"kind": "Deployment", "name": "web"})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("rejected"));
    assert!(api.requests().iter().all(|r| r.starts_with("GET ")));
}

#[tokio::test]
async fn closed_sessions_deny_everything() {
    let f = fixture(Profile::Deploy, permissive(), true).await;
    f.server.session().close().await.unwrap();
    let outcome = f.server.call("agent", "session_info", Map::new()).await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("closed"));
    assert!(f.api.requests().is_empty());
}

#[tokio::test]
async fn audit_masks_arguments() {
    let f = fixture(Profile::Debug, permissive(), false).await;
    f.server
        .call(
            "agent",
            "exec",
            args(json!({"pod": "web-0", "command": ["sh", "-c", "mysql --password=hunter2"]})),
        )
        .await;
    let text = std::fs::read_to_string(&f.audit_path).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("password=***"));
}

/// An applied manifest is parsed and masked before it is written to the
/// audit log (#193): Secret values, env values and ConfigMap values under a
/// credential-like name never get there, plain or base64.
#[tokio::test]
async fn audit_masks_applied_manifests() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let tls_key = "-----BEGIN PRIVATE KEY-----MIIEvQIBADAN";
    let yaml = format!(
        "\
apiVersion: v1
kind: Secret
metadata:
  name: tls
data:
  tls.key: {}
stringData:
  token: Plain-Str1ng-Token
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: app
data:
  DB_PASS: Cm-Hunter-2
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  template:
    spec:
      containers:
      - name: app
        env:
        - name: DB_PASSWORD
          value: Env-S3cr3t
",
        b64(tls_key)
    );
    f.server
        .call(
            "agent",
            "apply_manifest",
            args(json!({"yaml": yaml, "namespace": NS})),
        )
        .await;
    let text = std::fs::read_to_string(&f.audit_path).unwrap();
    assert_no_values(
        &text,
        &[
            tls_key,
            "MIIEvQIBADAN",
            "Plain-Str1ng-Token",
            "Cm-Hunter-2",
            "Env-S3cr3t",
        ],
    );
    assert!(
        text.contains("tls.key") && text.contains("DB_PASSWORD"),
        "{text}"
    );
}

fn tool_names(server: &McpServer) -> Vec<String> {
    server.tools().iter().map(|t| t.name.to_string()).collect()
}

#[tokio::test]
async fn tools_beyond_the_ceiling_are_offered_marked() {
    let read_only = fixture(Profile::ReadOnly, permissive(), false).await;
    let tools = read_only.server.tools();
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref().map(str::to_owned))
            .unwrap_or_else(|| panic!("{name} is not offered"))
    };
    assert!(!description("get_resource").contains("Requires your approval in Kubach"));
    // Up to deploy, marked so the agent can ask (#417).
    for marked in [
        "get_logs",
        "exec",
        "apply_manifest",
        "scale",
        "rollout_restart",
        "delete_resource",
    ] {
        assert!(
            description(marked).contains("Requires your approval in Kubach"),
            "{marked}: {}",
            description(marked)
        );
    }
    // No metrics API in the fixture catalog.
    assert!(!tool_names(&read_only.server).contains(&"top_pods".to_string()));

    // Within the ceiling: no mark.
    let deploy = fixture(Profile::Deploy, permissive(), false).await;
    let tools = deploy.server.tools();
    for name in ["apply_manifest", "scale", "exec", "get_logs"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        assert!(
            !tool
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("Requires your approval"),
            "{name}"
        );
    }

    // `kubach mcp --profile debug` hides what it never allows (#15).
    let api = FakeApi::start().await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let capped = Session::from_parts(
        crate::testutil::scope_capped(Profile::Debug, &permissive(), &[NS]),
        Identity::ClientOnly { client },
        catalog(),
        Capability::ALL.into_iter().collect(),
    );
    let server = McpServer::new(capped, Arc::new(DenyAll), None);
    let names = tool_names(&server);
    assert!(names.contains(&"get_logs".to_string()));
    assert!(names.contains(&"exec".to_string()));
    for hidden in [
        "apply_manifest",
        "scale",
        "rollout_restart",
        "delete_resource",
    ] {
        assert!(!names.contains(&hidden.to_string()), "{hidden}");
    }
}

// ------------------------------------------------------------- rights on request (#227)

fn grant_requests(f: &Fixture) -> Vec<Grant> {
    f.confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match request.kind {
            RequestKind::Grant { grant } => Some(grant),
            _ => None,
        })
        .collect()
}

fn call_requests(f: &Fixture) -> usize {
    f.confirmer
        .requests()
        .iter()
        .filter(|request| matches!(request.kind, RequestKind::Call | RequestKind::Exec { .. }))
        .count()
}

#[tokio::test]
async fn no_secret_without_the_users_answer() {
    // The user says no: nothing reaches the API.
    let f = session_fixture(Profile::ReadOnly, permissive(), false, false).await;
    let outcome = f
        .server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(outcome.is_error);
    assert!(!outcome.text.contains("hunter2"));
    assert_eq!(grant_requests(&f), [Grant::secret(NS, "db")]);
    assert!(f.api.requests().is_empty(), "{:?}", f.api.requests());

    // No window answers for the Secret.
    let f = session_fixture(Profile::ReadOnly, permissive(), true, false).await;
    f.server.session().set_grants(grants(&[Grant::read(NS)]));
    let server = McpServer::new(f.server.session().clone(), Arc::new(DenyAll), None);
    let outcome = server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(outcome.is_error);
    assert!(f.api.requests().is_empty());
    // No answer is not a refusal: asked again next time.
    assert_eq!(
        f.server.session().grants().state(&Grant::secret(NS, "db")),
        None
    );
}

#[tokio::test]
async fn revealing_asks_for_that_secret_and_warns() {
    let f = session_fixture(Profile::ReadOnly, permissive(), true, false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_secret_value",
            args(json!({"name": "db", "key": "password"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("hunter2"), "{}", outcome.text);
    // Reading the namespace comes with the access (#15); only the Secret
    // is asked for.
    assert_eq!(grant_requests(&f), [Grant::secret(NS, "db")]);
    let warning = f
        .confirmer
        .requests()
        .into_iter()
        .find(|r| {
            r.kind
                == RequestKind::Grant {
                    grant: Grant::secret(NS, "db"),
                }
        })
        .unwrap();
    assert!(
        warning.details.contains("LLM provider"),
        "{}",
        warning.details
    );
    assert!(warning.details.contains("Rotate"), "{}", warning.details);
    assert_eq!(warning.context, "minikube");

    // Remembered for this Secret only.
    f.server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert_eq!(grant_requests(&f).len(), 1);
    f.server
        .call("agent", "get_secret_value", args(json!({"name": "api"})))
        .await;
    assert_eq!(grant_requests(&f).last(), Some(&Grant::secret(NS, "api")));

    // Values never reach the audit log.
    let audit = std::fs::read_to_string(&f.audit_path).unwrap();
    assert!(
        !audit.contains("hunter2") && !audit.contains("aHVudGVyMg"),
        "{audit}"
    );
    let lines = audit_lines(&f.audit_path);
    let grants: Vec<&Value> = lines
        .iter()
        .filter(|l| l["tool"].as_str().unwrap_or("").starts_with("grant:"))
        .collect();
    assert_eq!(
        grants[1]["args"],
        json!({"right": "secret", "namespace": NS, "name": "db"})
    );
    assert_eq!(grants[1]["result"], "granted");
    assert_eq!(grants[1]["decided_by"], "user");
    let reveal = lines
        .iter()
        .find(|l| l["tool"] == "get_secret_value")
        .unwrap();
    assert_eq!(reveal["args"], json!({"name": "db", "key": "password"}));
}

#[tokio::test]
async fn reveal_off_tells_the_user_and_asks_nothing() {
    let policy = SecurityPolicy {
        hide_secret_values: true,
        ..permissive()
    };
    let f = session_fixture(Profile::ReadOnly, policy, true, false).await;
    let outcome = f
        .server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("RevealDisabled"), "{}", outcome.text);
    assert!(
        outcome.text.contains("Settings → Security"),
        "{}",
        outcome.text
    );
    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].kind,
        RequestKind::RevealBlocked { name: "db".into() }
    );
    assert!(f.api.requests().is_empty());
    assert!(f.server.session().grants().is_empty());
}

#[tokio::test]
async fn checks_and_metadata_hold_no_values_or_hashes() {
    let f = session_fixture(Profile::ReadOnly, permissive(), true, false).await;
    let checked = f
        .server
        .call("agent", "check_secret", args(json!({"name": "db"})))
        .await;
    assert!(!checked.is_error, "{}", checked.text);
    let listed = f
        .server
        .call("agent", "list_secrets", args(json!({})))
        .await;
    assert!(!listed.is_error, "{}", listed.text);
    let compared = f
        .server
        .call(
            "agent",
            "compare_secret_values",
            args(json!({"name": "db", "key": "password", "otherName": "db", "otherKey": "copy"})),
        )
        .await;
    assert!(!compared.is_error, "{}", compared.text);
    assert!(
        compared.text.contains("\"equal\": true"),
        "{}",
        compared.text
    );
    for text in [&checked.text, &listed.text, &compared.text] {
        for leaked in ["hunter2", "aHVudGVyMg", "shop", "sha256", "fingerprint"] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
    }
    assert!(
        checked.text.contains("\"host\": \"db\""),
        "{}",
        checked.text
    );
    // No question at all: Secret access is in the minimum (#15).
    assert!(grant_requests(&f).is_empty());
    let audit = std::fs::read_to_string(&f.audit_path).unwrap();
    assert!(!audit.contains("hunter2"), "{audit}");
}

#[tokio::test]
async fn a_refusal_is_not_asked_again() {
    let f = session_fixture(Profile::ReadOnly, permissive(), false, false).await;
    for _ in 0..3 {
        let outcome = f
            .server
            .call("agent", "get_secret_value", args(json!({"name": "db"})))
            .await;
        assert!(outcome.is_error);
    }
    assert_eq!(grant_requests(&f), [Grant::secret(NS, "db")]);
    assert_eq!(
        f.server.session().grants().state(&Grant::secret(NS, "db")),
        Some(GrantState::Denied)
    );
    let last = f
        .server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(last.text.contains("does not ask again"), "{}", last.text);
    assert!(
        f.api.requests().iter().all(|r| !r.contains("secrets/db")),
        "{:?}",
        f.api.requests()
    );
}

#[tokio::test]
async fn reading_needs_no_question_and_is_audited() {
    let f = session_fixture(Profile::ReadOnly, SecurityPolicy::default(), false, false).await;
    let outcome = f
        .server
        .call("agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(f.confirmer.requests().is_empty());
    assert!(f.server.session().grants().is_granted(&Grant::read(NS)));
    let lines = audit_lines(&f.audit_path);
    let grant = lines
        .iter()
        .find(|l| l["tool"] == "grant:list_resources")
        .expect("the right is audited");
    assert_eq!(grant["decided_by"], "access");
    assert_eq!(grant["result"], "access");
    // Secrets at the minimum "read": not even metadata without asking.
    let outcome = f
        .server
        .call("agent", "list_secrets", args(json!({})))
        .await;
    assert!(outcome.is_error);
    assert!(matches!(
        f.confirmer.requests()[0].kind,
        RequestKind::Access { .. }
    ));
    assert!(f.api.requests().iter().all(|r| !r.contains("/secrets")));
}

#[tokio::test]
async fn rights_end_with_the_session() {
    let f = session_fixture(Profile::Deploy, permissive(), true, true).await;
    let outcome = f
        .server
        .call("agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(!outcome.text.contains("denied"), "{}", outcome.text);
    assert!(f.server.session().grants().is_granted(&Grant::read(NS)));
    f.server.session().close().await.unwrap();
    let outcome = f
        .server
        .call("agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(
        outcome.is_error && outcome.text.contains("closed"),
        "{}",
        outcome.text
    );
    assert!(
        f.server
            .session()
            .decide(Grant::modify(NS), true)
            .await
            .is_err()
    );
    // A new session starts with nothing.
    let fresh = session_fixture(Profile::Deploy, permissive(), true, true).await;
    assert!(fresh.server.session().grants().is_empty());
}

#[tokio::test]
async fn revoking_read_takes_back_what_stands_on_it() {
    let f = session_fixture(Profile::Deploy, permissive(), true, true).await;
    let session = f.server.session().clone();
    session.decide(Grant::read(NS), true).await.unwrap();
    session.decide(Grant::secret(NS, "db"), true).await.unwrap();
    session.revoke(&Grant::read(NS)).await.unwrap();
    assert_eq!(
        session.grants().state(&Grant::secret(NS, "db")),
        Some(GrantState::Denied)
    );
    let outcome = f
        .server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(outcome.is_error);
    assert!(
        f.confirmer.requests().is_empty(),
        "revoked rights are not asked again"
    );
}

#[tokio::test]
async fn every_change_is_confirmed_and_rights_come_with_the_access() {
    let f = session_fixture(Profile::Deploy, permissive(), true, true).await;
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    let outcome = f.server.call("agent", "scale", args(scale.clone())).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    // No question for the namespace's rights (#15), each change confirmed.
    assert!(grant_requests(&f).is_empty());
    assert!(f.server.session().grants().is_granted(&Grant::modify(NS)));
    assert_eq!(call_requests(&f), 1);
    f.server.call("agent", "scale", args(scale)).await;
    f.server
        .call(
            "agent",
            "rollout_restart",
            args(json!({"kind": "Deployment", "name": "web"})),
        )
        .await;
    assert_eq!(call_requests(&f), 3);

    // Delete, apply and exec are confirmed every time.
    let delete = json!({"kind": "Widget", "apiVersion": "example.com/v1", "name": "w"});
    f.server
        .call("agent", "delete_resource", args(delete.clone()))
        .await;
    f.server
        .call("agent", "delete_resource", args(delete))
        .await;
    assert_eq!(call_requests(&f), 5);
    f.server
        .call(
            "agent",
            "exec",
            args(json!({"pod": "web-0", "command": ["id"]})),
        )
        .await;
    assert_eq!(call_requests(&f), 6);
    assert!(grant_requests(&f).is_empty());
    // Exec carries the exact argv for the dialog (#194).
    assert_eq!(
        f.confirmer.requests().last().map(|request| &request.kind),
        Some(&RequestKind::Exec {
            command: vec!["id".into()]
        })
    );
}

#[tokio::test]
async fn client_only_confirms_every_change() {
    let f = session_fixture(Profile::Deploy, permissive(), true, false).await;
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    f.server.call("agent", "scale", args(scale.clone())).await;
    f.server.call("agent", "scale", args(scale)).await;
    assert!(grant_requests(&f).is_empty());
    assert_eq!(call_requests(&f), 2);
}

fn log_requests(f: &Fixture) -> usize {
    f.api
        .requests()
        .iter()
        .filter(|r| r.contains("/pods/web-0/log"))
        .count()
}

#[tokio::test]
async fn logs_of_untouched_pods_need_no_confirmation() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call("agent", "get_logs", args(json!({"pod": "web-0"})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("ready"));
    assert!(f.confirmer.requests().is_empty());
    // The agent changed nothing: not even the pod is read.
    assert_eq!(f.api.requests().len(), 1, "{:?}", f.api.requests());
}

#[tokio::test]
async fn logs_of_pods_the_agent_shaped_are_confirmed() {
    // Applying Deployment web for real marks it; its pod's logs are asked.
    let f = fixture(Profile::Deploy, permissive(), true).await;
    let yaml =
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: 1\n";
    let applied = f
        .server
        .call(
            "agent",
            "apply_manifest",
            args(json!({"yaml": yaml, "dryRun": false})),
        )
        .await;
    assert!(!applied.is_error, "{}", applied.text);
    assert!(f.server.session().was_changed(NS, "Deployment", "web"));
    let outcome = f
        .server
        .call("agent", "get_logs", args(json!({"pod": "web-0"})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let asked = f.confirmer.requests();
    let logs = asked.last().unwrap();
    assert_eq!(logs.tool, "get_logs");
    assert!(logs.summary.contains("web-0"), "{}", logs.summary);
    assert!(logs.details.contains("Deployment web"), "{}", logs.details);
    assert_eq!(log_requests(&f), 1);
    assert_eq!(
        audit_lines(&f.audit_path).last().unwrap()["decided_by"],
        "user"
    );

    // Refused: the logs are never fetched. The path through a ConfigMap
    // the pod uses counts as well.
    for (kind, name) in [
        ("Deployment", "web"),
        ("ConfigMap", "app-config"),
        ("Pod", "web-0"),
    ] {
        let f = fixture(Profile::Deploy, permissive(), false).await;
        f.server.session().note_change(NS, kind, name);
        let outcome = f
            .server
            .call("agent", "get_logs", args(json!({"pod": "web-0"})))
            .await;
        assert!(outcome.is_error, "{kind}");
        assert!(outcome.text.contains("rejected"), "{}", outcome.text);
        assert_eq!(log_requests(&f), 0, "{kind}");
        let asked = f.confirmer.requests();
        assert_eq!(asked.len(), 1, "{kind}");
        assert!(
            asked[0].details.contains(&format!("{kind} {name}")),
            "{}",
            asked[0].details
        );
    }

    // A change elsewhere in the namespace does not ask for this pod.
    let f = fixture(Profile::Deploy, permissive(), false).await;
    f.server.session().note_change(NS, "ConfigMap", "unrelated");
    let outcome = f
        .server
        .call("agent", "get_logs", args(json!({"pod": "web-0"})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(f.confirmer.requests().is_empty());
}

fn pod_log_requests(f: &Fixture) -> Vec<String> {
    f.api
        .requests()
        .into_iter()
        .filter(|r| r.contains("/log"))
        .collect()
}

#[tokio::test]
async fn logs_of_a_workload_read_its_pods_and_containers() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"kind": "StatefulSet", "name": "db", "allContainers": true})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    // Only the StatefulSet's own pods, sorted, init containers first.
    let streams: Vec<&str> = outcome
        .text
        .lines()
        .filter_map(|line| line.strip_suffix(" failed"))
        .collect();
    assert_eq!(
        streams,
        [
            "[db-0/init] ERROR db-0",
            "[db-0/db] ERROR db-0",
            "[db-1/init] ERROR db-1",
            "[db-1/db] ERROR db-1"
        ],
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("[db-0/init] hello from db-0/init"));
    assert!(!outcome.text.contains("db-other"), "{}", outcome.text);
    assert!(!outcome.text.contains("hunter2"), "{}", outcome.text);
    assert_eq!(pod_log_requests(&f).len(), 4);
    assert!(f.confirmer.requests().is_empty());

    // All containers of one pod; tail and previous apply to each stream.
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"pod": "db-0", "allContainers": true, "tailLines": 5, "previous": true})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("[db-0/init] hello"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("[db-0/db] hello"), "{}", outcome.text);
    let logs = pod_log_requests(&f);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert!(
        logs.iter()
            .all(|r| r.contains("tailLines=5") && r.contains("previous=true")),
        "{logs:?}"
    );
}

#[tokio::test]
async fn logs_by_selector_are_filtered_per_line_after_masking() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let call = |arguments: Value| f.server.call("agent", "get_logs", args(arguments));
    let outcome = call(json!({"labelSelector": "app=db", "filter": "ERROR"})).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let lines: Vec<&str> = outcome.text.lines().collect();
    // The selector picks every labeled pod, its default container each.
    assert_eq!(
        lines,
        [
            "[db-0/db] ERROR db-0 failed",
            "[db-1/db] ERROR db-1 failed",
            "[db-other/db] ERROR db-other failed"
        ]
    );

    let outcome =
        call(json!({"labelSelector": "app=db", "filter": "^ERROR db-[01] ", "regex": true})).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(outcome.text.lines().count(), 2, "{}", outcome.text);

    // The filter cannot probe what masking hides.
    let outcome = call(json!({"labelSelector": "app=db", "filter": "hunter2"})).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("no line matched"), "{}", outcome.text);

    // One stream: no prefix, as before.
    let outcome = call(json!({"pod": "web-0", "filter": "ready"})).await;
    assert_eq!(outcome.text, "ready\n");

    // A container the pods lack is said, not guessed.
    let outcome = call(json!({"labelSelector": "app=db", "container": "sidecar"})).await;
    assert!(
        outcome.text.contains("db-0 has no container sidecar"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn logs_by_min_level_cover_the_formats_and_count_what_was_read() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let call = |arguments: Value| f.server.call("agent", "get_logs", args(arguments));

    let outcome = call(json!({"pod": "levels-0", "minLevel": "warn"})).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let lines: Vec<&str> = outcome.text.lines().collect();
    assert_eq!(
        lines,
        [
            "(minLevel warn: 9 read, 7 with a recognized level, 4 matched; \
             2 without a recognized level were excluded)",
            "",
            "level=error msg=\"disk full\" password=***",
            "{\"level\":\"warn\",\"token_count\":***,\"msg\":\"slow\"}",
            "{\"level\":50,\"msg\":\"boom\"}",
            "E1103 12:00:00.000000       1 main.go:1] failed",
        ]
    );

    // Both narrow the result, and the note says so.
    let outcome = call(json!({"pod": "levels-0", "minLevel": "error", "filter": "boom"})).await;
    let lines: Vec<&str> = outcome.text.lines().collect();
    assert_eq!(
        lines,
        [
            "(minLevel error: 9 read, 7 with a recognized level, 1 matched and the filter; \
             2 without a recognized level were excluded)",
            "",
            "{\"level\":50,\"msg\":\"boom\"}",
        ]
    );

    // The level comes from the masked line, and the filter can't see past it.
    let outcome = call(json!({"pod": "levels-0", "minLevel": "warn", "filter": "hunter2"})).await;
    assert!(!outcome.text.contains("hunter2"), "{}", outcome.text);
    assert!(
        outcome.text.contains("0 matched and the filter"),
        "{}",
        outcome.text
    );
    assert!(
        !outcome.text.contains("no line matched"),
        "{}",
        outcome.text
    );
}

#[tokio::test]
async fn logs_in_an_unknown_format_say_so_under_min_level() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"labelSelector": "app=db", "minLevel": "error"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome
            .text
            .starts_with("(minLevel error: 9 read, 0 with a recognized level, 0 matched; "),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("says nothing about their levels"),
        "{}",
        outcome.text
    );
    assert!(!outcome.text.contains("ERROR db-0"), "{}", outcome.text);

    // Without minLevel nothing changes: no counts, the same note.
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"labelSelector": "app=db", "filter": "absent"})),
        )
        .await;
    assert!(
        outcome.text.starts_with("(no line matched the filter)"),
        "{}",
        outcome.text
    );
    assert!(!outcome.text.contains("minLevel"), "{}", outcome.text);
}

#[tokio::test]
async fn logs_of_many_pods_are_capped_and_say_what_was_left_out() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"labelSelector": "app=many"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(pod_log_requests(&f).len(), 10);
    assert!(
        outcome.text.starts_with(
            "(12 pods match; only the first 10 were read. Left out: many-10, many-11)"
        ),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("[many-09/db] hello"));

    // Streams are capped too: 10 pods × 2 containers is the limit.
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"labelSelector": "app=many", "allContainers": true})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(pod_log_requests(&f).len(), 20);
}

#[tokio::test]
async fn logs_of_several_shaped_pods_are_asked_once() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    f.server.session().note_change(NS, "StatefulSet", "db");
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"kind": "StatefulSet", "name": "db", "allContainers": true})),
        )
        .await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("rejected"), "{}", outcome.text);
    assert!(pod_log_requests(&f).is_empty());
    let asked = f.confirmer.requests();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].summary.contains("db-0, db-1"),
        "{}",
        asked[0].summary
    );
    assert!(
        asked[0]
            .details
            .contains("Pod db-1: This agent created or changed StatefulSet db"),
        "{}",
        asked[0].details
    );

    let f = fixture(Profile::Deploy, permissive(), true).await;
    f.server.session().note_change(NS, "StatefulSet", "db");
    let outcome = f
        .server
        .call(
            "agent",
            "get_logs",
            args(json!({"labelSelector": "app=db"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(f.confirmer.requests().len(), 1);
    assert_eq!(pod_log_requests(&f).len(), 3);
}

/// `kubach mcp --grant-read`: the session's server without any window.
async fn launched_with_grant_read(
    profile: Profile,
    policy: SecurityPolicy,
    impersonated: bool,
) -> (Fixture, McpServer) {
    let f = session_fixture(profile, policy, false, impersonated).await;
    let audit = Arc::new(AuditLog::open(&f.audit_path).unwrap());
    let server = McpServer::new(f.server.session().clone(), Arc::new(DenyAll), Some(audit));
    server.grant_read_at_launch().await.unwrap();
    (f, server)
}

#[tokio::test]
async fn grant_read_flag_reads_without_a_window() {
    let (f, server) = launched_with_grant_read(Profile::Deploy, permissive(), true).await;
    let outcome = server
        .call(
            "agent",
            "get_resource",
            args(json!({"kind": "deployments", "name": "web"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let entries = server.session().grants().entries();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].at_launch);
    let lines = audit_lines(&f.audit_path);
    assert_eq!(lines[0]["decided_by"], "launch-args");
    assert_eq!(lines[0]["args"], json!({"right": "read", "namespace": NS}));

    // Secrets and changes still need the window: without one, refused.
    let reveal = server
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert!(
        reveal.is_error && !reveal.text.contains("hunter2"),
        "{}",
        reveal.text
    );
    let scale = server
        .call(
            "agent",
            "scale",
            args(json!({"kind": "Deployment", "name": "web", "replicas": 3})),
        )
        .await;
    assert!(scale.is_error, "{}", scale.text);
    assert!(f.api.requests().iter().all(|r| r.starts_with("GET ")));
    assert!(!f.api.requests().iter().any(|r| r.contains("/secrets")));
    // ...and with a window they are asked for, not taken from the flag.
    let asking = McpServer::new(server.session().clone(), f.confirmer.clone(), None);
    asking
        .call("agent", "get_secret_value", args(json!({"name": "db"})))
        .await;
    assert_eq!(grant_requests(&f), [Grant::secret(NS, "db")]);

    // Revoking works as for any right.
    server.session().revoke(&Grant::read(NS)).await.unwrap();
    let after = server
        .call("agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(
        after.is_error && after.text.contains("does not ask again"),
        "{}",
        after.text
    );
}

#[tokio::test]
async fn grant_read_flag_does_not_widen_the_settings() {
    // Beyond the minimum the flag gives nothing: without a window, refused.
    let policy = SecurityPolicy::default();
    let (f, server) = launched_with_grant_read(Profile::ReadOnly, policy, false).await;
    for (tool, arguments) in [
        ("list_secrets", json!({})),
        (
            "scale",
            json!({"kind": "Deployment", "name": "web", "replicas": 3}),
        ),
        ("get_logs", json!({"pod": "web-0"})),
        (
            "list_resources",
            json!({"kind": "Pod", "namespace": OTHER_NS}),
        ),
        ("get_resource", json!({"kind": "Secret", "name": "db"})),
    ] {
        let outcome = server.call("agent", tool, args(arguments)).await;
        assert!(outcome.is_error, "{tool}: {}", outcome.text);
    }
    assert!(f.api.requests().is_empty());
    // Only the session's own namespaces.
    assert!(
        server
            .session()
            .grant_read_at_launch(OTHER_NS)
            .await
            .is_err()
    );
    assert!(!server.session().grants().is_granted(&Grant::read(OTHER_NS)));
}

#[tokio::test]
async fn beyond_the_ceiling_the_user_is_asked_once() {
    let f = session_fixture(Profile::ReadOnly, permissive(), false, true).await;
    f.server.session().set_grants(grants(&[Grant::read(NS)]));
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    let outcome = f
        .server
        .call("claude-code", "scale", args(scale.clone()))
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("did not allow changes"),
        "{}",
        outcome.text
    );
    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 1);
    let RequestKind::Access { raise, current, .. } = &requests[0].kind else {
        panic!("{:?}", requests[0].kind);
    };
    assert_eq!(*raise, Access::profile(Profile::Deploy));
    assert_eq!(*current, Access::ALL.capped(Profile::ReadOnly));
    assert_eq!(
        requests[0].summary,
        format!("claude-code asks for {raise} on minikube (namespace {NS})")
    );
    // Refused: the same request does not bother the user again.
    let again = f.server.call("claude-code", "scale", args(scale)).await;
    assert!(again.text.contains("not asked again"), "{}", again.text);
    assert_eq!(f.confirmer.requests().len(), 1);
    assert!(f.api.requests().is_empty());
    let lines = audit_lines(&f.audit_path);
    let ceiling = lines
        .iter()
        .find(|l| l["tool"] == "access:scale")
        .expect("the answer is audited");
    assert_eq!(ceiling["result"], "refused");
    assert_eq!(ceiling["args"]["raise"]["profile"], "deploy");
    assert_eq!(ceiling["args"]["savedToSettings"], false);
}

#[tokio::test]
async fn an_allowed_raise_keeps_rights_and_confirmations() {
    let f = fixture_with(
        Profile::ReadOnly,
        permissive(),
        ScriptedConfirmer::approving_always(),
        true,
    )
    .await;
    f.server.session().set_grants(grants(&[Grant::read(NS)]));
    let outcome = f
        .server
        .call(
            "agent",
            "delete_resource",
            args(json!({"kind": "Deployment", "name": "web"})),
        )
        .await;
    let kinds: Vec<String> = f
        .confirmer
        .requests()
        .iter()
        .map(|r| match &r.kind {
            RequestKind::Access { .. } => "access".to_owned(),
            RequestKind::Grant { grant } => format!("grant {grant}"),
            RequestKind::Call => "call".to_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    // The access, then the deletion itself: the namespace's right comes
    // with the access (#15).
    assert_eq!(kinds, ["access", "call"], "{}", outcome.text);
    let session = f.server.session();
    assert_eq!(session.access().profile, Profile::Deploy);
    assert!(session.raises().saved);
    let lines = audit_lines(&f.audit_path);
    let ceiling = lines
        .iter()
        .find(|l| l["tool"] == "access:delete_resource")
        .unwrap();
    assert_eq!(ceiling["result"], "always");
    assert_eq!(ceiling["args"]["savedToSettings"], true);
    // A second change asks no raise again, only the deletion.
    let before = f.confirmer.requests().len();
    f.server
        .call(
            "agent",
            "delete_resource",
            args(json!({"kind": "Deployment", "name": "web"})),
        )
        .await;
    let after = f.confirmer.requests();
    assert_eq!(after.len(), before + 1);
    assert_eq!(after.last().unwrap().kind, RequestKind::Call);
}

#[tokio::test]
async fn rbac_is_checked_in_the_named_namespace_not_by_hiding_tools() {
    let api = FakeApi::start().await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::Deploy, &permissive(), &[NS]);
    let capabilities = HashSet::from([Capability::ListEvents, Capability::PodLogs]);
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    );
    session.set_grants(grants(&[Grant::read(NS)]));
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session, confirmer.clone(), None);
    let names = tool_names(&server);
    for offered in [
        "get_logs",
        "exec",
        "scale",
        "rollout_restart",
        "delete_resource",
    ] {
        assert!(names.contains(&offered.to_string()), "{offered}");
    }
    let outcome = server
        .call(
            "agent",
            "exec",
            args(json!({"pod": "web-0", "command": ["id"]})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome
            .text
            .contains(&format!("no right to create pods/exec in namespace {NS}")),
        "{}",
        outcome.text
    );
    let outcome = server
        .call(
            "agent",
            "scale",
            args(json!({"kind": "Deployment", "name": "web", "replicas": 2})),
        )
        .await;
    assert!(
        outcome
            .text
            .contains("no right to patch deployments/scale.apps"),
        "{}",
        outcome.text
    );
    // Not a question for the user: raising the ceiling adds no RBAC rights.
    assert!(confirmer.requests().is_empty());
    assert!(api.requests().is_empty());
}

/// The same checks through the MCP protocol with an rmcp client.
#[tokio::test]
async fn works_over_the_mcp_protocol() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = f.server.clone();
    tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();
    let info = client.peer_info().unwrap();
    assert!(info.instructions.as_deref().unwrap_or("").contains(NS));
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "list_resources"));

    let escape = client
        .call_tool(
            CallToolRequestParams::new("list_resources")
                .with_arguments(args(json!({"kind": "Pod", "namespace": OTHER_NS}))),
        )
        .await
        .unwrap();
    assert_eq!(escape.is_error, Some(true));
    let ok = client
        .call_tool(CallToolRequestParams::new("session_info"))
        .await
        .unwrap();
    assert_ne!(ok.is_error, Some(true));
    assert!(f.api.requests().is_empty());
    let audit = audit_lines(&f.audit_path);
    // The client's self-reported name ends up in the audit log.
    assert_ne!(audit[0]["client"], "");
    client.cancel().await.unwrap();
}

/// Hears the progress notifications of its calls.
struct ProgressClient(tokio::sync::mpsc::UnboundedSender<String>);

impl rmcp::ClientHandler for ProgressClient {
    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        self.0.send(params.message.unwrap_or_default()).ok();
    }
}

/// A call waiting for the user says so through the protocol right away,
/// to a client with a progress token, and any call of `session_info` meanwhile
/// shows it (#116).
#[tokio::test]
async fn a_call_waiting_for_the_user_says_so_over_the_mcp_protocol() {
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let gate = Arc::new(crate::testutil::Gate::new());
    let server = McpServer::new(f.server.session().clone(), gate.clone(), None);
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let (told, mut heard) = tokio::sync::mpsc::unbounded_channel();
    let client = ProgressClient(told).serve(client_io).await.unwrap();
    assert!(
        client
            .peer_info()
            .and_then(|info| info.instructions.clone())
            .is_some_and(|text| text.contains("waitingForUser"))
    );

    // The rmcp client sends a progress token with every request.
    let scale = CallToolRequestParams::new("scale").with_arguments(args(
        json!({"kind": "Deployment", "name": "web", "replicas": 3}),
    ));
    let peer = client.peer().clone();
    let call = tokio::spawn(async move { peer.call_tool(scale).await });
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), heard.recv())
        .await
        .expect("a progress notification")
        .unwrap();
    assert!(message.contains("Kubach window"), "{message}");
    assert!(message.contains("web"), "{message}");

    let info = client
        .call_tool(CallToolRequestParams::new("session_info"))
        .await
        .unwrap();
    let info = serde_json::to_value(&info.content).unwrap();
    let info = info[0]["text"].as_str().unwrap();
    let info: Value = serde_json::from_str(info).unwrap();
    assert_eq!(info["waitingForUser"][0]["tool"], "scale");

    gate.release();
    let done = call.await.unwrap().unwrap();
    assert_eq!(done.is_error, Some(true));
    assert!(f.server.session().waiting().list().is_empty());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn http_requires_the_bearer_token() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let endpoint = crate::transport::serve_http(f.server.clone(), 0)
        .await
        .unwrap();
    assert!(endpoint.addr.ip().is_loopback());
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let send = |auth: Option<String>, origin: Option<&'static str>| {
        let addr = endpoint.addr;
        async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let mut request = format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\ncontent-type: application/json\r\n\
                 accept: application/json, text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n",
                addr.port(),
                body.len()
            );
            if let Some(auth) = auth {
                request.push_str(&format!("authorization: Bearer {auth}\r\n"));
            }
            if let Some(origin) = origin {
                request.push_str(&format!("origin: {origin}\r\n"));
            }
            request.push_str("\r\n");
            request.push_str(body);
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response).await;
            response.lines().next().unwrap_or("").to_owned()
        }
    };
    assert!(send(None, None).await.contains("401"));
    assert!(send(Some("wrong".into()), None).await.contains("401"));
    assert!(
        send(Some(endpoint.token.clone()), None)
            .await
            .contains("200")
    );
    // Browsers send Origin; cross-site pages must be refused.
    assert!(
        send(Some(endpoint.token.clone()), Some("https://evil.example"))
            .await
            .contains("403")
    );
    endpoint.shutdown().await;
}

// ------------------------------------------------------------- Secret writes (#230)

/// A value only the user knows; must never leave Kubach except to the API.
const TYPED: &str = "s3cr3t-Typed-By-The-User";
const GENERATED: &str = "Gen3rated-0nly-1n-Kubach-xyz";

fn b64(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value)
}

fn provided(set: &[(&str, &str)], remove: &[&str]) -> SecretInput {
    SecretInput {
        set: set
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).into()))
            .collect(),
        remove: remove.iter().map(|k| k.to_string()).collect(),
    }
}

/// No value, plain or base64, in `text`.
fn assert_no_values(text: &str, values: &[&str]) {
    for value in values {
        // The messages print neither the value nor the text holding it.
        assert!(!text.contains(value), "a value leaked");
        assert!(!text.contains(&b64(value)), "a base64 value leaked");
    }
}

async fn writing_fixture(confirmer: ScriptedConfirmer) -> Fixture {
    let f = fixture_with(Profile::Deploy, permissive(), confirmer, false).await;
    f.server.session().set_grants(grants(&[Grant::read(NS)]));
    f
}

fn secret_requests(f: &Fixture) -> Vec<SecretRequest> {
    f.confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match request.kind {
            RequestKind::Secret { secret } => Some(*secret),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn entered_values_reach_the_api_but_not_the_agent_or_the_audit() {
    let f = writing_fixture(ScriptedConfirmer::providing(provided(
        &[("password", TYPED), ("api-key", GENERATED)],
        &[],
    )))
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "request_secret_input",
            args(json!({
                "name": "db-new", "mode": "create", "keys": ["password", "api-key"],
                "explanation": "The app needs credentials."
            })),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    // The serialized answer to the agent: keys and sizes, no values.
    let wire = serde_json::to_string(&outcome).unwrap();
    assert_no_values(&wire, &[TYPED, GENERATED]);
    let answer: Value = serde_json::from_str(&outcome.text).unwrap();
    assert_eq!(answer["operation"], "created");
    assert_eq!(answer["keys"][0]["key"], "api-key");
    assert_eq!(answer["keys"][0]["size"], GENERATED.len());
    assert_eq!(answer["keys"][1]["change"], "added");
    // The audit log: keys and the operation, no values.
    let audit = std::fs::read_to_string(&f.audit_path).unwrap();
    assert_no_values(&audit, &[TYPED, GENERATED]);
    let lines = audit_lines(&f.audit_path);
    let last = lines.last().unwrap();
    assert_eq!(last["decided_by"], "user");
    assert_eq!(last["change"]["operation"], "create");
    assert_eq!(last["change"]["keys"], json!(["api-key", "password"]));
    // The user saw Kubach's form, without "remember".
    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].allow_remember);
    let form = &secret_requests(&f)[0];
    assert_eq!(form.op, SecretOp::Create);
    assert_eq!(form.secret_type, "Opaque");
    assert_eq!(form.keys, ["password", "api-key"]);
    assert_eq!(form.explanation, "The app needs credentials.");
    // The API got the values, created by Kubach's MCP field manager.
    let writes = f.api.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(writes[0].starts_with(&format!("POST /api/v1/namespaces/{NS}/secrets?")));
    assert!(
        writes[0].contains("fieldManager=kubach-mcp"),
        "{}",
        writes[0]
    );
    assert!(writes[0].contains(&b64(TYPED)));
}

#[tokio::test]
async fn cancelling_the_form_is_a_refusal() {
    let f = writing_fixture(ScriptedConfirmer::denying()).await;
    let outcome = f
        .server
        .call(
            "agent",
            "request_secret_input",
            args(json!({"name": "db-new", "mode": "create", "keys": ["password"]})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("declined"), "{}", outcome.text);
    assert!(
        outcome.text.contains("nothing was written"),
        "{}",
        outcome.text
    );
    assert!(f.api.writes().is_empty());
    let last = audit_lines(&f.audit_path).pop().unwrap();
    assert_eq!(last["result"], "rejected");
    assert_eq!(last["decided_by"], "user");
    assert!(last.get("change").is_none());
    // Not remembered: the next call asks again.
    f.server
        .call(
            "agent",
            "request_secret_input",
            args(json!({"name": "db-new", "mode": "create"})),
        )
        .await;
    assert_eq!(secret_requests(&f).len(), 2);
}

#[tokio::test]
async fn updates_patch_only_the_keys_the_user_changed() {
    let f = writing_fixture(ScriptedConfirmer::providing(provided(
        &[("password", TYPED)],
        &["copy"],
    )))
    .await;
    let outcome = f
        .server
        .call(
            "agent",
            "request_secret_input",
            args(json!({"name": "db", "mode": "update", "keys": ["password"]})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    // Neither the new value nor the old ones (hunter2).
    assert_no_values(&outcome.text, &[TYPED, "hunter2"]);
    let answer: Value = serde_json::from_str(&outcome.text).unwrap();
    assert_eq!(answer["operation"], "updated");
    assert_eq!(answer["keys"][0]["change"], "replaced");
    assert_eq!(answer["removed"], json!(["copy"]));
    assert_eq!(answer["usedBy"][0]["name"], "web-0");
    assert!(answer["next"].as_str().unwrap().contains("rollout_restart"));
    let form = &secret_requests(&f)[0];
    assert_eq!(form.op, SecretOp::Update);
    let existing: Vec<&str> = form.existing.iter().map(|k| k.key.as_str()).collect();
    assert_eq!(existing, ["copy", "password", "url"]);
    assert_eq!(form.used_by[0].kind, "Pod");
    assert_no_values(&format!("{form:?}"), &["hunter2"]);
    let writes = f.api.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(writes[0].starts_with(&format!("PATCH /api/v1/namespaces/{NS}/secrets/db?")));
    let body: Value = serde_json::from_str(writes[0].split_once('\n').unwrap().1).unwrap();
    assert_eq!(body["data"]["password"], b64(TYPED));
    assert_eq!(body["data"]["copy"], Value::Null);
    assert!(
        body["data"].get("url").is_none(),
        "untouched keys are not sent"
    );
    assert_eq!(body["metadata"]["resourceVersion"], "1");
    let audit = std::fs::read_to_string(&f.audit_path).unwrap();
    assert_no_values(&audit, &[TYPED, "hunter2"]);
}

#[tokio::test]
async fn copies_happen_inside_kubach() {
    let f = writing_fixture(ScriptedConfirmer::approving()).await;
    let outcome = f
        .server
        .call(
            "agent",
            "copy_secret",
            args(json!({"name": "db", "toName": "db-copy"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_no_values(&outcome.text, &["hunter2"]);
    let form = &secret_requests(&f)[0];
    assert_eq!(form.op, SecretOp::Copy);
    assert_eq!(form.source.as_ref().unwrap().name, "db");
    let writes = f.api.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let body: Value = serde_json::from_str(writes[0].split_once('\n').unwrap().1).unwrap();
    assert_eq!(body["metadata"]["name"], "db-copy");
    assert_eq!(body["data"]["password"], "aHVudGVyMg==");
    assert_eq!(
        body["metadata"]["annotations"],
        json!({"kubach.io/copied-from": format!("{NS}/db")})
    );
    assert!(body["metadata"].get("ownerReferences").is_none());
    let audit = std::fs::read_to_string(&f.audit_path).unwrap();
    assert_no_values(&audit, &["hunter2"]);
    assert_eq!(
        audit_lines(&f.audit_path).pop().unwrap()["change"]["operation"],
        "copy"
    );

    // Rejected: nothing is written.
    let f = writing_fixture(ScriptedConfirmer::denying()).await;
    let outcome = f
        .server
        .call(
            "agent",
            "copy_secret",
            args(json!({"name": "db", "toName": "db-copy"})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(f.api.writes().is_empty());
}

#[tokio::test]
async fn deleting_is_confirmed_every_time_and_shows_users() {
    let f = writing_fixture(ScriptedConfirmer::approving()).await;
    for _ in 0..2 {
        let outcome = f
            .server
            .call("agent", "delete_secret", args(json!({"name": "db"})))
            .await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert_no_values(&outcome.text, &["hunter2"]);
    }
    let forms = secret_requests(&f);
    assert_eq!(forms.len(), 2, "every deletion is asked");
    assert_eq!(forms[0].op, SecretOp::Delete);
    assert_eq!(forms[0].used_by[0].name, "web-0");
    assert!(forms[0].used_by[0].via[0].contains("key password"));
    let writes = f.api.writes();
    assert_eq!(writes.len(), 2);
    assert!(writes[0].starts_with(&format!("DELETE /api/v1/namespaces/{NS}/secrets/db")));
    // Exactly the Secret the user saw.
    assert!(writes[0].contains(r#""uid":"2""#), "{}", writes[0]);
}

#[tokio::test]
async fn a_temporary_service_account_may_change_only_approved_secrets() {
    // The Role mirror: approving a write records that Secret's name.
    let f = writing_fixture(ScriptedConfirmer::approving()).await;
    f.server
        .call("agent", "delete_secret", args(json!({"name": "db"})))
        .await;
    let rules = crate::rules::grant_rules(
        &{
            let mut g = f.server.session().grants();
            g.set(Grant::modify(NS), GrantState::Granted);
            g
        },
        NS,
        Access::ALL,
    );
    let named: Vec<_> = rules
        .iter()
        .filter(|r| r.resource_names.is_some())
        .collect();
    assert_eq!(named.len(), 1);
    assert_eq!(
        named[0].resource_names.as_deref(),
        Some(&["db".to_owned()][..])
    );
    assert_eq!(named[0].verbs, ["patch", "delete"]);
}

// ------------------------------------------------------------- topology (#327)

fn topology_json(outcome: &CallOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.text);
    serde_json::from_str(&outcome.text).unwrap()
}

fn node_ids(graph: &Value) -> Vec<String> {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn topology_is_read_from_the_session_namespace() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let outcome = f
        .server
        .call("agent", "get_topology", args(json!({})))
        .await;
    let graph = topology_json(&outcome);
    let ids = node_ids(&graph);
    for id in [
        format!("Deployment.apps/{NS}/web"),
        format!("Service/{NS}/web"),
        format!("Ingress.networking.k8s.io/{NS}/web"),
        format!("ConfigMap/{NS}/app-config"),
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    let selects = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["kind"] == "selects" && edge["to"] == format!("Deployment.apps/{NS}/web"))
        .expect("Service selects the Deployment");
    // Default deny: the policy layer says the Service's traffic is blocked.
    assert_eq!(selects["policy"]["access"], "blocked", "{selects}");
    // Values never: Secrets and ConfigMaps are listed by metadata only.
    assert!(!outcome.text.contains("hunter2"), "{}", outcome.text);
    assert!(!outcome.text.contains("aHVudGVy"), "{}", outcome.text);
    // Every request is a read in the session namespace (or its own
    // Namespace object, for namespaceSelector labels).
    let requests = f.api.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        assert!(request.starts_with("GET "), "{request}");
        assert!(
            request.contains(&format!("/namespaces/{NS}/"))
                || request.ends_with(&format!("/namespaces/{NS}")),
            "{request}"
        );
    }
    let lines = audit_lines(&f.audit_path);
    let last = lines.last().unwrap();
    assert_eq!(last["tool"], "get_topology");
    assert_eq!(last["allowed"], true);
}

#[tokio::test]
async fn topology_around_a_resource() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let graph = topology_json(
        &f.server
            .call(
                "agent",
                "get_topology",
                args(
                    json!({"resource": {"kind": "svc", "name": "web"}, "depth": 1,
                            "networkPolicies": false}),
                ),
            )
            .await,
    );
    let ids = node_ids(&graph);
    assert!(ids.contains(&format!("Deployment.apps/{NS}/web")));
    assert!(
        !ids.contains(&format!("ConfigMap/{NS}/app-config")),
        "{ids:?}"
    );
    assert!(graph.get("note").is_none());
    let missing = f
        .server
        .call(
            "agent",
            "get_topology",
            args(json!({"resource": {"kind": "Deployment", "name": "nope"}})),
        )
        .await;
    assert!(missing.is_error && missing.text.contains("not on the topology graph"));
}

#[tokio::test]
async fn topology_without_secret_access_skips_secrets() {
    let policy = SecurityPolicy::with_minimum(Access {
        secrets: false,
        ..Access::ALL
    });
    let f = fixture(Profile::ReadOnly, policy, false).await;
    let graph = topology_json(
        &f.server
            .call("agent", "get_topology", args(json!({})))
            .await,
    );
    assert!(
        graph["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "Secret"),
        "{graph}"
    );
    assert!(
        !f.api.requests().iter().any(|r| r.contains("/secrets")),
        "{:?}",
        f.api.requests()
    );
}

#[tokio::test]
async fn topology_sets_up_the_read_right_without_asking() {
    let f = session_fixture(Profile::ReadOnly, permissive(), true, false).await;
    let outcome = f
        .server
        .call("agent", "get_topology", args(json!({})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(grant_requests(&f).is_empty());
    assert!(f.server.session().grants().is_granted(&Grant::read(NS)));

    // Revoked by the user: refused, not asked again.
    let refused = session_fixture(Profile::ReadOnly, permissive(), false, false).await;
    refused
        .server
        .session()
        .revoke(&Grant::read(NS))
        .await
        .unwrap();
    let outcome = refused
        .server
        .call("agent", "get_topology", args(json!({})))
        .await;
    assert!(outcome.is_error);
    assert!(refused.api.requests().is_empty());
}

#[tokio::test]
async fn can_reach_explains_the_verdict() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    let verdict = topology_json(
        &f.server
            .call(
                "agent",
                "can_reach",
                args(json!({"from": {"kind": "Pod", "name": "web-0"},
                            "to": {"kind": "Service", "name": "web"}, "port": 80})),
            )
            .await,
    );
    assert_eq!(verdict["allowed"], false, "{verdict}");
    let first = &verdict["verdicts"][0];
    // Service port 80 maps to the pods' 8080.
    assert_eq!(first["port"], 8080, "{verdict}");
    assert_eq!(first["egress"]["allowed"], true);
    assert_eq!(
        first["ingress"]["isolatedBy"][0],
        format!("{NS}/default-deny"),
        "{verdict}"
    );
    // From outside: still blocked, and the address needs no namespace.
    let outside = topology_json(
        &f.server
            .call(
                "agent",
                "can_reach",
                args(json!({"from": {"address": "203.0.113.7"},
                            "to": {"kind": "Deployment", "name": "web"}, "port": "http"})),
            )
            .await,
    );
    assert_eq!(outside["allowed"], false, "{outside}");
    assert_eq!(outside["verdicts"][0]["port"], 8080, "{outside}");
}

// ------------------------------------------------- namespaces on request (#401)

/// A session started without namespaces (#401) against the fake API, with
/// `hints` for `list_namespaces`.
fn open_session(
    url: &str,
    policy: &SecurityPolicy,
    hints: Vec<crate::scope::NamespaceHint>,
) -> Arc<Session> {
    let client = Client::try_from(Config::new(url.parse().unwrap())).unwrap();
    let spec = crate::scope::SessionSpec {
        context: "minikube".into(),
        server_url: "https://127.0.0.1:8443".into(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: None,
        hints,
    };
    let scope = crate::scope::SessionScope::new(&spec, policy).unwrap();
    let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
    Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        capabilities,
    )
}

/// An API server that answers every request with `status` and `body`, and
/// counts the requests.
async fn answering(status: &'static str, body: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
            let body = body.to_string();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).await.unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                log.lock().unwrap().push(line.trim().to_owned());
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

fn namespaces_of(text: &str) -> Vec<(String, String, String)> {
    let out: Value = serde_json::from_str(text).unwrap();
    out["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["name"].as_str().unwrap().to_owned(),
                n["source"].as_str().unwrap().to_owned(),
                n["access"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn list_namespaces_falls_back_to_what_kubach_knows() {
    use crate::scope::{NamespaceHint, NamespaceSource};
    // A managed cluster: the user may not list namespaces.
    let forbidden = json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
        "message": "namespaces is forbidden", "reason": "Forbidden", "code": 403});
    let (url, seen) = answering("403 Forbidden", forbidden).await;
    let session = open_session(
        &url,
        &permissive(),
        vec![
            NamespaceHint::new("team-a", NamespaceSource::Kubeconfig),
            NamespaceHint::new("team-b", NamespaceSource::RecentlyOpened),
        ],
    );
    let mut table = Grants::new();
    table.set(Grant::read("team-b"), GrantState::Denied);
    table.set(Grant::read("granted"), GrantState::Granted);
    session.set_grants(table);
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let outcome = server
        .call("test-agent", "list_namespaces", Map::new())
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let out: Value = serde_json::from_str(&outcome.text).unwrap();
    assert_eq!(out["complete"], false);
    assert!(
        out["note"]
            .as_str()
            .unwrap()
            .contains("does not let you list")
    );
    assert_eq!(
        namespaces_of(&outcome.text),
        [
            ("team-a".into(), "kubeconfig".into(), "available".into()),
            ("team-b".into(), "recently-opened".into(), "refused".into()),
            ("granted".into(), "session".into(), "allowed".into()),
        ]
    );
    // One request: the list, with the user's credentials. Nothing else.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("GET /api/v1/namespaces?"), "{seen:?}");
}

#[tokio::test]
async fn list_namespaces_lists_names_when_allowed() {
    let list = json!({"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadataList",
    "metadata": {}, "items": [
        {"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadata", "metadata": {"name": "team-a"}},
        {"apiVersion": "meta.k8s.io/v1", "kind": "PartialObjectMetadata", "metadata": {"name": "kube-system"}},
    ]});
    let (url, _) = answering("200 OK", list).await;
    let several = permissive();
    let session = open_session(&url, &several, Vec::new());
    let server = McpServer::new(session, Arc::new(DenyAll), None);
    let outcome = server
        .call("test-agent", "list_namespaces", Map::new())
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let out: Value = serde_json::from_str(&outcome.text).unwrap();
    assert_eq!(out["complete"], true);
    assert_eq!(
        namespaces_of(&outcome.text),
        [
            ("team-a".into(), "cluster".into(), "available".into()),
            ("kube-system".into(), "cluster".into(), "available".into()),
        ]
    );
    // Names only: no labels, annotations or anything else of the objects.
    assert!(!outcome.text.contains("metadata"));
}

#[tokio::test]
async fn a_pinned_session_lists_only_its_namespaces() {
    let f = session_fixture(Profile::ReadOnly, permissive(), false, false).await;
    let outcome = f
        .server
        .call("test-agent", "list_namespaces", Map::new())
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(
        namespaces_of(&outcome.text),
        [(NS.into(), "launch-arguments".into(), "available".into())]
    );
    assert!(f.api.requests().is_empty());
}

#[tokio::test]
async fn reading_any_namespace_asks_nothing() {
    let api = FakeApi::start().await;
    let session = open_session(&api.url, &permissive(), Vec::new());
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);

    // Without a namespace there is nothing to read yet, and nothing asked.
    let outcome = server
        .call("test-agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(
        outcome.is_error && outcome.text.contains("list_namespaces"),
        "{}",
        outcome.text
    );
    assert!(confirmer.requests().is_empty());

    // Naming one reads it without a question (#15), and sets it up.
    let outcome = server
        .call(
            "test-agent",
            "list_resources",
            args(json!({"kind": "Pod", "namespace": NS})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(confirmer.requests().is_empty());
    assert_eq!(session.namespaces(), [NS]);

    // Now it is the session's namespace; another one is read just as well.
    let outcome = server
        .call("test-agent", "list_resources", args(json!({"kind": "Pod"})))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let outcome = server
        .call(
            "test-agent",
            "list_resources",
            args(json!({"kind": "Pod", "namespace": OTHER_NS})),
        )
        .await;
    // Allowed: the fake API serves no Pods there, but no policy stopped it.
    assert!(
        !outcome.text.starts_with("denied by policy"),
        "{}",
        outcome.text
    );
    assert!(confirmer.requests().is_empty());
    assert_eq!(session.namespaces(), [NS, OTHER_NS]);
}

#[tokio::test]
async fn escape_attempts_in_an_open_session_never_reach_the_api() {
    let api = FakeApi::start().await;
    let session = open_session(&api.url, &permissive(), Vec::new());
    session.set_grants(Grants::all(&[NS]));
    let server = McpServer::new(session, Arc::new(ScriptedConfirmer::approving()), None);
    // Another valid namespace is no escape in an open session (#15).
    let legit = |arguments: &Value| {
        let text = arguments.to_string();
        [
            format!("\"{OTHER_NS}\""),
            "\"kube-system\"".to_owned(),
            "\"all\"".to_owned(),
        ]
        .iter()
        .any(|ns| text.contains(ns.as_str()))
    };
    for (tool, arguments) in escape_attempts()
        .into_iter()
        .filter(|(_, arguments)| !legit(arguments))
    {
        let outcome = server
            .call("test-agent", tool, args(arguments.clone()))
            .await;
        assert!(
            outcome.is_error,
            "{tool} {arguments} was allowed: {}",
            outcome.text
        );
    }
    assert!(api.requests().is_empty(), "{:?}", api.requests());
}

#[tokio::test]
async fn helm_tools_need_the_users_helm_and_follow_the_ceiling() {
    let f = fixture(Profile::ReadOnly, permissive(), false).await;
    assert!(
        !tool_names(&f.server)
            .iter()
            .any(|name| name.starts_with("helm_"))
    );
    f.server.set_helm(crate::helm_tools::HelmEnv::default());
    let tools = f.server.tools();
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref().map(str::to_owned))
            .unwrap_or_else(|| panic!("{name} is not offered"))
    };
    for read in [
        "helm_search_charts",
        "helm_show_values",
        "helm_list_releases",
        "helm_release_history",
        "helm_get_values",
    ] {
        assert!(
            !description(read).contains("Requires your approval"),
            "{read}"
        );
    }
    for change in [
        "helm_install",
        "helm_upgrade",
        "helm_rollback",
        "helm_uninstall",
    ] {
        assert!(
            description(change).contains("Requires your approval in Kubach"),
            "{change}"
        );
    }
    // Without access to Secrets, reading releases (kept in Secrets) asks
    // for it, as list_secrets does (#53).
    let strict = fixture(
        Profile::ReadOnly,
        SecurityPolicy::with_minimum(Access {
            secrets: false,
            ..Access::ALL
        }),
        false,
    )
    .await;
    strict
        .server
        .set_helm(crate::helm_tools::HelmEnv::default());
    let strict_tools = strict.server.tools();
    for read in [
        "helm_list_releases",
        "helm_release_history",
        "helm_get_values",
    ] {
        let description = strict_tools
            .iter()
            .find(|t| t.name == read)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default();
        assert!(
            description.contains("Requires your approval in Kubach"),
            "{read}: {description}"
        );
    }
    assert!(f.server.instructions().contains("helm_install"));
    assert!(f.server.instructions().contains("helm_get_values"));
    assert!(description("helm_upgrade").contains("valuesPatch"));
    // A path or an unknown repository is refused before anything runs.
    let outcome = f
        .server
        .call(
            "agent",
            "helm_show_values",
            args(json!({"chart": "/etc/passwd"})),
        )
        .await;
    assert!(
        outcome.text.starts_with("denied by policy"),
        "{}",
        outcome.text
    );
    assert!(f.confirmer.requests().is_empty());
    assert!(f.api.requests().is_empty());
}

#[tokio::test]
async fn helm_get_values_masks_secrets_and_never_falls_back() {
    let f = fixture(Profile::ReadOnly, permissive(), true).await;
    f.server.set_helm(crate::helm_tools::HelmEnv::default());
    let outcome = f
        .server
        .call(
            "agent",
            "helm_get_values",
            args(json!({"release": "web", "revision": 2, "namespace": NS})),
        )
        .await;
    assert!(!outcome.is_error, "the read failed");
    for secret in [
        "hunter2-in-values",
        "first-key-in-values",
        "eyJhbGciOiJIUzI1NiJ9",
    ] {
        assert!(!outcome.text.contains(secret), "a value is not masked");
    }
    let answer: Value = serde_json::from_str(&outcome.text).unwrap();
    assert_eq!(answer["revision"], 2);
    assert_eq!(answer["chart"], "podinfo-6.15.0");
    let values = answer["values"].as_str().unwrap();
    assert!(values.contains("replicaCount: 2"), "{values}");
    assert!(values.contains("user: app"), "{values}");
    assert_eq!(
        answer["maskedKeys"],
        json!(["apiKeys[0]", "auth.password", "extra.webhook"])
    );
    // Reading needs nothing but read(ns), and asks nothing.
    assert!(f.confirmer.requests().is_empty());

    // The namespace's identity may not read the storage: a clear refusal,
    // and no other credentials are tried.
    let outcome = f
        .server
        .call(
            "agent",
            "helm_get_values",
            args(json!({"release": "web", "revision": 9, "namespace": NS})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome
            .text
            .contains("does not read them with other credentials"),
        "{}",
        outcome.text
    );
    let reads: Vec<String> = f
        .api
        .requests()
        .into_iter()
        .filter(|r| r.contains("sh.helm.release"))
        .collect();
    assert_eq!(
        reads,
        [
            format!("GET /api/v1/namespaces/{NS}/secrets/sh.helm.release.v1.web.v2"),
            format!("GET /api/v1/namespaces/{NS}/secrets/sh.helm.release.v1.web.v9"),
        ]
    );
}

// ------------------------------------------------- don't ask again (#30)

/// An impersonated session whose user ticks "don't ask again" wherever it
/// is offered.
async fn remembering_fixture() -> Fixture {
    fixture_with(
        Profile::Deploy,
        permissive(),
        ScriptedConfirmer::approving_for_session(),
        true,
    )
    .await
}

fn remembered(action: crate::RememberedAction) -> crate::Remembered {
    crate::Remembered {
        action,
        namespace: NS.into(),
    }
}

fn web_deployment(host_path: bool) -> String {
    let volumes = if host_path {
        "      volumes:\n      - name: host\n        hostPath:\n          path: /\n"
    } else {
        ""
    };
    format!(
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  template:\n    \
         spec:\n      containers:\n      - name: app\n        image: nginx\n{volumes}"
    )
}

#[tokio::test]
async fn dont_ask_again_holds_for_that_change_in_that_namespace() {
    use crate::RememberedAction;
    let f = remembering_fixture().await;
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    let first = f.server.call("agent", "scale", args(scale.clone())).await;
    assert!(!first.is_error, "{}", first.text);
    assert!(
        first.text.contains("chose not to be asked again"),
        "{}",
        first.text
    );
    assert_eq!(call_requests(&f), 1);
    assert!(f.confirmer.requests()[0].allow_remember);
    assert_eq!(
        f.server.session().remembered(),
        vec![remembered(RememberedAction::Scale)]
    );

    // The next scale runs without a window, and everyone is told.
    let patches = |f: &Fixture| {
        f.api
            .requests()
            .iter()
            .filter(|r| r.starts_with("PATCH "))
            .count()
    };
    let before = patches(&f);
    let second = f.server.call("agent", "scale", args(scale)).await;
    assert!(!second.is_error, "{}", second.text);
    assert!(
        second
            .text
            .starts_with("Approved by the user's earlier choice"),
        "{}",
        second.text
    );
    assert_eq!(call_requests(&f), 1);
    assert_eq!(patches(&f), before + 1);
    let audit = audit_lines(&f.audit_path);
    let scales: Vec<&Value> = audit
        .iter()
        .filter(|line| line["tool"] == "scale")
        .collect();
    assert_eq!(scales[0]["decided_by"], "user");
    assert_eq!(scales[0]["remembered"], "saved");
    assert_eq!(scales[1]["decided_by"], "remembered");
    assert_eq!(scales[1]["remembered"], "used");
    assert!(
        audit
            .iter()
            .filter(|line| line["tool"] != "scale")
            .all(|line| line.get("remembered").is_none())
    );

    // Another kind of change is asked on its own.
    let restart = json!({"kind": "Deployment", "name": "web"});
    f.server
        .call("agent", "rollout_restart", args(restart.clone()))
        .await;
    assert_eq!(call_requests(&f), 2);
    f.server
        .call("agent", "rollout_restart", args(restart))
        .await;
    assert_eq!(call_requests(&f), 2);

    // Deleting is asked every time and never offers it.
    let delete = json!({"kind": "Widget", "apiVersion": "example.com/v1", "name": "w"});
    for _ in 0..2 {
        f.server
            .call("agent", "delete_resource", args(delete.clone()))
            .await;
    }
    assert_eq!(call_requests(&f), 4);
    assert!(!f.confirmer.requests()[3].allow_remember);

    // session_info lists it.
    let info = f
        .server
        .call("agent", "session_info", Map::new())
        .await
        .text;
    let info: Value = serde_json::from_str(&info).unwrap();
    assert_eq!(info["notAskedAgain"][0]["action"], "scale");
    assert_eq!(info["notAskedAgain"][0]["namespace"], NS);
}

#[tokio::test]
async fn dont_ask_again_never_covers_dangerous_fields() {
    let f = remembering_fixture().await;
    let apply = |host_path: bool| {
        args(json!({"yaml": web_deployment(host_path), "namespace": NS, "dryRun": false}))
    };
    let outcome = f.server.call("agent", "apply_manifest", apply(false)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(call_requests(&f), 1);
    f.server.call("agent", "apply_manifest", apply(false)).await;
    assert_eq!(call_requests(&f), 1, "remembered");
    // A hostPath needs "I understand" every time (#194).
    for _ in 0..2 {
        f.server.call("agent", "apply_manifest", apply(true)).await;
    }
    let requests = f.confirmer.requests();
    assert_eq!(call_requests(&f), 3);
    assert!(!requests[1].dangerous.is_empty());
    assert!(!requests[1].allow_remember && !requests[2].allow_remember);
}

#[tokio::test]
async fn secrets_are_asked_past_dont_ask_again() {
    let f = remembering_fixture().await;
    for _ in 0..2 {
        f.server
            .call(
                "agent",
                "delete_secret",
                args(json!({"name": "db", "explanation": "rotate"})),
            )
            .await;
    }
    let forms: Vec<_> = f
        .confirmer
        .requests()
        .into_iter()
        .filter(|request| matches!(request.kind, RequestKind::Secret { .. }))
        .collect();
    assert_eq!(forms.len(), 2, "every deletion is asked");
    assert!(forms.iter().all(|request| !request.allow_remember));
    assert!(f.server.session().remembered().is_empty());
}

#[tokio::test]
async fn dont_ask_again_ends_with_the_right_or_the_session() {
    use crate::RememberedAction;
    let f = remembering_fixture().await;
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    f.server.call("agent", "scale", args(scale.clone())).await;
    let session = f.server.session();
    assert!(session.is_remembered(&remembered(RememberedAction::Scale)));

    // Taking `modify(ns)` back forgets it.
    session.revoke(&Grant::modify(NS)).await.unwrap();
    assert!(session.remembered().is_empty());

    // "Ask again" in Settings forgets it too.
    session.decide(Grant::modify(NS), true).await.unwrap();
    f.server.call("agent", "scale", args(scale.clone())).await;
    assert_eq!(call_requests(&f), 2);
    assert_eq!(session.remembered().len(), 1);
    session.forget_remembered();
    f.server.call("agent", "scale", args(scale.clone())).await;
    assert_eq!(call_requests(&f), 3, "asked again");

    // A new session starts with nothing remembered; this one forgets it
    // when it ends.
    assert_eq!(session.remembered().len(), 1);
    session.close().await.unwrap();
    assert!(session.remembered().is_empty());
    assert!(!session.is_remembered(&remembered(RememberedAction::Scale)));
}

#[tokio::test]
async fn client_only_never_offers_dont_ask_again() {
    let f = fixture_with(
        Profile::Deploy,
        permissive(),
        ScriptedConfirmer::approving_for_session(),
        false,
    )
    .await;
    let scale = json!({"kind": "Deployment", "name": "web", "replicas": 3});
    f.server.call("agent", "scale", args(scale.clone())).await;
    f.server.call("agent", "scale", args(scale)).await;
    assert_eq!(call_requests(&f), 2);
    assert!(f.confirmer.requests().iter().all(|r| !r.allow_remember));
    assert!(f.server.session().remembered().is_empty());
}

// ------------------------------------------------ custom resource kinds (#65)

fn without_crds() -> SecurityPolicy {
    SecurityPolicy::with_minimum(Access {
        crds: false,
        ..Access::ALL
    })
}

/// The two tables of `list_kinds`: built-in rows, then custom rows and the
/// header line of the custom section.
fn kinds_of(text: &str) -> (String, String) {
    let (builtin, custom) = text.split_once("\nCUSTOM RESOURCES").unwrap_or((text, ""));
    (builtin.to_owned(), custom.to_owned())
}

#[tokio::test]
async fn list_kinds_offers_custom_resources_that_need_approval() {
    let f = session_fixture(Profile::Deploy, without_crds(), false, false).await;
    let outcome = f.server.call("test-agent", "list_kinds", Map::new()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let (builtin, custom) = kinds_of(&outcome.text);
    assert!(
        builtin.contains("Deployment\tapps/v1\tdeployments"),
        "{builtin}"
    );
    assert!(!builtin.contains("example.com"), "{builtin}");
    assert!(
        custom.contains("Widget\texample.com/v1\twidgets"),
        "{custom}"
    );
    assert!(
        custom.contains("Widget\tother.example.com/v1\twidgets"),
        "{custom}"
    );
    assert!(
        custom.contains("Certificate\tcert-manager.io/v1\tcertificates"),
        "{custom}"
    );
    assert!(
        custom.contains("Application\targoproj.io/v1alpha1"),
        "{custom}"
    );
    assert!(custom.contains("need the user's approval"), "{custom}");
    assert!(
        custom.contains("list_resources or get_resource"),
        "{custom}"
    );
    // Never cluster-scoped kinds, Secrets or identity objects.
    for hidden in [
        "ClusterIssuer",
        "Secret",
        "ServiceAccount",
        "Role",
        "Namespace",
        "Node",
    ] {
        assert!(
            !outcome
                .text
                .lines()
                .any(|l| l.starts_with(&format!("{hidden}\t"))),
            "{hidden}: {}",
            outcome.text
        );
    }
    // Listing kinds asks nobody and reaches no API.
    assert!(f.confirmer.requests().is_empty());
    assert!(f.api.requests().is_empty());
}

#[tokio::test]
async fn list_kinds_says_when_custom_resources_are_granted() {
    let f = session_fixture(Profile::Deploy, permissive(), false, false).await;
    let outcome = f.server.call("test-agent", "list_kinds", Map::new()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let (builtin, custom) = kinds_of(&outcome.text);
    assert!(custom.contains("is granted in this session"), "{custom}");
    assert!(!custom.contains("approval"), "{custom}");
    assert!(custom.contains("Widget\texample.com/v1"), "{custom}");
    // Custom kinds are listed once, in their own section.
    assert!(!builtin.contains("example.com"), "{builtin}");
    // Access to custom resources reads built-in kinds beyond the
    // allowlist too, but never identity objects.
    for hidden in ["ServiceAccount", "Role", "RoleBinding", "Secret"] {
        assert!(
            !outcome
                .text
                .lines()
                .any(|l| l.starts_with(&format!("{hidden}\t"))),
            "{hidden}: {}",
            outcome.text
        );
    }
}

/// The texts the agent reads agree with the policy (#57): a Deploy-profile
/// session has access to changes, yet the policy confirms each one, so
/// nothing may say changes run without asking.
#[tokio::test]
async fn texts_say_changes_are_confirmed_as_the_policy_does() {
    use crate::policy::{Confirmation, ForwardPortRef, KindRef, Request};
    let f = fixture(Profile::Deploy, permissive(), false).await;
    let session = f.server.session();
    assert_eq!(session.access().profile, Profile::Deploy);
    let ns = Some(NS.to_owned());
    let changes = [
        Request::Apply {
            yaml: "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: web\n".into(),
            namespace: ns.clone(),
            dry_run: false,
        },
        Request::Scale {
            kind: KindRef::new("Deployment"),
            name: "web".into(),
            namespace: ns.clone(),
            replicas: 1,
        },
        Request::Restart {
            kind: KindRef::new("Deployment"),
            name: "web".into(),
            namespace: ns.clone(),
        },
        Request::Delete {
            kind: KindRef::new("Deployment"),
            name: "web".into(),
            namespace: ns.clone(),
        },
        Request::Exec {
            pod: "web-0".into(),
            container: None,
            namespace: ns.clone(),
            command: vec!["id".into()],
        },
        Request::PortForward {
            namespace: ns.clone(),
            target: "pod/web-0".into(),
            port: ForwardPortRef::Number(8080),
            local_port: None,
        },
        Request::HelmChange {
            namespace: ns.clone(),
            release: "web".into(),
        },
    ];
    for request in &changes {
        let allowed = session
            .policy()
            .check(request)
            .unwrap_or_else(|denial| panic!("{request:?}: {denial:?}"));
        assert_eq!(allowed.confirmation, Confirmation::Required, "{request:?}");
    }

    let instructions = f.server.instructions();
    let info = f.server.call("agent", "session_info", Map::new()).await;
    assert!(!info.is_error, "{}", info.text);
    for (what, text) in [
        ("instructions", &instructions),
        ("session_info", &info.text),
    ] {
        assert!(
            !text.contains("Without asking"),
            "{what} says \"Without asking\""
        );
        assert!(
            text.contains("approves no change"),
            "{what}: no \"approves no change\""
        );
        assert!(text.contains("one by one"), "{what}: no \"one by one\"");
        assert!(
            text.contains("don't ask again"),
            "{what}: no \"don't ask again\""
        );
    }
    assert!(instructions.contains("even within your access"));

    // A tool beyond the access: allowing the access approves no call.
    let read_only = fixture(Profile::ReadOnly, permissive(), false).await;
    let tools = read_only.server.tools();
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref().map(str::to_owned))
            .unwrap_or_else(|| panic!("{name} is not offered"))
    };
    for change in [
        "apply_manifest",
        "scale",
        "rollout_restart",
        "delete_resource",
        "exec",
        "port_forward_start",
    ] {
        assert!(
            description(change).contains("each one is still confirmed"),
            "{change}: {}",
            description(change)
        );
    }
    let logs = description("get_logs");
    assert!(logs.contains("Requires your approval in Kubach"), "{logs}");
    assert!(!logs.contains("each one is still confirmed"), "{logs}");
}

// ------------------------------------------------------------- debug_container (#64)

fn ephemeral_patches(f: &Fixture) -> Vec<String> {
    f.api
        .writes()
        .into_iter()
        .filter(|w| w.starts_with("PATCH ") && w.contains("/ephemeralcontainers"))
        .collect()
}

fn patch_body(write: &str) -> Value {
    serde_json::from_str(write.split_once('\n').unwrap().1).unwrap()
}

/// A Debug session where the API server holds the boundary and the user
/// would tick "don't ask again" wherever it is offered.
async fn debug_fixture() -> Fixture {
    let f = fixture_with(
        Profile::Debug,
        permissive(),
        ScriptedConfirmer::approving_for_session(),
        true,
    )
    .await;
    f.server
        .session()
        .set_grants(grants(&[Grant::read(NS), Grant::modify(NS)]));
    f
}

#[tokio::test]
async fn debug_container_is_dry_run_then_confirmed_every_time() {
    let f = debug_fixture().await;
    let call = json!({"pod": "distroless-0", "targetContainer": "app", "name": "dbg"});
    let outcome = f
        .server
        .call("agent", "debug_container", args(call.clone()))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("is running"), "{}", outcome.text);
    assert!(outcome.text.contains("container dbg"), "{}", outcome.text);
    let patches = ephemeral_patches(&f);
    assert_eq!(patches.len(), 2, "{patches:?}");
    assert!(patches[0].contains("dryRun=All"), "{}", patches[0]);
    assert!(!patches[1].contains("dryRun"), "{}", patches[1]);
    let body = patch_body(&patches[1]);
    let container = &body["spec"]["ephemeralContainers"][0];
    assert_eq!(container["name"], "dbg");
    assert_eq!(container["targetContainerName"], "app");
    assert_eq!(
        container["image"],
        kubach_core::streams::debug::DEFAULT_IMAGE
    );
    assert_eq!(container["stdin"], true);
    // Nothing privileged, no host settings, no capabilities.
    assert!(container.get("securityContext").is_none(), "{container}");

    let requests = f.confirmer.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.tool, "debug_container");
    assert_eq!(request.kind, RequestKind::Call);
    assert!(
        request.summary.contains("distroless-0"),
        "{}",
        request.summary
    );
    assert!(request.summary.contains("dbg"), "{}", request.summary);
    assert!(
        request.details.contains("Target container: app"),
        "{}",
        request.details
    );
    assert!(
        request.dangerous[0]
            .message
            .contains("until the pod is deleted"),
        "{:?}",
        request.dangerous
    );
    assert!(request.dangerous.iter().any(|finding| {
        finding
            .message
            .contains("process namespace of container app")
    }));
    // Permanent: never "don't ask again", so the next call asks again.
    assert!(!request.allow_remember);
    f.server.call("agent", "debug_container", args(call)).await;
    assert_eq!(f.confirmer.requests().len(), 2);
}

#[tokio::test]
async fn debug_container_shows_the_command_and_the_agents_image() {
    let f = fixture(Profile::Debug, permissive(), false).await;
    let outcome = f
        .server
        .call(
            "agent",
            "debug_container",
            args(
                json!({"pod": "distroless-0", "image": "nicolaka/netshoot:v0.13",
                        "command": ["ss", "-tlnp"]}),
            ),
        )
        .await;
    assert!(outcome.is_error);
    let request = f.confirmer.requests().pop().unwrap();
    assert_eq!(
        request.kind,
        RequestKind::Exec {
            command: vec!["ss".into(), "-tlnp".into()]
        }
    );
    assert!(
        request.summary.contains("nicolaka/netshoot:v0.13"),
        "{}",
        request.summary
    );
    assert!(
        request.summary.contains("then run a command"),
        "{}",
        request.summary
    );
    assert!(
        request
            .dangerous
            .iter()
            .any(|finding| finding.message.contains("chosen by the agent"))
    );
    // Rejected: only the dry run reached the pod.
    let patches = ephemeral_patches(&f);
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert!(patches[0].contains("dryRun=All"));
}

#[tokio::test]
async fn debug_container_follows_restricted_pod_security() {
    let f = debug_fixture().await;
    let outcome = f
        .server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "restricted-0", "targetContainer": "app", "name": "dbg"})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("runs as user 472"),
        "{}",
        outcome.text
    );
    let patches = ephemeral_patches(&f);
    assert_eq!(patches.len(), 3, "{patches:?}");
    let body = patch_body(&patches[2]);
    let context = &body["spec"]["ephemeralContainers"][0]["securityContext"];
    assert_eq!(context["runAsNonRoot"], true);
    assert_eq!(context["runAsUser"], 472);
    assert_eq!(context["allowPrivilegeEscalation"], false);
    assert_eq!(context["capabilities"]["drop"], json!(["ALL"]));
    assert!(context["capabilities"].get("add").is_none());
    assert!(context.get("privileged").is_none());
    assert!(
        f.confirmer.requests()[0]
            .dangerous
            .iter()
            .any(|finding| finding.path.ends_with("securityContext"))
    );
}

#[tokio::test]
async fn debug_containers_that_cannot_work_are_not_asked_about() {
    let f = debug_fixture().await;
    let outcome = f
        .server
        .call("agent", "debug_container", args(json!({"pod": "locked-0"})))
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("Pod Security level"),
        "{}",
        outcome.text
    );
    assert!(f.confirmer.requests().is_empty());
    let patches = ephemeral_patches(&f);
    assert_eq!(patches.len(), 2);
    assert!(patches.iter().all(|p| p.contains("dryRun=All")));

    let outcome = f
        .server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "pending-0"})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("is Pending"), "{}", outcome.text);
    let outcome = f
        .server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "distroless-0", "targetContainer": "sidecar"})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("no container \"sidecar\""),
        "{}",
        outcome.text
    );
    let outcome = f
        .server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "distroless-0", "name": "app"})),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("already has"), "{}", outcome.text);
    assert!(f.confirmer.requests().is_empty());
    assert_eq!(ephemeral_patches(&f).len(), 2);
}

#[tokio::test]
async fn debug_container_needs_debug_access_rbac_and_valid_input() {
    // Beyond read-only access: the user is asked for the debug level
    // first, and without it no API request is made.
    let read_only = fixture(Profile::ReadOnly, permissive(), false).await;
    let outcome = read_only
        .server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "distroless-0"})),
        )
        .await;
    assert!(outcome.is_error);
    let asked = read_only.confirmer.requests();
    assert_eq!(asked.len(), 1);
    assert!(
        matches!(&asked[0].kind, RequestKind::Access { raise, .. } if raise.profile == Profile::Debug),
        "{:?}",
        asked[0].kind
    );
    assert!(read_only.api.requests().is_empty());

    let f = fixture(Profile::Debug, permissive(), true).await;
    for bad in [
        json!({"pod": "distroless-0", "image": "busybox; rm -rf /"}),
        json!({"pod": "distroless-0", "image": "-busybox"}),
        json!({"pod": "distroless-0", "name": "Not_A_Name"}),
        json!({"pod": "distroless-0", "targetContainer": "../app"}),
        json!({"pod": "distroless-0", "command": ["sh", "-c", "id\nrm -rf /"]}),
        json!({"pod": "distroless-0", "securityContext": {"privileged": true}}),
    ] {
        let outcome = f
            .server
            .call("agent", "debug_container", args(bad.clone()))
            .await;
        assert!(outcome.is_error, "{bad}");
    }
    assert!(f.api.requests().is_empty(), "{:?}", f.api.requests());

    // The user's RBAC: patching ephemeral containers, exec for a command.
    let api = FakeApi::start().await;
    let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
    let scope = scope_with(Profile::Debug, &permissive(), &[NS]);
    let without = |missing: Capability| {
        Capability::ALL
            .into_iter()
            .filter(|c| *c != missing)
            .collect::<HashSet<_>>()
    };
    let session = Session::from_parts(
        scope.clone(),
        Identity::ClientOnly {
            client: client.clone(),
        },
        catalog(),
        without(Capability::PatchEphemeralContainers),
    );
    session.set_grants(grants(&[Grant::read(NS)]));
    let server = McpServer::new(session, Arc::new(ScriptedConfirmer::approving()), None);
    let outcome = server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "distroless-0"})),
        )
        .await;
    assert!(
        outcome
            .text
            .contains("no right to patch pods/ephemeralcontainers"),
        "{}",
        outcome.text
    );
    let session = Session::from_parts(
        scope,
        Identity::ClientOnly { client },
        catalog(),
        without(Capability::PodExec),
    );
    session.set_grants(grants(&[Grant::read(NS)]));
    let server = McpServer::new(session, Arc::new(ScriptedConfirmer::approving()), None);
    let outcome = server
        .call(
            "agent",
            "debug_container",
            args(json!({"pod": "distroless-0", "command": ["ps"]})),
        )
        .await;
    assert!(
        outcome.text.contains("no right to create pods/exec"),
        "{}",
        outcome.text
    );
    assert!(api.writes().is_empty(), "{:?}", api.writes());
}

/// The token of the vault context in the tests of #113.
#[cfg(unix)]
const VAULT_TOKEN: &str = "vault-token-of-113";

/// Answers every question with a yes, except the one about the user's
/// credentials when `allow_user` is off, and notes what each window showed
/// and which temporary kubeconfigs were on disk meanwhile.
#[cfg(unix)]
struct VaultConfirmer {
    dir: std::path::PathBuf,
    allow_user: bool,
    /// Closed while the user is asked about their credentials: the vault
    /// locked.
    locks: Mutex<Option<Arc<Session>>>,
    helm: Mutex<Vec<(crate::helm_tools::HelmChange, String, usize)>>,
}

#[cfg(unix)]
impl crate::confirm::Confirmer for VaultConfirmer {
    fn confirm<'a>(
        &'a self,
        request: &'a crate::confirm::ConfirmationRequest,
    ) -> kubach_core::plugin::BoxFuture<'a, crate::confirm::ConfirmationDecision> {
        Box::pin(async move {
            use crate::confirm::ConfirmationDecision;
            use crate::helm_tools::HelmCredentials;
            let RequestKind::Helm { helm } = &request.kind else {
                return ConfirmationDecision::Approved;
            };
            let files = vault_files(&self.dir);
            self.helm
                .lock()
                .unwrap()
                .push(((**helm).clone(), request.details.clone(), files));
            if helm.credentials != HelmCredentials::AskUser {
                return ConfirmationDecision::Approved;
            }
            let locks = self.locks.lock().unwrap().take();
            if let Some(session) = locks {
                let _ = session.close().await;
            }
            if self.allow_user {
                ConfirmationDecision::Approved
            } else {
                ConfirmationDecision::Rejected { reason: None }
            }
        })
    }
}

#[cfg(unix)]
fn vault_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

/// A session under a temporary ServiceAccount on a vault context, with a
/// fake `helm` that notes how it was called and what its kubeconfig was.
#[cfg(unix)]
struct VaultHelm {
    _api: FakeApi,
    server: McpServer,
    confirmer: Arc<VaultConfirmer>,
    audit_path: std::path::PathBuf,
    /// Where the temporary kubeconfigs go.
    kubeconfigs: std::path::PathBuf,
    log: std::path::PathBuf,
    root: tempfile::TempDir,
}

#[cfg(unix)]
impl VaultHelm {
    /// `on_change`: shell run for a real (not dry) change.
    async fn start(allow_user: bool, on_change: &str) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let kubeconfigs = root.path().join("run").join("kubeconfig");
        let log = root.path().join("helm.log");
        let manifest = root.path().join("manifest.yaml");
        std::fs::write(&manifest, INFRA_MANIFEST).unwrap();
        let dry_run = root.path().join("dry-run.json");
        std::fs::write(&dry_run, infra_release(3).to_string()).unwrap();
        let helm = root.path().join("helm");
        std::fs::write(
            &helm,
            format!(
                r#"#!/bin/sh
kubeconfig=; context=; dry=; previous=; destination=
for argument in "$@"; do
  [ "$previous" = --kubeconfig ] && kubeconfig="$argument"
  [ "$previous" = --kube-context ] && context="$argument"
  [ "$previous" = --destination ] && destination="$argument"
  [ "$argument" = --dry-run=server ] && dry=dry
  previous="$argument"
done
if [ "$1" = pull ]; then
  [ -d '{chart}' ] || {{ echo 'Error: no such chart' >&2; exit 1; }}
  tar -czf "$destination/infra-1.0.0.tgz" -C '{chart}' infra
  exit 0
fi
if [ "$1" = template ] && [ -n "$dry" ]; then
  {{
    printf 'context=%s token=%s ' "$context" "${{HELM_KUBETOKEN:-none}}"
    printf 'credentials=%s vault-files=%s\n' "$(grep -c '{VAULT_TOKEN}' "$kubeconfig")" \
      "$(ls '{kubeconfigs}' 2>/dev/null | wc -l | tr -d ' ')"
  }} >> '{renders}'
  cat > /dev/null
  [ -f '{live}' ] || {{ echo 'Error: secrets "password=hunter2" is forbidden' >&2; exit 1; }}
  cat '{live}'
  exit 0
fi
if [ "$1" = template ]; then cat '{manifest}'; exit 0; fi
{{
  printf '%s %s context=%s token=%s ' "$1" "$dry" "$context" "${{HELM_KUBETOKEN:-none}}"
  printf 'file=%s ' "$(ls -l "$kubeconfig" | cut -c1-10)"
  printf 'dir=%s ' "$(ls -ld "$(dirname "$kubeconfig")" | cut -c1-10)"
  printf 'credentials=%s\n' "$(grep -c '{VAULT_TOKEN}' "$kubeconfig")"
}} >> '{log}'
case "$1" in install|upgrade) cat > /dev/null ;; esac
if [ -n "$dry" ]; then cat '{dry_run}'; exit 0; fi
{on_change}
echo 'STATUS: deployed'
"#,
                manifest = manifest.display(),
                log = log.display(),
                dry_run = dry_run.display(),
                chart = root.path().join("chart").display(),
                live = root.path().join("live.yaml").display(),
                renders = root.path().join("renders.log").display(),
                kubeconfigs = kubeconfigs.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helm, std::fs::Permissions::from_mode(0o755)).unwrap();

        let api = FakeApi::start().await;
        let client = Client::try_from(Config::new(api.url.parse().unwrap())).unwrap();
        let scope = scope_with(Profile::Deploy, &permissive(), &[NS]);
        let context = scope.context.clone();
        let identity = Identity::TemporaryServiceAccount(
            crate::identity::TemporaryServiceAccount::fake(client, NS),
        );
        let capabilities: HashSet<Capability> = Capability::ALL.into_iter().collect();
        let session = Session::from_parts(scope, identity, catalog(), capabilities);
        session.set_grants(grants(&[Grant::read(NS)]));
        let audit_path = root.path().join("audit.jsonl");
        let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
        let confirmer = Arc::new(VaultConfirmer {
            dir: kubeconfigs.clone(),
            allow_user,
            locks: Mutex::default(),
            helm: Mutex::default(),
        });
        let server = McpServer::new(session, confirmer.clone(), Some(audit));
        let config = kube::config::Kubeconfig::from_yaml(&format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: {context}\n\
             clusters: [{{name: c, cluster: {{server: 'https://vault.example.com'}}}}]\n\
             users: [{{name: u, user: {{token: {VAULT_TOKEN}}}}}]\n\
             contexts: [{{name: {context}, context: {{cluster: c, user: u}}}}]\n"
        ))
        .unwrap();
        server.set_helm(crate::helm_tools::HelmEnv {
            binary: Some(helm),
            kubeconfig: None,
            vault: Some(crate::helm_tools::VaultKubeconfig::new(
                config,
                kubeconfigs.clone(),
            )),
            paths: None,
        });
        Self {
            _api: api,
            server,
            confirmer,
            audit_path,
            kubeconfigs,
            log,
            root,
        }
    }

    /// What helm said about each call it got with a kubeconfig.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `(credentials, temporary kubeconfigs on disk)` of each Helm window.
    fn windows(&self) -> Vec<(crate::helm_tools::HelmCredentials, usize)> {
        self.confirmer
            .helm
            .lock()
            .unwrap()
            .iter()
            .map(|(change, _, files)| (change.credentials, *files))
            .collect()
    }

    /// Neither the answer nor the audit log has the vault's credentials
    /// or says where they were, and nothing is left on disk.
    fn assert_clean(&self, answer: &str) {
        let audit = std::fs::read_to_string(&self.audit_path).unwrap_or_default();
        let root = self.root.path().display().to_string();
        for text in [answer, audit.as_str()] {
            assert!(!text.contains(VAULT_TOKEN), "credentials leaked");
            assert!(!text.contains(&root), "a path leaked");
        }
        assert_eq!(vault_files(&self.kubeconfigs), 0);
    }
}

#[cfg(unix)]
fn vault_call(kind: &str, dry: bool, context: &str) -> String {
    let dry = if dry { "dry" } else { "" };
    format!(
        "{kind} {dry} context={context} token=none file=-rw------- dir=drwx------ credentials=1"
    )
}

/// The flow of a kubeconfig context on a vault context (#113): asked with
/// the objects, a dry run as the user, the diff window, the install. The
/// credentials reach helm in an owner-only file only while it runs.
#[cfg(unix)]
#[tokio::test]
async fn helm_on_a_vault_context_runs_as_the_user_after_consent() {
    use crate::helm_tools::{HelmCredentials, USER_IDENTITY};
    let f = VaultHelm::start(true, "").await;
    let context = f.server.session().scope().context.clone();

    let install = f
        .server
        .call(
            "agent",
            "helm_install",
            args(
                json!({"release": "infra", "chart": "oci://ghcr.io/example/infra",
                        "namespace": NS}),
            ),
        )
        .await;
    assert!(!install.is_error, "{}", install.text);
    assert_eq!(
        f.calls(),
        [
            vault_call("install", true, &context),
            vault_call("install", false, &context),
        ]
    );
    // No file while the user decides, in either window.
    assert_eq!(
        f.windows(),
        [(HelmCredentials::AskUser, 0), (HelmCredentials::User, 0)]
    );
    {
        let windows = f.confirmer.helm.lock().unwrap();
        let (asked, details, _) = &windows[0];
        assert_eq!(asked.identity, USER_IDENTITY);
        assert_eq!(asked.user_objects, ["ClusterRole infra-reader"]);
        assert!(details.contains("temporary file"), "{details}");
        let (diff, _, _) = &windows[1];
        assert_eq!(diff.identity, USER_IDENTITY);
        assert!(
            diff.cluster_objects
                .iter()
                .any(|object| object.contains("ClusterRole infra-reader")),
            "{:?}",
            diff.cluster_objects
        );
    }
    f.assert_clean(&install.text);
    let audit = audit_lines(&f.audit_path);
    let line = audit.last().unwrap();
    assert_eq!(line["change"]["ranAs"], "user", "{line}");

    // Upgrading goes the same way.
    let upgrade = f
        .server
        .call(
            "agent",
            "helm_upgrade",
            args(
                json!({"release": "infra", "chart": "oci://ghcr.io/example/infra",
                        "namespace": NS}),
            ),
        )
        .await;
    assert!(!upgrade.is_error, "{}", upgrade.text);
    assert_eq!(f.calls().len(), 4);
    assert_eq!(f.calls()[2], vault_call("upgrade", true, &context));
    assert_eq!(f.calls()[3], vault_call("upgrade", false, &context));
    assert_eq!(
        f.windows()[2..],
        [(HelmCredentials::AskUser, 0), (HelmCredentials::User, 0)]
    );
    f.assert_clean(&upgrade.text);

    // A rollback and an uninstall ask once and run once.
    for (tool, kind, arguments) in [
        (
            "helm_rollback",
            "rollback",
            json!({"release": "infra", "revision": 1, "namespace": NS}),
        ),
        (
            "helm_uninstall",
            "uninstall",
            json!({"release": "infra", "namespace": NS}),
        ),
    ] {
        let windows = f.windows().len();
        let outcome = f.server.call("agent", tool, args(arguments)).await;
        assert!(!outcome.is_error, "{tool}: {}", outcome.text);
        assert_eq!(
            f.calls().last().unwrap(),
            &vault_call(kind, false, &context)
        );
        assert_eq!(f.windows()[windows..], [(HelmCredentials::AskUser, 0)]);
        f.assert_clean(&outcome.text);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn helm_on_a_vault_context_writes_nothing_without_consent() {
    let f = VaultHelm::start(false, "").await;
    let refused = f
        .server
        .call(
            "agent",
            "helm_uninstall",
            args(json!({"release": "infra", "namespace": NS})),
        )
        .await;
    assert!(refused.is_error);
    assert!(refused.text.contains("did not allow"), "{}", refused.text);
    assert!(f.calls().is_empty(), "{:?}", f.calls());
    assert!(!f.kubeconfigs.exists());
    f.assert_clean(&refused.text);
}

#[cfg(unix)]
#[tokio::test]
async fn helm_on_a_vault_context_writes_nothing_once_the_vault_locks() {
    let f = VaultHelm::start(true, "").await;
    *f.confirmer.locks.lock().unwrap() = Some(f.server.session().clone());
    let ended = f
        .server
        .call(
            "agent",
            "helm_uninstall",
            args(json!({"release": "infra", "namespace": NS})),
        )
        .await;
    assert!(ended.is_error);
    assert!(f.calls().is_empty(), "{:?}", f.calls());
    assert!(!f.kubeconfigs.exists());
    f.assert_clean(&ended.text);
}

/// A failing helm: its error reaches the agent, the file does not stay.
#[cfg(unix)]
#[tokio::test]
async fn helm_failing_on_a_vault_context_leaves_no_file() {
    let f = VaultHelm::start(true, "echo 'Error: cannot delete' >&2; exit 1").await;
    let failed = f
        .server
        .call(
            "agent",
            "helm_uninstall",
            args(json!({"release": "infra", "namespace": NS})),
        )
        .await;
    assert!(failed.is_error);
    assert!(failed.text.contains("cannot delete"), "{}", failed.text);
    assert_eq!(f.calls().len(), 1);
    f.assert_clean(&failed.text);
}

/// The agent goes away while helm runs: dropping the call removes the file.
#[cfg(unix)]
#[tokio::test]
async fn a_cancelled_helm_call_on_a_vault_context_leaves_no_file() {
    let f = VaultHelm::start(true, "sleep 5").await;
    let mut call = Box::pin(f.server.call(
        "agent",
        "helm_uninstall",
        args(json!({"release": "infra", "namespace": NS})),
    ));
    while f.calls().is_empty() {
        tokio::select! {
            outcome = &mut call => panic!("helm ended early: {}", outcome.text),
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }
    assert_eq!(vault_files(&f.kubeconfigs), 1);
    drop(call);
    f.assert_clean("");
}

/// A session with neither a kubeconfig file nor the vault's credentials
/// refuses before asking anyone.
#[cfg(unix)]
#[tokio::test]
async fn helm_with_no_kubeconfig_at_all_is_refused_before_asking() {
    let f = VaultHelm::start(true, "").await;
    let binary = f.root.path().join("helm");
    f.server.set_helm(crate::helm_tools::HelmEnv {
        binary: Some(binary),
        ..crate::helm_tools::HelmEnv::default()
    });
    let refused = f
        .server
        .call(
            "agent",
            "helm_uninstall",
            args(json!({"release": "infra", "namespace": NS})),
        )
        .await;
    assert!(refused.is_error);
    assert!(refused.text.contains("ClusterRole"), "{}", refused.text);
    assert!(f.windows().is_empty());
    assert!(f.calls().is_empty(), "{:?}", f.calls());
}

/// `# Source:` comments as `helm template` writes them.
#[cfg(unix)]
const LOOKUP_RENDER: &str = "---\n# Source: infra/templates/secret.yaml\napiVersion: v1\n\
     kind: Secret\nmetadata:\n  name: infra-tls\ndata:\n  tls.key: Z2VuZXJhdGVk\n---\n\
     # Source: infra/templates/config.yaml\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  \
     name: infra\n---\n# Source: infra/templates/rbac.yaml\n\
     apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  \
     name: infra-reader\nrules: []\n";

#[cfg(unix)]
impl VaultHelm {
    /// The chart `helm pull` gives, and what `helm template` renders.
    fn lookup_chart(&self, secret_template: &str) {
        let templates = self.root.path().join("chart/infra/templates");
        std::fs::create_dir_all(&templates).unwrap();
        std::fs::write(
            self.root.path().join("chart/infra/Chart.yaml"),
            "name: infra\n",
        )
        .unwrap();
        std::fs::write(templates.join("secret.yaml"), secret_template).unwrap();
        std::fs::write(templates.join("config.yaml"), "kind: ConfigMap\n").unwrap();
        std::fs::write(self.root.path().join("manifest.yaml"), LOOKUP_RENDER).unwrap();
    }

    /// How helm was called for a render on the cluster.
    fn renders(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("renders.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn preview(&self, tool: &str) -> Value {
        let outcome = self
            .server
            .call(
                "agent",
                tool,
                args(
                    json!({"release": "infra", "chart": "oci://ghcr.io/example/infra",
                            "namespace": NS, "preview": true}),
                ),
            )
            .await;
        assert!(!outcome.is_error, "{tool}: {}", outcome.text);
        // Nobody asked, nothing of the user's on disk, helm changed nothing.
        assert!(self.windows().is_empty());
        assert!(self.calls().is_empty(), "{:?}", self.calls());
        assert!(!self.kubeconfigs.exists());
        self.assert_clean(&outcome.text);
        serde_json::from_str(&outcome.text).unwrap()
    }
}

#[cfg(unix)]
const USES_LOOKUP: &str = r#"{{ $old := lookup "v1" "Secret" "ns" "infra-tls" }}"#;

/// A chart with `lookup` that needs the user's credentials (#114): the
/// preview renders under the ServiceAccount's token, with the cluster-only
/// kubeconfig and before anyone is asked. What it read stays masked.
#[cfg(unix)]
#[tokio::test]
async fn a_lookup_preview_renders_under_the_service_account() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart(USES_LOOKUP);
    let live = LOOKUP_RENDER.replace("Z2VuZXJhdGVk", "cmVhbC1rZXktZnJvbS1jbHVzdGVy");
    std::fs::write(f.root.path().join("live.yaml"), live).unwrap();
    for tool in ["helm_install", "helm_upgrade"] {
        let preview = f.preview(tool).await;
        assert_eq!(preview["lookup"]["status"], "read", "{preview}");
        assert_eq!(preview["lookup"]["wholePreview"], false);
        assert!(
            preview["note"].as_str().unwrap().contains("ServiceAccount"),
            "{preview}"
        );
        let text = preview.to_string();
        assert!(!text.contains("cmVhbC1rZXktZnJvbS1jbHVzdGVy"), "{text}");
        assert!(!text.contains("real-key-from-cluster"), "{text}");
        assert!(
            preview["summary"].to_string().contains("Secret infra-tls"),
            "{preview}"
        );
        assert!(
            text.contains("ClusterRole infra-reader"),
            "the render's findings: {text}"
        );
    }
    assert_eq!(
        f.renders(),
        vec!["context=kubach token=fake-sa-token credentials=0 vault-files=0"; 2]
    );
}

/// The ServiceAccount cannot render it: the objects of the templates with
/// `lookup` are marked, and helm's error stays out of the answer.
#[cfg(unix)]
#[tokio::test]
async fn a_lookup_preview_marks_the_objects_when_the_render_fails() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart(USES_LOOKUP);
    let preview = f.preview("helm_upgrade").await;
    assert_eq!(preview["lookup"]["status"], "unavailable", "{preview}");
    assert_eq!(preview["lookup"]["objects"], json!(["Secret infra-tls"]));
    assert_eq!(preview["lookup"]["wholePreview"], false);
    assert_eq!(
        preview["summary"]["added"],
        json!([{"object": "Secret infra-tls", "lookupAffected": true}]),
        "{preview}"
    );
    assert!(
        preview["lookup"]["note"]
            .as_str()
            .unwrap()
            .contains("may be false"),
        "{preview}"
    );
    let text = preview.to_string();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("forbidden"), "{text}");
    assert!(!text.contains("Z2VuZXJhdGVk"), "{text}");
    assert_eq!(f.renders().len(), 1);
}

/// A computed include: which objects depend on `lookup` cannot be told.
#[cfg(unix)]
#[tokio::test]
async fn a_lookup_preview_marks_everything_when_the_mapping_is_unreliable() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart(&format!(
        r#"{USES_LOOKUP}{{{{ include (print $.Template.BasePath "/x.yaml") . }}}}"#
    ));
    let preview = f.preview("helm_install").await;
    assert_eq!(preview["lookup"]["status"], "unavailable", "{preview}");
    assert_eq!(preview["lookup"]["wholePreview"], true);
    assert!(
        preview["summary"]["added"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["lookupAffected"] == true),
        "{preview}"
    );
    assert!(
        preview["lookup"]["note"]
            .as_str()
            .unwrap()
            .contains("any change"),
        "{preview}"
    );
}

/// The chart's templates cannot be read: the preview says it does not know.
#[cfg(unix)]
#[tokio::test]
async fn a_lookup_preview_says_when_the_templates_are_unknown() {
    let f = VaultHelm::start(true, "").await;
    let preview = f.preview("helm_install").await;
    assert_eq!(preview["lookup"]["status"], "unknown", "{preview}");
    assert_eq!(preview["lookup"]["wholePreview"], true);
    assert!(f.renders().is_empty());
}

/// A render of a large chart: a changed ConfigMap, a Secret and a Deployment,
/// and enough objects that the diff takes pages (#115).
#[cfg(unix)]
fn large_render(mark: &str) -> String {
    let mut render = format!(
        "---\napiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  \
         name: infra-reader\nrules: []\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  \
         name: infra\ndata:\n  level: debug\n  DB_PASSWORD: hunter2-in-a-config-map\n---\n\
         apiVersion: v1\nkind: Secret\nmetadata:\n  name: infra-tls\ndata:\n  \
         tls.key: Z2VuZXJhdGVk\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  \
         name: web\nspec:\n  template:\n    spec:\n      containers:\n      - name: app\n        \
         image: web:{mark}\n"
    );
    for n in 0..40 {
        let filler = format!("{mark}-{n} ").repeat(150);
        render.push_str(&format!(
            "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: filler-{n}\ndata:\n  \
             text: {filler}\n"
        ));
    }
    render
}

#[cfg(unix)]
impl VaultHelm {
    async fn preview_diff(&self, query: Value) -> Result<Value, String> {
        let outcome = self
            .server
            .call("agent", "helm_preview_diff", args(query))
            .await;
        self.assert_clean(&outcome.text);
        if outcome.is_error {
            return Err(outcome.text);
        }
        Ok(serde_json::from_str(&outcome.text).unwrap())
    }
}

/// The default answer is a summary, the full diff comes by object and by
/// page, and a render that changes meanwhile does not reach the pages.
#[cfg(unix)]
#[tokio::test]
async fn a_preview_is_a_summary_and_its_pages_are_of_one_render() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart("kind: ConfigMap\n");
    let manifest = f.root.path().join("manifest.yaml");
    std::fs::write(&manifest, large_render("one")).unwrap();

    let preview = f.preview("helm_upgrade").await;
    let text = preview.to_string();
    assert!(preview.get("diff").is_none(), "{text}");
    assert!(text.len() < 16 * 1024, "{} bytes", text.len());
    let summary = &preview["summary"];
    assert_eq!(summary["counts"]["added"], 42, "{summary}");
    assert_eq!(summary["counts"]["changed"], 1, "{summary}");
    assert_eq!(summary["counts"]["unchanged"], 1, "{summary}");
    assert_eq!(
        summary["images"][0],
        json!({"object": "Deployment web", "new": "web:one",
               "path": "spec.template.spec.containers[app].image"})
    );
    assert_eq!(
        summary["configMapKeys"],
        json!([
            {"object": "ConfigMap infra", "key": "DB_PASSWORD", "change": "added",
             "new": "*** (23 bytes)"},
            {"object": "ConfigMap infra", "key": "level", "change": "added", "new": "debug"},
        ])
    );
    assert!(!text.contains("hunter2"), "{text}");

    let id = preview["fullDiff"]["previewId"]
        .as_str()
        .unwrap()
        .to_owned();
    let pages = preview["fullDiff"]["pages"].as_u64().unwrap();
    assert!(pages >= 2, "{preview}");

    // The chart renders differently from now on; the kept pages do not.
    let first = f.preview_diff(json!({"previewId": id})).await.unwrap();
    assert_eq!(first["pages"], pages);
    assert_eq!(first["nextPage"], 2);
    std::fs::write(&manifest, large_render("two")).unwrap();
    let mut whole = first["diff"].as_str().unwrap().to_owned();
    for page in 2..=pages {
        let next = f
            .preview_diff(json!({"previewId": id, "page": page}))
            .await
            .unwrap();
        assert_eq!(next["pages"], pages);
        whole.push_str(next["diff"].as_str().unwrap());
    }
    assert_eq!(
        whole.len() as u64,
        preview["fullDiff"]["bytes"].as_u64().unwrap()
    );
    assert!(whole.contains("image: web:one"), "{whole}");
    assert!(
        !whole.contains("web:two") && !whole.contains("two-0"),
        "another render"
    );
    assert!(!whole.contains("hunter2") && !whole.contains("Z2VuZXJhdGVk"));
    assert!(whole.contains("*** (9 bytes)"), "{whole}");
    assert!(f.renders().is_empty());
    assert!(f.calls().is_empty(), "{:?}", f.calls());

    // A filter takes the objects, and the same text.
    let one = f
        .preview_diff(json!({"previewId": id, "kind": "deployment", "name": "we"}))
        .await
        .unwrap();
    assert_eq!(one["matchedObjects"], 1);
    assert_eq!(one["pages"], 1);
    let diff = one["diff"].as_str().unwrap();
    assert!(diff.starts_with("=== added Deployment web\n"), "{diff}");
    assert!(
        diff.contains("image: web:one") && !diff.contains("filler"),
        "{diff}"
    );
    assert!(one.get("nextPage").is_none());
    let none = f
        .preview_diff(json!({"previewId": id, "kind": "Ingress"}))
        .await
        .unwrap();
    assert_eq!(none["matchedObjects"], 0);
    assert!(none["diff"].as_str().unwrap().contains("no change matches"));

    let beyond = f
        .preview_diff(json!({"previewId": id, "page": pages + 1}))
        .await
        .unwrap_err();
    assert!(beyond.contains("does not exist"), "{beyond}");

    // A new preview is a new render, under another id.
    let again = f.preview("helm_upgrade").await;
    assert_ne!(again["fullDiff"]["previewId"], json!(id));
    assert_eq!(again["summary"]["images"][0]["new"], "web:two", "{again}");
}

/// A preview that fell out of the session says so; nothing renders anew.
#[cfg(unix)]
#[tokio::test]
async fn a_preview_no_longer_kept_says_so() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart("kind: ConfigMap\n");
    let first = f.preview("helm_install").await;
    let id = first["fullDiff"]["previewId"].clone();
    for _ in 0..crate::helm_preview::MAX_KEPT_PREVIEWS {
        f.preview("helm_install").await;
    }
    let gone = f.preview_diff(json!({"previewId": id})).await.unwrap_err();
    assert!(gone.contains("is not kept"), "{gone}");
    assert!(gone.contains("new render"), "{gone}");
    assert!(f.renders().is_empty());
}

/// The kept diff ends with the session.
#[cfg(unix)]
#[tokio::test]
async fn a_closed_session_keeps_no_preview() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart("kind: ConfigMap\n");
    let preview = f.preview("helm_install").await;
    let id = preview["fullDiff"]["previewId"].as_str().unwrap();
    f.server.session().close().await.unwrap();
    let text = f
        .server
        .call("agent", "helm_preview_diff", args(json!({"previewId": id})))
        .await;
    assert!(text.is_error, "{}", text.text);
}

/// The summary is for the agent: the window keeps the whole diff, however
/// the summary would cut it (#115).
#[cfg(unix)]
#[tokio::test]
async fn the_confirmation_window_keeps_the_full_diff() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart("kind: ConfigMap\n");
    let mut release = infra_release(3);
    release["manifest"] = json!(large_render("one"));
    std::fs::write(f.root.path().join("dry-run.json"), release.to_string()).unwrap();
    let outcome = f
        .server
        .call(
            "agent",
            "helm_upgrade",
            args(json!({"release": "infra", "namespace": NS})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let windows = f.confirmer.helm.lock().unwrap();
    let (_, details, _) = windows.last().unwrap();
    assert!(details.starts_with("--- revision"), "{details}");
    assert!(details.contains("after the upgrade"), "{details}");
    for n in 0..40 {
        assert!(details.contains(&format!("filler-{n}\n")), "filler-{n}");
    }
    assert!(details.contains("image: web:one"), "{details}");
    assert!(!details.contains("previewId"), "{details}");
    assert!(!details.contains("hunter2"), "{details}");
}

/// A chart without `lookup`: the preview is as before, and the cluster is
/// not contacted for it.
#[cfg(unix)]
#[tokio::test]
async fn a_preview_without_lookup_is_not_marked() {
    let f = VaultHelm::start(true, "").await;
    f.lookup_chart("kind: Secret\n# lookup is only a word here\n");
    let preview = f.preview("helm_install").await;
    assert!(preview.get("lookup").is_none(), "{preview}");
    assert!(f.renders().is_empty());
}
