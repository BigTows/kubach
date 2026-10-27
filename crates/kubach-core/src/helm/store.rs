//! Reads release revisions from the cluster (Secret or ConfigMap driver).

use std::collections::BTreeMap;
use std::fmt::Debug;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::NamespaceResourceScope;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, ListParams};
use kube::{Client, Resource};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::runtime;

use super::{
    HelmError, Release, ReleaseSummary, RevisionDiff, StoredChart, decode_chart, decode_release,
    diff_revisions,
};

/// Secret type Helm uses for release storage.
pub const SECRET_TYPE: &str = "helm.sh/release.v1";
/// Label selector of Helm release storage objects.
pub const OWNER_SELECTOR: &str = "owner=helm";
/// Parallel GETs when listing releases.
const CONCURRENCY: usize = 8;

/// Helm storage driver (`HELM_DRIVER`). `sql` and `memory` are not supported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    #[default]
    Secret,
    ConfigMap,
}

impl Driver {
    pub const ALL: [Driver; 2] = [Driver::Secret, Driver::ConfigMap];

    /// `Secret` or `ConfigMap`: the kind of the storage objects.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Secret => "Secret",
            Self::ConfigMap => "ConfigMap",
        }
    }
}

/// Read-only access to Helm releases of one cluster.
#[derive(Clone)]
pub struct Releases {
    client: Client,
    driver: Driver,
}

/// A storage object identified by its labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRevision {
    pub namespace: String,
    /// Name of the Secret or ConfigMap.
    pub object: String,
    pub release: String,
    pub revision: u32,
    /// Changes with every write of the object (status updates included).
    pub resource_version: Option<String>,
}

impl Releases {
    pub fn new(client: Client) -> Self {
        Self::with_driver(client, Driver::Secret)
    }

    pub fn with_driver(client: Client, driver: Driver) -> Self {
        Self { client, driver }
    }

    pub fn driver(&self) -> Driver {
        self.driver
    }

    /// Latest revision of every release, sorted by namespace and name.
    /// `namespace: None` lists all namespaces (needs cluster-wide `list`).
    ///
    /// Only labels are listed, then just the latest revision of each release
    /// is fetched and decoded. Uninstalled releases kept with
    /// `--keep-history` are included with status `uninstalled`.
    pub async fn list(&self, namespace: Option<&str>) -> Result<Vec<ReleaseSummary>, HelmError> {
        let metas = self.list_meta(namespace, OWNER_SELECTOR).await?;
        let latest = latest_revisions(metas.iter().filter_map(stored_revision));
        let mut releases: Vec<ReleaseSummary> = futures::stream::iter(latest)
            .map(|stored| async move {
                // Deleted between list and get: skip it.
                match self.get_payload(&stored.namespace, &stored.object).await? {
                    Some(payload) => decode_summary(payload).await.map(Some),
                    None => Ok(None),
                }
            })
            .buffer_unordered(CONCURRENCY)
            .try_filter_map(|summary| async move { Ok(summary) })
            .try_collect()
            .await?;
        releases.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
        Ok(releases)
    }

    /// Decodes the summaries of the given storage objects (e.g. the latest
    /// revisions a watch saw), in their order. `Ok(None)`: the object is gone.
    pub async fn summaries(
        &self,
        stored: Vec<StoredRevision>,
    ) -> Vec<(StoredRevision, Result<Option<ReleaseSummary>, HelmError>)> {
        futures::stream::iter(stored)
            .map(|stored| async move {
                let summary = match self.get_payload(&stored.namespace, &stored.object).await {
                    Ok(Some(payload)) => decode_summary(payload).await.map(Some),
                    Ok(None) => Ok(None),
                    Err(error) => Err(error),
                };
                (stored, summary)
            })
            .buffered(CONCURRENCY)
            .collect()
            .await
    }

