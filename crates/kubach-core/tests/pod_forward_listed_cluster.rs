//! A pod listed through the resource cache (the way the browser and its
//! details get it) is a port-forward source, and the forward serves (#372).
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-372 \
//!     cargo test -p kubach-core --test pod_forward_listed_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own
//! Deployment (image `nginx:1.27-alpine`, port 80 named `http`).

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use kubach_core::cache::{ResourceCache, WatchStatus};
use kubach_core::discovery::ApiCatalog;
use kubach_core::streams::{ForwardSource, LocalPort, PortForwardManager};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const NAME: &str = "kubach-pod-web";

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

async fn http_get(addr: std::net::SocketAddr) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

fn is_ready(pod: &DynamicObject) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod
            .data
            .pointer("/status/conditions")
            .and_then(|c| c.as_array())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c["type"] == "Ready" && c["status"] == "True")
            })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn forwards_a_pod_listed_by_the_cache() {
    let (client, ns) = client().await;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    deployments
        .delete(NAME, &DeleteParams::default())
        .await
        .ok();
    let deployment: Deployment = serde_json::from_value(json!({
        "metadata": {"name": NAME},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": NAME}},
            "template": {
                "metadata": {"labels": {"app": NAME}},
                "spec": {"containers": [{
                    "name": "nginx",
                    "image": "nginx:1.27-alpine",
                    "ports": [{"name": "http", "containerPort": 80, "protocol": "TCP"}]
                }]}
            }
        }
    }))
    .unwrap();
    for _ in 0..30 {
        match deployments
            .create(&PostParams::default(), &deployment)
            .await
        {
            Ok(_) => break,
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }

    // The cause of #372: list items come without `kind`.
    let raw: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::erase::<Pod>(&()));
    let selector = format!("app={NAME}");
    for _ in 0..120 {
        let listed = raw.list(&ListParams::default().labels(&selector)).await;
        if listed.is_ok_and(|list| list.items.iter().any(is_ready)) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let listed = raw
        .list(&ListParams::default().labels(&selector))
        .await
        .unwrap();
    assert!(listed.items.iter().any(is_ready), "pod not ready");
    assert!(listed.items.iter().all(|pod| pod.types.is_none()));

    // Through the cache the pod has its kind and is a forward source.
    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let kind = catalog.find("", "Pod").unwrap().clone();
    let cache = ResourceCache::new(client.clone());
    let subscription = cache.subscribe(&kind, Some(&ns));
    let mut changes = subscription.changes();
    let mut pod = None;
    for _ in 0..120 {
        tokio::time::timeout(Duration::from_secs(1), changes.next())
            .await
            .ok();
        let snapshot = subscription.snapshot();
        if snapshot.status == WatchStatus::Ready {
            pod = snapshot
                .objects
                .iter()
                .find(|pod| {
                    pod.metadata
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with(NAME))
                        && is_ready(pod)
                })
                .cloned();
            if pod.is_some() {
                break;
            }
        }
    }
    let pod = pod.expect("the cache lists the pod");
    assert_eq!(pod.types.as_ref().map(|t| t.kind.as_str()), Some("Pod"));
    let source = ForwardSource::from_object(&pod).expect("a listed pod is a forward source");
    assert_eq!(source.kind, "Pod");
    assert_eq!(source.ports.len(), 1);
    assert_eq!(source.ports[0].name.as_deref(), Some("http"));
    assert_eq!(source.ports[0].port, 80);

    // The same local port or the nearest free one, and nginx answers.
    let manager = PortForwardManager::new();
    let info = manager
        .start(&client, &source, 80, LocalPort::Near(80))
        .await
        .unwrap();
    assert_eq!(info.source, source.label());
    assert_eq!(info.remote_port, 80);
    assert!(http_get(info.local_addr).await.contains("200 OK"));
    manager.stop(info.id);

    deployments
        .delete(NAME, &DeleteParams::default())
        .await
        .ok();
}
