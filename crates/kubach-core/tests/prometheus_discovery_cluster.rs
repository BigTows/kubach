//! Prometheus search against a real cluster with a look-alike (#200): a
//! Service labelled like kube-prometheus-stack in a namespace outside the
//! well-known ones, backed by a pod that answers the probe. It must rank
//! after every server in a well-known namespace, and Auto must not use it
//! without the user's confirmation. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-200 \
//!     cargo test -p kubach-core --test prometheus_discovery_cluster -- --ignored
//! ```
//!
//! The namespace must exist and not be well-known; the test creates and
//! deletes its own pod and Service (image `busybox:1.37`). With a real
//! Prometheus in `monitoring` (kube-prometheus-stack) it also checks that
//! the real one comes first.

use std::time::Duration;

use k8s_openapi::api::core::v1::{Pod, Service};
use kubach_core::metrics::prometheus::source::{self, Plan};
use kubach_core::metrics::prometheus::{Endpoint, Prometheus, well_known_namespace};
use kubach_core::settings::PrometheusSource;
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const NAME: &str = "prometheus-operated";

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
    let now = DeleteParams::default().grace_period(0);
    Api::<Service>::namespaced(client.clone(), ns)
        .delete(NAME, &now)
        .await
        .ok();
    Api::<Pod>::namespaced(client.clone(), ns)
        .delete(NAME, &now)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn look_alike_in_unusual_namespace_never_wins() {
    let (client, ns) = client().await;
    assert!(
        !well_known_namespace(&ns),
        "use a namespace that is not well-known"
    );
    cleanup(&client, &ns).await;

    // Answers `/api/v1/query` like Prometheus, whatever the query.
    let answer = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
    let pod: Pod = serde_json::from_value(json!({
        "metadata": { "name": NAME, "labels": { "kubach-test": NAME } },
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "httpd", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent",
                "command": ["sh", "-c", format!(
                    "mkdir -p /www/api/v1 && echo '{answer}' > /www/api/v1/query \
                     && httpd -f -p 9090 -h /www"
                )],
                "ports": [{ "containerPort": 9090 }]
            }]
        }
    }))
    .unwrap();
    let service: Service = serde_json::from_value(json!({
        "metadata": { "name": NAME, "labels": { "app": "kube-prometheus-stack-prometheus" } },
        "spec": {
            "selector": { "kubach-test": NAME },
            "ports": [{ "name": "web", "port": 9090, "targetPort": 9090 }]
        }
    }))
    .unwrap();
    let pods = Api::<Pod>::namespaced(client.clone(), &ns);
    let mut created = false;
    for _ in 0..30 {
        if pods.create(&PostParams::default(), &pod).await.is_ok() {
            created = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(created, "pod not created");
    Api::<Service>::namespaced(client.clone(), &ns)
        .create(&PostParams::default(), &service)
        .await
        .unwrap();

    let fake = Endpoint::parse(&format!("{ns}/{NAME}:web")).unwrap();
    let mut found = Vec::new();
    for _ in 0..60 {
        found = Prometheus::discover(&client)
            .await
            .map(|found| found.iter().map(|p| p.endpoint().clone()).collect())
            .unwrap_or_default();
        if found.contains(&fake) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    cleanup(&client, &ns).await;

    let at = found
        .iter()
        .position(|endpoint| *endpoint == fake)
        .expect("the look-alike answers the probe");
    assert!(
        found[at..]
            .iter()
            .all(|endpoint| endpoint.unusual_namespace().is_some()),
        "a well-known namespace ranks after the look-alike: {found:?}"
    );
    let real = found.iter().find(|e| e.unusual_namespace().is_none());
    let remembered = source::remembered(Ok(&found), 1).unwrap();
    let plan = Plan::of(&PrometheusSource::Auto, None, Some(&remembered));
    match real {
        Some(real) => assert_eq!(plan, Plan::Use(real.clone())),
        None => assert!(matches!(plan, Plan::Confirm(_)), "{plan:?}"),
    }

    // Found only in the unusual namespace: Auto waits for confirmation.
    let only = source::remembered(Ok(std::slice::from_ref(&fake)), 1).unwrap();
    assert_eq!(
        Plan::of(&PrometheusSource::Auto, None, Some(&only)),
        Plan::Confirm(fake)
    );
}
