//! Live release list: Helm storage objects are watched through the
//! [cache](crate::cache) (labels only, `owner=helm`), and only releases
//! whose latest storage object changed are fetched and decoded again.
//!
//! The same watches tell which storage driver a cluster uses: when the user
//! did not pick one for the context, the driver holding releases wins.

use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::DynamicObject;

use super::store::{OWNER_SELECTOR, SECRET_TYPE, latest_revisions, stored_revision};
use super::{Driver, HelmError, ReleaseListing, ReleaseSummary, StoredRevision};
use crate::cache::WatchFilter;
use crate::discovery::{ApiCatalog, ResourceKind};

/// Watch filter for the storage objects of `driver`: Helm-owned, metadata
/// only (labels carry release, revision and status; payloads are fetched
/// on demand).
pub fn watch_filter(driver: Driver) -> WatchFilter {
    WatchFilter {
        labels: Some(OWNER_SELECTOR.to_owned()),
        fields: match driver {
            Driver::Secret => Some(format!("type={SECRET_TYPE}")),
            Driver::ConfigMap => None,
        },
        metadata_only: true,
    }
}

/// The kind holding release revisions of `driver`.
pub fn storage_kind(catalog: &ApiCatalog, driver: Driver) -> Option<&ResourceKind> {
    catalog.find("", driver.kind())
}

/// Latest revision of every release among watched storage objects, sorted
/// by namespace and release name.
pub fn latest_in(objects: &[Arc<DynamicObject>]) -> Vec<StoredRevision> {
    latest_revisions(objects.iter().filter_map(|o| stored_revision(&o.metadata)))
}

/// Driver in use judging by where releases were found: the one holding
/// more releases (Secret on a tie, as Helm's default); `None` when neither
/// holds any.
pub fn detect_driver(secret_releases: usize, config_map_releases: usize) -> Option<Driver> {
    match (secret_releases, config_map_releases) {
        (0, 0) => None,
        (secrets, config_maps) if config_maps > secrets => Some(Driver::ConfigMap),
        _ => Some(Driver::Secret),
    }
}

/// Where the driver in use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverSource {
    /// Picked in the settings of the context.
    Settings,
    /// Releases were found in its storage.
    Detected,
    /// Nothing found and nothing picked: Helm's default.
    Default,
}

/// The driver for reads and `HELM_DRIVER`, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverChoice {
    pub driver: Driver,
    pub source: DriverSource,
}

impl DriverChoice {
    /// The setting wins; otherwise what was detected, else Secret.
    pub fn resolve(preference: Option<Driver>, detected: Option<Driver>) -> Self {
        match (preference, detected) {
            (Some(driver), _) => Self {
                driver,
                source: DriverSource::Settings,
            },
            (None, Some(driver)) => Self {
                driver,
                source: DriverSource::Detected,
            },
            (None, None) => Self {
                driver: Driver::default(),
                source: DriverSource::Default,
            },
        }
    }
}

impl Default for DriverChoice {
    fn default() -> Self {
        Self::resolve(None, None)
    }
}

/// What is known of one release.
#[derive(Debug)]
struct Entry {
    /// Storage object and its version the state was read from.
    object: String,
    resource_version: Option<String>,
    /// Last decoded summary, kept while a newer revision is being fetched.
    summary: Option<ReleaseSummary>,
    /// Why the last fetch failed.
    error: Option<(bool, String)>,
}

impl Entry {
    fn is_current(&self, stored: &StoredRevision) -> bool {
        self.object == stored.object && self.resource_version == stored.resource_version
    }
}

/// Decoded summaries of the latest revisions, refreshed incrementally from
/// watch snapshots: [`to_fetch`](Self::to_fetch) names the releases whose
/// latest storage object is new or changed, [`record`](Self::record) keeps
/// what was fetched, [`listing`](Self::listing) builds the list.
#[derive(Debug, Default)]
pub struct ReleaseFeed {
    entries: BTreeMap<(String, String), Entry>,
}

fn key(stored: &StoredRevision) -> (String, String) {
    (stored.namespace.clone(), stored.release.clone())
}

impl ReleaseFeed {
    /// Forgets everything, so the next refresh fetches every release.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Latest revisions not fetched yet at their current object version.
    /// A failed fetch is not retried until the object changes.
    pub fn to_fetch(&self, latest: &[StoredRevision]) -> Vec<StoredRevision> {
        latest
            .iter()
            .filter(|stored| {
                self.entries
                    .get(&key(stored))
                    .is_none_or(|entry| !entry.is_current(stored))
            })
            .cloned()
            .collect()
    }

