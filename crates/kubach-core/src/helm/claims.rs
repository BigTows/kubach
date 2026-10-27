//! PersistentVolumeClaims a `helm uninstall` leaves behind (#273).
//!
//! Helm deletes the objects of the release manifest, but not the claims a
//! StatefulSet created from its `volumeClaimTemplates` (Kubernetes keeps
//! them on purpose), nor claims the manifest marks
//! `helm.sh/resource-policy: keep`. Charts usually label what belongs to a
//! release `app.kubernetes.io/instance=<release>` or `release=<release>`, and
//! StatefulSet claims are named `<template>-<statefulset>-<ordinal>`.

use k8s_openapi::api::core::v1::PersistentVolumeClaim;
use kube::api::{Api, ListParams};
use kube::core::{ApiResource, DynamicObject};
use kube::{Client, ResourceExt as _};

use super::manifest::documents;
use crate::plugin::ResourcePlugin as _;
use crate::plugin::builtin::{PersistentVolumeClaimPlugin, action};

/// Labels charts put on what belongs to a release.
const RELEASE_LABELS: [&str; 2] = ["app.kubernetes.io/instance", "release"];

/// A StatefulSet of a manifest and the names of its claim templates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimTemplates {
    pub statefulset: String,
    pub templates: Vec<String>,
}

/// A claim left behind, with the UID it was listed with: deleting it later
/// leaves alone a claim recreated under the same name meanwhile (#345).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeftoverClaim {
    pub name: String,
    pub uid: Option<String>,
}

/// StatefulSets of the release `manifest` in `namespace` with claim
/// templates.
pub fn claim_templates(manifest: &str, namespace: &str) -> Vec<ClaimTemplates> {
    documents(manifest)
        .into_iter()
        .filter(|(resource, _)| resource.kind == "StatefulSet")
        .filter(|(resource, _)| {
            resource
                .namespace
                .as_deref()
                .is_none_or(|ns| ns == namespace)
        })
        .filter_map(|(resource, value)| {
            let templates: Vec<String> = value["spec"]["volumeClaimTemplates"]
                .as_array()?
                .iter()
                .filter_map(|template| template["metadata"]["name"].as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
            (!templates.is_empty()).then_some(ClaimTemplates {
                statefulset: resource.name,
                templates,
            })
        })
        .collect()
}

/// Whether `claim` is `<template>-<statefulset>-<ordinal>` of one of `sets`.
fn from_template(claim: &str, sets: &[ClaimTemplates]) -> bool {
    sets.iter().any(|set| {
        set.templates.iter().any(|template| {
            claim
                .strip_prefix(template.as_str())
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|rest| rest.strip_prefix(set.statefulset.as_str()))
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|ordinal| {
                    !ordinal.is_empty() && ordinal.chars().all(|c| c.is_ascii_digit())
                })
        })
    })
}

/// The claims in `claims` that stay after uninstalling `release`
/// from `namespace`, sorted: labelled for the release or created from a
/// claim template of its StatefulSets, and not deleted with the manifest
/// (a claim in the manifest goes, unless it is kept). Claims already being
/// deleted are left out.
pub fn leftover_claims(
    release: &str,
    namespace: &str,
    manifest: &str,
    claims: &[PersistentVolumeClaim],
) -> Vec<LeftoverClaim> {
    let sets = claim_templates(manifest, namespace);
    let deleted: Vec<String> = documents(manifest)
        .into_iter()
        .map(|(resource, _)| resource)
        .filter(|resource| resource.kind == "PersistentVolumeClaim" && !resource.keep)
        .filter(|resource| {
            resource
                .namespace
                .as_deref()
                .is_none_or(|ns| ns == namespace)
        })
        .map(|resource| resource.name)
        .collect();
    let mut left: Vec<LeftoverClaim> = claims
        .iter()
        .filter(|claim| claim.metadata.deletion_timestamp.is_none())
        .filter(|claim| claim.namespace().is_none_or(|ns| ns == namespace))
        .filter(|claim| {
            let labels = claim.labels();
            let labelled = RELEASE_LABELS
                .iter()
                .any(|key| labels.get(*key).is_some_and(|value| value == release));
            labelled || from_template(&claim.name_any(), &sets)
        })
        .map(|claim| LeftoverClaim {
            name: claim.name_any(),
            uid: claim.uid(),
        })
        .filter(|claim| !deleted.contains(&claim.name))
        .collect();
    left.sort();
    left.dedup_by(|a, b| a.name == b.name);
    left
}

