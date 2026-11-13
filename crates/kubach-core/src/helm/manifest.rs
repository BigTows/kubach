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
/// kind or name are skipped: Helm's own manifest is well-formed. What has
/// to be certain goes through [`strict_resources`].
pub fn manifest_resources(manifest: &str) -> Vec<ManifestResource> {
    documents(manifest).into_iter().map(|(r, _)| r).collect()
}

pub(super) fn documents(manifest: &str) -> Vec<(ManifestResource, Value)> {
    // Parse document by document so one odd document does not hide the rest.
    split(manifest)
        .into_iter()
        .filter_map(|text| yaml::parse(text).ok())
        .flatten()
        .flat_map(|doc| flatten(doc.value))
        .filter_map(|value| Some((resource(&value)?, value)))
        .collect()
}

/// The objects of one document: Helm creates the `items` of a document
/// that has them (`kind: List`), not the document itself.
fn flatten(value: Value) -> Vec<Value> {
    match value {
        Value::Object(mut fields) if fields.get("items").is_some_and(Value::is_array) => {
            match fields.remove("items") {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            }
        }
        other => vec![other],
    }
}

/// Why a document of a manifest does not give objects that are certain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// A line that starts with `---` or `...` and is not just that.
    Separator,
    /// Not YAML, or YAML that parsers read in different ways: a repeated
    /// key, a merge key (`<<`).
    Syntax,
    NotAMapping,
    /// No `apiVersion` or `kind` as text.
    NoKind,
    /// No `metadata.name` as text (`generateName` among them).
    NoName,
    /// `metadata.namespace` is not text.
    Namespace,
    /// `items` that is not a list of objects.
    Items,
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Separator => {
                "a line starts with a document marker (--- or ...) and holds more than the marker"
            }
            Self::Syntax => {
                "it is not YAML every parser reads the same way (a syntax error, a repeated key \
                 or a merge key <<)"
            }
            Self::NotAMapping => "it is not a mapping",
            Self::NoKind => "it has no apiVersion or kind as text",
            Self::NoName => "it has no metadata.name as text (generateName is not a name)",
            Self::Namespace => "its metadata.namespace is not text",
            Self::Items => "its items are not a list of objects",
        })
    }
}

/// A document [`strict_documents`] refuses: its number among the documents
/// that hold anything, from 0, and why. Nothing of its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("document {}: {reason}", index + 1)]
pub struct UnreadDocument {
    pub index: usize,
    pub reason: Unread,
}

/// The objects of `manifest` in order, or the first document that does not
/// give certain ones. For whoever decides by the objects of a chart what a
/// user has to be asked: a skipped document would be an object nobody saw.
///
/// Stricter than Helm on purpose, where a document can be read in more than
/// one way: Helm's YAML takes the last of a repeated key and lets a merge
/// key override a key written before it, this parser does neither. The
/// `items` of a document are its objects, as for Helm, and one level only.
pub fn strict_documents(manifest: &str) -> Result<Vec<(ManifestResource, Value)>, UnreadDocument> {
    let mut found = Vec::new();
    let mut index = 0;
    for text in strict_split(manifest) {
        let text = text.map_err(|reason| UnreadDocument { index, reason })?;
        if text.lines().all(yaml::is_blank) {
            continue;
        }
        let fail = |reason| UnreadDocument { index, reason };
        let value = yaml::parse_strict(text).map_err(|()| fail(Unread::Syntax))?;
        if !value.is_object() {
            return Err(fail(Unread::NotAMapping));
        }
        let objects = match value.get("items") {
            None => vec![value],
            Some(Value::Array(_)) => flatten(value),
            Some(_) => return Err(fail(Unread::Items)),
        };
        for object in objects {
            found.push((strict_resource(&object).map_err(fail)?, object));
        }
        index += 1;
    }
    Ok(found)
}

/// [`strict_documents`] without the parsed objects.
pub fn strict_resources(manifest: &str) -> Result<Vec<ManifestResource>, UnreadDocument> {
    Ok(strict_documents(manifest)?
        .into_iter()
        .map(|(resource, _)| resource)
        .collect())
}

