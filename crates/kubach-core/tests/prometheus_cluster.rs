//! Prometheus discovery and queries against a real cluster. Ignored by
//! default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube \
//! KUBACH_TEST_PROMETHEUS=monitoring/kube-prometheus-stack-prometheus:http-web \
//! KUBACH_TEST_DISCOVERED=monitoring/kube-prometheus-stack-prometheus:http-web \
//! KUBACH_TEST_POD=kube-system/coredns-559f6c778d-vshbg \
//! KUBACH_TEST_DEPLOYMENT=kube-system/coredns \
//! KUBACH_TEST_NODE=minikube \
//!     cargo test -p kubach-core --test prometheus_cluster -- --ignored --nocapture
//! ```
//!
//! `KUBACH_TEST_PROMETHEUS` is the address to connect to (as in the
//! settings). The others are optional: the endpoint discovery must find, and
//! objects whose CPU, memory and network charts must all have data. The test
//! only reads.

use kubach_core::metrics::prometheus::{
    Endpoint, Metric, Prometheus, PrometheusError, Range, Target, Window, Workload,
};
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

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `namespace/name` from the environment.
fn namespaced(name: &str) -> Option<(String, String)> {
    let value = env(name)?;
    let (namespace, name) = value.split_once('/').expect("namespace/name");
    Some((namespace.to_owned(), name.to_owned()))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster with Prometheus (KUBACH_TEST_CONTEXT, KUBACH_TEST_PROMETHEUS)"]
async fn discovers_and_queries_prometheus() {
    let client = client().await;

    let found = Prometheus::discover(&client).await.unwrap();
    let described: Vec<String> = found.iter().map(|p| p.endpoint().describe()).collect();
    println!("discovered: {described:?}");
    assert!(
        found
            .iter()
            .all(|p| matches!(p.endpoint(), Endpoint::Service { .. })),
        "{described:?}"
    );
    if let Some(expected) = env("KUBACH_TEST_DISCOVERED") {
        assert_eq!(described[0], expected);
    }

    let address = env("KUBACH_TEST_PROMETHEUS").expect("KUBACH_TEST_PROMETHEUS");
    let endpoint = Endpoint::parse(&address).unwrap();
    let prometheus = Prometheus::connect(&client, endpoint).await.unwrap();
    let missing = Target::Pod {
        namespace: "kube-system".into(),
        name: "no-such-pod".into(),
    };
    let charts = prometheus.charts(&missing, Range::Hour).await.unwrap();
    assert_eq!(
        charts.iter().map(|c| c.metric).collect::<Vec<_>>(),
        Metric::ALL
    );
    assert!(charts.iter().all(|c| c.is_empty()));

    let mut targets = Vec::new();
    if let Some((namespace, name)) = namespaced("KUBACH_TEST_POD") {
        targets.push(Target::Pod { namespace, name });
    }
    if let Some((namespace, name)) = namespaced("KUBACH_TEST_DEPLOYMENT") {
        targets.push(Target::Workload {
            kind: Workload::Deployment,
            namespace,
            name,
        });
    }
    if let Some(name) = env("KUBACH_TEST_NODE") {
        targets.push(Target::Node { name });
    }
    for target in targets {
        for chart in prometheus.charts(&target, Range::Hour).await.unwrap() {
            let lines = chart.metric.lines().len();
            assert_eq!(chart.series.len(), lines, "{target:?} {:?}", chart.metric);
            for series in &chart.series {
                let latest = series.latest().unwrap();
                println!(
                    "{target:?} {} {}: {} points, latest {}",
                    chart.metric.title(),
                    series.name,
                    series.points.len(),
                    chart.metric.format(latest)
                );
                assert!(latest >= 0.0);
            }
        }
    }

    // Table usage (#179): two queries for all pods, nodes by name.
    let usage = prometheus.pod_usage(None).await.unwrap();
    if let Some((namespace, name)) = namespaced("KUBACH_TEST_POD") {
        let pod = usage
            .pod(&namespace, &name)
            .expect("usage of KUBACH_TEST_POD");
        println!("usage {namespace}/{name}: {:?}", pod.total());
        assert!(pod.total().memory > 0);
        let scoped = prometheus
            .pod_usage(Some(std::slice::from_ref(&namespace)))
            .await
            .unwrap();
        assert!(scoped.pod(&namespace, &name).is_some());
        assert!(
            prometheus
                .pod_usage(Some(&["no-such-namespace".to_owned()]))
                .await
                .unwrap()
                .pod(&namespace, &name)
                .is_none()
        );
    }
    if let Some(name) = env("KUBACH_TEST_NODE") {
        let nodes = prometheus.node_usage().await.unwrap();
        let node = nodes.node(&name).expect("usage of KUBACH_TEST_NODE");
        println!("usage node {name}: {:?}", node.usage);
        assert!(node.usage.cpu > 0 && node.usage.memory > 0);
    }

    // Prometheus' own error comes through the proxy as a 400 with its body.
    let bad = prometheus
        .query_range("sum(rate(x[]))", Window::now(Range::Hour))
        .await;
    assert!(
        matches!(&bad, Err(PrometheusError::Query { kind, .. }) if kind == "bad_data"),
        "{bad:?}"
    );

    let unreachable = "kube-system/no-such-service:9090";
    assert!(
        Prometheus::connect(&client, Endpoint::parse(unreachable).unwrap())
            .await
            .is_err()
    );
}
