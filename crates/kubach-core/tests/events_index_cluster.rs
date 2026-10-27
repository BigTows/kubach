//! The incrementally decoded event timeline against a real cluster (#370):
//! an [`EventFeed`] follows new events, a changed count and deletions, and
//! agrees with a one-shot list. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-370 \
//!     cargo test -p kubach-core --test events_index_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own Event.

use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::Event;
use kubach_core::ApiCatalog;
use kubach_core::cache::{ResourceCache, WatchStatus};
use kubach_core::events::{self, EventFeed, EventFilter, EventGroup};
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const EVENT: &str = "kubach-index-probe";
const MESSAGE: &str = "kubach #370 probe";
const TIMEOUT: Duration = Duration::from_secs(110);

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

/// Waits until the probe's group in `feed` satisfies `done`.
async fn wait_for(feed: &EventFeed, what: &str, done: impl Fn(Option<&EventGroup>) -> bool) {
    let start = Instant::now();
    loop {
        let snapshot = feed.recent_groups(&EventFilter::default(), usize::MAX);
        let probe = snapshot.groups.iter().find(|g| g.latest.message == MESSAGE);
        if snapshot.status == WatchStatus::Ready && done(probe) {
            return;
        }
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn feed_follows_events_incrementally() {
    let (client, ns) = client().await;
    let api: Api<Event> = Api::namespaced(client.clone(), &ns);
    api.delete(EVENT, &DeleteParams::default()).await.ok();

    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let cache = ResourceCache::new(client.clone());
    let feed = EventFeed::subscribe(&cache, &catalog, Some(&ns)).unwrap();
    wait_for(&feed, "the probe to be gone", |probe| probe.is_none()).await;

    // What the feed decoded incrementally is what a full list decodes.
    let listed = events::list(&client, &catalog, Some(&ns)).await.unwrap();
    assert_eq!(feed.snapshot().events, listed);

    let event: Event = serde_json::from_value(json!({
        "metadata": { "name": EVENT, "namespace": ns },
        "involvedObject": { "kind": "Pod", "namespace": ns, "name": "kubach-probe" },
        "type": "Warning",
        "reason": "Probe",
        "message": MESSAGE,
        "count": 1,
        "firstTimestamp": "2026-10-25T10:00:00Z",
        "lastTimestamp": "2026-10-25T10:00:00Z",
        "source": { "component": "kubach-test" },
    }))
    .unwrap();
    api.create(&PostParams::default(), &event).await.unwrap();
    wait_for(&feed, "the new event", |probe| {
        probe.is_some_and(|g| g.count == 1)
    })
    .await;

    // The event repeats: a new resourceVersion, decoded again.
    let patch = json!({ "count": 9, "lastTimestamp": "2026-10-25T10:05:00Z" });
    api.patch(EVENT, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .unwrap();
    wait_for(&feed, "the new count", |probe| {
        probe.is_some_and(|g| g.count == 9 && g.events == 1)
    })
    .await;
    let warnings = EventFilter {
        warnings_only: true,
        ..EventFilter::default()
    };
    let only = feed.recent_groups(&warnings, usize::MAX);
    assert!(only.groups.iter().any(|g| g.latest.message == MESSAGE));
    assert!(only.groups.iter().all(|g| g.latest.is_warning()));

    api.delete(EVENT, &DeleteParams::default()).await.unwrap();
    wait_for(&feed, "the deletion", |probe| probe.is_none()).await;
    let listed = events::list(&client, &catalog, Some(&ns)).await.unwrap();
    assert_eq!(feed.snapshot().events, listed);
}
