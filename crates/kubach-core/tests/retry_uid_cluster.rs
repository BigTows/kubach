//! No server retry of writes and UID-guarded deletes (#345) against a real
//! cluster, through Kubach's own client stack. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-345 \
//!     cargo test -p kubach-core --test retry_uid_cluster -- --ignored --test-threads=1
//! ```
//!
//! The namespace must exist; the tests create and delete objects in it only.

use std::time::{Duration, Instant};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kubach_core::plugin::builtin::{ConfigMapPlugin, PodPlugin, action};
use kubach_core::plugin::{PluginError, ResourcePlugin as _};
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt as _};
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

/// `value` as the object the UI holds: typed and with its UID.
fn dynamic<K: serde::Serialize>(value: &K, api_version: &str, kind: &str) -> DynamicObject {
    let mut value = serde_json::to_value(value).unwrap();
    value["apiVersion"] = api_version.into();
    value["kind"] = kind.into();
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn evict_under_a_budget_is_refused_at_once() {
    let (client, ns) = client().await;
    let deploys: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let pdbs: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), &ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let deploy: Deployment = serde_json::from_value(json!({
        "metadata": {"name": "guarded"},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "guarded"}},
                 "template": {"metadata": {"labels": {"app": "guarded"}},
                              "spec": {"terminationGracePeriodSeconds": 0, "containers": [{
                                  "name": "c", "image": "busybox:1.37",
                                  "imagePullPolicy": "IfNotPresent",
                                  "command": ["sleep", "3600"]}]}}}
    }))
    .unwrap();
    deploys
        .create(&PostParams::default(), &deploy)
        .await
        .unwrap();
    let pdb: PodDisruptionBudget = serde_json::from_value(json!({
        "metadata": {"name": "guarded"},
        "spec": {"minAvailable": 1, "selector": {"matchLabels": {"app": "guarded"}}}
    }))
    .unwrap();
    pdbs.create(&PostParams::default(), &pdb).await.unwrap();

    // Running, and the budget has seen it (no disruption allowed).
    let mut pod = None;
    for _ in 0..240 {
        let list = pods
            .list(&ListParams::default().labels("app=guarded"))
            .await
            .unwrap();
        let running = list
            .items
            .into_iter()
            .find(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"));
        let observed = pdbs
            .get("guarded")
            .await
            .unwrap()
            .status
            .is_some_and(|s| s.current_healthy == Some(1) && s.disruptions_allowed == Some(0));
        if running.is_some() && observed {
            pod = running;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let pod = pod.expect("the guarded pod did not run");

    let object = dynamic(&pod, "v1", "Pod");
    let started = Instant::now();
    let result = PodPlugin
        .run_action(&client, &object, &action::EVICT, &Default::default())
        .await;
    let elapsed = started.elapsed();
    let refused = matches!(&result, Err(PluginError::Refused(reason))
        if reason.contains("PodDisruptionBudget guarded"));

    deploys
        .delete("guarded", &DeleteParams::background())
        .await
        .ok();
    pdbs.delete("guarded", &DeleteParams::default()).await.ok();

    assert!(refused, "{result:?}");
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    // Still there: the refused eviction is not repeated later.
    assert!(pods.get_opt(&pod.name_any()).await.unwrap().is_some());
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn delete_leaves_a_recreated_object_and_accepts_a_gone_one() {
    let (client, ns) = client().await;
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let map: ConfigMap = serde_json::from_value(json!({
        "metadata": {"name": "recreated"}, "data": {"v": "1"}
    }))
    .unwrap();
    // Left by an earlier failed run.
    maps.delete("recreated", &DeleteParams::default())
        .await
        .ok();
    let old = maps.create(&PostParams::default(), &map).await.unwrap();
    let stale = dynamic(&old, "v1", "ConfigMap");
    maps.delete("recreated", &DeleteParams::default())
        .await
        .unwrap();
    let new = maps.create(&PostParams::default(), &map).await.unwrap();
    assert_ne!(old.uid(), new.uid());

    // The stale object is not deleted: the new one stays.
    let result = ConfigMapPlugin
        .run_action(&client, &stale, &action::DELETE, &Default::default())
        .await;
    let kept = maps.get_opt("recreated").await.unwrap();
    assert!(
        matches!(&result, Err(PluginError::Refused(reason)) if reason.contains("recreated")),
        "{result:?}"
    );
    assert_eq!(kept.and_then(|m| m.uid()), new.uid());

    // The current one is deleted; deleting it again is fine.
    let current = dynamic(&new, "v1", "ConfigMap");
    for _ in 0..2 {
        ConfigMapPlugin
            .run_action(&client, &current, &action::DELETE, &Default::default())
            .await
            .unwrap();
    }
    assert!(maps.get_opt("recreated").await.unwrap().is_none());
}
