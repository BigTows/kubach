//! Objects in the resource cache come without `managedFields` and the
//! last-applied annotation, while a GET of the same object (what the editor
//! does) still has them (#168). Also prints how much of real objects these
//! two fields take. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-168 \
//!     cargo test -p kubach-core --test cache_trim_cluster -- --ignored --nocapture
//! ```
//!
//! The test creates the namespace if it is missing and deletes it then; its
//! Deployment uses the image `nginx:1.27-alpine`.

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kubach_core::cache::{ResourceCache, Subscription, WatchStatus};
use kubach_core::discovery::ApiCatalog;
use kubach_core::secrets::mask::LAST_APPLIED;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const NAME: &str = "kubach-cache-trim";

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

fn size(object: &DynamicObject) -> usize {
    serde_json::to_vec(object).unwrap().len()
}

/// `object` without the two fields the cache drops.
fn trimmed(object: &DynamicObject) -> DynamicObject {
    let mut object = object.clone();
    object.metadata.managed_fields = None;
    if let Some(annotations) = &mut object.metadata.annotations {
        annotations.remove(LAST_APPLIED);
    }
    object
}

fn report(label: &str, objects: &[DynamicObject]) {
    let before: usize = objects.iter().map(size).sum();
    let after: usize = objects.iter().map(|object| size(&trimmed(object))).sum();
    println!(
        "{label}: {} object(s), {before} -> {after} bytes of JSON (-{:.1}%)",
        objects.len(),
        100.0 * (before - after) as f64 / before.max(1) as f64
    );
}

/// The objects of a loaded watch.
async fn loaded(subscription: &Subscription) -> Vec<std::sync::Arc<DynamicObject>> {
    let mut changes = subscription.changes();
    for _ in 0..60 {
        let snapshot = subscription.snapshot();
        if snapshot.status == WatchStatus::Ready {
            return snapshot.objects.to_vec();
        }
        tokio::time::timeout(Duration::from_secs(1), changes.next())
            .await
            .ok();
    }
    panic!("the watch did not load");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn cached_objects_have_no_managed_fields_or_last_applied() {
    let (client, ns) = client().await;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let namespace: Namespace = serde_json::from_value(json!({"metadata": {"name": ns}})).unwrap();
    let created_namespace = namespaces
        .create(&PostParams::default(), &namespace)
        .await
        .is_ok();

    // A Deployment the way `kubectl apply` leaves it: with the manifest in
    // the last-applied annotation.
    let manifest = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": NAME, "namespace": ns, "labels": {"app": NAME}},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": NAME}},
            "template": {
                "metadata": {"labels": {"app": NAME}},
                "spec": {"containers": [{
                    "name": "nginx",
                    "image": "nginx:1.27-alpine",
                    "ports": [{"name": "http", "containerPort": 80, "protocol": "TCP"}],
                    "resources": {
                        "requests": {"cpu": "10m", "memory": "16Mi"},
                        "limits": {"memory": "64Mi"}
                    }
                }]}
            }
        }
    });
    let mut deployment: Deployment = serde_json::from_value(manifest.clone()).unwrap();
    deployment.metadata.annotations = Some(
        [(
            LAST_APPLIED.to_owned(),
            serde_json::to_string(&manifest).unwrap() + "\n",
        )]
        .into(),
    );
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    deployments
        .delete(NAME, &DeleteParams::default())
        .await
        .ok();
    let mut created = false;
    for _ in 0..30 {
        match deployments
            .create(&PostParams::default(), &deployment)
            .await
        {
            Ok(_) => {
                created = true;
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    assert!(created, "the Deployment was not created");

    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let cache = ResourceCache::new(client.clone());
    let selector = ListParams::default().labels(&format!("app={NAME}"));

    // Wait until the Deployment has a pod (so both have several managers).
    let raw_pods: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::erase::<Pod>(&()));
    for _ in 0..120 {
        if raw_pods.list(&selector).await.is_ok_and(|list| {
            list.items
                .iter()
                .any(|pod| pod.data.get("status").is_some())
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    for (group, kind_name) in [("apps", "Deployment"), ("", "Pod")] {
        let kind = catalog.find(group, kind_name).unwrap().clone();
        let api: Api<DynamicObject> = kubach_core::discovery::api(&client, &kind, Some(&ns));

        // What the editor reads: the whole object.
        let fresh = api.list(&selector).await.unwrap().items;
        assert!(!fresh.is_empty(), "no {kind_name} in the cluster");
        for object in &fresh {
            assert!(object.metadata.managed_fields.is_some(), "{kind_name}");
        }
        if kind_name == "Deployment" {
            let annotations = fresh[0].metadata.annotations.as_ref().unwrap();
            assert!(annotations.contains_key(LAST_APPLIED));
        }
        report(&format!("{kind_name} of the test"), &fresh);

        // What the cache keeps.
        let subscription = cache.subscribe(&kind, Some(&ns));
        let cached = loaded(&subscription).await;
        let cached: Vec<_> = cached
            .iter()
            .filter(|object| {
                let labels = object.metadata.labels.as_ref();
                labels
                    .and_then(|labels| labels.get("app"))
                    .map(String::as_str)
                    == Some(NAME)
            })
            .collect();
        assert!(!cached.is_empty(), "no cached {kind_name}");
        for object in cached {
            assert!(object.metadata.managed_fields.is_none(), "{kind_name}");
            let annotations = object.metadata.annotations.as_ref();
            assert!(annotations.is_none_or(|a| !a.contains_key(LAST_APPLIED)));
            let text = serde_json::to_string(object.as_ref()).unwrap();
            assert!(!text.contains("managedFields"), "{text}");
            // The rest of the object is there.
            assert_eq!(object.types.as_ref().unwrap().kind, kind_name);
            assert!(object.metadata.uid.is_some());
            assert!(object.metadata.resource_version.is_some());
            assert!(object.data.get("spec").is_some());
            // Nothing else is lost: the same version from the API, trimmed,
            // is the cached object.
            if let Some(same) = fresh.iter().find(|fresh| {
                fresh.metadata.uid == object.metadata.uid
                    && fresh.metadata.resource_version == object.metadata.resource_version
            }) {
                let mut expected = trimmed(same);
                if expected
                    .metadata
                    .annotations
                    .as_ref()
                    .is_some_and(|a| a.is_empty())
                {
                    expected.metadata.annotations = None;
                }
                assert_eq!(expected.metadata, object.metadata);
                assert_eq!(expected.data, object.data);
            }
        }
    }

    // The share of the two fields in everything the cluster has of some kinds.
    for (group, kind_name) in [
        ("", "Pod"),
        ("apps", "Deployment"),
        ("apps", "ReplicaSet"),
        ("", "ConfigMap"),
        ("", "Service"),
        ("", "Node"),
    ] {
        let kind = catalog.find(group, kind_name).unwrap().clone();
        let api: Api<DynamicObject> = kubach_core::discovery::api(&client, &kind, None);
        let all = api.list(&ListParams::default()).await.unwrap().items;
        report(&format!("all {kind_name}s of the cluster"), &all);
    }

    deployments
        .delete(NAME, &DeleteParams::default())
        .await
        .ok();
    if created_namespace {
        namespaces.delete(&ns, &DeleteParams::default()).await.ok();
    }
}