/// Claims of `namespace`.
pub async fn list_claims(
    client: &Client,
    namespace: &str,
) -> Result<Vec<PersistentVolumeClaim>, kube::Error> {
    let api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), namespace);
    Ok(api.list(&ListParams::default()).await?.items)
}

/// Deletes the claims of `namespace` one by one with the claim plugin's
/// Delete action (#274), each only if it still has the listed UID; the
/// error text of each failure.
pub async fn delete_claims(
    client: &Client,
    namespace: &str,
    claims: &[LeftoverClaim],
) -> Vec<(String, Result<(), String>)> {
    let resource = ApiResource::erase::<PersistentVolumeClaim>(&());
    let mut results = Vec::with_capacity(claims.len());
    for LeftoverClaim { name, uid } in claims {
        let mut object = DynamicObject::new(name, &resource).within(namespace);
        object.metadata.uid = uid.clone();
        let result = PersistentVolumeClaimPlugin
            .run_action(client, &object, &action::DELETE, &Default::default())
            .await
            .map_err(|error| crate::plugin::bulk::describe_error(&error).0.english());
        results.push((name.clone(), result));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"---
# Source: db/templates/statefulset.yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: db
spec:
  volumeClaimTemplates:
  - metadata:
      name: data
  - metadata:
      name: logs
---
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: cache
spec: {}
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: uploads
  labels:
    app.kubernetes.io/instance: shop
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: archive
  labels:
    release: shop
  annotations:
    helm.sh/resource-policy: keep
"#;

    fn claim(name: &str, labels: &[(&str, &str)]) -> PersistentVolumeClaim {
        let mut claim = PersistentVolumeClaim::default();
        claim.metadata.name = Some(name.into());
        claim.metadata.namespace = Some("apps".into());
        claim.metadata.labels = Some(
            labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
        claim
    }

    #[test]
    fn finds_claim_templates_of_statefulsets() {
        assert_eq!(
            claim_templates(MANIFEST, "apps"),
            [ClaimTemplates {
                statefulset: "db".into(),
                templates: vec!["data".into(), "logs".into()],
            }]
        );
        assert!(claim_templates("kind: Deployment\nmetadata: {name: x}\n", "apps").is_empty());
    }

    #[test]
    fn claims_left_after_uninstall() {
        let mut deleting = claim("data-db-9", &[]);
        deleting.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                "2026-10-22T12:00:30Z".parse().unwrap(),
            ));
        let mut elsewhere = claim("data-db-5", &[]);
        elsewhere.metadata.namespace = Some("other".into());
        let claims = [
            claim("data-db-0", &[]),
            claim("logs-db-12", &[]),
            claim("data-db-x", &[]),
            claim("data-dbx-0", &[]),
            claim("data-cache-0", &[]),
            claim("cache", &[("app.kubernetes.io/instance", "shop")]),
            claim("legacy", &[("release", "shop")]),
            claim("foreign", &[("app.kubernetes.io/instance", "blog")]),
            // In the manifest: deleted by helm unless kept.
            claim("uploads", &[("app.kubernetes.io/instance", "shop")]),
            claim("archive", &[("release", "shop")]),
            deleting,
            elsewhere,
        ];
        let left = leftover_claims("shop", "apps", MANIFEST, &claims);
        let names: Vec<&str> = left.iter().map(|claim| claim.name.as_str()).collect();
        assert_eq!(
            names,
            ["archive", "cache", "data-db-0", "legacy", "logs-db-12"]
        );
        assert!(leftover_claims("shop", "apps", "", &[claim("x", &[])]).is_empty());
    }
}
