//! `minLevel` of `get_logs` against a real cluster (#117). Ignored by
//! default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-117 \
//!     cargo test -p kubach-mcp --test logs_min_level_cluster -- --ignored
//! ```
//!
//! The namespace is created here and deleted at the end.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod};
use kubach_mcp::{McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const POD: &str = "kubach-levels";

const LEVELED: &str = r#"level=info msg=started
level=error msg="disk full" password=hunter2
{"level":"warn","token_count":5}
{"level":50,"msg":"boom"}
E1103 12:00:00.000000       1 main.go:1] failed
I1103 12:00:00.000000       1 main.go:2] ok
    at main.go:3
"#;

const PLAIN: &str = "ERROR db failed\nstarting\n";

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

/// A container that prints `logs` and stays up.
fn printing(name: &str, logs: &str) -> Value {
    json!({
        "name": name,
        "image": "busybox:1.36",
        "command": ["sh", "-c", "printf '%s' \"$LOGS\"; sleep 600"],
        "env": [{"name": "LOGS", "value": logs}],
    })
}

async fn wait_until_running(pods: &Api<Pod>) -> bool {
    for _ in 0..90 {
        let pod = pods.get(POD).await.unwrap();
        let statuses = pod
            .status
            .and_then(|status| status.container_statuses)
            .unwrap_or_default();
        let running = |s: &k8s_openapi::api::core::v1::ContainerStatus| {
            s.state.as_ref().is_some_and(|s| s.running.is_some())
        };
        if statuses.len() == 2 && statuses.iter().all(running) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn min_level_reads_real_logs_and_counts_them() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let object = json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace}});
    namespaces
        .create(
            &PostParams::default(),
            &serde_json::from_value(object).unwrap(),
        )
        .await
        .unwrap();

    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Debug,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let server = McpServer::new(
        session.clone(),
        Arc::new(ScriptedConfirmer::approving()),
        None,
    );

    let pods: Api<Pod> = Api::namespaced(user, &namespace);
    let pod = json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [printing("app", LEVELED), printing("plain", PLAIN)],
        },
    });
    pods.create(
        &PostParams::default(),
        &serde_json::from_value(pod).unwrap(),
    )
    .await
    .unwrap();
    let running = wait_until_running(&pods).await;

    let logs = |extra: Value| {
        let mut call = args(json!({"pod": POD, "namespace": namespace}));
        call.extend(args(extra));
        server.call("test", "get_logs", call)
    };
    let leveled = logs(json!({"container": "app", "minLevel": "warn"})).await;
    let unknown = logs(json!({"container": "plain", "minLevel": "warn"})).await;
    let both = logs(json!({"allContainers": true, "minLevel": "error"})).await;
    let unfiltered = logs(json!({"container": "app"})).await;

    // Gone before any assert can fail.
    let closed = session.close().await;
    let _ = namespaces
        .delete(&namespace, &DeleteParams::default())
        .await;

    let mut gone = false;
    for _ in 0..60 {
        gone = namespaces.get_opt(&namespace).await.unwrap().is_none();
        if gone {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    closed.unwrap();
    assert!(gone, "namespace {namespace} is still there");
    assert!(running, "the pod did not start");
    let lines: Vec<&str> = leveled.text.lines().collect();
    assert_eq!(
        lines,
        [
            "(minLevel warn: 7 read, 6 with a recognized level, 4 matched; \
             1 without a recognized level were excluded)",
            "",
            "level=error msg=\"disk full\" password=***",
            // Masking takes the brace with the value: no longer JSON.
            "{\"level\":\"warn\",\"token_count\":***",
            "{\"level\":50,\"msg\":\"boom\"}",
            "E1103 12:00:00.000000       1 main.go:1] failed",
        ],
        "{}",
        leveled.text
    );

    assert!(
        unknown
            .text
            .starts_with("(minLevel warn: 2 read, 0 with a recognized level, 0 matched; "),
        "{}",
        unknown.text
    );
    assert!(
        !unknown.text.contains("ERROR db failed"),
        "{}",
        unknown.text
    );

    // Counted across the streams; each line names its own.
    assert!(
        both.text
            .starts_with("(minLevel error: 9 read, 6 with a recognized level, 3 matched; "),
        "{}",
        both.text
    );
    assert!(
        both.text
            .contains(&format!("[{POD}/app] {{\"level\":50,\"msg\":\"boom\"}}")),
        "{}",
        both.text
    );

    // Without minLevel the logs come as before: masked, and no note.
    assert!(
        unfiltered.text.starts_with("level=info msg=started\n"),
        "{}",
        unfiltered.text
    );
    assert!(unfiltered.text.contains("    at main.go:3"));
    assert!(!unfiltered.text.contains("hunter2"));
}
