//! GitOps CRD plugins: Argo CD and Flux.
//!
//! The CRDs are read through [`DynamicObject`] field access, so no CRD type
//! crates are needed. The plugins only show up when the cluster serves these
//! kinds.

pub(crate) mod argo;
pub(crate) mod flux;

use kubach_i18n::msg;
use std::sync::Arc;

use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams};
use serde_json::{Value, json};

pub use argo::{ApplicationPlugin, ApplicationSetPlugin};
pub use flux::{FluxKustomizationPlugin, FluxSourcePlugin, HelmReleasePlugin};

use super::name_of;
use crate::plugin::{
    ConditionInfo, Health, HealthStatus, ObjectRef, PluginError, PluginRegistry, Relation,
    RelationKind, ResourcePlugin,
};

/// Registers the Argo CD and Flux plugins.
pub fn register(registry: &mut PluginRegistry) {
    let plugins: [Arc<dyn ResourcePlugin>; 5] = [
        Arc::new(ApplicationPlugin),
        Arc::new(ApplicationSetPlugin),
        Arc::new(FluxKustomizationPlugin),
        Arc::new(HelmReleasePlugin),
        Arc::new(FluxSourcePlugin),
    ];
    for plugin in plugins {
        registry.register(plugin);
    }
}

/// Action ids of the GitOps plugins.
pub mod action {
    use crate::plugin::ActionId;

    pub use super::super::action::{DELETE, RESUME, SUSPEND};
    /// Argo CD: re-compare with Git (`argocd.argoproj.io/refresh=normal`).
    pub const REFRESH: ActionId = ActionId::new("refresh");
    /// Argo CD: refresh, bypassing the manifest cache (`=hard`).
    pub const HARD_REFRESH: ActionId = ActionId::new("hard-refresh");
    /// Argo CD: start a sync operation.
    pub const SYNC: ActionId = ActionId::new("sync");
    /// Flux: request a reconciliation now.
    pub const RECONCILE: ActionId = ActionId::new("reconcile");
}

/// String at a JSON pointer, `""` when absent.
pub(crate) fn s<'a>(obj: &'a DynamicObject, pointer: &str) -> &'a str {
    obj.data
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

pub(crate) fn is_suspended(obj: &DynamicObject) -> bool {
    obj.data.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true)
}

/// `status.conditions`; Argo CD conditions have no `status` and count as
/// `True`: they are events, present while the problem lasts.
pub(crate) fn conditions(obj: &DynamicObject) -> Vec<ConditionInfo> {
    let types = obj.types.as_ref();
    let str_of = |c: &Value, key: &str| c.get(key).and_then(Value::as_str).map(str::to_owned);
    obj.data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let type_ = str_of(c, "type")?;
            Some(super::condition_polarity(types, &type_).condition(
                type_,
                str_of(c, "status").unwrap_or_else(|| "True".into()),
                str_of(c, "reason"),
                str_of(c, "message"),
                str_of(c, "lastTransitionTime").and_then(|t| t.parse().ok()),
            ))
        })
        .collect()
}

/// API handle for the object's own kind; `plural` is the resource name.
pub(crate) fn api_of(
    client: &Client,
    obj: &DynamicObject,
    plural: &str,
) -> Result<Api<DynamicObject>, PluginError> {
    let types = obj
        .types
        .as_ref()
        .ok_or_else(|| PluginError::InvalidObject("object has no apiVersion/kind".into()))?;
    let (group, version) = types
        .api_version
        .split_once('/')
        .unwrap_or(("", &types.api_version));
    let gvk = GroupVersionKind::gvk(group, version, &types.kind);
    let resource = ApiResource::from_gvk_with_plural(&gvk, plural);
    Ok(match obj.metadata.namespace.as_deref() {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
        None => Api::all_with(client.clone(), &resource),
    })
}

/// Merge-patches the object.
pub(crate) async fn merge_patch(
    client: &Client,
    obj: &DynamicObject,
    plural: &str,
    patch: Value,
) -> Result<(), PluginError> {
    api_of(client, obj, plural)?
        .patch(name_of(obj)?, &PatchParams::default(), &Patch::Merge(patch))
        .await?;
    Ok(())
}

