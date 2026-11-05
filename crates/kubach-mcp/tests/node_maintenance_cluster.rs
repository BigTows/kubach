//! `cordon_node`, `uncordon_node` and `drain_node` against a real cluster
//! (#105). Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube cargo test -p kubach-mcp --test node_maintenance_cluster -- --ignored
//! ```
//!
//! Only on minikube: the node is cordoned for a moment. The drain is
//! limited by `podSelector` to this test's pods in `kubach-test-105`, so it
//! leaves everything else on the node alone. The node is uncordoned and the
//! namespace deleted at the end, also when the test fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kubach_mcp::{
    Access, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec,
};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt as _};
use serde_json::{Map, Value, json};

const NAMESPACE: &str = "kubach-test-105";
const SELECTOR: &str = "kubach-test=node-maintenance";

async fn config() -> (Config, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    assert_eq!(context, "minikube", "this test cordons a node");
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

fn pod_spec() -> Value {
    json!({
        "terminationGracePeriodSeconds": 0,
        "containers": [{"name": "pause", "image": "registry.k8s.io/pause:3.9"}],
    })
}

fn deployment(name: &str) -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": name}},
            "template": {
                "metadata": {"labels": {"app": name, "kubach-test": "node-maintenance"}},
                "spec": pod_spec(),
            },
        },
    }))
    .unwrap()
}

async fn set_up(client: &Client) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let namespace = serde_json::from_value(json!({"metadata": {"name": NAMESPACE}})).unwrap();
    namespaces
        .create(&PostParams::default(), &namespace)
        .await
        .unwrap();
    let post = PostParams::default();
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), NAMESPACE);
    for name in ["web", "guarded"] {
        deployments.create(&post, &deployment(name)).await.unwrap();
    }
    let bare: Pod = serde_json::from_value(json!({
        "metadata": {"name": "bare", "labels": {"kubach-test": "node-maintenance"}},
        "spec": pod_spec(),
    }))
    .unwrap();
    Api::<Pod>::namespaced(client.clone(), NAMESPACE)
        .create(&post, &bare)
        .await
        .unwrap();
    let budget: PodDisruptionBudget = serde_json::from_value(json!({
        "metadata": {"name": "guard"},
        "spec": {"minAvailable": 1, "selector": {"matchLabels": {"app": "guarded"}}},
    }))
    .unwrap();
    Api::<PodDisruptionBudget>::namespaced(client.clone(), NAMESPACE)
        .create(&post, &budget)
        .await
        .unwrap();
}

/// The test's three pods, once they run and the budget counts its pod.
async fn ready_pods(client: &Client) -> Vec<Pod> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);
    let budgets: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), NAMESPACE);
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let list = pods
            .list(&ListParams::default().labels(SELECTOR))
            .await
            .unwrap()
            .items;
        let running = list
            .iter()
            .filter(|pod| pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
            .count();
        let healthy = budgets
            .get("guard")
            .await
            .unwrap()
            .status
            .is_some_and(|s| s.current_healthy == Some(1) && s.disruptions_allowed == Some(0));
        if running == 3 && healthy {
            return list;
        }
        assert!(Instant::now() < until, "pods not running: {list:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn cordoned(node: &Node) -> bool {
    node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true)
}

