//! Downloads the whole log of a two-container pod from a real cluster and
//! checks that nothing is missing, that several containers are tagged, and
//! that a pod that is not there saves nothing. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-214 \
//!     cargo test -p kubach-core --test log_download_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own pod
//! (image `busybox`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kubach_core::streams::{
    LogDownloadError, LogDownloadOptions, LogSource, SourceFailure, download_logs,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const POD: &str = "kubach-log-download";
/// More than the lines a logs tab asks for at first.
const LINES: usize = 5000;

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

/// Prints `<name> 0` … `<name> LINES-1`, then `<name> done` and waits.
fn printer(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "image": "busybox",
        "command": ["sh", "-c", format!("i=0; while [ $i -lt {LINES} ]; do echo \"{name} $i\"; i=$((i+1)); done; echo \"{name} done\"; sleep 3600")],
    })
}

/// The file once `container` has printed everything.
async fn download_until_done(
    client: &Client,
    sources: &[LogSource],
    options: LogDownloadOptions,
    target: &std::path::Path,
) -> String {
    for _ in 0..60 {
        let written = AtomicU64::new(0);
        let report = download_logs(client, sources.to_vec(), options, target, &written)
            .await
            .unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let text = std::fs::read_to_string(target).unwrap();
        assert_eq!(report.bytes, text.len() as u64);
        assert_eq!(written.load(Ordering::Relaxed), report.bytes);
        let done = sources
            .iter()
            .all(|source| text.contains(&format!("{} done", source.container)));
        if done {
            return text;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("the containers did not finish printing");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn the_whole_log_is_saved() {
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
            "containers": [printer("a"), printer("b")],
        },
    }))
    .unwrap();
    pods.create(&PostParams::default(), &pod).await.unwrap();
    for _ in 0..120 {
        let running = pods.get(POD).await.is_ok_and(|pod| {
            pod.status.and_then(|status| status.phase).as_deref() == Some("Running")
        });
        if running {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let dir = tempfile::tempdir().unwrap();
    let source = |container: &str| LogSource::new(&ns, POD, container);

    // One container, as the server sends it: every line, in order.
    let single = dir.path().join("single.log");
    let text = download_until_done(
        &client,
        &[source("a")],
        LogDownloadOptions::default(),
        &single,
    )
    .await;
    let expected: String = (0..LINES)
        .map(|i| format!("a {i}\n"))
        .chain(["a done\n".to_owned()])
        .collect();
    assert_eq!(text, expected);

    // Both containers with timestamps: `pod/container time text`.
    let both = dir.path().join("both.log");
    let options = LogDownloadOptions {
        previous: false,
        timestamps: true,
    };
    let text = download_until_done(&client, &[source("a"), source("b")], options, &both).await;
    for container in ["a", "b"] {
        let tag = format!("{POD}/{container} ");
        let lines: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix(&tag))
            .collect();
        assert_eq!(lines.len(), LINES + 1, "{container}");
        for (i, line) in lines.iter().take(LINES).enumerate() {
            let (time, rest) = line.split_once(' ').unwrap();
            assert!(
                time.parse::<k8s_openapi::jiff::Timestamp>().is_ok(),
                "{line}"
            );
            assert_eq!(rest, format!("{container} {i}"));
        }
    }
    assert_eq!(text.lines().count(), 2 * (LINES + 1));

    // No previous instance of a container that never restarted: the
    // server refuses, nothing is saved.
    let previous = dir.path().join("previous.log");
    let options = LogDownloadOptions {
        previous: true,
        timestamps: false,
    };
    let written = AtomicU64::new(0);
    let error = download_logs(&client, vec![source("a")], options, &previous, &written)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            LogDownloadError::Source {
                reason: SourceFailure::Other(_),
                ..
            }
        ),
        "{error}"
    );

    // A pod that is not there.
    let missing = dir.path().join("missing.log");
    let gone = LogSource::new(&ns, "kubach-log-download-missing", "a");
    let error = download_logs(
        &client,
        vec![gone],
        LogDownloadOptions::default(),
        &missing,
        &written,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            LogDownloadError::Source {
                reason: SourceFailure::NotFound,
                ..
            }
        ),
        "{error}"
    );

    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();

    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["both.log", "single.log"]);
}
