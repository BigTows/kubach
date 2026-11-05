//! `allNamespaces` and `fieldSelector` against a real cluster (#107): one
//! call reads several namespaces under their temporary ServiceAccounts, and
//! the session gets no cluster-wide right. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-107 \
//!     cargo test -p kubach-mcp --test all_namespaces_cluster -- --ignored
//! ```
//!
//! The test creates `<namespace>-a`, `-b`, `-c` and a CRD and deletes them
//! at the end. The opening of unknown namespaces is only exercised declined,
//! so no other namespace of the cluster is touched.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_mcp::{
    Access, Grant, McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, PostParams,
};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const GROUP: &str = "crd107.kubach-test.example.com";
const CRD: &str = "gizmos.crd107.kubach-test.example.com";

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

fn from_json<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn namespaces(base: &str) -> [String; 3] {
    ["a", "b", "c"].map(|suffix| format!("{base}-{suffix}"))
}

async fn prepare(user: &Client, base: &str) {
    let all: Api<Namespace> = Api::all(user.clone());
    for name in namespaces(base) {
        all.create(
            &PostParams::default(),
            &from_json(json!({"metadata": {"name": name}})),
        )
        .await
        .unwrap();
        let maps: Api<ConfigMap> = Api::namespaced(user.clone(), &name);
        maps.create(
            &PostParams::default(),
            &from_json(json!({"metadata": {"name": format!("map-of-{name}")}})),
        )
        .await
        .unwrap();
        let secrets: Api<Secret> = Api::namespaced(user.clone(), &name);
        secrets
            .create(
                &PostParams::default(),
                &from_json(json!({"metadata": {"name": "hidden"},
                                  "stringData": {"password": "hunter2-107"}})),
            )
            .await
            .unwrap();
    }
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    crds.create(
        &PostParams::default(),
        &from_json(json!({
            "metadata": {"name": CRD},
            "spec": {
                "group": GROUP,
                "scope": "Namespaced",
                "names": {"plural": "gizmos", "singular": "gizmo", "kind": "Gizmo"},
                "versions": [{
                    "name": "v1", "served": true, "storage": true,
                    "schema": {"openAPIV3Schema": {
                        "type": "object",
                        "x-kubernetes-preserve-unknown-fields": true,
                    }},
                }],
            },
        })),
    )
    .await
    .unwrap();
    let gizmo =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(GROUP, "v1", "Gizmo"), "gizmos");
    for name in namespaces(base) {
        let gizmos: Api<DynamicObject> = Api::namespaced_with(user.clone(), &name, &gizmo);
        let object = DynamicObject::new(&format!("gizmo-of-{name}"), &gizmo);
        let mut created = false;
        for _ in 0..60 {
            if gizmos.create(&PostParams::default(), &object).await.is_ok() {
                created = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(created, "the CRD was not served in time");
    }
}

async fn clean_up(user: &Client, base: &str) {
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    let _ = crds.delete(CRD, &DeleteParams::default()).await;
    let all: Api<Namespace> = Api::all(user.clone());
    for name in namespaces(base) {
        let _ = all.delete(&name, &DeleteParams::default()).await;
    }
    for name in namespaces(base) {
        for _ in 0..120 {
            if matches!(all.get_opt(&name).await, Ok(None)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

fn spec(base: &Config, context: &str, pinned: Vec<String>) -> SessionSpec {
    SessionSpec {
        context: context.to_owned(),
        server_url: base.cluster_url.to_string(),
        namespaces: pinned,
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    }
}

/// Names of the cluster-wide RBAC objects, to see that none is added.
async fn cluster_rbac(user: &Client) -> Vec<String> {
    let roles: Api<ClusterRole> = Api::all(user.clone());
    let bindings: Api<ClusterRoleBinding> = Api::all(user.clone());
    let mut names: Vec<String> = roles
        .list(&ListParams::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .filter_map(|role| role.metadata.name)
        .collect();
    names.extend(
        bindings
            .list(&ListParams::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .filter_map(|binding| binding.metadata.name),
    );
    names.retain(|name| name.contains("kubach"));
    names.sort();
    names
}

async fn pinned_session(base: &Config, context: &str, namespace: &str, user: &Client) {
    let names = namespaces(namespace);
    let policy = SecurityPolicy::with_minimum(Access::ALL);
    let session = Session::start(base, &spec(base, context, names.to_vec()), &policy)
        .await
        .unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let rbac_before = cluster_rbac(user).await;

    // Three fresh namespaces in one call: each gets its ServiceAccount, and
    // the first read does not lose the race with its new Role.
    let call = args(json!({"kind": "ConfigMap", "allNamespaces": true}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.starts_with("Coverage: complete. All 3"),
        "{}",
        outcome.text
    );
    for name in &names {
        assert!(
            outcome.text.contains(&format!("map-of-{name}")),
            "{}",
            outcome.text
        );
        assert_eq!(
            session.mode_in(name),
            kubach_mcp::SessionMode::TemporaryServiceAccount
        );
    }
    assert!(confirmer.requests().is_empty());
    assert_eq!(cluster_rbac(user).await, rbac_before);

    // A namespace's client still cannot read cluster-wide.
    let client = session.client_in(&names[0]).unwrap();
    let everywhere: Api<ConfigMap> = Api::all(client);
    assert!(everywhere.list(&ListParams::default()).await.is_err());

    // Field selectors on the real API server.
    let call = args(json!({"kind": "ConfigMap", "allNamespaces": true,
                           "fieldSelector": format!("metadata.name=map-of-{}", names[1])}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(
        outcome.text.contains("(1 configmaps in 3 namespaces)"),
        "{}",
        outcome.text
    );
    let call = args(json!({"kind": "Pod", "namespace": names[0],
                           "fieldSelector": "status.phase!=Running,spec.nodeName=minikube"}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    for call in [
        json!({"kind": "ConfigMap", "allNamespaces": true, "fieldSelector": "spec.nope=1"}),
        json!({"kind": "ConfigMap", "namespace": names[0], "fieldSelector": "spec.nope=1"}),
    ] {
        let outcome = server.call("test", "list_resources", args(call)).await;
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(
            outcome.text.contains("does not accept fieldSelector"),
            "{}",
            outcome.text
        );
    }

    // Events go the same way.
    let call = args(json!({"allNamespaces": true}));
    let outcome = server.call("test", "list_events", call).await;
    assert!(
        outcome.text.starts_with("Coverage: complete. All 3"),
        "{}",
        outcome.text
    );

    // Secrets stay out, whatever the flag.
    let call = args(json!({"kind": "Secret", "allNamespaces": true}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(!outcome.text.contains("hunter2-107"), "{}", outcome.text);

    // Custom resources: one right per namespace, under each ServiceAccount.
    let call = args(json!({"kind": "Gizmo", "apiVersion": format!("{GROUP}/v1"),
                           "allNamespaces": true}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.starts_with("Coverage: complete. All 3"),
        "{}",
        outcome.text
    );
    for name in &names {
        assert!(
            outcome.text.contains(&format!("gizmo-of-{name}")),
            "{}",
            outcome.text
        );
    }

    // A revoked namespace is out at once, and the answer says so.
    session
        .revoke(&Grant::read(names[2].clone()))
        .await
        .unwrap();
    let call = args(json!({"kind": "ConfigMap", "allNamespaces": true}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        !outcome.text.contains(&format!("map-of-{}", names[2])),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("refused or revoked"),
        "{}",
        outcome.text
    );

    session.close().await.unwrap();
}

async fn open_session(base: &Config, context: &str, namespace: &str) {
    let names = namespaces(namespace);
    let policy = SecurityPolicy::with_minimum(Access::ALL);
    let session = Session::start(base, &spec(base, context, Vec::new()), &policy)
        .await
        .unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let one = args(json!({"kind": "ConfigMap", "namespace": names[0]}));
    let outcome = server.call("test", "list_resources", one).await;
    assert!(!outcome.is_error, "{}", outcome.text);

    // Declined: the open namespace is read, the cluster's others are named,
    // and none gets a ServiceAccount.
    let call = args(json!({"kind": "ConfigMap", "allNamespaces": true}));
    let outcome = server.call("test", "list_resources", call).await;
    assert!(
        outcome.text.starts_with("Coverage: PARTIAL"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains(&format!("map-of-{}", names[0])),
        "{}",
        outcome.text
    );
    assert!(outcome.text.contains("kube-system"), "{}", outcome.text);
    assert!(
        !outcome.text.contains(&format!("map-of-{}", names[1])),
        "{}",
        outcome.text
    );
    assert_eq!(confirmer.requests().len(), 1);
    assert_eq!(session.boundaries().len(), 1);

    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn all_namespaces_stays_within_the_session() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    // Nothing of the session's kinds is left over from a failed run.
    clean_up(&user, &namespace).await;
    prepare(&user, &namespace).await;
    let outcome = caught(async {
        pinned_session(&base, &context, &namespace, &user).await;
        open_session(&base, &context, &namespace).await;
    })
    .await;
    clean_up(&user, &namespace).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// `future`, with a panic caught so the clean-up still runs.
async fn caught<T>(
    future: impl std::future::Future<Output = T>,
) -> Result<T, Box<dyn std::any::Any + Send>> {
    use futures::FutureExt as _;
    std::panic::AssertUnwindSafe(future).catch_unwind().await
}
