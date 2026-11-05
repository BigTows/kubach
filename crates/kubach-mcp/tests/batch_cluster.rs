//! `scale_batch` and `rollout_restart_batch` against a real cluster (#108):
//! under temporary ServiceAccounts, with the API server enforcing the UID a
//! patch carries. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-108 \
//!     cargo test -p kubach-mcp --test batch_cluster -- --ignored
//! ```
//!
//! The test creates `<namespace>-a` and `<namespace>-b` and deletes them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kubach_core::plugin::BoxFuture;
use kubach_core::plugin::builtin::{restart_object, scale_object};
use kubach_mcp::{
    ConfirmationDecision, ConfirmationRequest, Confirmer, McpServer, Profile, RequestKind,
    ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, PostParams};
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

fn deployment(name: &str, replicas: i32) -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name, "labels": {"batch": "yes"}},
        "spec": {
            "replicas": replicas,
            "selector": {"matchLabels": {"app": name}},
            "template": {
                "metadata": {"labels": {"app": name}},
                "spec": {"containers": [{
                    "name": "pause",
                    "image": "registry.k8s.io/pause:3.9",
                }]},
            },
        },
    }))
    .unwrap()
}

/// Creates a Deployment while the first batch window is open, then approves.
struct Late {
    answer: ScriptedConfirmer,
    deployments: Api<Deployment>,
    batches: Mutex<Vec<ConfirmationRequest>>,
}

impl Confirmer for Late {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        Box::pin(async move {
            if matches!(request.kind, RequestKind::Batch { .. }) {
                let first = {
                    let mut batches = self.batches.lock().unwrap();
                    batches.push(request.clone());
                    batches.len() == 1
                };
                if first {
                    self.deployments
                        .create(&PostParams::default(), &deployment("late", 1))
                        .await
                        .unwrap();
                }
            }
            self.answer.confirm(request).await
        })
    }
}

async fn replicas(api: &Api<Deployment>, name: &str) -> i32 {
    let deployment = api.get(name).await.unwrap();
    deployment.spec.replicas.unwrap()
}

async fn restarted(api: &Api<Deployment>, name: &str) -> bool {
    let deployment = api.get(name).await.unwrap();
    deployment
        .spec
        .template
        .metadata
        .and_then(|metadata| metadata.annotations)
        .is_some_and(|annotations| annotations.contains_key("kubectl.kubernetes.io/restartedAt"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_batch_across_namespaces_changes_exactly_what_was_approved() {
    let (base, context, prefix) = config().await;
    let (ns_a, ns_b) = (format!("{prefix}-a"), format!("{prefix}-b"));
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let post = PostParams::default();
    for name in [&ns_a, &ns_b] {
        let namespace = serde_json::from_value(json!({"metadata": {"name": name}})).unwrap();
        namespaces.create(&post, &namespace).await.unwrap();
    }
    let in_a: Api<Deployment> = Api::namespaced(user.clone(), &ns_a);
    let in_b: Api<Deployment> = Api::namespaced(user.clone(), &ns_b);
    in_a.create(&post, &deployment("web", 2)).await.unwrap();
    in_a.create(&post, &deployment("api", 1)).await.unwrap();
    in_b.create(&post, &deployment("db", 3)).await.unwrap();

    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(Late {
        answer: ScriptedConfirmer::approving(),
        deployments: in_a.clone(),
        batches: Mutex::default(),
    });
    let server = McpServer::new(session.clone(), confirmer.clone(), None);

    let scale = args(json!({
        "kind": "Deployment", "labelSelector": "batch=yes",
        "namespaces": [ns_a, ns_b], "replicas": 0,
    }));
    let outcome = server.call("agent", "scale_batch", scale).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(confirmer.batches.lock().unwrap().len(), 1);
    assert_eq!(replicas(&in_a, "web").await, 0);
    assert_eq!(replicas(&in_a, "api").await, 0);
    assert_eq!(replicas(&in_b, "db").await, 0);
    assert_eq!(replicas(&in_a, "late").await, 1, "not approved");
    assert!(
        outcome.text.contains(&format!(
            "Appeared after the user's answer, not touched: Deployment {ns_a}/late"
        )),
        "{}",
        outcome.text
    );

    // The answer's rollback, as it stands.
    let rollback = outcome
        .text
        .lines()
        .find_map(|line| line.strip_prefix("To roll back, call scale_batch with: "))
        .expect("a rollback line");
    let rollback: Value = serde_json::from_str(rollback).unwrap();
    let outcome = server.call("agent", "scale_batch", args(rollback)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(replicas(&in_a, "web").await, 2);
    assert_eq!(replicas(&in_a, "api").await, 1);
    assert_eq!(replicas(&in_b, "db").await, 3);

    let restart = args(json!({
        "kind": "Deployment", "namespace": ns_a, "concurrency": 2,
        "objects": [{"name": "web"}, {"name": "missing"}],
    }));
    let outcome = server.call("agent", "rollout_restart_batch", restart).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(restarted(&in_a, "web").await);
    assert!(!restarted(&in_a, "api").await);
    assert!(outcome.text.contains("missing"), "{}", outcome.text);

    // The API server itself turns down a patch for another object.
    let resource = ApiResource::erase::<Deployment>(&());
    let dynamic: Api<DynamicObject> = Api::namespaced_with(user.clone(), &ns_a, &resource);
    let stale = "00000000-0000-0000-0000-000000000000";
    let scaled = scale_object(&dynamic, "api", stale, 5).await;
    assert!(scaled.is_err(), "scale with a stale UID went through");
    let sent = restart_object(&dynamic, "api", stale).await;
    assert!(sent.is_err(), "restart with a stale UID went through");
    assert_eq!(replicas(&in_a, "api").await, 1);
    assert!(!restarted(&in_a, "api").await);

    session.close().await.unwrap();
    for name in [&ns_a, &ns_b] {
        namespaces
            .delete(name, &DeleteParams::default())
            .await
            .unwrap();
    }
}