/// Deletes the object with background propagation, like `kubectl delete`.
pub(crate) async fn delete_with_plural(
    client: &Client,
    obj: &DynamicObject,
    plural: &str,
) -> Result<(), PluginError> {
    let result = api_of(client, obj, plural)?
        .delete(name_of(obj)?, &super::delete_params(obj))
        .await;
    Ok(crate::deletion::finish(result)?)
}

/// Merge patch setting one annotation.
pub(crate) fn annotation_patch(key: &str, value: &str) -> Value {
    json!({"metadata": {"annotations": {key: value}}})
}

/// Health from the kstatus `Ready`/`Reconciling`/`Stalled` conditions (Flux).
pub(crate) fn ready_health(obj: &DynamicObject) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        return Health::new(HealthStatus::Progressing, "Terminating");
    }
    if is_suspended(obj) {
        return Health::phrase(HealthStatus::Unknown, msg!("state-suspended"));
    }
    let conditions = conditions(obj);
    let find = |t: &str| conditions.iter().find(|c| c.type_ == t);
    let text = |c: &ConditionInfo| {
        c.message
            .clone()
            .or_else(|| c.reason.clone())
            .unwrap_or_else(|| c.type_.clone())
    };
    if let Some(c) = find("Stalled").filter(|c| c.status == "True") {
        return Health::new(HealthStatus::Failed, text(c));
    }
    let observed = obj
        .data
        .pointer("/status/observedGeneration")
        .and_then(Value::as_i64);
    let stale = matches!((obj.metadata.generation, observed), (Some(g), Some(o)) if o < g);
    let reconciling = find("Reconciling").is_some_and(|c| c.status == "True");
    match find("Ready") {
        Some(c) if c.status == "True" && !stale => Health::new(HealthStatus::Healthy, text(c)),
        Some(c) if c.status == "False" && !reconciling => {
            Health::new(HealthStatus::Degraded, text(c))
        }
        Some(c) => Health::new(HealthStatus::Progressing, text(c)),
        None if reconciling || stale => Health::new(HealthStatus::Progressing, "Reconciling"),
        None => Health::unknown(),
    }
}

/// `group/version`, or `version` for the core group.
pub(crate) fn api_version(group: &str, version: &str) -> String {
    if group.is_empty() {
        version.to_owned()
    } else {
        format!("{group}/{version}")
    }
}

pub(crate) fn reference(
    api_version: impl Into<String>,
    kind: impl Into<String>,
    name: impl Into<String>,
    namespace: Option<String>,
) -> Relation {
    Relation {
        kind: RelationKind::References,
        target: ObjectRef {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            namespace,
        },
    }
}

pub(crate) fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

