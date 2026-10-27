//! Read-only Helm 3 support, implemented natively over the release storage
//! objects (no `helm` binary, nothing installed in the cluster).
//!
//! Helm keeps one Secret (default driver) or ConfigMap per release revision,
//! named `sh.helm.release.v1.<name>.v<revision>` and labelled
//! `owner=helm,name=<name>,version=<revision>,status=<status>`. Its `release`
//! key holds `base64(gzip(json))`. [`codec`] decodes it, [`values`] rebuilds
//! computed values, [`diff`] compares revisions and [`store`] talks to the API.
//!
//! Values and manifests may contain secrets, so they are wrapped in
//! [`Sensitive`]: the UI should mask them until the user reveals them.

pub mod chart;
pub mod claims;
pub mod cli;
pub mod codec;
pub mod diagnose;
pub mod diff;
pub mod live;
pub mod manifest;
pub mod oci;
pub mod progress;
pub mod repo;
pub mod store;
pub mod target_namespace;
pub mod updates;
pub mod values;

use std::collections::BTreeMap;
use std::fmt;

use k8s_openapi::jiff::Timestamp;

pub use chart::StoredChart;
pub use codec::{decode_chart, decode_release, encode_release, release_from_json};
pub use diff::{RevisionDiff, diff_revisions, unified_diff};
pub use live::{DriverChoice, DriverSource, ReleaseFeed};
pub use manifest::{ManifestDiff, ManifestResource, diff_manifests, manifest_resources};
pub use store::{Driver, ReleaseListing, Releases, StoredRevision};
pub use values::{ComputedValues, check_values_yaml, coalesce_values, values_to_yaml};

#[derive(Debug, thiserror::Error)]
pub enum HelmError {
    #[error("API request failed: {0}")]
    Api(#[source] Box<kube::Error>),
    #[error("release {namespace}/{name} not found")]
    NotFound { namespace: String, name: String },
    #[error("revision {revision} of release {namespace}/{name} not found")]
    RevisionNotFound {
        namespace: String,
        name: String,
        revision: u32,
    },
    #[error("storage object {0} has no release payload")]
    MissingPayload(String),
    #[error("release payload is not valid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("release payload is not valid gzip: {0}")]
    Gzip(#[source] std::io::Error),
    #[error("release payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Storage objects could not be watched or read (the watch retries).
    #[error("{message}")]
    Unavailable { forbidden: bool, message: String },
}

impl HelmError {
    /// The API refused the request (HTTP 403, RBAC).
    pub fn is_forbidden(&self) -> bool {
        match self {
            Self::Api(error) => {
                matches!(error.as_ref(), kube::Error::Api(status) if status.is_forbidden())
            }
            Self::Unavailable { forbidden, .. } => *forbidden,
            _ => false,
        }
    }
}

impl From<kube::Error> for HelmError {
    fn from(error: kube::Error) -> Self {
        Self::Api(Box::new(error))
    }
}

/// Data that may contain secrets (values, rendered manifests). It is returned
/// as is, but `Debug` is redacted and the UI should mask it by default.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Sensitive<T>(T);

impl<T> Sensitive<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Access to the secret data; call only to show it after the user asked.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Sensitive<U> {
        Sensitive(f(self.0))
    }
}

impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sensitive(<redacted>)")
    }
}

/// Release status as stored by Helm (`info.status`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReleaseStatus {
    Unknown,
    Deployed,
    Uninstalled,
    Superseded,
    Failed,
    Uninstalling,
    PendingInstall,
    PendingUpgrade,
    PendingRollback,
    /// A status this version does not know.
    Other(String),
}