    /// Stores fetch results (see [`Releases::summaries`](super::Releases::summaries)).
    pub fn record(
        &mut self,
        fetched: Vec<(StoredRevision, Result<Option<ReleaseSummary>, HelmError>)>,
    ) {
        for (stored, result) in fetched {
            let entry = self.entries.entry(key(&stored)).or_insert_with(|| Entry {
                object: String::new(),
                resource_version: None,
                summary: None,
                error: None,
            });
            entry.object = stored.object;
            entry.resource_version = stored.resource_version;
            match result {
                Ok(Some(summary)) => {
                    entry.summary = Some(summary);
                    entry.error = None;
                }
                // Deleted meanwhile: the watch drops it soon.
                Ok(None) => entry.error = None,
                Err(error) => entry.error = Some((error.is_forbidden(), error.to_string())),
            }
        }
    }

    /// Releases among `latest` with what is known of them, sorted by
    /// namespace and name. Releases not fetched yet are left out; a release
    /// being refreshed shows its previous summary. `failures` (e.g. failing
    /// watches by namespace) are listed with failed fetches. Entries of
    /// releases that are gone are dropped.
    pub fn listing(
        &mut self,
        latest: &[StoredRevision],
        failures: Vec<(String, HelmError)>,
    ) -> ReleaseListing {
        let wanted: std::collections::BTreeSet<_> = latest.iter().map(key).collect();
        self.entries.retain(|key, _| wanted.contains(key));
        let mut listing = ReleaseListing {
            releases: Vec::new(),
            errors: failures,
        };
        for stored in latest {
            let Some(entry) = self.entries.get(&key(stored)) else {
                continue;
            };
            if let Some(summary) = &entry.summary {
                listing.releases.push(summary.clone());
            }
            if let Some((forbidden, message)) = &entry.error {
                listing.errors.push((
                    stored.namespace.clone(),
                    HelmError::Unavailable {
                        forbidden: *forbidden,
                        message: format!("release {}: {message}", stored.release),
                    },
                ));
            }
        }
        listing
            .releases
            .sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
        listing.errors.sort_by(|a, b| a.0.cmp(&b.0));
        listing
    }

