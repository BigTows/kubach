//! The agent's debug container against a real cluster (#64): a distroless
//! pod (only `/pause`, no shell) in a namespace that enforces the
//! restricted Pod Security level, a session at the debug level, a debug
//! container the user approves, a command in it, then `exec` into it.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-64 \
//!     cargo test -p kubach-mcp --test debug_container_cluster -- --ignored
//! ```
//!
//! A missing namespace is created (labelled restricted) and deleted at the
//! end with the pod; the session's objects go with the session.

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

const POD: &str = "kubach-test-distroless";

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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn the_agent_debugs_a_distroless_pod() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created_namespace = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created_namespace {
        let object: Namespace = serde_json::from_value(json!({"metadata": {
            "name": namespace,
            "labels": {"pod-security.kubernetes.io/enforce": "restricted"},
        }}))
        .unwrap();
        namespaces
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    // Restricted and without a shell: nothing to exec into.
    let pod: Pod = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD},
        "spec": {
            "securityContext": {"runAsNonRoot": true, "runAsUser": 65532,
                                "seccompProfile": {"type": "RuntimeDefault"}},
            "containers": [{"name": "app", "image": "registry.k8s.io/pause:3.9",
                            "securityContext": {"allowPrivilegeEscalation": false,
                                                "capabilities": {"drop": ["ALL"]}}}],
        },
    }))
    .unwrap();
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    let _ = eventually(async || pods.get_opt(POD).await.unwrap().is_none()).await;
    pods.create(&PostParams::default(), &pod).await.unwrap();
    let running = eventually(async || {
        pods.get_opt(POD).await.ok().flatten().is_some_and(|pod| {
            pod.status.and_then(|status| status.phase).as_deref() == Some("Running")
        })
    })
    .await;
    assert!(running, "pod {POD} did not start");

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

    // Exec finds no shell in the app container.
    let outcome = server
        .call(
            "test",
            "exec",
            args(json!({"pod": POD, "namespace": namespace, "command": ["sh", "-c", "id"]})),
        )
        .await;
    assert!(
        outcome.is_error || !outcome.text.contains("exit: Success"),
        "{}",
        outcome.text
    );

    let outcome = server
        .call(
            "test",
            "debug_container",
            args(
                json!({"pod": POD, "namespace": namespace, "targetContainer": "app",
                        "name": "dbg", "command": ["ps"]}),
            ),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("exit: Success"), "{}", outcome.text);
    // The shared process namespace shows the app's process.
    assert!(outcome.text.contains("/pause"), "{}", outcome.text);
    assert!(
        outcome.text.contains("runs as user 65532"),
        "{}",
        outcome.text
    );
    let asked = confirmer
        .requests()
        .into_iter()
        .find(|request| request.tool == "debug_container")
        .expect("the debug container was confirmed");
    assert!(matches!(asked.kind, RequestKind::Exec { .. }));
    assert!(
        asked
            .dangerous
            .iter()
            .any(|finding| finding.message.contains("until the pod is deleted")),
        "{:?}",
        asked.dangerous
    );

    // The debug container keeps running for exec.
    let outcome = server
        .call(
            "test",
            "exec",
            args(
                json!({"pod": POD, "namespace": namespace, "container": "dbg",
                        "command": ["ls", "/proc/1/root"]}),
            ),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("exit: Success"), "{}", outcome.text);

    let live = pods.get(POD).await.unwrap();
    let ephemeral = live
        .spec
        .and_then(|spec| spec.ephemeral_containers)
        .unwrap_or_default();
    assert_eq!(ephemeral.len(), 1);
    let context = ephemeral[0].security_context.clone().unwrap();
    assert_eq!(context.privileged, None);
    assert_eq!(context.run_as_non_root, Some(true));

    // A temporary ServiceAccount's Role may patch ephemeral containers.
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let role = format!("kubach-mcp-{}", session.id());
    if let Some(role) = roles.get_opt(&role).await.unwrap() {
        let rules = role.rules.unwrap_or_default();
        let rule = rules
            .iter()
            .find(|rule| {
                rule.resources
                    .iter()
                    .flatten()
                    .any(|r| r == "pods/ephemeralcontainers")
            })
            .expect("the Role allows debug containers");
        assert_eq!(rule.verbs, vec!["patch".to_string()]);
    }

    session.close().await.unwrap();
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    if created_namespace {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
}
