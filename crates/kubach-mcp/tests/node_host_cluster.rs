//! `node_host_read` against a real cluster (#106). Ignored by default; run
//! with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube cargo test -p kubach-mcp --test node_host_cluster -- --ignored --nocapture
//! ```
//!
//! Only on minikube: every command runs through a temporary privileged pod
//! in `default` on the node, which the call deletes; the test checks that
//! none is left. The commands only read.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Node, Pod};
use kubach_core::streams::node_shell;
use kubach_mcp::{
    CallOutcome, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec,
};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt as _};
use serde_json::{Map, Value, json};

async fn config() -> (Config, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    assert_eq!(context, "minikube", "this test runs privileged pods");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context)
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// The windows about a command on a node so far.
fn commands(confirmer: &ScriptedConfirmer) -> Vec<(Vec<String>, bool, usize)> {
    confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match request.kind {
            RequestKind::NodeCommand { command, .. } => {
                Some((command, request.allow_remember, request.dangerous.len()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs minikube (KUBACH_TEST_CONTEXT)"]
async fn the_agent_reads_a_nodes_host_one_approved_command_at_a_time() {
    let (base, context) = config().await;
    let client = Client::try_from(base.clone()).unwrap();
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
        ttl: Some(Duration::from_secs(300)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    // Approves, and ticks "don't ask again" wherever it is offered.
    let confirmer = Arc::new(ScriptedConfirmer::approving_for_session());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let node = Api::<Node>::all(client.clone())
        .list(&ListParams::default())
        .await
        .unwrap()
        .items
        .remove(0)
        .name_any();
    let call = |command: &'static str, arguments: Value| {
        let server = &server;
        let node = node.clone();
        async move {
            let outcome: CallOutcome = server
                .call(
                    "test",
                    "node_host_read",
                    args(json!({"node": node, "command": command, "args": arguments})),
                )
                .await;
            println!("--- {command} {arguments}\n{}", outcome.text);
            outcome
        }
    };

    // Not a listed program, or an argument off its list: refused before
    // anyone is asked.
    let shell = call("sh", json!(["-c", "id"])).await;
    let follow = call("journalctl", json!(["-f"])).await;
    let relative = call("cat", json!(["etc/shadow"])).await;
    assert!(shell.is_error && follow.is_error && relative.is_error);
    assert!(
        commands(&confirmer).is_empty(),
        "{:?}",
        confirmer.requests()
    );

    // A file of the host, with the exact command in the window.
    let first = call("cat", json!(["/etc/os-release"])).await;
    assert!(!first.is_error, "{}", first.text);
    assert!(first.text.contains("ID="), "{}", first.text);
    assert_eq!(
        commands(&confirmer),
        [(
            vec!["cat".to_owned(), "/etc/os-release".to_owned()],
            true,
            0
        )]
    );
    // The same command again runs on the tick; another file asks.
    let again = call("cat", json!(["/etc/os-release"])).await;
    assert!(
        !again.is_error && again.text.contains("ID="),
        "{}",
        again.text
    );
    assert!(again.text.contains("earlier choice"), "{}", again.text);
    assert_eq!(commands(&confirmer).len(), 1);
    let other = call("cat", json!(["/etc/hostname"])).await;
    assert!(!other.is_error, "{}", other.text);
    assert_eq!(commands(&confirmer).len(), 2);

    // The journal of the host, cut to its last lines without a pager.
    let journal = call("journalctl", json!(["-n", "5"])).await;
    assert!(!journal.is_error, "{}", journal.text);
    assert!(journal.text.contains("exit: "), "{}", journal.text);
    // PID 1 of the host, not of the pod.
    let init = call("cat", json!(["/proc/1/comm"])).await;
    assert!(!init.is_error, "{}", init.text);
    assert!(!init.text.contains("\nsh\n"), "{}", init.text);

    // A file that may hold credentials: its own tick, never remembered.
    let before = commands(&confirmer).len();
    let environ = call("head", json!(["-c", "16", "/proc/1/environ"])).await;
    assert!(!environ.is_error, "{}", environ.text);
    let asked = commands(&confirmer);
    assert_eq!(asked.len(), before + 1);
    assert_eq!((asked[before].1, asked[before].2), (false, 1));
    let environ_again = call("head", json!(["-c", "16", "/proc/1/environ"])).await;
    assert!(!environ_again.is_error, "{}", environ_again.text);
    assert_eq!(commands(&confirmer).len(), before + 2);

    session.close().await.unwrap();
    // Every call deleted its pod.
    let left = Api::<Pod>::namespaced(client, node_shell::DEFAULT_NAMESPACE)
        .list(&ListParams::default().labels(&node_shell::label_selector()))
        .await
        .unwrap();
    let left: Vec<String> = left
        .items
        .iter()
        .filter(|pod| pod.metadata.deletion_timestamp.is_none())
        .map(|pod| pod.name_any())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