/// Unique value for request annotations (RFC 3339 with nanoseconds, like the Flux CLI).
pub(crate) fn request_stamp() -> String {
    Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::obj;

    fn flux(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "kustomize.toolkit.fluxcd.io/v1", "kind": "Kustomization",
            "metadata": {"name": "k", "namespace": "flux-system", "generation": 2},
            "spec": {}, "status": status
        }))
    }

    #[test]
    fn ready_condition_maps_to_health() {
        let ready = flux(json!({"observedGeneration": 2, "conditions": [
            {"type": "Ready", "status": "True", "reason": "ReconciliationSucceeded",
             "message": "Applied revision: main@sha1:abc"}
        ]}));
        assert_eq!(
            ready_health(&ready),
            Health::new(HealthStatus::Healthy, "Applied revision: main@sha1:abc")
        );

        let failed = flux(json!({"observedGeneration": 2, "conditions": [
            {"type": "Ready", "status": "False", "reason": "BuildFailed"}
        ]}));
        assert_eq!(
            ready_health(&failed),
            Health::new(HealthStatus::Degraded, "BuildFailed")
        );

        let stalled = flux(json!({"conditions": [
            {"type": "Stalled", "status": "True", "message": "invalid path"},
            {"type": "Ready", "status": "False"}
        ]}));
        assert_eq!(ready_health(&stalled).status, HealthStatus::Failed);

        let progressing = flux(json!({"conditions": [
            {"type": "Ready", "status": "Unknown", "message": "Reconciliation in progress"}
        ]}));
        assert_eq!(ready_health(&progressing).status, HealthStatus::Progressing);

        // A generation the controller has not observed yet is still progressing.
        let stale = flux(json!({"observedGeneration": 1, "conditions": [
            {"type": "Ready", "status": "True"}
        ]}));
        assert_eq!(ready_health(&stale).status, HealthStatus::Progressing);

        assert_eq!(ready_health(&flux(json!({}))), Health::unknown());
    }

    /// `(type, status, negative, warning, is_good)` of each condition.
    fn tones(o: &DynamicObject) -> Vec<(String, String, bool, bool, Option<bool>)> {
        conditions(o)
            .into_iter()
            .map(|c| {
                let good = c.is_good();
                (c.type_, c.status, c.negative, c.warning, good)
            })
            .collect()
    }

    #[test]
    fn argo_conditions_are_problems() {
        let types = [
            "ComparisonError",
            "SyncError",
            "InvalidSpecError",
            "UnknownError",
            "DeletionError",
            "SharedResourceWarning",
            "OrphanedResourceWarning",
            "ExcludedResourceWarning",
            "RepeatedResourceWarning",
        ];
        let conditions: Vec<Value> = types
            .iter()
            .map(|t| json!({"type": t, "message": "boom"}))
            .collect();
        let app = obj(json!({
            "apiVersion": "argoproj.io/v1alpha1", "kind": "Application",
            "metadata": {"name": "a", "namespace": "argocd"},
            "status": {"conditions": conditions}
        }));
        let expected: Vec<_> = types
            .iter()
            .map(|t| {
                let error = t.ends_with("Error");
                (t.to_string(), "True".to_owned(), error, !error, Some(false))
            })
            .collect();
        assert_eq!(tones(&app), expected);
    }

    #[test]
    fn appset_error_occurred_is_negative() {
        let appset = obj(json!({
            "apiVersion": "argoproj.io/v1alpha1", "kind": "ApplicationSet",
            "metadata": {"name": "s", "namespace": "argocd"},
            "status": {"conditions": [
                {"type": "ErrorOccurred", "status": "True"},
                {"type": "ParametersGenerated", "status": "True"},
                {"type": "ResourcesUpToDate", "status": "False"}
            ]}
        }));
        assert_eq!(
            tones(&appset),
            [
                (
                    "ErrorOccurred".into(),
                    "True".into(),
                    true,
                    false,
                    Some(false)
                ),
                (
                    "ParametersGenerated".into(),
                    "True".into(),
                    false,
                    false,
                    Some(true)
                ),
                (
                    "ResourcesUpToDate".into(),
                    "False".into(),
                    false,
                    false,
                    Some(false)
                ),
            ]
        );
        // `Application` of another group is not Argo CD: default polarity.
        let other = obj(json!({
            "apiVersion": "app.k8s.io/v1beta1", "kind": "Application",
            "metadata": {"name": "a"},
            "status": {"conditions": [{"type": "Ready", "status": "True"}]}
        }));
        assert_eq!(
            tones(&other),
            [("Ready".into(), "True".into(), false, false, Some(true))]
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(api_version("", "v1"), "v1");
        assert_eq!(api_version("apps", "v1"), "apps/v1");
        assert_eq!(
            annotation_patch("a", "b"),
            json!({"metadata": {"annotations": {"a": "b"}}})
        );
        let stamp = request_stamp();
        assert!(stamp.parse::<Timestamp>().is_ok(), "{stamp}");
    }

    #[test]
    fn registered_for_crd_kinds() {
        let r = PluginRegistry::with_builtins();
        assert_eq!(
            r.plugin_for("argoproj.io", "Application").name(),
            "argocd-application"
        );
        assert_eq!(
            r.plugin_for("argoproj.io", "ApplicationSet").name(),
            "argocd-applicationset"
        );
        assert_eq!(
            r.plugin_for("kustomize.toolkit.fluxcd.io", "Kustomization")
                .name(),
            "flux-kustomization"
        );
        assert_eq!(
            r.plugin_for("helm.toolkit.fluxcd.io", "HelmRelease").name(),
            "flux-helmrelease"
        );
        for kind in ["GitRepository", "OCIRepository", "HelmRepository"] {
            assert_eq!(
                r.plugin_for("source.toolkit.fluxcd.io", kind).name(),
                "flux-source"
            );
        }
    }
}