impl ReleaseStatus {
    pub fn parse(status: &str) -> Self {
        match status {
            "unknown" | "" => Self::Unknown,
            "deployed" => Self::Deployed,
            "uninstalled" => Self::Uninstalled,
            "superseded" => Self::Superseded,
            "failed" => Self::Failed,
            "uninstalling" => Self::Uninstalling,
            "pending-install" => Self::PendingInstall,
            "pending-upgrade" => Self::PendingUpgrade,
            "pending-rollback" => Self::PendingRollback,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The string Helm uses (`helm list` STATUS column).
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "unknown",
            Self::Deployed => "deployed",
            Self::Uninstalled => "uninstalled",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
            Self::Uninstalling => "uninstalling",
            Self::PendingInstall => "pending-install",
            Self::PendingUpgrade => "pending-upgrade",
            Self::PendingRollback => "pending-rollback",
            Self::Other(other) => other,
        }
    }

    /// An install/upgrade/rollback/uninstall is in progress.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            Self::PendingInstall
                | Self::PendingUpgrade
                | Self::PendingRollback
                | Self::Uninstalling
        )
    }
}

impl fmt::Display for ReleaseStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Chart metadata kept in the release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChartInfo {
    pub name: String,
    pub version: String,
    pub app_version: Option<String>,
    pub description: Option<String>,
    /// URL of the chart's icon (`icon` in `Chart.yaml`).
    pub icon: Option<String>,
    /// Project home page (`home` in `Chart.yaml`).
    pub home: Option<String>,
    /// Source code URLs (`sources` in `Chart.yaml`).
    pub sources: Vec<String>,
}

impl ChartInfo {
    /// `name-version`, as in the `helm list` CHART column.
    pub fn reference(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// One row of `helm list` / `helm history`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSummary {
    pub name: String,
    pub namespace: String,
    pub revision: u32,
    pub status: ReleaseStatus,
    pub chart: ChartInfo,
    /// `info.last_deployed`.
    pub updated: Option<Timestamp>,
    pub description: String,
}

/// A hook (`helm.sh/hook` annotated resource) of a revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub name: String,
    pub kind: String,
    /// Template path inside the chart.
    pub path: String,
    /// `pre-install`, `post-upgrade`, `test`…
    pub events: Vec<String>,
    pub weight: i64,
    pub delete_policies: Vec<String>,
    pub last_run: Option<HookRun>,
    pub manifest: Sensitive<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookRun {
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    /// `Running`, `Succeeded`, `Failed` or `Unknown`.
    pub phase: String,
}

/// A fully decoded release revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub name: String,
    pub namespace: String,
    pub revision: u32,
    pub status: ReleaseStatus,
    pub chart: ChartInfo,
    pub first_deployed: Option<Timestamp>,
    pub last_deployed: Option<Timestamp>,
    pub deleted: Option<Timestamp>,
    pub description: String,
    pub notes: String,
    /// Values supplied by the user (`helm get values`).
    pub user_values: Sensitive<serde_json::Value>,
    /// Default `values.yaml` of the chart.
    pub chart_values: Sensitive<serde_json::Value>,
    /// Rendered manifest without hooks (`helm get manifest`).
    pub manifest: Sensitive<String>,
    pub hooks: Vec<Hook>,
    /// Custom release labels (`helm install --labels`).
    pub labels: BTreeMap<String, String>,
}

impl Release {
    pub fn summary(&self) -> ReleaseSummary {
        ReleaseSummary {
            name: self.name.clone(),
            namespace: self.namespace.clone(),
            revision: self.revision,
            status: self.status.clone(),
            chart: self.chart.clone(),
            updated: self.last_deployed,
            description: self.description.clone(),
        }
    }

    /// Chart defaults merged with user values (`helm get values --all`).
    pub fn computed_values(&self) -> ComputedValues {
        coalesce_values(self.chart_values.expose(), self.user_values.expose())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: u16) -> HelmError {
        let status = serde_json::from_value(serde_json::json!({
            "status": "Failure", "code": code, "message": "denied",
            "reason": if code == 403 { "Forbidden" } else { "NotFound" }
        }))
        .unwrap();
        HelmError::from(kube::Error::Api(Box::new(status)))
    }

    #[test]
    fn detects_forbidden_api_errors() {
        assert!(api_error(403).is_forbidden());
        assert!(!api_error(404).is_forbidden());
        assert!(!HelmError::MissingPayload("x".into()).is_forbidden());
    }
}
