//! A Secret through a pod against a real cluster (#190, #192), in a
//! client-only session: the manifest's Secret reads are dangerous fields of
//! the confirmation, the logs of the agent's own pod are confirmed, and
//! kinds beyond the allowlist are refused. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-190 \
//!     cargo test -p kubach-mcp --test secret_through_pod_cluster -- --ignored
//! ```
//!
//! The namespace must exist and the node must have `nginx:1.27-alpine`; the
//! test's Secret and Job are deleted at the end. Values are fake.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Pod, Secret};
use kubach_mcp::{
    McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session, SessionMode,
    SessionSpec,
};
use kube::api::{Api, DeleteParams, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt};
use serde_json::{Map, Value, json};

const SECRET: &str = "kubach-test-db";
const JOB: &str = "kubach-test-leak";
const VALUE: &str = "fake-value-190";

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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_secret_through_a_pod_is_shown_and_confirmed() {
    let (base, context, namespace) = config().await;
    let policy = SecurityPolicy {
        temporary_objects: false,
        ..SecurityPolicy::with_minimum(kubach_mcp::Access::ALL)
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: vec![namespace.clone()],
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    assert_eq!(session.mode_in(&namespace), SessionMode::ClientOnly);
    let user = Client::try_from(base.clone()).unwrap();
    let secrets: Api<Secret> = Api::namespaced(user.clone(), &namespace);
    let jobs: Api<Job> = Api::namespaced(user.clone(), &namespace);
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let secret: Secret = serde_json::from_value(json!({
        "metadata": {"name": SECRET},
        "stringData": {"PASSWORD": VALUE},
    }))
    .unwrap();
    secrets.create(&Default::default(), &secret).await.unwrap();

    let approving = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), approving.clone(), None);

    // The Job of the audit's scenario: envFrom a Secret, print it.
    let job = json!({
        "apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": JOB},
        "spec": {"backoffLimit": 0, "template": {"spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "leak", "image": "nginx:1.27-alpine", "imagePullPolicy": "IfNotPresent",
                "command": ["sh", "-c", "echo \"password=$PASSWORD\"; echo leaked"],
                "envFrom": [{"secretRef": {"name": SECRET}}],
            }],
        }}},
    });
    let outcome = server
        .call(
            "test-agent",
            "apply_manifest",
            args(json!({"yaml": job.to_string(), "dryRun": false})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let asked = approving.requests();
    let apply = asked
        .iter()
        .find(|r| r.tool == "apply_manifest" && r.kind == RequestKind::Call)
        .expect("apply was confirmed");
    assert!(
        apply.dangerous.iter().any(|f| f
            .message
            .contains(&format!("manifest reads Secret \"{SECRET}\""))),
        "{:#?}",
        apply.dangerous
    );

    // Wait for the Job's pod to finish.
    let mut pod_name = None;
    for _ in 0..120 {
        let list = pods
            .list(&ListParams::default().labels(&format!("job-name={JOB}")))
            .await
            .unwrap();
        if let Some(pod) = list.items.iter().find(|p| {
            p.status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .is_some_and(|phase| phase == "Succeeded" || phase == "Failed")
        }) {
            pod_name = Some(pod.name_any());
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let pod_name = pod_name.expect("the Job's pod finished within a minute");

    // Refused: no logs reach the agent.
    let denying = Arc::new(ScriptedConfirmer::denying());
    let refused = McpServer::new(session.clone(), denying.clone(), None)
        .call("test-agent", "get_logs", args(json!({"pod": pod_name})))
        .await;
    assert!(refused.is_error, "{}", refused.text);
    assert!(!refused.text.contains("leaked"), "{}", refused.text);
    let logs = denying.requests();
    let logs = logs
        .iter()
        .find(|r| r.tool == "get_logs" && r.kind == RequestKind::Call)
        .expect("the logs were asked for");
    assert!(
        logs.details.contains(&format!("Job {JOB}")),
        "{}",
        logs.details
    );

    // Approved: the logs come.
    let read = server
        .call("test-agent", "get_logs", args(json!({"pod": pod_name})))
        .await;
    assert!(!read.is_error, "{}", read.text);
    assert!(read.text.contains("leaked"), "{}", read.text);

    // Kinds beyond the allowlist are refused (Lease is served everywhere).
    let lease = server
        .call(
            "test-agent",
            "list_resources",
            args(json!({"kind": "Lease"})),
        )
        .await;
    assert!(lease.is_error);
    assert!(lease.text.contains("Allow other kinds"), "{}", lease.text);

    let _ = jobs.delete(JOB, &DeleteParams::background()).await;
    let _ = secrets.delete(SECRET, &DeleteParams::default()).await;
    session.close().await.unwrap();
}
