//! Custom resources under a real session (#103): a fake API server that
//! authorizes like the real one. The temporary ServiceAccount may do what
//! its Role says and nothing else, and a Role with rights its author lacks
//! is refused.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kube::Config;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::McpServer;
use crate::access::Access;
use crate::confirm::{RequestKind, ScriptedConfirmer};
use crate::scope::SessionSpec;
use crate::session::Session;
use crate::settings::{ImpersonationTarget, Profile, SecurityPolicy};

const NS: &str = "longhorn-system";
const GROUP: &str = "longhorn.io";
const TOKEN: &str = "Bearer sa-token";
const VOLUMES: &str = "/apis/longhorn.io/v1beta2/namespaces/longhorn-system/volumes";

/// What the fake cluster knows.
#[derive(Default)]
struct Cluster {
    /// The rules of the session's Role.
    role: Vec<Value>,
    /// Every Role patch, to count them.
    patches: usize,
    /// Reads of volumes that reached the API.
    volume_reads: usize,
    /// Whether the user's account may touch `longhorn.io`.
    user_reads_volumes: bool,
}

impl Cluster {
    fn role_allows(&self, group: &str, resource: &str, verb: &str) -> bool {
        let names = |rule: &Value, key: &str, value: &str| {
            rule[key]
                .as_array()
                .is_some_and(|list| list.iter().any(|item| item == value))
        };
        self.role.iter().any(|rule| {
            names(rule, "apiGroups", group)
                && names(rule, "resources", resource)
                && names(rule, "verbs", verb)
        })
    }

    fn role_names_volumes(rules: &[Value]) -> bool {
        rules.iter().any(|rule| rule["apiGroups"][0] == GROUP)
    }

    /// The whole API: `(status, body)` of one request.
    fn answer(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> (&'static str, Value) {
        let sa = token == Some(TOKEN);
        let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let path = path.split('?').next().unwrap_or(path);
        let rbac = format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{NS}");
        let accounts = format!("/api/v1/namespaces/{NS}/serviceaccounts");
        match (method, path) {
            ("GET", "/api") => ("200 OK", json!({"kind": "APIVersions", "versions": ["v1"]})),
            ("GET", "/apis") => (
                "200 OK",
                json!({"kind": "APIGroupList", "apiVersion": "v1", "groups": [{
                    "name": GROUP,
                    "versions": [{"groupVersion": "longhorn.io/v1beta2", "version": "v1beta2"}],
                    "preferredVersion": {"groupVersion": "longhorn.io/v1beta2", "version": "v1beta2"},
                }]}),
            ),
            ("GET", "/api/v1") => (
                "200 OK",
                json!({"kind": "APIResourceList", "groupVersion": "v1", "resources": [
                    {"name": "pods", "singularName": "pod", "namespaced": true, "kind": "Pod",
                     "verbs": ["get", "list"]},
                ]}),
            ),
            ("GET", "/apis/longhorn.io/v1beta2") => (
                "200 OK",
                json!({"kind": "APIResourceList", "groupVersion": "longhorn.io/v1beta2",
                    "resources": [
                    {"name": "volumes", "singularName": "volume", "namespaced": true,
                     "kind": "Volume", "verbs": ["get", "list"]},
                ]}),
            ),
            ("POST", "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews") => {
                let attributes = &request["spec"]["resourceAttributes"];
                let group = attributes["group"].as_str().unwrap_or("");
                let resource = attributes["resource"].as_str().unwrap_or("");
                let verb = attributes["verb"].as_str().unwrap_or("");
                let allowed = if sa {
                    self.role_allows(group, resource, verb)
                } else {
                    group != GROUP || self.user_reads_volumes
                };
                let mut review = request.clone();
                review["status"] = json!({"allowed": allowed});
                ("201 Created", review)
            }
            ("GET", VOLUMES) => {
                self.volume_reads += 1;
                let allowed = if sa {
                    self.role_allows(GROUP, "volumes", "list")
                } else {
                    self.user_reads_volumes
                };
                if allowed {
                    ("200 OK", volume_list())
                } else {
                    ("403 Forbidden", forbidden())
                }
            }
            ("POST", p) if p == accounts => {
                let mut account = request.clone();
                account["metadata"]["uid"] = json!("sa-uid");
                ("201 Created", account)
            }
            ("POST", p) if p == format!("{rbac}/roles") => {
                self.role = request["rules"].as_array().cloned().unwrap_or_default();
                ("201 Created", request)
            }
            ("POST", p) if p == format!("{rbac}/rolebindings") => ("201 Created", request),
            ("POST", p) if p.starts_with(&accounts) && p.ends_with("/token") => (
                "201 Created",
                json!({"apiVersion": "authentication.k8s.io/v1", "kind": "TokenRequest",
                       "metadata": {}, "spec": {"audiences": []},
                       "status": {"token": "sa-token",
                                  "expirationTimestamp": "2099-01-01T00:00:00Z"}}),
            ),
            ("PATCH", p) if p.starts_with(&format!("{rbac}/roles/")) => {
                let rules = request["rules"].as_array().cloned().unwrap_or_default();
                if Self::role_names_volumes(&rules) && !self.user_reads_volumes {
                    return ("403 Forbidden", forbidden());
                }
                self.role = rules;
                self.patches += 1;
                (
                    "200 OK",
                    json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
                           "metadata": {"name": "role", "namespace": NS},
                           "rules": self.role}),
                )
            }
            _ => (
                "404 Not Found",
                json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                       "message": "not found", "reason": "NotFound", "code": 404}),
            ),
        }
    }
}

