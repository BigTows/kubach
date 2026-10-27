//! Traffic layer (#326) against a real Prometheus. Ignored by default; run
//! with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube \
//! KUBACH_TEST_PROMETHEUS=monitoring/kube-prometheus-stack-prometheus:http-web \
//! KUBACH_TEST_TRAFFIC_NAMESPACE=kubach-test-326 \
//!     cargo test -p kubach-core --test traffic_cluster -- --ignored --nocapture
//! ```
//!
//! Every plugin's probe and queries must be valid PromQL for that server
//! (an empty answer is fine). With `KUBACH_TEST_TRAFFIC_NAMESPACE`, that
//! namespace must hold counters in the format of all three sources
//! (frontend → cart: Istio 44 req/s with 4 failing, Linkerd 31 req/s with
//! 1 failing, Hubble 8 req/s and 16 + 2 flows/s; cart → redis: Istio
//! 21000 B/s), as a scraped exporter does: the test then checks probes,
//! rates and errors end to end. It only reads.

use std::time::Duration;

use kubach_core::metrics::prometheus::{Endpoint, Prometheus};
use kubach_core::topology::traffic::{
    self, NamespaceScope, PLUGINS, Peer, TrafficSnapshot, TrafficWindow,
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

fn flow<'a>(
    snapshot: &'a TrafficSnapshot,
    namespace: &str,
    source: &str,
    destination: &str,
) -> &'a traffic::TrafficFlow {
    snapshot
        .flows
        .iter()
        .find(|flow| {
            flow.key.source == Some(Peer::new(namespace, source))
                && flow.key.destination == Peer::new(namespace, destination)
        })
        .unwrap_or_else(|| panic!("{source} -> {destination} in {snapshot:?}"))
}

fn near(actual: Option<f64>, expected: f64) -> bool {
    actual.is_some_and(|actual| (actual - expected).abs() <= expected * 0.1)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster with Prometheus (KUBACH_TEST_CONTEXT, KUBACH_TEST_PROMETHEUS)"]
async fn traffic_queries_run_on_a_real_prometheus() {
    let client = client().await;
    let address = env("KUBACH_TEST_PROMETHEUS").expect("KUBACH_TEST_PROMETHEUS");
    let prometheus = Prometheus::connect(&client, Endpoint::parse(&address).unwrap())
        .await
        .unwrap();

    // Valid PromQL everywhere, whatever the cluster has.
    for plugin in PLUGINS {
        for window in TrafficWindow::ALL {
            let all = traffic::fetch(&prometheus, plugin, &NamespaceScope::All, window)
                .await
                .unwrap_or_else(|e| panic!("{} {window:?}: {e}", plugin.id()));
            assert!(all.partial.is_empty(), "{}: {:?}", plugin.id(), all.partial);
        }
    }

    let Some(namespace) = env("KUBACH_TEST_TRAFFIC_NAMESPACE") else {
        return;
    };
    // Counters need two scrapes in the window before rates show.
    let mut confirmed = Vec::new();
    for _ in 0..24 {
        confirmed = traffic::confirm(&prometheus, &PLUGINS).await;
        if confirmed.len() == PLUGINS.len() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    println!("confirmed: {confirmed:?}");
    assert_eq!(confirmed, ["istio", "linkerd", "hubble"]);
    let scope = NamespaceScope::of(Some(std::slice::from_ref(&namespace)));
    let mut snapshots = Vec::new();
    for plugin in PLUGINS {
        let mut snapshot = None;
        for _ in 0..24 {
            let polled = traffic::fetch(&prometheus, plugin, &scope, TrafficWindow::OneMinute)
                .await
                .unwrap();
            if !polled.flows.is_empty() {
                snapshot = Some(polled);
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let snapshot = snapshot.unwrap_or_else(|| panic!("{}: no flows", plugin.id()));
        println!("{}: {:?}", plugin.id(), snapshot.flows);
        snapshots.push(snapshot);
    }
    let istio = flow(&snapshots[0], &namespace, "frontend", "cart");
    assert!(near(istio.traffic.requests, 44.), "{istio:?}");
    assert!(near(istio.traffic.errors, 4.), "{istio:?}");
    assert_eq!(istio.key.service, Some(Peer::new(&namespace, "cart")));
    // 30 of 44 under 50 ms, 43 under 100: p95 between.
    assert!(
        istio.traffic.p95_ms.is_some_and(|p| p > 50. && p <= 100.),
        "{istio:?}"
    );
    let redis = flow(&snapshots[0], &namespace, "cart", "redis");
    assert!(near(redis.traffic.bytes, 21000.), "{redis:?}");
    let linkerd = flow(&snapshots[1], &namespace, "frontend", "cart");
    assert!(near(linkerd.traffic.requests, 31.), "{linkerd:?}");
    assert!(near(linkerd.traffic.errors, 1.), "{linkerd:?}");
    let hubble = flow(&snapshots[2], &namespace, "frontend", "cart");
    assert!(near(hubble.traffic.requests, 8.), "{hubble:?}");
    assert!(near(hubble.traffic.flows, 16.), "{hubble:?}");
    assert!(near(hubble.traffic.dropped, 2.), "{hubble:?}");
}