async fn exercise(
    server: McpServer,
    confirmer: Arc<ScriptedConfirmer>,
    client: Client,
    nodes: Api<Node>,
    node: String,
) {
    let pods: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);
    let before = ready_pods(&client).await;
    let uid_of = |prefix: &str| {
        before
            .iter()
            .find(|pod| pod.name_any().starts_with(prefix))
            .and_then(|pod| pod.uid())
            .unwrap()
    };
    let (web, guarded, bare) = (uid_of("web-"), uid_of("guarded-"), uid_of("bare"));

    // The preview tells the three apart and changes nothing.
    let preview = server
        .call(
            "test",
            "drain_node",
            args(json!({"name": node, "podSelector": SELECTOR, "preview": true})),
        )
        .await;
    assert!(!preview.is_error, "{}", preview.text);
    let text = &preview.text;
    assert!(text.contains("To evict (1):"), "{text}");
    assert!(text.contains("Blocked by a PodDisruptionBudget"), "{text}");
    assert!(text.contains("PodDisruptionBudget guard"), "{text}");
    assert!(text.contains("No controller"), "{text}");
    assert!(confirmer.requests().is_empty());
    assert!(!cordoned(&nodes.get(&node).await.unwrap()));

    // Not one pod of another namespace comes into this drain.
    let drain = server
        .call(
            "test",
            "drain_node",
            args(json!({"name": node, "podSelector": SELECTOR, "timeoutSeconds": 15})),
        )
        .await;
    assert!(!drain.is_error, "{}", drain.text);
    let text = &drain.text;
    assert!(text.contains("1 of 2 pods are gone"), "{text}");
    assert!(text.contains("guard"), "{text}");
    assert!(text.contains("Left in place"), "{text}");
    assert!(text.contains("stays cordoned"), "{text}");
    let requests = confirmer.requests();
    let [window] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(window.kind, RequestKind::Call);
    assert!(window.summary.contains(&node), "{}", window.summary);
    for pod in &before {
        let label = format!("{NAMESPACE}/{}", pod.name_any());
        assert!(window.details.contains(&label), "{}", window.details);
    }
    assert!(
        !window.details.contains("kube-system"),
        "{}",
        window.details
    );
    assert!(!window.allow_remember);
    assert!(!window.dangerous.is_empty());

    assert!(cordoned(&nodes.get(&node).await.unwrap()));
    let now: Vec<String> = pods
        .list(&ListParams::default().labels(SELECTOR))
        .await
        .unwrap()
        .items
        .iter()
        .filter_map(|pod| pod.uid())
        .collect();
    assert!(!now.contains(&web), "the evicted pod is still here");
    assert!(now.contains(&guarded), "the budget's pod was evicted");
    assert!(now.contains(&bare), "the pod without a controller is gone");

    // Cordoning a cordoned node asks nobody.
    let again = server
        .call("test", "cordon_node", args(json!({"name": node})))
        .await;
    assert!(again.text.contains("already"), "{}", again.text);
    assert_eq!(confirmer.requests().len(), 1);

    let uncordon = server
        .call("test", "uncordon_node", args(json!({"name": node})))
        .await;
    assert!(!uncordon.is_error, "{}", uncordon.text);
    assert!(!cordoned(&nodes.get(&node).await.unwrap()));

    let cordon = server
        .call("test", "cordon_node", args(json!({"name": node})))
        .await;
    assert!(!cordon.is_error, "{}", cordon.text);
    assert!(cordoned(&nodes.get(&node).await.unwrap()));
    let uncordon = server
        .call("test", "uncordon_node", args(json!({"name": node})))
        .await;
    assert!(!uncordon.is_error, "{}", uncordon.text);
    assert!(!cordoned(&nodes.get(&node).await.unwrap()));
    assert_eq!(confirmer.requests().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT=minikube)"]
async fn a_drain_keeps_to_what_the_user_saw_and_budgets_hold() {
    let (base, context) = config().await;
    let client = Client::try_from(base.clone()).unwrap();
    let nodes: Api<Node> = Api::all(client.clone());
    let list = tokio::time::timeout(Duration::from_secs(120), nodes.list(&ListParams::default()))
        .await
        .expect("the API server does not answer")
        .unwrap();
    let node = list.items[0].name_any();
    assert!(
        !cordoned(&list.items[0]),
        "{node} is cordoned already: someone else is at work, and uncordoning it here would undo \
         that"
    );
    set_up(&client).await;

    let policy = SecurityPolicy {
        mcp_enabled: true,
        ..SecurityPolicy::with_minimum(Access {
            profile: Profile::Deploy,
            nodes: true,
            ..Access::NONE
        })
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
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);

    // In a task of its own, so a failed assert still reaches the clean-up.
    let result = tokio::spawn(exercise(
        server,
        confirmer,
        client.clone(),
        nodes.clone(),
        node.clone(),
    ))
    .await;

    let schedulable = json!({"spec": {"unschedulable": null}});
    let uncordoned = nodes
        .patch(
            &node,
            &PatchParams::default(),
            &Patch::Strategic(schedulable),
        )
        .await;
    let _ = session.close().await;
    let namespaces: Api<Namespace> = Api::all(client);
    let deleted = namespaces.delete(NAMESPACE, &DeleteParams::default()).await;
    uncordoned.expect("uncordon the node");
    deleted.expect("delete the namespace");
    let until = Instant::now() + Duration::from_secs(90);
    while namespaces.get_opt(NAMESPACE).await.unwrap().is_some() {
        assert!(Instant::now() < until, "{NAMESPACE} is still there");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}
