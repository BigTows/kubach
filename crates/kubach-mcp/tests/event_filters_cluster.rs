//! `list_events` filters against a real API server (#111): `type` and
//! `reason` find an event behind more events than a read takes, and a window
//! says when it was not reached. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-111 \
//!     cargo test -p kubach-mcp --test event_filters_cluster -- --ignored
//! ```
//!
//! The test creates `<namespace>` and `<namespace>-b`, about 5100 events in
//! the first, and deletes both at the end.

use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt as _, stream};
use k8s_openapi::api::core::v1::{Event, Namespace};
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kubach_mcp::{
    Access, McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

/// More than a read takes, so whatever sorts after them is out of its reach.
const FILLERS: usize = 5100;
const REASON: &str = "Kubach111";

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn event(namespace: &str, name: &str, kind: &str, reason: &str, age_seconds: i64) -> Event {
    let time = Timestamp::now() - SignedDuration::from_secs(age_seconds);
    serde_json::from_value(json!({
        "metadata": {"name": name, "namespace": namespace},
        "involvedObject": {"kind": "ConfigMap", "name": "subject", "namespace": namespace},
        "type": kind,
        "reason": reason,
        "message": format!("message of {name}"),
        "firstTimestamp": time.to_string(),
        "lastTimestamp": time.to_string(),
        "source": {"component": "kubach-test"},
    }))
    .unwrap()
}

async fn clean_up(user: &Client, names: &[String]) {
    let all: Api<Namespace> = Api::all(user.clone());
    for name in names {
        let _ = all.delete(name, &DeleteParams::default()).await;
    }
    for name in names {
        for _ in 0..240 {
            if matches!(all.get_opt(name).await, Ok(None)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

async fn prepare(user: &Client, names: &[String]) {
    let all: Api<Namespace> = Api::all(user.clone());
    for name in names {
        let namespace = serde_json::from_value(json!({"metadata": {"name": name}})).unwrap();
        all.create(&PostParams::default(), &namespace)
            .await
            .unwrap();
    }
    let crowded: Api<Event> = Api::namespaced(user.clone(), &names[0]);
    let created: Vec<_> = stream::iter(0..FILLERS)
        .map(|index| {
            let crowded = crowded.clone();
            let filler = event(
                &names[0],
                &format!("a-filler-{index:05}"),
                "Normal",
                "Filler",
                60,
            );
            async move { crowded.create(&PostParams::default(), &filler).await }
        })
        .buffer_unordered(32)
        .collect()
        .await;
    assert!(created.iter().all(Result::is_ok));
    for (namespace, name, kind, reason, age) in [
        (&names[0], "z-warning", "Warning", REASON, 10),
        (&names[1], "z-warning-b", "Warning", REASON, 10),
        (&names[1], "z-old-b", "Warning", REASON, 7200),
    ] {
        let events: Api<Event> = Api::namespaced(user.clone(), namespace);
        events
            .create(
                &PostParams::default(),
                &event(namespace, name, kind, reason, age),
            )
            .await
            .unwrap();
    }
}

async fn check(base: &Config, context: &str, names: &[String]) {
    let spec = SessionSpec {
        context: context.to_owned(),
        server_url: base.cluster_url.to_string(),
        namespaces: names.to_vec(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let policy = SecurityPolicy::with_minimum(Access::ALL);
    let session = Session::start(base, &spec, &policy).await.unwrap();
    let server = McpServer::new(session, Arc::new(ScriptedConfirmer::denying()), None);
    let (crowded, quiet) = (&names[0], &names[1]);
    let call = async |value: Value| {
        let outcome = server.call("test", "list_events", args(value)).await;
        (outcome.is_error, outcome.text)
    };

    // The selector reaches past the read's bound.
    let (failed, text) = call(json!({"namespace": crowded, "type": "Warning"})).await;
    assert!(!failed, "{text}");
    assert!(text.contains("message of z-warning"), "{text}");
    assert!(!text.contains("filler"), "{text}");
    let (_, text) = call(json!({"namespace": crowded, "reason": REASON})).await;
    assert!(text.contains("message of z-warning"), "{text}");
    assert!(!text.contains("filler"), "{text}");

    // No selector: the read stops, and the window is not called covered.
    let (_, text) = call(json!({"namespace": crowded, "sinceSeconds": 600})).await;
    assert!(
        text.starts_with("Window: the last 600 s, NOT fully covered"),
        "{text}"
    );

    // With one, the same window is read to the end.
    let (_, text) =
        call(json!({"namespace": crowded, "type": "Warning", "sinceSeconds": 600})).await;
    assert!(
        text.starts_with("Window: the last 600 s, fully covered: all 1 events"),
        "{text}"
    );

    // Time is cut on our side.
    let (_, text) = call(json!({"namespace": quiet, "sinceSeconds": 600})).await;
    assert!(text.contains("message of z-warning-b"), "{text}");
    assert!(!text.contains("z-old-b"), "{text}");
    assert!(text.contains("fully covered: all 1 events"), "{text}");

    // Across namespaces, with the coverage line.
    let (_, text) =
        call(json!({"allNamespaces": true, "reason": REASON, "sinceSeconds": 600})).await;
    assert!(text.starts_with("Coverage: complete. All 2"), "{text}");
    assert!(text.contains("fully covered: all 2 events"), "{text}");
    let (_, text) = call(json!({"allNamespaces": true, "sinceSeconds": 600})).await;
    assert!(text.starts_with("Coverage: PARTIAL"), "{text}");
    assert!(
        text.contains(&format!("newer events may be missing in: {crowded}")),
        "{text}"
    );
    assert!(text.contains("NOT fully covered"), "{text}");

    // What would widen the selector stops here.
    let (failed, text) = call(json!({"namespace": crowded, "reason": "Filler,type=Warning"})).await;
    assert!(failed, "{text}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn filters_work_on_a_real_api_server() {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let base = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    let user = Client::try_from(base.clone()).unwrap();
    let names = [namespace.clone(), format!("{namespace}-b")];
    clean_up(&user, &names).await;
    let run = async {
        prepare(&user, &names).await;
        check(&base, &context, &names).await;
    };
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(run)).await;
    clean_up(&user, &names).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
