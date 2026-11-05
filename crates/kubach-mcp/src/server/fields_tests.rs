//! `fields` under a real server loop (#110). The fake API server answers as
//! a real one does: items of a list have no `kind`, one object has it.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::McpServer;
use super::all_namespaces_tests::{args, session};
use crate::confirm::{Confirmer, ScriptedConfirmer};

const TEAMS: [&str; 2] = ["team-a", "team-b"];

fn widget(namespace: &str) -> Value {
    json!({
        "metadata": {"name": format!("w-{namespace}"), "namespace": namespace},
        "spec": {
            "password": "hunter2-crd",
            "auth": {"clientSecret": "hunter2-nested"},
            "size": 3,
        },
    })
}

fn config_map(namespace: &str) -> Value {
    json!({
        "metadata": {"name": format!("c-{namespace}"), "namespace": namespace},
        "data": {"DB_PASS": "hunter2-map", "DB_HOST": "db.local"},
    })
}

fn deployment(namespace: &str, name: &str, strategy: Option<&str>) -> Value {
    let mut object = json!({
        "metadata": {"name": name, "namespace": namespace, "uid": "uid-of-it"},
        "spec": {"template": {"spec": {"containers": [{
            "name": "app",
            "image": "nginx:1",
            "env": [{"name": "API_TOKEN", "value": "hunter2-env"}],
        }]}}},
    });
    if let Some(strategy) = strategy {
        object["spec"]["strategy"] = json!({"type": strategy});
    }
    object
}

fn list(kind: &str, items: Vec<Value>) -> Value {
    json!({"apiVersion": "v1", "kind": format!("{kind}List"), "metadata": {}, "items": items})
}

fn with_kind(mut object: Value, api_version: &str, kind: &str) -> Value {
    object["apiVersion"] = json!(api_version);
    object["kind"] = json!(kind);
    object
}

fn answer(path: &str) -> Value {
    let path = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["api", "v1", "namespaces", ns, "configmaps"] => list("ConfigMap", vec![config_map(ns)]),
        ["api", "v1", "namespaces", ns, "configmaps", _] => {
            with_kind(config_map(ns), "v1", "ConfigMap")
        }
        ["apis", "example.com", "v1", "namespaces", ns, "widgets"] => {
            list("Widget", vec![widget(ns)])
        }
        ["apis", "example.com", "v1", "namespaces", ns, "widgets", _] => {
            with_kind(widget(ns), "example.com/v1", "Widget")
        }
        ["apis", "apps", "v1", "namespaces", ns, "deployments"] => list(
            "Deployment",
            vec![
                deployment(ns, "web", Some("Recreate")),
                deployment(ns, "bare", None),
            ],
        ),
        ["apis", "apps", "v1", "namespaces", ns, "deployments", name] => with_kind(
            deployment(ns, name, Some("Recreate")),
            "apps/v1",
            "Deployment",
        ),
        _ => json!({"apiVersion": "v1", "kind": "List", "metadata": {}, "items": []}),
    }
}

async fn serve() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
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
                let path = line.split_whitespace().nth(1).unwrap_or_default();
                log.lock().unwrap().push(path.to_owned());
                let body = answer(path).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = write.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, seen)
}

async fn server() -> (McpServer, Arc<Mutex<Vec<String>>>) {
    let (url, seen) = serve().await;
    let session = session(&url, &TEAMS, &TEAMS, &[]);
    let confirmer: Arc<dyn Confirmer> = Arc::new(ScriptedConfirmer::denying());
    (McpServer::new(session, confirmer, None), seen)
}

/// The text of a call that has to succeed.
async fn read(server: &McpServer, tool: &str, call: Value) -> String {
    let outcome = server.call("agent", tool, args(call)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    outcome.text
}

#[tokio::test]
async fn one_field_of_many_objects_comes_without_the_rest() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "Deployment", "namespace": "team-a", "fields": ["spec.strategy.type"]}),
    )
    .await;
    assert!(text.contains("type: Recreate"), "{text}");
    assert!(text.contains("name: web"), "{text}");
    assert!(text.contains("name: bare"), "{text}");
    assert_eq!(text.matches("namespace: team-a").count(), 2, "{text}");
    assert!(text.contains("(2 deployments in team-a)"), "{text}");
    for left_out in ["nginx", "uid-of-it", "NAME\t", "containers"] {
        assert!(!text.contains(left_out), "{left_out} in {text}");
    }
}

#[tokio::test]
async fn an_object_without_the_field_says_so_and_the_others_still_answer() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "Deployment", "namespace": "team-a", "fields": ["spec.strategy.type"]}),
    )
    .await;
    let bare = text.split("---\n").find(|d| d.contains("name: bare"));
    let web = text.split("---\n").find(|d| d.contains("name: web"));
    let (bare, web) = (bare.unwrap(), web.unwrap());
    assert!(bare.contains("fieldsNotFound"), "{bare}");
    assert!(bare.contains("- spec.strategy.type"), "{bare}");
    assert!(!web.contains("fieldsNotFound"), "{web}");
}