    /// Latest revisions in several namespaces (`None`: all namespaces).
    /// A namespace that fails (no access, say) does not hide the others:
    /// its error is returned next to what the rest listed.
    pub async fn list_in(&self, namespaces: Option<&[String]>) -> ReleaseListing {
        let Some(namespaces) = namespaces else {
            return merge_listings(vec![(String::new(), self.list(None).await)]);
        };
        let results = futures::stream::iter(namespaces.iter().cloned())
            .map(|namespace| async move {
                let result = self.list(Some(&namespace)).await;
                (namespace, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        merge_listings(results)
    }

    /// All revisions of a release, oldest first (`helm history`).
    pub async fn history(&self, namespace: &str, name: &str) -> Result<Vec<Release>, HelmError> {
        let selector = format!("{OWNER_SELECTOR},name={name}");
        let payloads = self.list_payloads(namespace, &selector).await?;
        let revisions = runtime::blocking(move || decode_history(&payloads)).await?;
        if revisions.is_empty() {
            return Err(HelmError::NotFound {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            });
        }
        Ok(revisions)
    }

    /// One revision, or the latest one when `revision` is `None`.
    pub async fn get(
        &self,
        namespace: &str,
        name: &str,
        revision: Option<u32>,
    ) -> Result<Release, HelmError> {
        let revision = match revision {
            Some(revision) => revision,
            None => {
                let selector = format!("{OWNER_SELECTOR},name={name}");
                let metas = self.list_meta(Some(namespace), &selector).await?;
                latest_revisions(metas.iter().filter_map(stored_revision))
                    .first()
                    .map(|stored| stored.revision)
                    .ok_or_else(|| HelmError::NotFound {
                        namespace: namespace.to_owned(),
                        name: name.to_owned(),
                    })?
            }
        };
        let payload = self
            .get_payload(namespace, &object_name(name, revision))
            .await?
            .ok_or_else(|| HelmError::RevisionNotFound {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                revision,
            })?;
        runtime::blocking(move || decode_release(&payload)).await
    }

    /// The chart kept in one revision, for `helm upgrade` with the same chart.
    pub async fn stored_chart(
        &self,
        namespace: &str,
        name: &str,
        revision: u32,
    ) -> Result<StoredChart, HelmError> {
        let payload = self
            .get_payload(namespace, &object_name(name, revision))
            .await?
            .ok_or_else(|| HelmError::RevisionNotFound {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                revision,
            })?;
        runtime::blocking(move || decode_chart(&payload)).await
    }

    /// Diff of revision `from` against revision `to`.
    pub async fn diff(
        &self,
        namespace: &str,
        name: &str,
        from: u32,
        to: u32,
    ) -> Result<RevisionDiff, HelmError> {
        let (old, new) = futures::try_join!(
            self.get(namespace, name, Some(from)),
            self.get(namespace, name, Some(to)),
        )?;
        Ok(diff_revisions(&old, &new))
    }

    async fn list_meta(
        &self,
        namespace: Option<&str>,
        selector: &str,
    ) -> Result<Vec<ObjectMeta>, HelmError> {
        let params = ListParams::default().labels(selector);
        let metas = match self.driver {
            Driver::Secret => {
                let params = params.fields(&format!("type={SECRET_TYPE}"));
                metadata(self.api::<Secret>(namespace), &params).await?
            }
            Driver::ConfigMap => metadata(self.api::<ConfigMap>(namespace), &params).await?,
        };
        Ok(metas)
    }

    async fn list_payloads(
        &self,
        namespace: &str,
        selector: &str,
    ) -> Result<Vec<Vec<u8>>, HelmError> {
        let params = ListParams::default().labels(selector);
        let payloads = match self.driver {
            Driver::Secret => {
                let params = params.fields(&format!("type={SECRET_TYPE}"));
                let list = self.api::<Secret>(Some(namespace)).list(&params).await?;
                list.items
                    .iter()
                    .map(secret_payload)
                    .collect::<Result<_, _>>()?
            }
            Driver::ConfigMap => {
                let list = self.api::<ConfigMap>(Some(namespace)).list(&params).await?;
                list.items
                    .iter()
                    .map(config_map_payload)
                    .collect::<Result<_, _>>()?
            }
        };
        Ok(payloads)
    }

    async fn get_payload(
        &self,
        namespace: &str,
        object: &str,
    ) -> Result<Option<Vec<u8>>, HelmError> {
        match self.driver {
            Driver::Secret => self
                .api::<Secret>(Some(namespace))
                .get_opt(object)
                .await?
                .map(|secret| secret_payload(&secret))
                .transpose(),
            Driver::ConfigMap => self
                .api::<ConfigMap>(Some(namespace))
                .get_opt(object)
                .await?
                .map(|config_map| config_map_payload(&config_map))
                .transpose(),
        }
    }

    fn api<K>(&self, namespace: Option<&str>) -> Api<K>
    where
        K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>,
    {
        match namespace {
            Some(namespace) => Api::namespaced(self.client.clone(), namespace),
            None => Api::all(self.client.clone()),
        }
    }
}

async fn metadata<K>(api: Api<K>, params: &ListParams) -> Result<Vec<ObjectMeta>, HelmError>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
{
    let list = api.list_metadata(params).await?;
    Ok(list.items.into_iter().map(|item| item.metadata).collect())
}

/// Result of [`Releases::list_in`].
#[derive(Debug, Default)]
pub struct ReleaseListing {
    /// Sorted by namespace and name.
    pub releases: Vec<ReleaseSummary>,
    /// Namespaces that could not be listed (empty name: all namespaces),
    /// sorted by namespace.
    pub errors: Vec<(String, HelmError)>,
}

fn merge_listings(
    results: Vec<(String, Result<Vec<ReleaseSummary>, HelmError>)>,
) -> ReleaseListing {
    let mut listing = ReleaseListing::default();
    for (namespace, result) in results {
        match result {
            Ok(releases) => listing.releases.extend(releases),
            Err(error) => listing.errors.push((namespace, error)),
        }
    }
    listing
        .releases
        .sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    listing.errors.sort_by(|a, b| a.0.cmp(&b.0));
    listing
}

/// `sh.helm.release.v1.<name>.v<revision>`.
pub fn object_name(release: &str, revision: u32) -> String {
    format!("sh.helm.release.v1.{release}.v{revision}")
}

/// The Helm payload of a release Secret (already un-base64'd by Kubernetes).
pub fn secret_payload(secret: &Secret) -> Result<Vec<u8>, HelmError> {
    secret
        .data
        .as_ref()
        .and_then(|data| data.get("release"))
        .filter(|_| secret.type_.as_deref() == Some(SECRET_TYPE))
        .map(|bytes| bytes.0.clone())
        .ok_or_else(|| HelmError::MissingPayload(object_label(&secret.metadata)))
}

/// The Helm payload of a release ConfigMap.
pub fn config_map_payload(config_map: &ConfigMap) -> Result<Vec<u8>, HelmError> {
    config_map
        .data
        .as_ref()
        .and_then(|data| data.get("release"))
        .map(|text| text.as_bytes().to_vec())
        .ok_or_else(|| HelmError::MissingPayload(object_label(&config_map.metadata)))
}

fn object_label(meta: &ObjectMeta) -> String {
    format!(
        "{}/{}",
        meta.namespace.as_deref().unwrap_or_default(),
        meta.name.as_deref().unwrap_or_default()
    )
}

/// The release and revision a storage object holds, from its labels.
pub fn stored_revision(meta: &ObjectMeta) -> Option<StoredRevision> {
    let labels = meta.labels.as_ref()?;
    Some(StoredRevision {
        namespace: meta.namespace.clone()?,
        object: meta.name.clone()?,
        release: labels.get("name")?.clone(),
        revision: labels.get("version")?.parse().ok()?,
        resource_version: meta.resource_version.clone(),
    })
}

/// Decodes the summary of one payload off the I/O threads: base64, gzip and
/// JSON of a large release take long enough to stall the watches sharing the
/// two-thread runtime (ADR 0011).
async fn decode_summary(payload: Vec<u8>) -> Result<ReleaseSummary, HelmError> {
    runtime::blocking(move || decode_release(&payload).map(|release| release.summary())).await
}

/// Decodes revisions of one release, oldest first.
fn decode_history(payloads: &[Vec<u8>]) -> Result<Vec<Release>, HelmError> {
    let mut revisions = payloads
        .iter()
        .map(|payload| decode_release(payload))
        .collect::<Result<Vec<_>, _>>()?;
    revisions.sort_by_key(|release| release.revision);
    Ok(revisions)
}

/// Highest revision per `(namespace, release)`, sorted by that key.
pub fn latest_revisions(
    revisions: impl IntoIterator<Item = StoredRevision>,
) -> Vec<StoredRevision> {
    let mut latest: BTreeMap<(String, String), StoredRevision> = BTreeMap::new();
    for stored in revisions {
        let key = (stored.namespace.clone(), stored.release.clone());
        match latest.get(&key) {
            Some(current) if current.revision >= stored.revision => {}
            _ => {
                latest.insert(key, stored);
            }
        }
    }
    latest.into_values().collect()
}

#[cfg(test)]
mod tests {
    use k8s_openapi::ByteString;

    use super::*;
    use crate::helm::codec::tests::release_json;
    use crate::helm::{ReleaseStatus, encode_release};

    fn meta(namespace: &str, release: &str, revision: u32) -> ObjectMeta {
        ObjectMeta {
            namespace: Some(namespace.into()),
            name: Some(object_name(release, revision)),
            labels: Some(BTreeMap::from([
                ("owner".into(), "helm".into()),
                ("name".into(), release.into()),
                ("version".into(), revision.to_string()),
                ("status".into(), "deployed".into()),
            ])),
            ..Default::default()
        }
    }

    /// A release Secret as Helm creates it.
    fn secret(revision: u32) -> Secret {
        let payload = encode_release(&release_json(revision, "deployed", revision));
        Secret {
            metadata: meta("apps", "web", revision),
            type_: Some(SECRET_TYPE.into()),
            data: Some(BTreeMap::from([(
                "release".into(),
                ByteString(payload.into_bytes()),
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn picks_latest_revision_per_release() {
        let metas = [
            meta("apps", "web", 2),
            meta("apps", "web", 10),
            meta("apps", "web", 9),
            meta("apps", "db", 1),
            meta("other", "web", 1),
        ];
        let latest = latest_revisions(metas.iter().filter_map(stored_revision));
        let keys: Vec<_> = latest
            .iter()
            .map(|s| (s.namespace.as_str(), s.release.as_str(), s.revision))
            .collect();
        assert_eq!(
            keys,
            [("apps", "db", 1), ("apps", "web", 10), ("other", "web", 1)]
        );
        assert_eq!(latest[1].object, "sh.helm.release.v1.web.v10");
    }

    #[test]
    fn ignores_objects_without_helm_labels() {
        let mut broken = meta("apps", "web", 1);
        broken
            .labels
            .as_mut()
            .unwrap()
            .insert("version".into(), "x".into());
        assert_eq!(stored_revision(&broken), None);
        assert_eq!(stored_revision(&ObjectMeta::default()), None);
    }

    #[test]
    fn decodes_secret_fixture() {
        let release = decode_release(&secret_payload(&secret(2)).unwrap()).unwrap();
        assert_eq!(release.revision, 2);
        assert_eq!(release.status, ReleaseStatus::Deployed);
        assert_eq!(release.summary().chart.reference(), "nginx-1.2.0");
    }

    #[test]
    fn decodes_config_map_fixture() {
        let payload = encode_release(&release_json(1, "failed", 1));
        let config_map = ConfigMap {
            metadata: meta("apps", "web", 1),
            data: Some(BTreeMap::from([("release".into(), payload)])),
            ..Default::default()
        };
        let release = decode_release(&config_map_payload(&config_map).unwrap()).unwrap();
        assert_eq!(release.status, ReleaseStatus::Failed);
    }

    #[test]
    fn rejects_secret_of_other_type() {
        let mut opaque = secret(1);
        opaque.type_ = Some("Opaque".into());
        assert!(matches!(
            secret_payload(&opaque),
            Err(HelmError::MissingPayload(name)) if name == "apps/sh.helm.release.v1.web.v1"
        ));
    }

    #[test]
    fn listings_merge_sorted_and_keep_errors() {
        let summary = |namespace: &str, name: &str| {
            let mut summary = decode_release(&secret_payload(&secret(1)).unwrap())
                .unwrap()
                .summary();
            summary.namespace = namespace.into();
            summary.name = name.into();
            summary
        };
        let listing = merge_listings(vec![
            (
                "web".into(),
                Ok(vec![summary("web", "b"), summary("web", "a")]),
            ),
            (
                "locked".into(),
                Err(HelmError::NotFound {
                    namespace: "locked".into(),
                    name: String::new(),
                }),
            ),
            ("api".into(), Ok(vec![summary("api", "z")])),
        ]);
        let names: Vec<_> = listing
            .releases
            .iter()
            .map(|r| format!("{}/{}", r.namespace, r.name))
            .collect();
        assert_eq!(names, ["api/z", "web/a", "web/b"]);
        assert_eq!(listing.errors.len(), 1);
        assert_eq!(listing.errors[0].0, "locked");
        assert!(!listing.errors[0].1.is_forbidden());
    }

    #[test]
    fn history_is_sorted_by_revision() {
        let payloads: Vec<_> = [3, 1, 2]
            .into_iter()
            .map(|r| secret_payload(&secret(r)).unwrap())
            .collect();
        let history = decode_history(&payloads).unwrap();
        let order: Vec<_> = history
            .iter()
            .map(|r| (r.revision, r.description.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                (1, "Install complete"),
                (2, "Upgrade complete"),
                (3, "Upgrade complete")
            ]
        );
    }
}