fn volume_list() -> Value {
    json!({"apiVersion": "longhorn.io/v1beta2", "kind": "VolumeList", "metadata": {},
           "items": [{"apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
                      "metadata": {"name": "pvc-data", "namespace": NS}}]})
}

/// What the API server answered in the report of #103.
fn forbidden() -> Value {
    json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "reason": "Forbidden",
           "code": 403,
           "message": "volumes.longhorn.io is forbidden: User \"system:serviceaccount:\
                       longhorn-system:kubach-mcp-1\" cannot list resource \"volumes\" in API \
                       group \"longhorn.io\" in the namespace \"longhorn-system\""})
}

async fn start_cluster(user_reads_volumes: bool) -> (String, Arc<Mutex<Cluster>>) {
    let cluster = Arc::new(Mutex::new(Cluster {
        user_reads_volumes,
        ..Cluster::default()
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let shared = cluster.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let cluster = shared.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let mut length = 0usize;
                    let mut token = None;
                    let mut aggregated = false;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if header == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse().unwrap_or(0);
                            } else if name.eq_ignore_ascii_case("authorization") {
                                token = Some(value.trim().to_owned());
                            } else if name.eq_ignore_ascii_case("accept") {
                                aggregated = value.contains("apidiscovery");
                            }
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body).await;
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let path = parts.next().unwrap_or("");
                    // Without aggregated discovery the client falls back to
                    // the plain documents.
                    let path = if aggregated { "/aggregated" } else { path };
                    let (status, answer) = cluster.lock().unwrap().answer(
                        method,
                        path,
                        token.as_deref(),
                        &String::from_utf8_lossy(&body),
                    );
                    let answer = answer.to_string();
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\n\r\n{answer}",
                        answer.len()
                    );
                    if write.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, cluster)
}

