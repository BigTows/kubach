//! Nodes for the agent against a real cluster (#61, #62). Ignored by
//! default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube cargo test -p kubach-mcp --test nodes_cluster -- --ignored
//! ```
//!
//! Read-only: nothing is created in the cluster.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::Node;
use kubach_mcp::{
    McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt as _};
use serde_json::{Map, Value, json};

async fn config() -> (Config, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT)"]
async fn the_agent_reads_nodes_after_the_user_allows_them() {
    let (base, context) = config().await;
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
        ttl: Some(Duration::from_secs(300)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let node = Api::<Node>::all(Client::try_from(base.clone()).unwrap())
        .list(&ListParams::default())
        .await
        .unwrap()
        .items
        .remove(0)
        .name_any();

    // The tools say they need the user's approval first.
    let tools = server.tools();
    let description = tools
        .iter()
        .find(|t| t.name == "list_nodes")
        .and_then(|t| t.description.as_deref())
        .unwrap_or_default()
        .to_owned();
    assert!(
        description.contains("Requires your approval in Kubach"),
        "{description}"
    );

    let listed = server.call("test", "list_nodes", Map::new()).await;
    let described = server
        .call("test", "describe_node", args(json!({"name": node})))
        .await;
    let top = server.call("test", "top_nodes", Map::new()).await;
    session.close().await.unwrap();

    // One question, for nodes; then everything reads.
    let asked: Vec<_> = confirmer
        .requests()
        .into_iter()
        .filter(|r| matches!(&r.kind, RequestKind::Access { raise, .. } if raise.nodes))
        .collect();
    assert_eq!(asked.len(), 1, "{:?}", confirmer.requests());
    assert!(!listed.is_error, "{}", listed.text);
    assert!(listed.text.contains(&node), "{}", listed.text);
    assert!(listed.text.contains("PROBLEMS"), "{}", listed.text);
    assert!(!described.is_error, "{}", described.text);
    for part in [
        "Conditions:",
        "MemoryPressure",
        "Allocatable",
        "Allocated (pods' requests / limits against allocatable)",
        "Pods: ",
        "Events:",
        "Evictions:",
    ] {
        assert!(described.text.contains(part), "{part}: {}", described.text);
    }
    // metrics-server is an addon: either usage or a clear answer.
    assert!(
        (!top.is_error && top.text.contains(&node))
            || top.text.contains("metrics-server is not installed"),
        "{}",
        top.text
    );
}
