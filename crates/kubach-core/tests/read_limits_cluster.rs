//! Read limits against a real cluster (#195): `exec` of an endless command
//! stops at its limit, megabyte log lines are cut, a log fetch stops at its
//! byte limit. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-195 \
//!     cargo test -p kubach-core --test read_limits_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own pod
//! (image `busybox:1.37`, not pulled when present).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kubach_core::streams::{
    CUT_LINE_MARK, HistoryState, LogEvent, LogHistory, LogOptions, LogSource, MAX_HISTORY_BYTES,
    exec_capture, fetch_history, fetch_log_lines, is_pod_ready, stream_logs,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const POD: &str = "kubach-read-limits";
const LIMIT: usize = 64 * 1024;

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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn reads_stop_at_their_limits() {
    let (client, ns) = client().await;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();
    while pods.get_opt(POD).await.unwrap().is_some() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let long_lines = "i=0; while true; do echo \"short $i\"; \
                      printf '%01048576d\\n' 0; \
                      i=$((i+1)); sleep 2; done";
    let pod: Pod = serde_json::from_value(json!({
        "metadata": { "name": POD },
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [
                { "name": "shell", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent", "command": ["sleep", "3600"] },
                { "name": "long", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent", "command": ["sh", "-c", long_lines] },
            ],
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
    tokio::time::sleep(Duration::from_secs(15)).await;

    // `yes` never ends: the capture stops at the limit and closes the
    // connection, well before the timeout.
    let started = std::time::Instant::now();
    let capture = tokio::time::timeout(
        Duration::from_secs(30),
        exec_capture(&client, &ns, POD, Some("shell"), ["yes"], LIMIT),
    )
    .await
    .expect("exec of `yes` stops at the limit")
    .unwrap();
    assert!(capture.truncated);
    assert_eq!(capture.stdout.len(), LIMIT);
    assert!(capture.stdout.starts_with(b"y\ny\n"));
    assert!(started.elapsed() < Duration::from_secs(20));

    let capture = exec_capture(&client, &ns, POD, Some("shell"), ["echo", "hi"], LIMIT)
        .await
        .unwrap();
    assert!(!capture.truncated);
    assert!(capture.status.success());
    assert_eq!(capture.stdout, b"hi\n");
    // Kubernetes does not kill an exec'd process when its connection
    // closes: `yes` stays, but blocked on its full output (sleeping), not
    // producing anything more.
    let mut idle = false;
    for _ in 0..5 {
        let ps = exec_capture(
            &client,
            &ns,
            POD,
            Some("shell"),
            ["ps", "-o", "comm,stat"],
            LIMIT,
        )
        .await
        .unwrap();
        let ps = String::from_utf8_lossy(&ps.stdout).into_owned();
        let running = ps.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("yes") && fields.next().is_some_and(|s| s.starts_with('R'))
        });
        if !running {
            idle = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(idle, "`yes` keeps running after the connection closed");

    // Megabyte lines come out cut, the short ones whole. The kubelet counts
    // `tailLines` in 16 KiB log records, not lines, so ask for the whole
    // log (it rotates at about 10 MiB).
    let source = Arc::new(LogSource::new(&ns, POD, "long"));
    let fetched = fetch_log_lines(&client, source.clone(), false, 100_000, 64 * 1024 * 1024)
        .await
        .unwrap();
    assert!(!fetched.truncated);
    assert!(fetched.lines.len() >= 4);
    assert!(fetched.lines.iter().any(|l| l.text.starts_with("short ")));
    assert!(
        fetched
            .lines
            .iter()
            .any(|l| l.text.ends_with(CUT_LINE_MARK))
    );
    for line in &fetched.lines {
        assert!(line.text.starts_with("short ") || line.text.ends_with(CUT_LINE_MARK));
        assert!(line.text.len() <= 64 * 1024 + CUT_LINE_MARK.len());
    }
    // A fetch stops at its byte limit.
    let fetched = fetch_log_lines(&client, source.clone(), false, 100_000, 1024 * 1024)
        .await
        .unwrap();
    assert!(fetched.truncated);
    assert!(fetched.lines.len() <= 2);

    // A following stream cuts them too.
    let options = LogOptions {
        tail_lines: Some(4),
        ..LogOptions::default()
    };
    let mut stream = stream_logs(&client, vec![(*source).clone()], options);
    let mut seen = Vec::new();
    while seen.len() < 6 {
        let event = tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .expect("log lines")
            .expect("stream open");
        if let LogEvent::Lines { lines, .. } = event {
            seen.extend(lines);
        }
    }
    assert!(seen.iter().any(|line| line.text.ends_with(CUT_LINE_MARK)));
    assert!(
        seen.iter()
            .all(|line| line.text.len() <= 64 * 1024 + CUT_LINE_MARK.len())
    );

    // Earlier lines: every fetch stays within its byte limit and loading
    // ends (at the beginning or the cap) without holding more than the
    // budget.
    let mut history = LogHistory::new(std::slice::from_ref(&source), 4);
    history.observe(&source, &seen, 0);
    for _ in 0..20 {
        if history.state() != HistoryState::More {
            break;
        }
        let requests = history.plan();
        assert!(
            requests
                .iter()
                .all(|r| r.limit_bytes <= 2 * MAX_HISTORY_BYTES)
        );
        let fetched = fetch_history(&client, requests, false).await;
        let answered = fetched
            .into_iter()
            .map(|(request, lines)| (request, lines.unwrap()))
            .collect();
        history.apply(answered);
    }
    assert_ne!(history.state(), HistoryState::More);
    assert!(history.known_bytes() <= MAX_HISTORY_BYTES + 2 * 1024 * 1024);

    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();
}
