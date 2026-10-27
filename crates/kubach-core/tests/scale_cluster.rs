//! The replica stepper's relative scale (#347) against a real cluster.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-347 \
//!     cargo test -p kubach-core --test scale_cluster -- --ignored --test-threads=1
//! ```
//!
//! The namespace must exist; the test creates and deletes objects in it only.

use k8s_openapi::api::apps::v1::Deployment;
use kubach_core::plugin::builtin::{DeploymentPlugin, action};
use kubach_core::plugin::{ActionParams, PluginError, ResourcePlugin as _};
use kube::api::{Api, DeleteParams, DynamicObject, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

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
    (kubach_core::redact::client(config).unwrap(), namespace)
}

fn dynamic(deploy: &Deployment) -> DynamicObject {
    let mut value = serde_json::to_value(deploy).unwrap();
    value["apiVersion"] = "apps/v1".into();
    value["kind"] = "Deployment".into();
    serde_json::from_value(value).unwrap()
}

fn step(delta: i32, allow_zero: bool) -> ActionParams {
    let mut params = ActionParams::new();
    params.insert(action::DELTA.into(), json!(delta));
    if allow_zero {
        params.insert(action::ALLOW_ZERO.into(), json!(true));
    }
    params
}

async fn replicas(deploys: &Api<Deployment>) -> i32 {
    let deploy = deploys.get("stepper").await.unwrap();
    deploy.spec.unwrap().replicas.unwrap()
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn steps_from_the_live_count_not_the_shown_one() {
    let (client, ns) = client().await;
    let deploys: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let deploy: Deployment = serde_json::from_value(json!({
        "metadata": {"name": "stepper"},
        "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "stepper"}},
                 "template": {"metadata": {"labels": {"app": "stepper"}},
                              "spec": {"terminationGracePeriodSeconds": 0, "containers": [{
                                  "name": "c", "image": "registry.k8s.io/pause:3.9"}]}}}
    }))
    .unwrap();
    let created = deploys
        .create(&PostParams::default(), &deploy)
        .await
        .unwrap();
    // What the UI drew: 3 replicas, never refreshed below.
    let shown = dynamic(&created);

    // 3 → «−» → 2.
    DeploymentPlugin
        .run_action(&client, &shown, &action::SCALE, &step(-1, false))
        .await
        .unwrap();
    assert_eq!(replicas(&deploys).await, 2);

    // kubectl scale to 5 behind the UI's back, then «−» from the stale
    // view (still 3): 4, not 2.
    deploys
        .patch_scale(
            "stepper",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec": {"replicas": 5}})),
        )
        .await
        .unwrap();
    DeploymentPlugin
        .run_action(&client, &shown, &action::SCALE, &step(-1, false))
        .await
        .unwrap();
    assert_eq!(replicas(&deploys).await, 4);

    // Down to 1, then an unconfirmed step to 0 is refused, a confirmed one
    // goes through, and nothing goes below 0.
    deploys
        .patch_scale(
            "stepper",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec": {"replicas": 1}})),
        )
        .await
        .unwrap();
    let refused = DeploymentPlugin
        .run_action(&client, &shown, &action::SCALE, &step(-1, false))
        .await;
    assert!(
        matches!(refused, Err(PluginError::Refused(_))),
        "{refused:?}"
    );
    assert_eq!(replicas(&deploys).await, 1);
    DeploymentPlugin
        .run_action(&client, &shown, &action::SCALE, &step(-1, true))
        .await
        .unwrap();
    assert_eq!(replicas(&deploys).await, 0);
    let negative = DeploymentPlugin
        .run_action(&client, &shown, &action::SCALE, &step(-1, true))
        .await;
    assert!(
        matches!(negative, Err(PluginError::Refused(_))),
        "{negative:?}"
    );
    assert_eq!(replicas(&deploys).await, 0);

    deploys
        .delete("stepper", &DeleteParams::default())
        .await
        .unwrap();
}
