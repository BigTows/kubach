//! Custom resources under a temporary ServiceAccount against a real cluster
//! (#103): the default minimum has no custom resources, the first read asks
//! the user and then returns the data, and a change goes through as well.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-103 \
//!     cargo test -p kubach-mcp --test crd_access_cluster -- --ignored
//! ```
//!
//! The test creates the namespace and a CRD of its own and deletes both at
//! the end, so point it at a namespace that does not exist yet.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::api::rbac::v1::Role;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_mcp::{
    Access, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec,
};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const GROUP: &str = "crd103.kubach-test.example.com";
const CRD: &str = "gadgets.crd103.kubach-test.example.com";

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

/// The namespace, the CRD and object `first`, once the kind is served.
async fn prepare(user: &Client, namespace: &str) -> Api<DynamicObject> {
    let namespaces: Api<Namespace> = Api::all(user.clone());
    namespaces
        .create(
            &PostParams::default(),
            &from_json(json!({"metadata": {"name": namespace}})),
        )
        .await
        .unwrap();
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    crds.create(
        &PostParams::default(),
        &from_json(json!({
            "metadata": {"name": CRD},
            "spec": {
                "group": GROUP,
                "scope": "Namespaced",
                "names": {"plural": "gadgets", "singular": "gadget", "kind": "Gadget"},
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
    let gadget =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(GROUP, "v1", "Gadget"), "gadgets");
    let gadgets: Api<DynamicObject> = Api::namespaced_with(user.clone(), namespace, &gadget);
    let first = DynamicObject::new("first", &gadget);
    for _ in 0..60 {
        if gadgets.create(&PostParams::default(), &first).await.is_ok() {
            return gadgets;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the CRD was not served in time");
}

async fn clean_up(user: &Client, namespace: &str) {
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    let _ = crds.delete(CRD, &DeleteParams::default()).await;
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let _ = namespaces.delete(namespace, &DeleteParams::default()).await;
    for _ in 0..120 {
        if matches!(namespaces.get_opt(namespace).await, Ok(None)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("namespace {namespace} was not deleted in time");
}

fn asked_custom_resources(confirmer: &ScriptedConfirmer) -> usize {
    confirmer
        .requests()
        .iter()
        .filter(|request| matches!(&request.kind, RequestKind::Access { raise, .. } if raise.crds))
        .count()
}

async fn check(base: &Config, context: String, namespace: &str, gadgets: &Api<DynamicObject>) {
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let policy = SecurityPolicy::default();
    assert_eq!(policy.minimum, Access::NONE);
    let list = json!({"kind": "Gadget", "apiVersion": format!("{GROUP}/v1"),
                      "namespace": namespace});

    // A refusal is said in words, and the API is not reached.
    let session = Session::start(base, &spec, &policy).await.unwrap();
    let refusing = Arc::new(ScriptedConfirmer::denying());
    let server = McpServer::new(session.clone(), refusing.clone(), None);
    let outcome = server
        .call("test", "list_resources", args(list.clone()))
        .await;
    assert!(outcome.is_error);
    assert!(outcome.text.contains("did not allow"), "{}", outcome.text);
    assert_eq!(asked_custom_resources(&refusing), 1);
    session.close().await.unwrap();

    // Allowed: the very next call returns the data, no retry needed.
    let session = Session::start(base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let outcome = server.call("test", "list_kinds", Map::new()).await;
    assert!(outcome.text.contains("need the user's approval"));
    let outcome = server
        .call("test", "list_resources", args(list.clone()))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("first"), "{}", outcome.text);
    assert_eq!(asked_custom_resources(&confirmer), 1);
    assert_eq!(
        session.mode_in(namespace),
        kubach_mcp::SessionMode::TemporaryServiceAccount
    );

    let mut get = list.clone();
    get["name"] = json!("first");
    let outcome = server.call("test", "get_resource", args(get)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_eq!(asked_custom_resources(&confirmer), 1);

    // The Role names this kind and only the verbs used.
    let roles: Api<Role> = Api::namespaced(gadgets.clone().into_client(), namespace);
    let role = roles
        .get(&format!("kubach-mcp-{}", session.id()))
        .await
        .unwrap();
    let rules = role.rules.unwrap_or_default();
    let rule = rules
        .iter()
        .find(|rule| rule.api_groups.as_deref() == Some(&[GROUP.to_owned()][..]))
        .expect("a rule for the custom resource");
    assert_eq!(rule.resources.as_deref(), Some(&["gadgets".to_owned()][..]));
    assert_eq!(rule.verbs, ["get", "list"]);

    // Changing one under the same ServiceAccount: asked, then applied.
    let yaml = format!(
        "apiVersion: {GROUP}/v1\nkind: Gadget\nmetadata:\n  name: second\nspec:\n  size: 1\n"
    );
    let outcome = server
        .call(
            "test",
            "apply_manifest",
            args(json!({"yaml": yaml, "namespace": namespace, "dryRun": false})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(gadgets.get("second").await.is_ok());

    let delete = json!({"kind": "Gadget", "apiVersion": format!("{GROUP}/v1"),
                        "namespace": namespace, "name": "second"});
    let outcome = server.call("test", "delete_resource", args(delete)).await;
    assert!(!outcome.is_error, "{}", outcome.text);

    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn custom_resources_work_under_a_temporary_service_account() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let gadgets = prepare(&user, &namespace).await;
    // Whatever the checks say, nothing stays behind in the cluster.
    let outcome = caught(check(&base, context, &namespace, &gadgets)).await;
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
