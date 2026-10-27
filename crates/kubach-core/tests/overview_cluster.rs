//! Statistics of the Overview screen (#414) against a real cluster. Ignored
//! by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube \
//! KUBACH_TEST_PROMETHEUS=monitoring/kube-prometheus-stack-prometheus:http-web \
//!     cargo test -p kubach-core --test overview_cluster -- --ignored --nocapture
//! ```
//!
//! Counts Nodes and the Pods of `kube-system` the way the screen does, and
//! asks Prometheus (with kube-state-metrics) for the charts of the screen.
//! The test only reads.

use kubach_core::metrics::overview::{PodPhase, cluster_stats};
use kubach_core::metrics::prometheus::{Endpoint, Prometheus, Range};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

async fn client() -> Client {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    Client::try_from(config).unwrap()
}

async fn list(client: &Client, kind: &str, namespace: Option<&str>) -> Vec<DynamicObject> {
    let api = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", kind));
    let api: Api<DynamicObject> = match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, &api),
        None => Api::all_with(client.clone(), &api),
    };
    api.list(&ListParams::default()).await.unwrap().items
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster with Prometheus and kube-state-metrics (KUBACH_TEST_CONTEXT, KUBACH_TEST_PROMETHEUS)"]
async fn counts_and_charts_the_cluster() {
    let client = client().await;

    let nodes = list(&client, "Node", None).await;
    let pods = list(&client, "Pod", Some("kube-system")).await;
    let stats = cluster_stats(&nodes, &pods);
    println!("{stats:#?}");
    assert!(stats.nodes.total >= 1);
    assert!(stats.nodes.ready >= 1);
    assert!(stats.nodes.pod_capacity > 0);
    assert!(stats.cpu.allocatable > 0 && stats.memory.allocatable > 0);
    assert!(stats.pods.get(PodPhase::Running) > 0);
    // coredns asks for CPU and memory.
    assert!(stats.cpu.requests > 0 && stats.memory.requests > 0);

    let address = std::env::var("KUBACH_TEST_PROMETHEUS").expect("KUBACH_TEST_PROMETHEUS");
    let prometheus = Prometheus::connect(&client, Endpoint::parse(&address).unwrap())
        .await
        .unwrap();
    let scope = ["kube-system".to_owned()];
    for namespaces in [None, Some(&scope[..])] {
        let charts = prometheus
            .overview_usage(namespaces, Range::Hour)
            .await
            .unwrap();
        let status = prometheus
            .overview_status(namespaces, Range::Hour)
            .await
            .unwrap();
        println!(
            "{namespaces:?}: cpu {} points, memory {} points, usage {:?}, restarts {:?}",
            charts.cpu.series.first().map_or(0, |s| s.points.len()),
            charts.memory.series.first().map_or(0, |s| s.points.len()),
            status.usage,
            status.restarts,
        );
        assert!(!charts.cpu.is_empty() && !charts.memory.is_empty());
        let usage = status.usage.expect("current usage");
        assert!(usage.cpu > 0 && usage.memory > 0);
        let phases = status.phases.expect("kube-state-metrics phases");
        let running = phases
            .series
            .iter()
            .find(|(phase, _)| *phase == PodPhase::Running)
            .expect("running pods");
        assert!(running.1.last().is_some_and(|p| p.value >= 1.0));
        assert!(status.restarts.is_some());
        let oldest = prometheus.overview_oldest(namespaces).await.unwrap();
        println!("oldest sample: {oldest:?}");
        assert!(oldest.is_some());
    }
    // The pods of one namespace use less than all of them.
    let all = prometheus.overview_status(None, Range::Hour).await.unwrap();
    let one = prometheus
        .overview_status(Some(&scope), Range::Hour)
        .await
        .unwrap();
    assert!(one.usage.unwrap().memory <= all.usage.unwrap().memory);
}