/// The session of the report: reading without asking, no custom resources.
async fn server_on(
    url: &str,
    policy: &SecurityPolicy,
    confirmer: Arc<ScriptedConfirmer>,
) -> McpServer {
    assert_eq!(policy.minimum, Access::NONE);
    let spec = SessionSpec {
        context: "minikube".into(),
        server_url: url.to_owned(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let base = Config::new(url.parse().unwrap());
    let session = Session::start(&base, &spec, policy).await.unwrap();
    McpServer::new(session, confirmer, None)
}

fn list_volumes() -> Map<String, Value> {
    match json!({"kind": "Volume", "apiVersion": "longhorn.io/v1beta2", "namespace": NS}) {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

fn access_questions(confirmer: &ScriptedConfirmer) -> Vec<Access> {
    confirmer
        .requests()
        .iter()
        .filter_map(|request| match &request.kind {
            RequestKind::Access { raise, .. } => Some(*raise),
            _ => None,
        })
        .collect()
}

const CRDS: Access = Access {
    crds: true,
    ..Access::NONE
};

#[tokio::test]
async fn the_first_read_of_a_custom_resource_asks_and_then_returns_the_data() {
    let (url, cluster) = start_cluster(true).await;
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = server_on(&url, &SecurityPolicy::default(), confirmer.clone()).await;

    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("pvc-data"), "{}", outcome.text);
    assert_eq!(access_questions(&confirmer), [CRDS]);
    {
        let cluster = cluster.lock().unwrap();
        // Exactly what reading uses, of this very kind.
        assert!(cluster.role_allows(GROUP, "volumes", "get"));
        assert!(cluster.role_allows(GROUP, "volumes", "list"));
        assert!(!cluster.role_allows(GROUP, "volumes", "watch"));
        assert!(!cluster.role_allows(GROUP, "volumes", "delete"));
        assert!(!cluster.role_allows(GROUP, "*", "list"));
        assert!(!cluster.role_allows("", "secrets", "list"));
    }

    // Nothing more to ask or to patch the next time.
    let patches = cluster.lock().unwrap().patches;
    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(access_questions(&confirmer), [CRDS]);
    assert_eq!(cluster.lock().unwrap().patches, patches);
}

#[tokio::test]
async fn refusing_custom_resources_gives_the_agent_the_reason() {
    let (url, cluster) = start_cluster(true).await;
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = server_on(&url, &SecurityPolicy::default(), confirmer.clone()).await;

    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("did not allow"), "{}", outcome.text);
    assert!(
        outcome.text.contains("custom resources"),
        "{}",
        outcome.text
    );

    // The answer stands: no second question, no raw 403.
    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("not asked again"), "{}", outcome.text);
    assert_eq!(access_questions(&confirmer), [CRDS]);
    assert_eq!(cluster.lock().unwrap().volume_reads, 0);
}

#[tokio::test]
async fn a_user_without_the_right_is_told_so_instead_of_a_raw_403() {
    let (url, cluster) = start_cluster(false).await;
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = server_on(&url, &SecurityPolicy::default(), confirmer.clone()).await;

    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(outcome.is_error);
    assert!(
        outcome
            .text
            .contains("your Kubernetes account has no right"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("volumes.longhorn.io"),
        "{}",
        outcome.text
    );
    assert!(!outcome.text.contains("API error"), "{}", outcome.text);
    assert_eq!(cluster.lock().unwrap().volume_reads, 0);
}

#[tokio::test]
async fn an_impersonated_identity_without_the_right_is_explained() {
    let (url, cluster) = start_cluster(false).await;
    let policy = SecurityPolicy {
        impersonation: Some(ImpersonationTarget {
            user: "kubach:mcp".into(),
            groups: Vec::new(),
        }),
        temporary_objects: false,
        ..SecurityPolicy::default()
    };
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = server_on(&url, &policy, confirmer).await;

    let outcome = server.call("agent", "list_resources", list_volumes()).await;
    assert!(outcome.is_error);
    assert!(
        outcome.text.contains("only the cluster administrator"),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("kubach:mcp"), "{}", outcome.text);
    assert!(!outcome.text.contains("API error"), "{}", outcome.text);
    // Here the API server decides: the user's own rights say nothing.
    assert_eq!(cluster.lock().unwrap().volume_reads, 1);
}
