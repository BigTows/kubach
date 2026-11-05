//! `wait_for_condition` against a real cluster (#109), under a temporary
//! ServiceAccount. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-109 \
//!     cargo test -p kubach-mcp --test wait_cluster -- --ignored
//! ```
//!
//! The test creates the namespace and a CRD of its own and deletes both at
//! the end, so point it at a namespace that does not exist yet.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::{Namespace, Pod};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_mcp::{
    McpServer, Profile, ScriptedConfirmer, SecurityPolicy, Session, SessionMode, SessionSpec,
};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const GROUP: &str = "wait109.kubach-test.example.com";
const CRD: &str = "gizmos.wait109.kubach-test.example.com";

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

fn pod(name: &str, memory: &str) -> Pod {
    from_json(json!({
        "metadata": {"name": name, "labels": {"app": "kubach-wait"}},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "pause",
                "image": "registry.k8s.io/pause:3.9",
                "resources": {"requests": {"memory": memory}},
            }],
        },
    }))
}

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
    let gizmos: Api<DynamicObject> = Api::namespaced_with(user.clone(), namespace, &gizmo);
    let probe = DynamicObject::new("probe", &gizmo);
    for _ in 0..60 {
        if gizmos.create(&PostParams::default(), &probe).await.is_ok() {
            let _ = gizmos.delete("probe", &DeleteParams::default()).await;
            return gizmos;
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
}

async fn scenario(base: &Config, context: String, namespace: &str, gizmos: Api<DynamicObject>) {
    let user = Client::try_from(base.clone()).unwrap();
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    // The default minimum: reading built-in kinds, no custom resources.
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let session = Session::start(base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let pods: Api<Pod> = Api::namespaced(user, namespace);
    let wait = |value: Value| server.call("test", "wait_for_condition", args(value));

    // A pod that appears only after the wait began, then becomes ready.
    let created = {
        let pods = pods.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            pods.create(&PostParams::default(), &pod("kubach-wait", "4Mi"))
                .await
                .unwrap();
        })
    };
    let named = |condition: &str, timeout: u64| {
        json!({"kind": "Pod", "name": "kubach-wait", "namespace": namespace,
               "condition": condition, "timeoutSeconds": timeout})
    };
    let exists = wait(named("exists", 60)).await;
    assert!(!exists.is_error, "{}", exists.text);
    created.await.unwrap();
    assert_eq!(
        session.mode_in(namespace),
        SessionMode::TemporaryServiceAccount,
        "the read was the temporary ServiceAccount's"
    );
    let ready = wait(named("Ready=True", 120)).await;
    assert!(!ready.is_error, "{}", ready.text);

    // All objects, and at least one: the second pod cannot be scheduled.
    pods.create(&PostParams::default(), &pod("kubach-wait-stuck", "100Ti"))
        .await
        .unwrap();
    let selector = json!({"kind": "Pod", "labelSelector": "app=kubach-wait",
        "namespace": namespace, "condition": "Ready=True", "timeoutSeconds": 5});
    let started = Instant::now();
    let stuck = wait(selector).await;
    assert!(stuck.is_error, "{}", stuck.text);
    assert!(started.elapsed() < Duration::from_secs(9));
    assert!(
        stuck.text.contains("1 of 2 have Ready=True"),
        "{}",
        stuck.text
    );
    assert!(stuck.text.contains("not scheduled:"), "{}", stuck.text);
    assert!(stuck.text.contains("Met means: every"), "{}", stuck.text);
    let none = json!({"kind": "Pod", "labelSelector": "app=kubach-none",
        "namespace": namespace, "condition": "Ready=True", "timeoutSeconds": 3});
    let none = wait(none).await;
    assert!(none.is_error, "{}", none.text);
    assert!(none.text.contains("no object matches"), "{}", none.text);

    // Going away.
    for name in ["kubach-wait", "kubach-wait-stuck"] {
        pods.delete(name, &DeleteParams::default()).await.unwrap();
    }
    let deleted = wait(named("deleted", 60)).await;
    assert!(!deleted.is_error, "{}", deleted.text);
    let gone = json!({"kind": "Pod", "labelSelector": "app=kubach-wait",
        "namespace": namespace, "condition": "deleted", "timeoutSeconds": 60});
    let gone = wait(gone).await;
    assert!(!gone.is_error, "{}", gone.text);

    // A custom resource: the first call asks, the Role follows, and the
    // object appears under nothing more than `get`.
    let gizmo_api =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(GROUP, "v1", "Gizmo"), "gizmos");
    let created = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        gizmos
            .create(
                &PostParams::default(),
                &DynamicObject::new("first", &gizmo_api),
            )
            .await
            .unwrap();
    });
    let custom = json!({"kind": "Gizmo", "apiVersion": format!("{GROUP}/v1"), "name": "first",
        "namespace": namespace, "condition": "exists", "timeoutSeconds": 60});
    let custom = wait(custom).await;
    assert!(!custom.is_error, "{}", custom.text);
    created.await.unwrap();
    assert_eq!(confirmer.requests().len(), 1, "one question, for crds");

    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn waits_on_a_real_cluster() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let gizmos = prepare(&user, &namespace).await;
    // Cleans up however the scenario ends.
    let result = tokio::spawn({
        let (base, namespace) = (base.clone(), namespace.clone());
        async move { scenario(&base, context, &namespace, gizmos).await }
    })
    .await;
    clean_up(&user, &namespace).await;
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}
