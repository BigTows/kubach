//! The agent's port-forward against a real cluster (#17): a pod with a
//! web server, a session at the debug level, a forward the user approves,
//! a request through `127.0.0.1`, and the forward gone with the session.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-17 \
//!     cargo test -p kubach-mcp --test port_forward_cluster -- --ignored
//! ```
//!
//! A missing namespace is created and deleted at the end with the pod; the
//! session's objects go with the session.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod};
use k8s_openapi::api::rbac::v1::Role;
use kubach_mcp::{
    Access, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const POD: &str = "kubach-test-forward";

async fn config() -> (Config, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context, namespace)
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// Waits up to two minutes for `check`.
async fn eventually(mut check: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..240 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn http_get(address: &str) -> Option<String> {
    let mut stream = TcpStream::connect(address).await.ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .ok()?;
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut reply))
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&reply).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn the_agent_forwards_to_a_pod_until_the_session_ends() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created_namespace = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created_namespace {
        let object: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let pod: Pod = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD},
        "spec": {"containers": [{"name": "web", "image": "nginx:alpine",
                                 "ports": [{"name": "http", "containerPort": 80}]}]},
    }))
    .unwrap();
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    let _ = eventually(async || pods.get_opt(POD).await.unwrap().is_none()).await;
    pods.create(&PostParams::default(), &pod).await.unwrap();
    let ready = eventually(async || {
        pods.get_opt(POD).await.ok().flatten().is_some_and(|pod| {
            pod.status
                .and_then(|status| status.conditions)
                .unwrap_or_default()
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
    })
    .await;
    assert!(ready, "pod {POD} did not become ready");

    let policy = SecurityPolicy::with_minimum(Access::profile(Profile::Debug));
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Debug,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let outcome = server
        .call(
            "test",
            "port_forward_start",
            args(json!({"target": format!("pod/{POD}"), "port": "http", "namespace": namespace})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        confirmer
            .requests()
            .iter()
            .any(|request| matches!(request.kind, RequestKind::PortForward { .. }))
    );
    let json_start = outcome.text.find('{').unwrap();
    let result: Value = serde_json::from_str(&outcome.text[json_start..]).unwrap();
    let address = result["address"].as_str().unwrap().to_owned();
    assert!(address.starts_with("127.0.0.1:"), "{address}");

    // A temporary ServiceAccount's Role has port-forward while it runs.
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let role = format!("kubach-mcp-{}", session.id());
    if let Some(role) = roles.get_opt(&role).await.unwrap() {
        let resources: Vec<String> = role
            .rules
            .unwrap_or_default()
            .into_iter()
            .flat_map(|rule| rule.resources.unwrap_or_default())
            .collect();
        assert!(
            resources.iter().any(|r| r == "pods/portforward"),
            "{resources:?}"
        );
    }

    let reply = http_get(&address).await.unwrap_or_default();
    assert!(reply.starts_with("HTTP/1."), "{reply}");

    session.close().await.unwrap();
    assert!(session.forwards().manager().list().is_empty());
    assert!(TcpStream::connect(&address).await.is_err() || http_get(&address).await.is_none());
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    if created_namespace {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
}