#[tokio::test]
async fn all_namespaces_gives_documents_under_the_coverage_note() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "Deployment", "allNamespaces": true, "fields": ["spec.strategy.type"]}),
    )
    .await;
    assert!(text.starts_with("Coverage: complete"), "{text}");
    assert_eq!(text.matches("namespace: team-a").count(), 2, "{text}");
    assert_eq!(text.matches("namespace: team-b").count(), 2, "{text}");
    assert!(text.contains("(4 deployments in 2 namespaces)"), "{text}");
    assert!(!text.contains("NAMESPACE\t"), "{text}");
}

#[tokio::test]
async fn get_resource_gives_the_fields_of_one_object() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "get_resource",
        json!({"kind": "Deployment", "name": "web", "namespace": "team-a",
               "fields": ["spec.strategy.type", "spec.nothing"]}),
    )
    .await;
    assert!(text.contains("type: Recreate"), "{text}");
    assert!(text.contains("name: web"), "{text}");
    assert!(text.contains("namespace: team-a"), "{text}");
    assert!(text.contains("- spec.nothing"), "{text}");
    assert!(!text.contains("nginx"), "{text}");
}

/// Every way to ask for a value the whole answer masks: by a path to it, to
/// what holds it, and in each tool.
#[tokio::test]
async fn no_selection_shows_what_the_whole_answer_masks() {
    let (server, _) = server().await;
    let cases = [
        (
            "Widget",
            "w-team-a",
            vec![
                "spec.password",
                "spec.auth.clientSecret",
                "spec.auth",
                "spec",
            ],
        ),
        (
            "ConfigMap",
            "c-team-a",
            vec!["data.DB_PASS", "data['DB_PASS']", "data"],
        ),
        (
            "Deployment",
            "web",
            vec![
                "spec.template.spec.containers[*].env[*].value",
                "spec.template.spec.containers[0].env",
                "spec.template",
            ],
        ),
    ];
    for (kind, name, paths) in cases {
        let version = (kind == "Widget").then_some("example.com/v1");
        let whole =
            json!({"kind": kind, "apiVersion": version, "name": name, "namespace": "team-a"});
        let whole = read(&server, "get_resource", whole).await;
        assert!(!whole.contains("hunter2"), "{whole}");
        for path in paths {
            let fields = json!([path]);
            for (tool, call) in [
                (
                    "get_resource",
                    json!({"kind": kind, "apiVersion": version, "name": name,
                           "namespace": "team-a", "fields": fields}),
                ),
                (
                    "list_resources",
                    json!({"kind": kind, "apiVersion": version, "namespace": "team-a",
                           "fields": fields}),
                ),
                (
                    "list_resources",
                    json!({"kind": kind, "apiVersion": version, "allNamespaces": true,
                           "fields": fields}),
                ),
            ] {
                let text = read(&server, tool, call).await;
                assert!(!text.contains("hunter2"), "{tool} {kind} {path}: {text}");
                assert!(text.contains("***"), "{tool} {kind} {path}: {text}");
                assert!(!text.contains("fieldsNotFound"), "{tool} {path}: {text}");
            }
        }
    }
}

#[tokio::test]
async fn a_selected_value_outside_masking_is_shown() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "ConfigMap", "namespace": "team-a",
               "fields": ["data.DB_HOST", "data.DB_PASS"]}),
    )
    .await;
    assert!(text.contains("DB_HOST: db.local"), "{text}");
    assert!(
        text.contains("DB_PASS: '*** (11 bytes)'") || text.contains("DB_PASS: \"*** (11 bytes)\""),
        "{text}"
    );
}

#[tokio::test]
async fn a_malformed_path_is_refused_before_the_api() {
    let (server, seen) = server().await;
    for (tool, call) in [
        (
            "list_resources",
            json!({"kind": "Pod", "namespace": "team-a", "fields": ["spec..type"]}),
        ),
        (
            "list_resources",
            json!({"kind": "Pod", "allNamespaces": true, "fields": ["{.spec}"]}),
        ),
        (
            "get_resource",
            json!({"kind": "Pod", "name": "a", "namespace": "team-a", "fields": ["a[x]"]}),
        ),
        (
            "get_resource",
            json!({"kind": "Pod", "name": "a", "namespace": "team-a", "fields": []}),
        ),
    ] {
        let outcome = server.call("agent", tool, args(call)).await;
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(
            outcome.text.contains("spec.strategy.type"),
            "{}",
            outcome.text
        );
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn describe_resource_takes_no_fields() {
    let (server, seen) = server().await;
    let call = json!({"kind": "Pod", "name": "a", "namespace": "team-a", "fields": ["spec"]});
    let outcome = server.call("agent", "describe_resource", args(call)).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("fields"), "{}", outcome.text);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_table_stays_without_fields() {
    let (server, _) = server().await;
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "ConfigMap", "namespace": "team-a"}),
    )
    .await;
    assert!(text.starts_with("NAME\t"), "{text}");
    assert!(!text.contains("---"), "{text}");
}

#[test]
fn the_schemas_offer_fields() {
    let specs = super::catalog();
    let schema = |name: &str| {
        let spec = specs.iter().find(|spec| spec.name == name).unwrap();
        spec.schema["properties"].get("fields").cloned()
    };
    for tool in ["get_resource", "list_resources"] {
        assert_eq!(schema(tool).unwrap()["type"], "array", "{tool}");
    }
    assert!(schema("describe_resource").is_none());
}
