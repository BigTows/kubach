//! "Who can" graph against a real cluster. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-117 \
//!     cargo test -p kubach-core --test rbac_graph_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own
//! ServiceAccount, Role and RoleBindings. Cluster-wide objects are only read.

use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kubach_core::rbac::{
    AccessGraph, GraphNodeKind, RbacSnapshot, ResourceAccess, Subject, who_can,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const SA: &str = "kubach-graph-ci";
const ROLE: &str = "kubach-graph-reader";
const READ: &str = "kubach-graph-read";
const VIEW: &str = "kubach-graph-view";

async fn client() -> (Client, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    (Client::try_from(config).unwrap(), namespace)
}

async fn cleanup(client: &Client, ns: &str) {
    let params = DeleteParams::default();
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), ns);
    for name in [READ, VIEW] {
        bindings.delete(name, &params).await.ok();
    }
    let roles: Api<Role> = Api::namespaced(client.clone(), ns);
    roles.delete(ROLE, &params).await.ok();
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), ns);
    accounts.delete(SA, &params).await.ok();
}

fn from<T: k8s_openapi::serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn graph_of_namespace_grants() {
    let (client, ns) = client().await;
    cleanup(&client, &ns).await;
    let post = PostParams::default();

    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), &ns);
    accounts
        .create(&post, &from(json!({ "metadata": { "name": SA } })))
        .await
        .unwrap();
    let roles: Api<Role> = Api::namespaced(client.clone(), &ns);
    roles
        .create(
            &post,
            &from(json!({
                "metadata": { "name": ROLE },
                "rules": [{ "apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list"] }],
            })),
        )
        .await
        .unwrap();
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), &ns);
    let rbac = "rbac.authorization.k8s.io";
    bindings
        .create(
            &post,
            &from(json!({
                "metadata": { "name": READ },
                "roleRef": { "apiGroup": rbac, "kind": "Role", "name": ROLE },
                "subjects": [
                    { "kind": "ServiceAccount", "name": SA },
                    { "kind": "User", "name": "kubach-graph-alice", "apiGroup": rbac },
                ],
            })),
        )
        .await
        .unwrap();
    // The built-in `view` ClusterRole, bound only in this namespace.
    bindings
        .create(
            &post,
            &from(json!({
                "metadata": { "name": VIEW },
                "roleRef": { "apiGroup": rbac, "kind": "ClusterRole", "name": "view" },
                "subjects": [{ "kind": "User", "name": "kubach-graph-alice", "apiGroup": rbac }],
            })),
        )
        .await
        .unwrap();

    let snapshot = RbacSnapshot::fetch(&client).await;
    cleanup(&client, &ns).await;
    let snapshot = snapshot.unwrap();

    let access = ResourceAccess::new("get", "", "pods").in_namespace(ns.clone());
    let graph = AccessGraph::from_grants(&who_can(&snapshot, &access));
    let find = |wanted: &GraphNodeKind| {
        graph
            .nodes
            .iter()
            .position(|node| &node.kind == wanted)
            .unwrap_or_else(|| panic!("no node {wanted:?}"))
    };
    let alice = find(&GraphNodeKind::Subject(Subject::User(
        "kubach-graph-alice".into(),
    )));
    let account = find(&GraphNodeKind::Subject(Subject::ServiceAccount {
        namespace: ns.clone(),
        name: SA.into(),
    }));

    // alice → two bindings → reader Role and view ClusterRole → two rules.
    let chain = graph.chain(alice);
    let roles: Vec<String> = chain
        .iter()
        .filter_map(|&ix| match &graph.nodes[ix].kind {
            GraphNodeKind::Role(role) => Some(format!("{} {}", role.kind, role.name)),
            _ => None,
        })
        .collect();
    assert!(roles.contains(&format!("Role {ROLE}")), "{roles:?}");
    assert!(roles.contains(&"ClusterRole view".to_owned()), "{roles:?}");
    assert_eq!(graph.edges_within(&chain).len(), 6);

    // The ServiceAccount shares alice's binding node.
    let shared: Vec<usize> = graph
        .chain(account)
        .into_iter()
        .filter(|ix| chain.contains(ix))
        .collect();
    assert_eq!(shared.len(), 3, "binding, role and rule are shared");

    for edge in &graph.edges {
        assert_eq!(
            graph.nodes[edge.from].column + 1,
            graph.nodes[edge.to].column
        );
    }
}
