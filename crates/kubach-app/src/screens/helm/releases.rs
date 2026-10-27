//! Release data of the Releases page: the storage watch, the storage driver
//! and the list.
//!
//! The list follows the cluster: storage objects (`owner=helm`, labels only)
//! are watched through the cache and releases whose latest object changed
//! are fetched again (`helm::live`). The same watches pick the storage
//! driver unless the context's settings name one.

use gpui_kit::*;
use kubach_core::cache::{ResourceCache, SubscriptionSet, WatchError, WatchStatus};
use kubach_core::helm::{
    Driver, DriverChoice, HelmError, ReleaseFeed, ReleaseListing, Releases, StoredRevision, live,
};
use kubach_core::runtime;
use kubach_i18n::t;

use super::super::activity::watch_set;
use super::{Load, Namespaces};
use crate::cluster::Cluster;

/// Helm storage objects of the listed namespaces, watched through the cache:
/// one set of watches per driver that may be in use.
struct StorageWatch {
    namespaces: Namespaces,
    /// Driver picked in the settings; `None` watches both to detect it.
    preference: Option<Driver>,
    sets: Vec<(Driver, SubscriptionSet)>,
    _listener: Task<()>,
}

/// The releases were read from the storage watches again (by the watch or
/// after a fetch); `listed` tells whether the list was updated.
pub(super) enum ReleasesEvent {
    Refreshed { listed: bool },
}

/// Releases of the selected namespaces, kept current by the storage watch.
pub(super) struct HelmReleases {
    cluster: Entity<Cluster>,
    /// Namespaces of the current list; `None` until something is loaded.
    loaded_for: Option<Namespaces>,
    list: Load<ReleaseListing>,
    storage: Option<StorageWatch>,
    /// Summaries of the latest revisions, refreshed as storage changes.
    feed: ReleaseFeed,
    driver: DriverChoice,
    /// Latest storage object of every release, from the watch.
    latest: Vec<StoredRevision>,
    /// Failing watches of the driver in use, by namespace.
    watch_errors: Vec<(Option<String>, WatchError)>,
    /// Summaries being fetched; changes meanwhile are picked up after it.
    fetch_task: Option<Task<()>>,
}

impl EventEmitter<ReleasesEvent> for HelmReleases {}

impl HelmReleases {
    pub(super) fn new(cluster: Entity<Cluster>) -> Self {
        Self {
            cluster,
            loaded_for: None,
            list: Load::Loading,
            storage: None,
            feed: ReleaseFeed::default(),
            driver: DriverChoice::default(),
            latest: Vec::new(),
            watch_errors: Vec::new(),
            fetch_task: None,
        }
    }

    pub(super) fn loaded_for(&self) -> Option<&Namespaces> {
        self.loaded_for.as_ref()
    }

    pub(super) fn list(&self) -> &Load<ReleaseListing> {
        &self.list
    }

    /// Whether releases are being read: the watches are still loading or
    /// summaries are being fetched (e.g. after Reload).
    pub(super) fn busy(&self) -> bool {
        self.fetch_task.is_some() || (self.storage.is_some() && matches!(self.list, Load::Loading))
    }

    pub(super) fn driver(&self) -> DriverChoice {
        self.driver
    }

    /// Context switch or reconnect: start over.
    pub(super) fn reset(&mut self) {
        self.loaded_for = None;
        self.list = Load::Loading;
        self.storage = None;
        self.feed.clear();
        self.driver = DriverChoice::default();
        self.latest.clear();
        self.watch_errors.clear();
        self.fetch_task = None;
    }

    /// Hidden: no watches. Known summaries are kept, so showing the screen
    /// again only fetches what changed.
    pub(super) fn stop(&mut self) {
        self.storage = None;
    }

    /// Whether the watches are missing or follow other namespaces or another
    /// driver preference.
    pub(super) fn needs_watch(&self, wanted: &Namespaces, preference: Option<Driver>) -> bool {
        self.storage
            .as_ref()
            .is_none_or(|storage| storage.namespaces != *wanted || storage.preference != preference)
    }

