//! Resources of a rendered release manifest: what `helm uninstall` deletes
//! and what a `helm upgrade` adds, changes or removes.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

use super::{Sensitive, unified_diff};
use crate::apply::yaml;

/// `helm.sh/resource-policy: keep` leaves a resource in place on uninstall
/// and when an upgrade drops it.
const RESOURCE_POLICY: &str = "helm.sh/resource-policy";

/// One object of a manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestResource {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    /// As written in the manifest; `None` for cluster-scoped objects and
    /// for namespaced ones Helm puts into the release namespace.
    pub namespace: Option<String>,
    /// Annotated `helm.sh/resource-policy: keep`.
    pub keep: bool,
}

impl ManifestResource {
    /// Identity regardless of the keep annotation.
    fn key(&self) -> (String, String, String, Option<String>) {
        (
            group(&self.api_version).to_owned(),
            self.kind.clone(),
            self.name.clone(),
            self.namespace.clone(),
        )
    }
}

/// `Deployment web`, `Role apps/reader`, with `(kept)` for kept ones.
impl fmt::Display for ManifestResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.kind)?;
        if let Some(namespace) = &self.namespace {
            write!(f, "{namespace}/")?;
        }
        f.write_str(&self.name)?;
        if self.keep {
            f.write_str(" (kept: resource-policy)")?;
        }
        Ok(())
    }
}

/// API group of an `apiVersion`: a version bump is the same object.
fn group(api_version: &str) -> &str {
    api_version.rsplit_once('/').map_or("", |(group, _)| group)
}

/// Objects of `manifest` in order. Documents that don't parse or have no
/// kind or name are skipped: Helm's own manifest is well-formed.
pub fn manifest_resources(manifest: &str) -> Vec<ManifestResource> {
    documents(manifest).into_iter().map(|(r, _)| r).collect()
}

pub(super) fn documents(manifest: &str) -> Vec<(ManifestResource, Value)> {
    // Parse document by document so one odd document does not hide the rest.
    split(manifest)
        .into_iter()
        .filter_map(|text| yaml::parse(text).ok())
        .flatten()
        .filter_map(|doc| Some((resource(&doc.value)?, doc.value)))
        .collect()
}

/// Documents split on `---` lines.
fn split(manifest: &str) -> Vec<&str> {
    let mut starts = vec![0];
    let mut offset = 0;
    for line in manifest.split_inclusive('\n') {
        if line.trim_end() == "---" || line.starts_with("--- ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts.push(manifest.len());
    starts
        .windows(2)
        .map(|pair| &manifest[pair[0]..pair[1]])
        .collect()
}

fn resource(value: &Value) -> Option<ManifestResource> {
    let text = |value: &Value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let metadata = &value["metadata"];
    Some(ManifestResource {
        api_version: text(&value["apiVersion"]).unwrap_or_default(),
        kind: text(&value["kind"])?,
        name: text(&metadata["name"])?,
        namespace: text(&metadata["namespace"]),
        keep: metadata["annotations"][RESOURCE_POLICY]
            .as_str()
            .is_some_and(|policy| policy.trim().eq_ignore_ascii_case("keep")),
    })
}

/// How an upgrade changes one object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Added,
    Changed,
    /// Deleted by the upgrade, unless the old object is kept.
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceChange {
    pub kind: ChangeKind,
    /// The new object, or the old one when removed.
    pub resource: ManifestResource,
}

/// Comparison of the deployed manifest with the one an upgrade renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestDiff {
    /// Added, changed and removed objects, in that order, unchanged left out.
    pub changes: Vec<ResourceChange>,
    /// `diff -u` of the whole manifests; empty when they are equal.
    pub text: Sensitive<String>,
}

impl ManifestDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.text.expose().is_empty()
    }
}

/// Compares the `old` (deployed) manifest with the `new` one.
pub fn diff_manifests(old: &str, new: &str) -> ManifestDiff {
    let index = |manifest: &str| -> BTreeMap<_, _> {
        documents(manifest)
            .into_iter()
            .map(|(resource, value)| (resource.key(), (resource, value)))
            .collect()
    };
    let (before, after) = (index(old), index(new));
    let mut changes = Vec::new();
    for (key, (resource, value)) in &after {
        let kind = match before.get(key) {
            None => ChangeKind::Added,
            Some((_, old)) if old != value => ChangeKind::Changed,
            Some(_) => continue,
        };
        changes.push(ResourceChange {
            kind,
            resource: resource.clone(),
        });
    }
    for (key, (resource, _)) in &before {
        if !after.contains_key(key) {
            changes.push(ResourceChange {
                kind: ChangeKind::Removed,
                resource: resource.clone(),
            });
        }
    }
    changes.sort_by(|a, b| (a.kind, &a.resource).cmp(&(b.kind, &b.resource)));
    ManifestDiff {
        changes,
        text: Sensitive::new(unified_diff(old, new, "deployed", "upgrade")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPLOYED: &str = "---
# Source: web/templates/service.yaml
apiVersion: v1
kind: Service
metadata:
  name: web
spec:
  ports:
    - port: 80
---
# Source: web/templates/deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 1
---
# Source: web/templates/pvc.yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: data
  annotations:
    helm.sh/resource-policy: keep
---
# Source: web/templates/role.yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: reader
  namespace: other
";

    #[test]
    fn lists_resources_with_kept_ones_marked() {
        let resources = manifest_resources(DEPLOYED);
        let shown: Vec<String> = resources.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "Service web",
                "Deployment web",
                "PersistentVolumeClaim data (kept: resource-policy)",
                "Role other/reader"
            ]
        );
        assert_eq!(resources[1].api_version, "apps/v1");
    }

    #[test]
    fn skips_empty_and_broken_documents() {
        let manifest = "---\n# only a comment\n---\nkind: [\n---\nkind: ConfigMap\nmetadata:\n  name: a\n---\nkind: Secret\n";
        let resources = manifest_resources(manifest);
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].name, "a");
        assert!(manifest_resources("").is_empty());
    }

    #[test]
    fn classifies_changes() {
        let upgraded = DEPLOYED
            .replace("replicas: 1", "replicas: 3")
            .replace("kind: Role\n", "kind: RoleBinding\n");
        let diff = diff_manifests(DEPLOYED, &upgraded);
        let changes: Vec<(ChangeKind, String)> = diff
            .changes
            .iter()
            .map(|c| (c.kind, c.resource.to_string()))
            .collect();
        assert_eq!(
            changes,
            [
                (ChangeKind::Added, "RoleBinding other/reader".to_owned()),
                (ChangeKind::Changed, "Deployment web".to_owned()),
                (ChangeKind::Removed, "Role other/reader".to_owned()),
            ]
        );
        let text = diff.text.expose();
        assert!(text.starts_with("--- deployed\n+++ upgrade\n"), "{text}");
        assert!(text.contains("-  replicas: 1\n+  replicas: 3\n"), "{text}");
    }

    #[test]
    fn version_bumps_and_comments_are_not_changes() {
        let upgraded = DEPLOYED
            .replace("apps/v1", "apps/v2")
            .replace("# Source: web/templates/service.yaml", "# moved");
        let diff = diff_manifests(DEPLOYED, &upgraded);
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].kind, ChangeKind::Changed);
        assert_eq!(diff.changes[0].resource.kind, "Deployment");
        assert!(!diff.text.expose().is_empty());

        let same = diff_manifests(DEPLOYED, DEPLOYED);
        assert!(same.is_empty());
    }
}