    /// Whether every release in `latest` has been fetched at least once.
    pub fn is_complete(&self, latest: &[StoredRevision]) -> bool {
        latest.iter().all(|stored| {
            self.entries
                .get(&key(stored))
                .is_some_and(|entry| entry.summary.is_some() || entry.error.is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::*;
    use crate::helm::store::object_name;
    use crate::helm::{ChartInfo, ReleaseStatus};

    fn object(namespace: &str, release: &str, revision: u32, rv: &str) -> Arc<DynamicObject> {
        Arc::new(DynamicObject {
            types: None,
            metadata: ObjectMeta {
                namespace: Some(namespace.into()),
                name: Some(object_name(release, revision)),
                resource_version: Some(rv.into()),
                labels: Some(BTreeMap::from([
                    ("owner".into(), "helm".into()),
                    ("name".into(), release.into()),
                    ("version".into(), revision.to_string()),
                ])),
                ..Default::default()
            },
            data: serde_json::json!({}),
        })
    }

    fn summary(stored: &StoredRevision, status: ReleaseStatus) -> ReleaseSummary {
        ReleaseSummary {
            name: stored.release.clone(),
            namespace: stored.namespace.clone(),
            revision: stored.revision,
            status,
            chart: ChartInfo::default(),
            updated: None,
            description: String::new(),
        }
    }

    fn fetch_all(feed: &mut ReleaseFeed, latest: &[StoredRevision], status: ReleaseStatus) {
        let fetched = feed
            .to_fetch(latest)
            .into_iter()
            .map(|stored| {
                let summary = summary(&stored, status.clone());
                (stored, Ok(Some(summary)))
            })
            .collect();
        feed.record(fetched);
    }

    #[test]
    fn detects_the_driver_holding_releases() {
        assert_eq!(detect_driver(0, 0), None);
        assert_eq!(detect_driver(2, 0), Some(Driver::Secret));
        assert_eq!(detect_driver(0, 1), Some(Driver::ConfigMap));
        assert_eq!(detect_driver(1, 3), Some(Driver::ConfigMap));
        assert_eq!(detect_driver(2, 2), Some(Driver::Secret));
    }

    #[test]
    fn settings_win_over_detection() {
        let choice = DriverChoice::resolve(Some(Driver::Secret), Some(Driver::ConfigMap));
        assert_eq!(choice.driver, Driver::Secret);
        assert_eq!(choice.source, DriverSource::Settings);
        let choice = DriverChoice::resolve(None, Some(Driver::ConfigMap));
        assert_eq!(
            (choice.driver, choice.source),
            (Driver::ConfigMap, DriverSource::Detected)
        );
        let choice = DriverChoice::default();
        assert_eq!(
            (choice.driver, choice.source),
            (Driver::Secret, DriverSource::Default)
        );
    }

    #[test]
    fn filters_select_helm_objects_by_labels_only() {
        let secret = watch_filter(Driver::Secret);
        assert_eq!(secret.labels.as_deref(), Some("owner=helm"));
        assert_eq!(secret.fields.as_deref(), Some("type=helm.sh/release.v1"));
        assert!(secret.metadata_only);
        assert_eq!(watch_filter(Driver::ConfigMap).fields, None);
    }

    #[test]
    fn fetches_only_new_and_changed_releases() {
        let mut feed = ReleaseFeed::default();
        let objects = vec![
            object("apps", "web", 1, "10"),
            object("apps", "web", 2, "11"),
            object("apps", "db", 1, "12"),
        ];
        let latest = latest_in(&objects);
        assert_eq!(feed.to_fetch(&latest).len(), 2);
        assert!(!feed.is_complete(&latest));
        fetch_all(&mut feed, &latest, ReleaseStatus::Deployed);
        assert!(feed.is_complete(&latest));
        assert!(feed.to_fetch(&latest).is_empty());

        // An upgrade of web writes v3; a status update of db bumps its version.
        let objects = vec![
            object("apps", "web", 2, "13"),
            object("apps", "web", 3, "14"),
            object("apps", "db", 1, "15"),
            object("apps", "cache", 1, "16"),
        ];
        let latest = latest_in(&objects);
        let fetch: Vec<_> = feed
            .to_fetch(&latest)
            .iter()
            .map(|s| format!("{}.v{}", s.release, s.revision))
            .collect();
        assert_eq!(fetch, ["cache.v1", "db.v1", "web.v3"]);

        // Until fetched, known releases keep their previous summary and the
        // new one is not shown yet.
        let listing = feed.listing(&latest, Vec::new());
        let shown: Vec<_> = listing
            .releases
            .iter()
            .map(|r| format!("{}.v{}", r.name, r.revision))
            .collect();
        assert_eq!(shown, ["db.v1", "web.v2"]);
    }

    #[test]
    fn gone_releases_are_dropped_and_failures_kept_until_changed() {
        let mut feed = ReleaseFeed::default();
        let latest = latest_in(&[object("apps", "web", 1, "1"), object("apps", "db", 1, "2")]);
        let fetched = latest
            .iter()
            .map(|stored| {
                let result = if stored.release == "db" {
                    Err(HelmError::MissingPayload("apps/db".into()))
                } else {
                    Ok(Some(summary(stored, ReleaseStatus::Deployed)))
                };
                (stored.clone(), result)
            })
            .collect();
        feed.record(fetched);
        // The failure is not retried while the object stays the same.
        assert!(feed.to_fetch(&latest).is_empty());
        let listing = feed.listing(
            &latest,
            vec![(
                "locked".into(),
                HelmError::Unavailable {
                    forbidden: true,
                    message: "forbidden".into(),
                },
            )],
        );
        assert_eq!(listing.releases.len(), 1);
        let errors: Vec<_> = listing
            .errors
            .iter()
            .map(|(ns, e)| format!("{ns}: {e}"))
            .collect();
        assert_eq!(
            errors,
            [
                "apps: release db: storage object apps/db has no release payload",
                "locked: forbidden"
            ]
        );
        assert!(listing.errors[1].1.is_forbidden());

        // web is uninstalled: its objects are deleted.
        let latest = latest_in(&[object("apps", "db", 1, "3")]);
        assert_eq!(feed.to_fetch(&latest).len(), 1);
        let listing = feed.listing(&latest, Vec::new());
        assert!(listing.releases.is_empty());
        assert_eq!(feed.entries.len(), 1);

        feed.clear();
        assert_eq!(feed.to_fetch(&latest).len(), 1);
    }
}