/// Documents split on lines that are `---` and nothing else. Helm writes
/// only such lines between the documents of a manifest; a document with
/// any other line that starts with a document marker is an error.
fn strict_split(manifest: &str) -> Vec<Result<&str, Unread>> {
    let mut documents = Vec::new();
    let (mut start, mut offset) = (0, 0);
    let mut marker = false;
    let mut close = |start: usize, end: usize, marker: &mut bool| {
        documents.push(if std::mem::take(marker) {
            Err(Unread::Separator)
        } else {
            Ok(&manifest[start..end])
        });
    };
    for line in manifest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            close(start, offset, &mut marker);
            start = offset + line.len();
        } else if line.starts_with("---") || line.starts_with("...") {
            marker = true;
        }
        offset += line.len();
    }
    close(start, manifest.len(), &mut marker);
    documents
}

fn strict_resource(value: &Value) -> Result<ManifestResource, Unread> {
    if !value.is_object() || value.get("items").is_some() {
        return Err(Unread::Items);
    }
    let text = |value: &Value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let metadata = &value["metadata"];
    let namespace = match &metadata["namespace"] {
        Value::Null => None,
        Value::String(namespace) => Some(namespace.clone()).filter(|s| !s.is_empty()),
        _ => return Err(Unread::Namespace),
    };
    Ok(ManifestResource {
        api_version: text(&value["apiVersion"]).ok_or(Unread::NoKind)?,
        kind: text(&value["kind"]).ok_or(Unread::NoKind)?,
        name: text(&metadata["name"]).ok_or(Unread::NoName)?,
        namespace,
        keep: keeps(metadata),
    })
}

/// The objects of `helm template`'s output, each with the template of its
/// `# Source:` comment (#114).
pub(super) fn manifest_documents(manifest: &str) -> Vec<(Option<String>, ManifestResource)> {
    split(manifest)
        .into_iter()
        .flat_map(|text| {
            let source = text
                .lines()
                .find_map(|line| line.strip_prefix("# Source: "))
                .map(|path| path.trim().to_owned());
            documents(text)
                .into_iter()
                .map(move |(resource, _)| (source.clone(), resource))
        })
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
        keep: keeps(metadata),
    })
}

fn keeps(metadata: &Value) -> bool {
    metadata["annotations"][RESOURCE_POLICY]
        .as_str()
        .is_some_and(|policy| policy.trim().eq_ignore_ascii_case("keep"))
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

/// One object an upgrade changes, with both sides as parsed: Secret values
/// included, so masking them is up to whoever shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectChange {
    pub kind: ChangeKind,
    /// The new object, or the old one when removed.
    pub resource: ManifestResource,
    pub old: Option<Sensitive<Value>>,
    pub new: Option<Sensitive<Value>>,
}

/// What differs between two manifests, object by object (#115).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectChanges {
    /// Added, changed and removed objects, in that order.
    pub changes: Vec<ObjectChange>,
    /// Objects on both sides that stay as they are.
    pub unchanged: usize,
}

/// Compares `old` with `new` object by object.
pub fn object_changes(old: &str, new: &str) -> ObjectChanges {
    let index = |manifest: &str| -> BTreeMap<_, _> {
        documents(manifest)
            .into_iter()
            .map(|(resource, value)| (resource.key(), (resource, value)))
            .collect()
    };
    let (mut before, after) = (index(old), index(new));
    let mut found = ObjectChanges::default();
    for (key, (resource, value)) in after {
        let previous = before.remove(&key).map(|(_, old)| old);
        let kind = match &previous {
            None => ChangeKind::Added,
            Some(old) if *old != value => ChangeKind::Changed,
            Some(_) => {
                found.unchanged += 1;
                continue;
            }
        };
        found.changes.push(ObjectChange {
            kind,
            resource,
            old: previous.map(Sensitive::new),
            new: Some(Sensitive::new(value)),
        });
    }
    for (resource, value) in before.into_values() {
        found.changes.push(ObjectChange {
            kind: ChangeKind::Removed,
            resource,
            old: Some(Sensitive::new(value)),
            new: None,
        });
    }
    found
        .changes
        .sort_by(|a, b| (a.kind, &a.resource).cmp(&(b.kind, &b.resource)));
    found
}

