//! Loads earlier log lines of a two-container pod against a real cluster and
//! checks that no line is lost or duplicated. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-160 \
//!     cargo test -p kubach-core --test log_history_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own pod
//! (image `busybox`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kubach_core::streams::{
    HistoryState, INITIAL_TAIL_LINES, LogEvent, LogHistory, LogLine, LogOptions, LogSource,
    fetch_history, is_pod_ready, stream_logs,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const POD: &str = "kubach-log-history";

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

fn counter(name: &str, sleep: &str) -> serde_json::Value {
    json!({
        "name": name,
        "image": "busybox",
        "command": ["sh", "-c", format!("i=0; while true; do echo \"{name} $i\"; i=$((i+1)); sleep {sleep}; done")],
    })
}

/// The numbers after the container name, in order.
fn numbers(lines: &[LogLine]) -> Vec<u64> {
    lines
        .iter()
        .map(|line| line.text.split_once(' ').unwrap().1.parse().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn earlier_lines_have_no_gaps_or_duplicates() {
    let (client, ns) = client().await;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();
    while pods.get_opt(POD).await.unwrap().is_some() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let pod: Pod = serde_json::from_value(json!({
        "metadata": { "name": POD },
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [counter("a", "0.01"), counter("b", "0.05")],
        },
    }))
    .unwrap();
    pods.create(&PostParams::default(), &pod).await.unwrap();
    for _ in 0..120 {
        if pods
            .get(POD)
            .await
            .map(|pod| is_pod_ready(&pod))
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // Enough lines for several loads of `a`.
    tokio::time::sleep(Duration::from_secs(20)).await;

    let sources: Vec<LogSource> = ["a", "b"]
        .iter()
        .map(|container| LogSource::new(&ns, POD, *container))
        .collect();
    let options = LogOptions {
        tail_lines: Some(INITIAL_TAIL_LINES),
        ..LogOptions::default()
    };
    let mut stream = stream_logs(&client, sources.clone(), options);
    let sources: Vec<Arc<LogSource>> = sources.into_iter().map(Arc::new).collect();
    let mut history = LogHistory::new(&sources, INITIAL_TAIL_LINES);
    // Everything shown, as the view holds it: earlier lines in front.
    let mut shown: Vec<LogLine> = Vec::new();

    let mut loads = 0;
    loop {
        // Live lines keep arriving between loads.
        let until = tokio::time::Instant::now() + Duration::from_millis(700);
        while let Ok(Some(event)) = tokio::time::timeout_at(until, stream.next()).await {
            if let LogEvent::Lines {
                source,
                lines,
                dropped,
            } = event
            {
                assert_eq!(dropped, 0);
                history.observe(&source, &lines, dropped);
                shown.extend(lines);
            }
        }
        if history.state() != HistoryState::More {
            break;
        }
        let requests = history.plan();
        let fetched = fetch_history(&client, requests, false).await;
        let answered = fetched
            .into_iter()
            .map(|(request, lines)| (request, lines.unwrap()))
            .collect();
        let earlier = history.apply(answered);
        let times: Vec<_> = earlier.iter().map(|l| l.timestamp.unwrap()).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "merged by time");
        shown.splice(0..0, earlier);
        loads += 1;
        assert!(loads < 10, "too many loads");
    }
    drop(stream);
    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();

    assert_eq!(history.state(), HistoryState::Beginning);
    assert!(loads >= 1);
    let mut per_source: HashMap<String, Vec<LogLine>> = HashMap::new();
    for line in shown {
        per_source
            .entry(line.source.container.clone())
            .or_default()
            .push(line);
    }
    for (container, lines) in per_source {
        let numbers = numbers(&lines);
        let expected: Vec<u64> = (0..numbers.len() as u64).collect();
        assert_eq!(numbers, expected, "{container}: consecutive from 0");
        assert!(numbers.len() > INITIAL_TAIL_LINES as usize, "{container}");
    }
}
