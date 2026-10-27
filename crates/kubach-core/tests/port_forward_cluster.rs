//! Port-forwards to a Service and a Deployment against a real cluster.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-81 \
//!     cargo test -p kubach-core --test port_forward_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own
//! Deployment and Services (image `nginx:1.27-alpine`).

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Pod, Service};
use kubach_core::streams::{ForwardSource, PortForwardManager, is_pod_ready};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, Resource};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    deployments
        .delete("kubach-web", &DeleteParams::default())
        .await
        .ok();
    let services: Api<Service> = Api::namespaced(client.clone(), ns);
    for name in ["kubach-web", "kubach-none"] {
        services.delete(name, &DeleteParams::default()).await.ok();
    }
}

/// Fetches an object the way the browser holds it.
async fn object<K: Resource<DynamicType = ()>>(
    client: &Client,
    ns: &str,
    name: &str,
) -> DynamicObject {
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), ns, &ApiResource::erase::<K>(&()));
    api.get(name).await.unwrap()
}

async fn ready_pods(client: &Client, ns: &str) -> Vec<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    pods.list(&ListParams::default().labels("app=kubach-web"))
        .await
        .unwrap()
        .items
        .iter()
        .filter(|pod| is_pod_ready(pod))
        .filter_map(|pod| pod.metadata.name.clone())
        .collect()
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn forwards_services_and_workloads() {
    let (client, ns) = client().await;
    cleanup(&client, &ns).await;
    for _ in 0..60 {
        if ready_pods(&client, &ns).await.is_empty()
            && Api::<Service>::namespaced(client.clone(), &ns)
                .get_opt("kubach-web")
                .await
                .unwrap()
                .is_none()
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let deployment: Deployment = serde_json::from_value(json!({
        "metadata": {"name": "kubach-web"},
        "spec": {
            "replicas": 2,
            "selector": {"matchLabels": {"app": "kubach-web"}},
            "template": {
                "metadata": {"labels": {"app": "kubach-web"}},
                "spec": {"containers": [{
                    "name": "nginx",
                    "image": "nginx:1.27-alpine",
                    "ports": [{"name": "web", "containerPort": 80}]
                }]}
            }
        }
    }))
    .unwrap();
    Api::<Deployment>::namespaced(client.clone(), &ns)
        .create(&PostParams::default(), &deployment)
        .await
        .unwrap();
    let services: Api<Service> = Api::namespaced(client.clone(), &ns);
    let service: Service = serde_json::from_value(json!({
        "metadata": {"name": "kubach-web"},
        "spec": {
            "selector": {"app": "kubach-web"},
            "ports": [
                {"name": "http", "port": 8080, "targetPort": "web"},
                {"name": "dns", "port": 53, "protocol": "UDP"}
            ]
        }
    }))
    .unwrap();
    services
        .create(&PostParams::default(), &service)
        .await
        .unwrap();
    let nothing: Service = serde_json::from_value(json!({
        "metadata": {"name": "kubach-none"},
        "spec": {"selector": {"app": "kubach-nothing"}, "ports": [{"port": 80}]}
    }))
    .unwrap();
    services
        .create(&PostParams::default(), &nothing)
        .await
        .unwrap();

    for _ in 0..120 {
        if ready_pods(&client, &ns).await.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(ready_pods(&client, &ns).await.len(), 2, "pods not ready");

    let manager = PortForwardManager::new();

    // Service port 8080 → targetPort "web" → containerPort 80.
    let svc =
        ForwardSource::from_object(&object::<Service>(&client, &ns, "kubach-web").await).unwrap();
    let info = manager.start(&client, &svc, 8080, 0).await.unwrap();
    assert_eq!(info.source, "svc/kubach-web");
    assert_eq!((info.source_port, info.remote_port), (8080, 80));
    assert!(ready_pods(&client, &ns).await.contains(&info.pod));
    assert!(http_get(info.local_addr).await.contains("200 OK"));

    // UDP ports are refused.
    let error = manager.start(&client, &svc, 53, 0).await.unwrap_err();
    assert!(error.to_string().contains("TCP only"), "{error}");

    // No pods behind the selector.
    let none =
        ForwardSource::from_object(&object::<Service>(&client, &ns, "kubach-none").await).unwrap();
    let error = manager.start(&client, &none, 80, 0).await.unwrap_err();
    assert!(error.to_string().contains("no pods match"), "{error}");

    // Deployment: template containerPort 80.
    let deploy =
        ForwardSource::from_object(&object::<Deployment>(&client, &ns, "kubach-web").await)
            .unwrap();
    assert_eq!(deploy.ports[0].port, 80);
    let deploy_info = manager.start(&client, &deploy, 80, 0).await.unwrap();
    assert_eq!(deploy_info.source, "deploy/kubach-web");
    assert!(http_get(deploy_info.local_addr).await.contains("200 OK"));

    // The pod goes away: the next connection moves to another ready pod.
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    pods.delete(&info.pod, &DeleteParams::default().grace_period(0))
        .await
        .unwrap();
    for _ in 0..30 {
        if pods.get_opt(&info.pod).await.unwrap().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(http_get(info.local_addr).await.contains("200 OK"));
    let moved = manager.get(info.id).unwrap();
    assert_ne!(moved.pod, info.pod);
    assert_eq!(moved.remote_port, 80);

    manager.stop_all();
    cleanup(&client, &ns).await;
}
