//! `fields` against a real cluster (#110): the API server's own list answers
//! (items without `kind`) go through masking before selection. Ignored by
//! default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-110 \
//!     cargo test -p kubach-mcp --test fields_cluster -- --ignored
//! ```
//!
//! The test creates `<namespace>` and `<namespace>-b` and a CRD and deletes
//! them at the end.

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_mcp::{
    Access, McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const GROUP: &str = "crd110.kubach-test.example.com";
const CRD: &str = "gizmos.crd110.kubach-test.example.com";

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

fn namespaces(base: &str) -> [String; 2] {
    [base.to_owned(), format!("{base}-b")]
}

async fn prepare(user: &Client, base: &str) {
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
    let all: Api<Namespace> = Api::all(user.clone());
    let gizmo =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(GROUP, "v1", "Gizmo"), "gizmos");
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
            &from_json(json!({
                "metadata": {"name": "app"},
                "data": {"DB_PASS": "hunter2-map", "DB_HOST": "db.local"},
            })),
        )
        .await
        .unwrap();
        let deployments: Api<Deployment> = Api::namespaced(user.clone(), &name);
        deployments
            .create(
                &PostParams::default(),
                &from_json(json!({
                    "metadata": {"name": "web"},
                    "spec": {
                        "replicas": 0,
                        "strategy": {"type": "Recreate"},
                        "selector": {"matchLabels": {"app": "web"}},
                        "template": {
                            "metadata": {"labels": {"app": "web"}},
                            "spec": {"containers": [{
                                "name": "app",
                                "image": "nginx:1",
                                "env": [{"name": "API_TOKEN", "value": "hunter2-env"}],
                            }]},
                        },
                    },
                })),
            )
            .await
            .unwrap();
        let gizmos: Api<DynamicObject> = Api::namespaced_with(user.clone(), &name, &gizmo);
        let mut object = DynamicObject::new("one", &gizmo);
        object.data = json!({"spec": {"password": "hunter2-crd", "size": 3}});
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

async fn read(server: &McpServer, tool: &str, call: Value) -> String {
    let outcome = server.call("test", tool, args(call)).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(!outcome.text.contains("hunter2"), "{}", outcome.text);
    outcome.text
}

async fn selections(base: &Config, context: &str, namespace: &str) {
    let names = namespaces(namespace);
    let policy = SecurityPolicy::with_minimum(Access::ALL);
    let spec = SessionSpec {
        context: context.to_owned(),
        server_url: base.cluster_url.to_string(),
        namespaces: names.to_vec(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(base, &spec, &policy).await.unwrap();
    let server = McpServer::new(
        session.clone(),
        Arc::new(ScriptedConfirmer::denying()),
        None,
    );

    // The case of the issue: a strategy per deployment, not their YAML.
    let text = read(
        &server,
        "list_resources",
        json!({"kind": "Deployment", "allNamespaces": true,
               "fields": ["spec.strategy.type", "spec.template.spec.containers[*].image",
                          "spec.nothing"]}),
    )
    .await;
    assert!(text.starts_with("Coverage: complete"), "{text}");
    assert_eq!(text.matches("type: Recreate").count(), 2, "{text}");
    assert_eq!(text.matches("image: nginx:1").count(), 2, "{text}");
    assert_eq!(text.matches("- spec.nothing").count(), 2, "{text}");
    for name in &names {
        assert!(text.contains(&format!("namespace: {name}\n")), "{text}");
    }
    assert!(!text.contains("uid"), "{text}");

    // Paths at what masking hides, through list items of the real API.
    for (kind, path) in [
        ("ConfigMap", "data.DB_PASS"),
        ("ConfigMap", "data"),
        ("Gizmo", "spec.password"),
        ("Gizmo", "spec"),
        (
            "Deployment",
            "spec.template.spec.containers[*].env[*].value",
        ),
    ] {
        for scope in [
            json!({"kind": kind, "namespace": names[0], "fields": [path]}),
            json!({"kind": kind, "allNamespaces": true, "fields": [path]}),
        ] {
            let text = read(&server, "list_resources", scope).await;
            assert!(text.contains("***"), "{kind} {path}: {text}");
        }
    }
    let text = read(
        &server,
        "get_resource",
        json!({"kind": "Gizmo", "name": "one", "namespace": names[0],
               "fields": ["spec.password", "spec.size"]}),
    )
    .await;
    assert!(text.contains("password: ***"), "{text}");
    assert!(text.contains("size: 3"), "{text}");

    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn fields_select_from_masked_objects() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    clean_up(&user, &namespace).await;
    prepare(&user, &namespace).await;
    let outcome = caught(selections(&base, &context, &namespace)).await;
    clean_up(&user, &namespace).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    let all: Api<Namespace> = Api::all(user.clone());
    for name in namespaces(&namespace) {
        assert!(all.get_opt(&name).await.unwrap().is_none(), "{name} left");
    }
    let crds: Api<CustomResourceDefinition> = Api::all(user);
    assert!(crds.get_opt(CRD).await.unwrap().is_none(), "the CRD left");
}

/// `future`, with a panic caught so the clean-up still runs.
async fn caught<T>(
    future: impl std::future::Future<Output = T>,
) -> Result<T, Box<dyn std::any::Any + Send>> {
    use futures::FutureExt as _;
    std::panic::AssertUnwindSafe(future).catch_unwind().await
}
