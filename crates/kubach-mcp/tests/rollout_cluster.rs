//! `wait_for_rollout` against a real cluster (#67). Ignored by default;
//! run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-67 \
//!     cargo test -p kubach-mcp --test rollout_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the Deployments are deleted at the end.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use kubach_mcp::{McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

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

fn deployment(name: &str, memory: &str) -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name},
        "spec": {
            "replicas": 2,
            "selector": {"matchLabels": {"app": name}},
            "template": {
                "metadata": {"labels": {"app": name}},
                "spec": {"containers": [{
                    "name": "pause",
                    "image": "registry.k8s.io/pause:3.9",
                    "resources": {"requests": {"memory": memory}},
                }]},
            },
        },
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn waits_for_a_rollout_and_says_why_one_is_stuck() {
    let (base, context, namespace) = config().await;
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::ReadOnly,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let server = McpServer::new(
        session.clone(),
        Arc::new(ScriptedConfirmer::approving()),
        None,
    );
    let user = Client::try_from(base.clone()).unwrap();
    let deployments: Api<Deployment> = Api::namespaced(user, &namespace);
    let post = PostParams::default();
    deployments
        .create(&post, &deployment("kubach-rollout-ok", "4Mi"))
        .await
        .unwrap();
    deployments
        .create(&post, &deployment("kubach-rollout-stuck", "100Ti"))
        .await
        .unwrap();

    let wait = |name: &str, timeout: u64| {
        args(json!({
            "kind": "Deployment", "name": name, "namespace": namespace,
            "timeoutSeconds": timeout,
        }))
    };
    let ok = server
        .call("test", "wait_for_rollout", wait("kubach-rollout-ok", 120))
        .await;
    let stuck = server
        .call("test", "wait_for_rollout", wait("kubach-rollout-stuck", 6))
        .await;

    for name in ["kubach-rollout-ok", "kubach-rollout-stuck"] {
        let _ = deployments.delete(name, &DeleteParams::default()).await;
    }
    session.close().await.unwrap();

    assert!(!ok.is_error, "{}", ok.text);
    assert!(ok.text.contains("rolled out"), "{}", ok.text);
    assert!(stuck.is_error, "{}", stuck.text);
    assert!(
        stuck.text.contains("has not rolled out after"),
        "{}",
        stuck.text
    );
    assert!(
        stuck.text.contains("Pods: 2 of 2 not ready"),
        "{}",
        stuck.text
    );
    assert!(stuck.text.contains("not scheduled:"), "{}", stuck.text);
    assert!(stuck.text.contains("Insufficient memory"), "{}", stuck.text);
}