    /// Watches the release storage of `namespaces`; the list follows it once
    /// refreshed.
    pub(super) fn watch(
        &mut self,
        cache: &ResourceCache,
        catalog: &kubach_core::ApiCatalog,
        namespaces: Namespaces,
        preference: Option<Driver>,
        cx: &mut Context<Self>,
    ) {
        let drivers = match preference {
            Some(driver) => vec![driver],
            None => Driver::ALL.to_vec(),
        };
        let sets: Vec<(Driver, SubscriptionSet)> = drivers
            .into_iter()
            .filter_map(|driver| {
                let kind = live::storage_kind(catalog, driver)?;
                let filter = live::watch_filter(driver);
                let set = cache.subscribe_many(kind, namespaces.as_deref(), filter);
                Some((driver, set))
            })
            .collect();
        let all = SubscriptionSet::new(
            sets.iter()
                .flat_map(|(_, set)| set.subscriptions().iter().cloned())
                .collect(),
        );
        // Changes are already coalesced by the cache (150 ms).
        let listener = watch_set(&all, cx, Self::changed);
        self.loaded_for = Some(namespaces.clone());
        self.storage = Some(StorageWatch {
            namespaces,
            preference,
            sets,
            _listener: listener,
        });
    }

    /// Fetches every release of the list again.
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        self.feed.clear();
        self.refresh(cx)
    }

    /// Refreshes and tells the screen.
    fn changed(&mut self, cx: &mut Context<Self>) {
        if let Some(listed) = self.refresh(cx) {
            cx.emit(ReleasesEvent::Refreshed { listed });
        }
    }

    /// Reads the storage watches: picks the driver, fetches releases whose
    /// latest object is new or changed, and updates the list once every
    /// release is known. `None` while there is nothing to read; otherwise
    /// whether the list was updated.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        let storage = self.storage.as_ref()?;
        let snapshots: Vec<_> = storage
            .sets
            .iter()
            .map(|(driver, set)| (*driver, set.snapshot()))
            .collect();
        if snapshots
            .iter()
            .any(|(_, snapshot)| snapshot.status == WatchStatus::Loading)
        {
            return None;
        }
        let latest: Vec<(Driver, Vec<StoredRevision>)> = snapshots
            .iter()
            .map(|(driver, snapshot)| (*driver, live::latest_in(&snapshot.objects)))
            .collect();
        let found = |driver: Driver| {
            latest
                .iter()
                .find(|(d, _)| *d == driver)
                .map_or(0, |(_, releases)| releases.len())
        };
        let choice = DriverChoice::resolve(
            storage.preference,
            live::detect_driver(found(Driver::Secret), found(Driver::ConfigMap)),
        );
        if choice.driver != self.driver.driver {
            self.feed.clear();
        }
        self.driver = choice;
        self.latest = latest
            .into_iter()
            .find(|(driver, _)| *driver == choice.driver)
            .map(|(_, releases)| releases)
            .unwrap_or_default();
        self.watch_errors = match snapshots.iter().find(|(d, _)| *d == choice.driver) {
            Some((_, snapshot)) => snapshot.errors.clone(),
            None => vec![(
                None,
                WatchError {
                    forbidden: false,
                    connection: false,
                    message: t!("helm-cluster-serves-no", kind = choice.driver.kind()),
                },
            )],
        };

        let fetch = self.feed.to_fetch(&self.latest);
        if !fetch.is_empty() && self.fetch_task.is_none() {
            self.fetch(fetch, cx);
        }
        let listed = self.feed.is_complete(&self.latest);
        if listed {
            let failures = self
                .watch_errors
                .iter()
                .map(|(namespace, error)| {
                    (
                        namespace.clone().unwrap_or_default(),
                        HelmError::Unavailable {
                            forbidden: error.forbidden,
                            message: error.message.clone(),
                        },
                    )
                })
                .collect();
            self.list = Load::Ready(self.feed.listing(&self.latest, failures));
        }
        cx.notify();
        Some(listed)
    }

    /// Fetches and decodes the given latest revisions, then refreshes again
    /// to pick up whatever changed meanwhile.
    fn fetch(&mut self, stored: Vec<StoredRevision>, cx: &mut Context<Self>) {
        let Some(client) = self
            .cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone())
        else {
            return;
        };
        let driver = self.driver.driver;
        let handle = runtime::spawn(async move {
            Releases::with_driver(client, driver)
                .summaries(stored)
                .await
        });
        self.fetch_task = Some(cx.spawn(async move |this, cx| {
            let result = handle.await;
            this.update(cx, |this, cx| {
                this.fetch_task = None;
                match result {
                    Ok(fetched) => {
                        if driver == this.driver.driver {
                            this.feed.record(fetched);
                        }
                        this.changed(cx);
                    }
                    Err(error) => {
                        this.list =
                            Load::Failed(t!("helm-listing-interrupted", error = error.to_string()));
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    /// Latest storage object of a release, from the watch.
    pub(super) fn stored_of(&self, namespace: &str, name: &str) -> Option<StoredRevision> {
        self.latest
            .iter()
            .find(|stored| stored.namespace == namespace && stored.release == name)
            .cloned()
    }
}
