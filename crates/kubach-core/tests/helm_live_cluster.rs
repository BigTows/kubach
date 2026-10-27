//! Live release list and driver detection (#102) against a real cluster:
//! storage objects are watched through the cache, only changed releases are
//! fetched again. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-102 \
//!     cargo test -p kubach-core --test helm_live_cluster -- --ignored
//! ```
//!
//! Needs `helm` on `PATH`. The namespace must exist; the test installs and
//! removes its own releases `kubach-live-s` (Secret) and `kubach-live-c`
//! (ConfigMap).

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use kubach_core::ApiCatalog;
use kubach_core::cache::{ResourceCache, Subscription, WatchStatus};
use kubach_core::helm::{Driver, ReleaseFeed, ReleaseStatus, Releases, StoredRevision, live};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

const SECRET_RELEASE: &str = "kubach-live-s";
const CONFIG_MAP_RELEASE: &str = "kubach-live-c";

async fn client() -> (Client, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    (Client::try_from(config).unwrap(), context, namespace)
}

fn write_chart(dir: &Path) {
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join("Chart.yaml"),
        "apiVersion: v2\nname: live\nversion: 0.1.0\n",
    )
    .unwrap();
    std::fs::write(dir.join("values.yaml"), "greeting: hello\n").unwrap();
    std::fs::write(
        dir.join("templates/greeting.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-greeting\n\
         data:\n  text: {{ .Values.greeting | quote }}\n",
    )
    .unwrap();
}

fn helm(context: &str, namespace: &str, driver: &str, args: &[&str]) {
    let output = Command::new("helm")
        .args(args)
        .args(["--kube-context", context, "--namespace", namespace])
        .env("HELM_DRIVER", driver)
        .output()
        .expect("helm on PATH");
    assert!(
        output.status.success(),
        "helm {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn latest_when(
    watch: &Subscription,
    mut done: impl FnMut(&[StoredRevision]) -> bool,
) -> Vec<StoredRevision> {
    for _ in 0..300 {
        let snapshot = watch.snapshot();
        if snapshot.status == WatchStatus::Ready {
            let latest = live::latest_in(&snapshot.objects);
            if done(&latest) {
                return latest;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("watch did not reach the expected state");
}

fn revision_of(latest: &[StoredRevision], release: &str) -> Option<u32> {
    latest
        .iter()
        .find(|s| s.release == release)
        .map(|s| s.revision)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE) and helm"]
async fn watches_releases_and_detects_the_driver() {
    let (client, context, namespace) = client().await;
    let chart = tempfile::tempdir().unwrap();
    write_chart(chart.path());
    let chart = chart.path().to_str().unwrap().to_owned();

    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let cache = ResourceCache::new(client.clone());
    let watch = |driver: Driver| {
        cache.subscribe_filtered(
            live::storage_kind(&catalog, driver).unwrap(),
            Some(&namespace),
            live::watch_filter(driver),
        )
    };
    let secrets = watch(Driver::Secret);
    let config_maps = watch(Driver::ConfigMap);
    // Nothing yet.
    let none = latest_when(&secrets, |_| true).await;
    assert!(none.is_empty(), "namespace already has releases: {none:?}");

    helm(
        &context,
        &namespace,
        "secret",
        &["install", SECRET_RELEASE, &chart],
    );
    let latest = latest_when(&secrets, |l| revision_of(l, SECRET_RELEASE) == Some(1)).await;
    let config_map_latest = latest_when(&config_maps, |_| true).await;
    assert_eq!(
        live::detect_driver(latest.len(), config_map_latest.len()),
        Some(Driver::Secret)
    );
    // Metadata only: the cached object has no payload.
    let objects = secrets.snapshot().objects;
    assert!(objects.iter().all(|o| o.data.get("data").is_none()));

    let releases = Releases::with_driver(client.clone(), Driver::Secret);
    let mut feed = ReleaseFeed::default();
    let fetch = feed.to_fetch(&latest);
    assert_eq!(fetch.len(), 1);
    feed.record(releases.summaries(fetch).await);
    let listing = feed.listing(&latest, Vec::new());
    assert_eq!(listing.releases.len(), 1);
    assert_eq!(listing.releases[0].status, ReleaseStatus::Deployed);

    // An upgrade shows up through the watch; only that release is fetched.
    helm(
        &context,
        &namespace,
        "secret",
        &["upgrade", SECRET_RELEASE, &chart, "--set", "greeting=hi"],
    );
    let latest = latest_when(&secrets, |l| revision_of(l, SECRET_RELEASE) == Some(2)).await;
    let fetch = feed.to_fetch(&latest);
    assert_eq!(fetch.len(), 1);
    assert_eq!(fetch[0].revision, 2);
    feed.record(releases.summaries(fetch).await);
    let listing = feed.listing(&latest, Vec::new());
    assert_eq!(listing.releases[0].revision, 2);
    assert!(feed.to_fetch(&latest).is_empty());

    // A ConfigMap release: seen by the other watch; with the Secret one it is
    // a tie, which Helm's default wins.
    helm(
        &context,
        &namespace,
        "configmap",
        &["install", CONFIG_MAP_RELEASE, &chart],
    );
    let config_map_latest = latest_when(&config_maps, |l| {
        revision_of(l, CONFIG_MAP_RELEASE) == Some(1)
    })
    .await;
    assert_eq!(
        live::detect_driver(latest.len(), config_map_latest.len()),
        Some(Driver::Secret)
    );
    let fetched = Releases::with_driver(client.clone(), Driver::ConfigMap)
        .summaries(config_map_latest.clone())
        .await;
    assert!(matches!(&fetched[0].1, Ok(Some(s)) if s.name == CONFIG_MAP_RELEASE));

    // Without the Secret release, ConfigMap is detected.
    helm(
        &context,
        &namespace,
        "secret",
        &["uninstall", SECRET_RELEASE],
    );
    let latest = latest_when(&secrets, |l| l.is_empty()).await;
    assert_eq!(
        live::detect_driver(latest.len(), config_map_latest.len()),
        Some(Driver::ConfigMap)
    );
    assert!(feed.listing(&latest, Vec::new()).releases.is_empty());

    helm(
        &context,
        &namespace,
        "configmap",
        &["uninstall", CONFIG_MAP_RELEASE],
    );
    latest_when(&config_maps, |l| l.is_empty()).await;
}