/// The objects of `new` that `old` does not have as they are: what a change
/// from `old` to `new` creates or changes, in the order of `new`.
pub fn written_resources(old: &str, new: &str) -> Vec<ManifestResource> {
    let before: BTreeMap<_, _> = documents(old)
        .into_iter()
        .map(|(resource, value)| (resource.key(), value))
        .collect();
    documents(new)
        .into_iter()
        .filter(|(resource, value)| before.get(&resource.key()) != Some(value))
        .map(|(resource, _)| resource)
        .collect()
}

/// Documents of `manifest` that hold something but give no object: they do
/// not parse, or have no kind or name. [`object_changes`] cannot see them.
pub fn unreadable_documents(manifest: &str) -> usize {
    split(manifest)
        .into_iter()
        .map(|text| match yaml::parse(text) {
            Ok(docs) => docs
                .into_iter()
                .filter(|doc| {
                    flatten(doc.value.clone())
                        .iter()
                        .any(|object| resource(object).is_none())
                })
                .count(),
            Err(_) => 1,
        })
        .sum()
}

/// Compares the `old` (deployed) manifest with the `new` one.
pub fn diff_manifests(old: &str, new: &str) -> ManifestDiff {
    ManifestDiff {
        changes: object_changes(old, new)
            .changes
            .into_iter()
            .map(|change| ResourceChange {
                kind: change.kind,
                resource: change.resource,
            })
            .collect(),
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
    fn object_changes_keep_both_sides_and_count_the_rest() {
        let upgraded = DEPLOYED
            .replace("replicas: 1", "replicas: 3")
            .replace("kind: Role\n", "kind: RoleBinding\n");
        let found = object_changes(DEPLOYED, &upgraded);
        assert_eq!(found.unchanged, 2);
        let changed = &found.changes[1];
        assert_eq!(changed.kind, ChangeKind::Changed);
        let replicas = |side: &Option<Sensitive<Value>>| {
            side.as_ref().unwrap().expose()["spec"]["replicas"].clone()
        };
        assert_eq!(
            (replicas(&changed.old), replicas(&changed.new)),
            (1.into(), 3.into())
        );
        assert!(found.changes[0].old.is_none() && found.changes[2].new.is_none());
    }

    #[test]
    fn counts_the_documents_it_cannot_read() {
        let manifest = "---\n# only a comment\n---\nkind: [\n---\nkind: ConfigMap\nmetadata:\n  name: a\n---\nkind: Secret\n";
        assert_eq!(unreadable_documents(manifest), 2);
        assert_eq!(unreadable_documents(DEPLOYED), 0);
        assert_eq!(unreadable_documents(""), 0);
    }

    const LIST: &str = "---
# Source: web/templates/list.yaml
apiVersion: v1
kind: List
items:
  - apiVersion: rbac.authorization.k8s.io/v1
    kind: ClusterRoleBinding
    metadata:
      name: in-a-list
  - apiVersion: v1
    kind: ConfigMap
    metadata:
      name: settings
      namespace: other
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: plain
";

    fn shown(resources: &[ManifestResource]) -> Vec<String> {
        resources.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_items_of_a_document_are_its_objects() {
        let expected = [
            "ClusterRoleBinding in-a-list",
            "ConfigMap other/settings",
            "ConfigMap plain",
        ];
        assert_eq!(shown(&strict_resources(LIST).unwrap()), expected);
        assert_eq!(shown(&manifest_resources(LIST)), expected);
        assert_eq!(unreadable_documents(LIST), 0);
        // Any document with `items`, whatever its kind, and an empty one.
        let wrapped = LIST.replace("kind: List", "kind: ConfigMap\nmetadata:\n  name: wrapper");
        assert_eq!(shown(&strict_resources(&wrapped).unwrap()), expected);
        let empty = "apiVersion: v1\nkind: List\nitems: []\n";
        assert_eq!(strict_resources(empty).unwrap(), []);
        // An object changed inside a list is a changed object.
        let changed = object_changes(LIST, &LIST.replace("namespace: other", "namespace: third"));
        let kinds: Vec<ChangeKind> = changed.changes.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ChangeKind::Added, ChangeKind::Removed]);
        assert_eq!(changed.unchanged, 2);
    }

    fn refused(manifest: &str) -> (usize, Unread) {
        let error = strict_resources(manifest).unwrap_err();
        (error.index, error.reason)
    }

    #[test]
    fn a_document_without_certain_objects_is_refused_by_its_number() {
        let first = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n---\n";
        let second = |text: &str| refused(&format!("# a comment\n---\n{first}{text}"));
        for (text, reason) in [
            ("kind: [\n", Unread::Syntax),
            // The last of a repeated key wins for Helm.
            (
                "apiVersion: v1\nkind: Secret\nkind: ConfigMap\nmetadata:\n  name: b\n",
                Unread::Syntax,
            ),
            // A merge key overrides the key before it for Helm.
            (
                "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: b\n  namespace: here\n  \
                 <<: {namespace: other}\n",
                Unread::Syntax,
            ),
            ("- a\n- b\n", Unread::NotAMapping),
            ("kind: ConfigMap\nmetadata:\n  name: b\n", Unread::NoKind),
            ("apiVersion: v1\nmetadata:\n  name: b\n", Unread::NoKind),
            (
                "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  generateName: b-\n",
                Unread::NoName,
            ),
            (
                "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: 12\n",
                Unread::NoName,
            ),
            // A number here, a string for a YAML 1.1 parser.
            (
                "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: b\n  namespace: 0o17\n",
                Unread::Namespace,
            ),
            ("apiVersion: v1\nkind: List\nitems:\n", Unread::Items),
            ("apiVersion: v1\nkind: List\nitems: [a]\n", Unread::Items),
            (
                "apiVersion: v1\nkind: List\nitems:\n- apiVersion: v1\n  kind: List\n  items: []\n",
                Unread::Items,
            ),
            (
                "apiVersion: v1\nkind: List\nitems:\n- apiVersion: v1\n  kind: ConfigMap\n",
                Unread::NoName,
            ),
        ] {
            assert_eq!(second(text), (1, reason), "{text}");
        }
        for marker in [
            "---x\n",
            "--- kind: Role\n",
            "--- # note\n",
            "...\n",
            "... \n",
        ] {
            let text = format!("{first}{marker}apiVersion: v1\nkind: Role\nmetadata:\n  name: r\n");
            assert_eq!(refused(&text), (1, Unread::Separator), "{marker}");
        }
        // The message has the number and the reason, nothing of the text.
        let message = strict_resources(&format!("{first}kind: [ secret-text\n"))
            .unwrap_err()
            .to_string();
        assert!(message.starts_with("document 2: "), "{message}");
        assert!(!message.contains("secret-text"), "{message}");
    }

    #[test]
    fn a_well_formed_manifest_reads_the_same_strictly() {
        assert_eq!(
            strict_resources(DEPLOYED).unwrap(),
            manifest_resources(DEPLOYED)
        );
        assert_eq!(strict_resources("").unwrap(), []);
        assert_eq!(
            strict_resources("---\n# only a comment\n---\n").unwrap(),
            []
        );
        // Anchors and an empty namespace are read as Helm reads them.
        let text = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: &n web\n  namespace:\n  \
                    labels:\n    app: *n\n";
        let (resource, value) = strict_documents(text).unwrap().remove(0);
        assert_eq!((resource.name.as_str(), resource.namespace), ("web", None));
        assert_eq!(value["metadata"]["labels"]["app"], "web");
    }

    #[test]
    fn written_resources_are_the_new_and_the_changed() {
        let upgraded = DEPLOYED
            .replace("replicas: 1", "replicas: 3")
            .replace("kind: Role\n", "kind: RoleBinding\n");
        assert_eq!(
            shown(&written_resources(DEPLOYED, &upgraded)),
            ["Deployment web", "RoleBinding other/reader"]
        );
        assert_eq!(
            written_resources("", DEPLOYED),
            manifest_resources(DEPLOYED)
        );
        assert_eq!(written_resources(DEPLOYED, DEPLOYED), []);
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
