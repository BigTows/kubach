//! Specialized plugins for built-in Kubernetes kinds.
//!
//! Each plugin decodes the [`DynamicObject`] into its `k8s-openapi` type and
//! falls back to [`GenericPlugin`] output when decoding fails.

mod config;
pub mod gitops;
mod jobs;
mod namespace;
mod network;
mod node;
mod pod;
mod storage;
mod workloads;
// Coverage of more kinds (#279).
mod autoscaling;
mod classes;
pub mod gateway;
mod netpol;
mod policy;
// Kinds that were generic until #294.
mod admission;
mod crd;
mod rbac;
mod refgrant;
pub mod vpa;

#[cfg(test)]
mod action_tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, Time};
use k8s_openapi::jiff::Timestamp;
use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, PatchParams, TypeMeta,
};
use kube::discovery::Scope;
use kube::{Client, Resource};
use serde::de::DeserializeOwned;

pub use admission::WebhookConfigurationPlugin;
pub use autoscaling::HorizontalPodAutoscalerPlugin;
pub use classes::{IngressClassPlugin, PriorityClassPlugin, StorageClassPlugin};
pub use config::{ConfigMapPlugin, SecretPlugin};
pub use crd::CustomResourceDefinitionPlugin;
pub use gateway::{GatewayClassPlugin, GatewayPlugin, RoutePlugin};
pub use jobs::{CronJobPlugin, JobPlugin};
pub use namespace::{BLOCKING_DELETION, NamespacePlugin};
pub use netpol::NetworkPolicyPlugin;
pub use network::{IngressPlugin, ServicePlugin};
pub use node::NodePlugin;
pub(crate) use node::cordon_patch;
pub use pod::{PodPlugin, pod_health_with_events};
pub use pod::{PodSummary, summarize as summarize_pod};
pub use policy::{LimitRangePlugin, PodDisruptionBudgetPlugin, ResourceQuotaPlugin};
pub use rbac::{RoleBindingPlugin, RolePlugin};
pub use refgrant::ReferenceGrantPlugin;
pub use storage::{
    CLAIM_USERS, PVC_PROTECTION, PersistentVolumeClaimPlugin, PersistentVolumePlugin,
    delete_used_by_note, pod_uses_claim, terminating_used_by,
};
pub use vpa::VerticalPodAutoscalerPlugin;
pub use workloads::{DaemonSetPlugin, DeploymentPlugin, ReplicaSetPlugin, StatefulSetPlugin};

use super::ObjectRef;
use super::{
    Action, ActionId, ActionParams, Cell, Column, ColumnKind, ConditionInfo, DetailSection,
    GenericPlugin, Placement, PluginError, PluginRegistry, Relation, RelationKind, ResourcePlugin,
    SectionBody, SelectorRelation, StreamKind,
};

/// Registers every built-in plugin.
pub fn register_all(registry: &mut PluginRegistry) {
    let plugins: [Arc<dyn ResourcePlugin>; 15] = [
        Arc::new(PodPlugin),
        Arc::new(DeploymentPlugin),
        Arc::new(StatefulSetPlugin),
        Arc::new(DaemonSetPlugin),
        Arc::new(ReplicaSetPlugin),
        Arc::new(JobPlugin),
        Arc::new(CronJobPlugin),
        Arc::new(ServicePlugin),
        Arc::new(IngressPlugin),
        Arc::new(ConfigMapPlugin),
        Arc::new(SecretPlugin),
        Arc::new(PersistentVolumeClaimPlugin),
        Arc::new(PersistentVolumePlugin),
        Arc::new(NodePlugin),
        Arc::new(NamespacePlugin),
    ];
    for plugin in plugins {
        registry.register(plugin);
    }
    // Kinds covered by #279.
    let more: [Arc<dyn ResourcePlugin>; 11] = [
        Arc::new(HorizontalPodAutoscalerPlugin),
        Arc::new(ResourceQuotaPlugin),
        Arc::new(LimitRangePlugin),
        Arc::new(PodDisruptionBudgetPlugin),
        Arc::new(NetworkPolicyPlugin),
        Arc::new(IngressClassPlugin),
        Arc::new(StorageClassPlugin),
        Arc::new(PriorityClassPlugin),
        Arc::new(GatewayClassPlugin),
        Arc::new(GatewayPlugin),
        Arc::new(RoutePlugin),
    ];
    for plugin in more {
        registry.register(plugin);
    }
    // Kinds covered by #294.
    let rest: [Arc<dyn ResourcePlugin>; 9] = [
        Arc::new(CustomResourceDefinitionPlugin),
        Arc::new(WebhookConfigurationPlugin::MUTATING),
        Arc::new(WebhookConfigurationPlugin::VALIDATING),
        Arc::new(RolePlugin::ROLE),
        Arc::new(RolePlugin::CLUSTER_ROLE),
        Arc::new(RoleBindingPlugin::ROLE_BINDING),
        Arc::new(RoleBindingPlugin::CLUSTER_ROLE_BINDING),
        Arc::new(VerticalPodAutoscalerPlugin),
        Arc::new(ReferenceGrantPlugin),
    ];
    for plugin in rest {
        registry.register(plugin);
    }
    gitops::register(registry);
}

/// Which status of a condition type reports a problem, and how bad it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Polarity {
    /// `True` is the good state (`Ready`, `Available`).
    Positive,
    /// `True` is a failure (`MemoryPressure`, `ReplicaFailure`, `SyncError`).
    Negative,
    /// `True` is something to look at, not a failure
    /// (`OrphanedResourceWarning`, Pod `DisruptionTarget`, Flux `Reconciling`).
    Warning,
}

impl Polarity {
    /// [`ConditionInfo`] with this polarity: `negative` and `warning` come
    /// from it.
    pub(crate) fn condition(
        self,
        type_: String,
        status: String,
        reason: Option<String>,
        message: Option<String>,
        last_transition: Option<Timestamp>,
    ) -> ConditionInfo {
        ConditionInfo {
            type_,
            status,
            reason,
            message,
            last_transition,
            negative: self == Polarity::Negative,
            warning: self == Polarity::Warning,
        }
    }
}

/// Polarity of condition `type_` of an object of `types`: the types the
/// kind's plugin lists, plus [`ConditionInfo::negative_by_default`] for
/// everything else (CRDs, and types the plugin does not list).
pub(crate) fn condition_polarity(types: Option<&TypeMeta>, type_: &str) -> Polarity {
    let (group, kind) = match types {
        Some(t) => (
            t.api_version.split_once('/').map_or("", |(group, _)| group),
            Some(t.kind.as_str()),
        ),
        None => ("", None),
    };
    let (negative, warning): (&[&str], &[&str]) = match (group, kind) {
        ("argoproj.io", Some("Application")) => {
            return gitops::argo::application_condition_polarity(type_);
        }
        ("argoproj.io", Some("ApplicationSet")) => (&gitops::argo::APPSET_NEGATIVE_CONDITIONS, &[]),
        (group, _) if group.ends_with(".toolkit.fluxcd.io") => {
            (&[], &gitops::flux::WARNING_CONDITIONS)
        }
        (_, Some("Node")) => (&node::NEGATIVE_CONDITIONS, &[]),
        (_, Some("Pod")) => (&[], &pod::WARNING_CONDITIONS),
        (_, Some("Deployment" | "ReplicaSet" | "ReplicationController")) => {
            (&workloads::NEGATIVE_CONDITIONS, &[])
        }
        (_, Some("Job")) => (&jobs::NEGATIVE_CONDITIONS, &[]),
        (_, Some("Namespace")) => (&[], &namespace::WARNING_CONDITIONS),
        (_, Some("PersistentVolumeClaim")) => {
            (&storage::NEGATIVE_CONDITIONS, &storage::WARNING_CONDITIONS)
        }
        ("autoscaling", Some("HorizontalPodAutoscaler")) => (&[], &autoscaling::WARNING_CONDITIONS),
        (gateway::GROUP, _) => (&gateway::NEGATIVE_CONDITIONS, &[]),
        ("apiextensions.k8s.io", Some("CustomResourceDefinition")) => {
            (&crd::NEGATIVE_CONDITIONS, &crd::WARNING_CONDITIONS)
        }
        (vpa::GROUP, Some("VerticalPodAutoscaler")) => {
            (&vpa::NEGATIVE_CONDITIONS, &vpa::WARNING_CONDITIONS)
        }
        _ => (&[], &[]),
    };
    if negative.contains(&type_) {
        Polarity::Negative
    } else if warning.contains(&type_) {
        Polarity::Warning
    } else if ConditionInfo::negative_by_default(type_) {
        Polarity::Negative
    } else {
        Polarity::Positive
    }
}

/// Action ids shared by several plugins.
pub mod action {
    use crate::plugin::ActionId;

    pub const DELETE: ActionId = ActionId::new("delete");
    pub const SCALE: ActionId = ActionId::new("scale");
    pub const RESTART: ActionId = ActionId::new("restart");
    pub const TRIGGER: ActionId = ActionId::new("trigger");
    pub const SUSPEND: ActionId = ActionId::new("suspend");
    pub const RESUME: ActionId = ActionId::new("resume");
    pub const CORDON: ActionId = ActionId::new("cordon");
    pub const UNCORDON: ActionId = ActionId::new("uncordon");
    /// Pods only: the Eviction API, which honours PodDisruptionBudgets.
    pub const EVICT: ActionId = ActionId::new("evict");
    /// Streams ([`crate::plugin::ActionKind::Stream`]): declared by plugins,
    /// opened by the UI or the MCP agent.
    pub const LOGS: ActionId = ActionId::new("logs");
    pub const EXEC: ActionId = ActionId::new("exec");
    pub const PORT_FORWARD: ActionId = ActionId::new("port-forward");
    pub const DEBUG: ActionId = ActionId::new("debug");
    pub(crate) const STREAMS: [ActionId; 4] = [LOGS, EXEC, PORT_FORWARD, DEBUG];
    /// Parameter id of [`SCALE`].
    pub const REPLICAS: &str = "replicas";
    /// Alternative parameter of [`SCALE`]: a step from the replicas the
    /// cluster has now, read from the `/scale` subresource and written back
    /// with its resourceVersion ([`crate::scale::scale_by`]).
    pub const DELTA: &str = "delta";
    /// With [`DELTA`]: the user confirmed a step down to zero.
    pub const ALLOW_ZERO: &str = "allow_zero";
}

/// Parameters of actions that take none.
pub(crate) static NO_PARAMS: ActionParams = BTreeMap::new();

/// Forward a local port to a ready pod of the object (opened by the UI).
pub(crate) const PORT_FORWARD_ACTION: Action = Action::stream(
    action::PORT_FORWARD,
    "Port forward",
    StreamKind::PortForward,
);

pub(crate) const DELETE_ACTION: Action = Action::api(action::DELETE, "Delete", true);

/// Decodes a dynamic object into a typed one.
pub(crate) fn decode<K: DeserializeOwned>(obj: &DynamicObject) -> Option<K> {
    serde_json::to_value(obj)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

pub(crate) fn decode_or_err<K: DeserializeOwned>(obj: &DynamicObject) -> Result<K, PluginError> {
    let value = serde_json::to_value(obj).map_err(|e| PluginError::InvalidObject(e.to_string()))?;
    serde_json::from_value(value).map_err(|e| PluginError::InvalidObject(e.to_string()))
}

/// Cells for an object that could not be decoded: name, namespace and age
/// where the column ids match, empty text elsewhere.
pub(crate) fn fallback_cells(obj: &DynamicObject, columns: &[Column]) -> Vec<Cell> {
    columns
        .iter()
        .map(|c| match c.id {
            "name" => Cell::Text(obj.metadata.name.clone().unwrap_or_default()),
            "namespace" => Cell::Text(obj.metadata.namespace.clone().unwrap_or_default()),
            "age" => Cell::Age(creation(obj)),
            _ => Cell::Text(String::new()),
        })
        .collect()
}

pub(crate) fn creation(obj: &DynamicObject) -> Option<Timestamp> {
    obj.metadata.creation_timestamp.as_ref().map(|t| t.0)
}

pub(crate) fn col(id: &'static str, title: &'static str, kind: ColumnKind) -> Column {
    Column { id, title, kind }
}

pub(crate) fn name_ns_columns() -> Vec<Column> {
    vec![
        col("name", "Name", ColumnKind::Text),
        col("namespace", "Namespace", ColumnKind::Text),
    ]
}

pub(crate) fn name_ns_cells(obj: &DynamicObject) -> Vec<Cell> {
    vec![
        Cell::Text(obj.metadata.name.clone().unwrap_or_default()),
        Cell::Text(obj.metadata.namespace.clone().unwrap_or_default()),
    ]
}

pub(crate) fn text(s: impl Into<String>) -> Cell {
    Cell::Text(s.into())
}

/// `"<none>"` for empty strings, like kubectl.
pub(crate) fn or_none(s: impl Into<String>) -> String {
    let s = s.into();
    if s.is_empty() { "<none>".into() } else { s }
}

/// Title of the first section of every built-in plugin: the object's own
/// fields, which join the main block of the details.
pub(crate) const OVERVIEW: &str = "Overview";

/// A key/value section; the [`OVERVIEW`] one joins the main block.
pub(crate) fn kv(title: impl Into<String>, pairs: Vec<(String, String)>) -> DetailSection {
    section(title, SectionBody::KeyValues(pairs))
}

/// A section; the [`OVERVIEW`] one joins the main block.
pub(crate) fn section(title: impl Into<String>, body: SectionBody) -> DetailSection {
    let title = title.into();
    let placement = if title == OVERVIEW {
        Placement::Overview
    } else {
        Placement::Section
    };
    DetailSection::new(title, body).placement(placement)
}

pub(crate) fn pair(k: impl Into<String>, v: impl Into<String>) -> (String, String) {
    (k.into(), v.into())
}

/// Pushes a key/value section unless it is empty.
pub(crate) fn push_kv(
    sections: &mut Vec<DetailSection>,
    title: impl Into<String>,
    pairs: Vec<(String, String)>,
) {
    if !pairs.is_empty() {
        sections.push(kv(title, pairs));
    }
}

/// Parts of a phrase joined with ` · ` (`HTTPS :443 · TLS Terminate`).
pub(crate) fn joined(parts: Vec<kubach_i18n::Message>) -> kubach_i18n::Message {
    parts
        .into_iter()
        .reduce(|first, second| kubach_i18n::msg!("details-joined", first = first, second = second))
        .unwrap_or_else(|| kubach_i18n::Message::new(""))
}

/// Pushes a section of notes (diagnostics, security) unless it is empty.
pub(crate) fn push_notes(
    sections: &mut Vec<DetailSection>,
    title: impl Into<String>,
    notes: Vec<crate::plugin::Note>,
) {
    if !notes.is_empty() {
        sections.push(section(title, SectionBody::Notes(notes)));
    }
}

pub(crate) fn map_pairs(map: Option<&BTreeMap<String, String>>) -> Vec<(String, String)> {
    map.into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `k=v,k2=v2`.
pub(crate) fn join_map(map: Option<&BTreeMap<String, String>>) -> String {
    map.into_iter()
        .flatten()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Renders a label selector in kubectl syntax. `None` selects nothing.
pub(crate) fn selector_string(selector: &LabelSelector) -> Option<String> {
    let mut parts: Vec<String> = selector
        .match_labels
        .iter()
        .flatten()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    for e in selector.match_expressions.iter().flatten() {
        let values = e.values.clone().unwrap_or_default().join(",");
        parts.push(match e.operator.as_str() {
            "In" => format!("{} in ({values})", e.key),
            "NotIn" => format!("{} notin ({values})", e.key),
            "Exists" => e.key.clone(),
            "DoesNotExist" => format!("!{}", e.key),
            other => format!("{} {other} ({values})", e.key),
        });
    }
    // An empty selector matches everything in workloads, which is never intended.
    (!parts.is_empty()).then(|| parts.join(","))
}

pub(crate) fn selects(
    obj: &DynamicObject,
    api_version: &str,
    target_kind: &str,
    label_selector: String,
) -> SelectorRelation {
    SelectorRelation {
        kind: RelationKind::Selects,
        api_version: api_version.into(),
        target_kind: target_kind.into(),
        namespace: obj.metadata.namespace.clone(),
        label_selector,
    }
}

/// Owner references plus `extra` relations, without duplicates.
pub(crate) fn relations_with(obj: &DynamicObject, extra: Vec<Relation>) -> Vec<Relation> {
    let mut out = GenericPlugin.relations(obj);
    for r in extra {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

pub(crate) fn time_str(t: Option<&Time>) -> String {
    t.map(|t| t.0.to_string()).unwrap_or_default()
}

/// Current time with second precision in RFC 3339, as kubectl writes it.
pub(crate) fn now_rfc3339() -> String {
    let now = Timestamp::now();
    Timestamp::from_second(now.as_second())
        .unwrap_or(now)
        .to_string()
}

pub(crate) fn name_of(obj: &DynamicObject) -> Result<&str, PluginError> {
    obj.metadata
        .name
        .as_deref()
        .ok_or_else(|| PluginError::InvalidObject("object has no name".into()))
}

/// API for the object's namespace (or cluster-wide), typed by `K`'s resource.
pub(crate) fn api_for<K: Resource<DynamicType = ()>>(
    client: &Client,
    obj: &DynamicObject,
) -> Api<DynamicObject> {
    let resource = ApiResource::erase::<K>(&());
    match obj.metadata.namespace.as_deref() {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
        None => Api::all_with(client.clone(), &resource),
    }
}

/// Delete options of every built-in delete action on `obj`: background
/// propagation, like `kubectl delete` (dependents are removed by the
/// garbage collector), and the precondition that the object still has the
/// UID it was loaded with, so one recreated under the same name meanwhile
/// is not deleted (#345).
pub(crate) fn delete_params(obj: &DynamicObject) -> DeleteParams {
    crate::deletion::with_uid(DeleteParams::background(), obj.metadata.uid.as_deref())
}

/// Patch options of every built-in action: no field manager, no dry run
/// (the server derives the manager from the user agent).
pub(crate) fn patch_params() -> PatchParams {
    PatchParams::default()
}

/// Deletes the object with background propagation, like `kubectl delete`.
/// Already gone counts as deleted; a recreated object is left alone
/// ([`crate::deletion::finish`]).
pub(crate) async fn delete<K: Resource<DynamicType = ()>>(
    client: &Client,
    obj: &DynamicObject,
) -> Result<(), PluginError> {
    let result = api_for::<K>(client, obj)
        .delete(name_of(obj)?, &delete_params(obj))
        .await;
    Ok(crate::deletion::finish(result)?)
}

/// Applies a patch to the object.
pub(crate) async fn patch<K: Resource<DynamicType = ()>>(
    client: &Client,
    obj: &DynamicObject,
    patch: &kube::api::Patch<serde_json::Value>,
) -> Result<(), PluginError> {
    api_for::<K>(client, obj)
        .patch(name_of(obj)?, &patch_params(), patch)
        .await?;
    Ok(())
}

/// Deletes an object of any kind (CRDs included) with background
/// propagation, like `kubectl delete`. The resource (plural, scope) comes
/// from discovery of the object's group version: guessing the plural from
/// the kind is wrong for some CRDs.
pub(crate) async fn delete_discovered(
    client: &Client,
    obj: &DynamicObject,
) -> Result<(), PluginError> {
    let types = obj
        .types
        .as_ref()
        .ok_or_else(|| PluginError::InvalidObject("object has no apiVersion/kind".into()))?;
    let (group, version) = types
        .api_version
        .split_once('/')
        .unwrap_or(("", &types.api_version));
    let gvk = GroupVersionKind::gvk(group, version, &types.kind);
    let (resource, caps) = kube::discovery::pinned_kind(client, &gvk).await?;
    let api: Api<DynamicObject> = match (&caps.scope, obj.metadata.namespace.as_deref()) {
        (Scope::Cluster, _) => Api::all_with(client.clone(), &resource),
        (Scope::Namespaced, Some(ns)) => Api::namespaced_with(client.clone(), ns, &resource),
        (Scope::Namespaced, None) => {
            return Err(PluginError::InvalidObject(format!(
                "{} is namespaced, but the object has no namespace",
                types.kind
            )));
        }
    };
    let result = api.delete(name_of(obj)?, &delete_params(obj)).await;
    Ok(crate::deletion::finish(result)?)
}

/// A reference to any object.
pub(crate) fn object_ref(
    api_version: &str,
    kind: &str,
    name: &str,
    namespace: Option<&str>,
) -> ObjectRef {
    ObjectRef {
        api_version: api_version.into(),
        kind: kind.into(),
        name: name.into(),
        namespace: namespace.map(str::to_owned),
    }
}

/// The object references `target` (by name, not by selector or owner).
pub(crate) fn references(target: ObjectRef) -> Relation {
    Relation {
        kind: RelationKind::References,
        target,
    }
}

/// `80`, `25%`.
pub(crate) fn int_or_string(
    v: &k8s_openapi::apimachinery::pkg::util::intstr::IntOrString,
) -> String {
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
    match v {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s.clone(),
    }
}

/// `run_action` of a plugin whose only action is Delete of a typed kind.
pub(crate) fn run_delete<'a, K>(
    client: &'a Client,
    obj: &'a DynamicObject,
    action_id: &'a ActionId,
) -> super::BoxFuture<'a, Result<(), PluginError>>
where
    K: Resource<DynamicType = ()> + 'a,
{
    Box::pin(async move {
        match *action_id {
            action::DELETE => delete::<K>(client, obj).await,
            _ => Err(unknown(action_id)),
        }
    })
}

/// Error for an action the plugin does not run: [`PluginError::Stream`] for
/// a shared stream id (logs, exec, …), [`PluginError::UnknownAction`]
/// otherwise.
pub(crate) fn unknown(id: &ActionId) -> PluginError {
    if action::STREAMS.contains(id) {
        PluginError::Stream(id.to_string())
    } else {
        PluginError::UnknownAction(id.to_string())
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use kube::api::DynamicObject;
    use serde_json::Value;

    pub fn obj(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }

    pub fn ts(s: &str) -> k8s_openapi::jiff::Timestamp {
        s.parse().unwrap()
    }

    pub fn section<'a>(
        sections: &'a [crate::plugin::DetailSection],
        title: &str,
    ) -> &'a crate::plugin::SectionBody {
        &sections
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("section {title} missing"))
            .body
    }

    /// Key/value pairs of a section; panics on other bodies.
    pub fn pairs<'a>(
        sections: &'a [crate::plugin::DetailSection],
        title: &str,
    ) -> &'a [(String, String)] {
        match section(sections, title) {
            crate::plugin::SectionBody::KeyValues(p) => p,
            other => panic!("{title} is {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_with_builtins() {
        let r = PluginRegistry::with_builtins();
        assert_eq!(r.plugin_for("", "Pod").name(), "pod");
        assert_eq!(r.plugin_for("apps", "Deployment").name(), "deployment");
        assert_eq!(r.plugin_for("batch", "CronJob").name(), "cronjob");
        assert_eq!(
            r.plugin_for("networking.k8s.io", "Ingress").name(),
            "ingress"
        );
        assert_eq!(r.plugin_for("", "Secret").name(), "secret");
        assert_eq!(r.plugin_for("", "Node").name(), "node");
        assert_eq!(r.plugin_for("", "Namespace").name(), "namespace");
        assert_eq!(
            r.plugin_for("", "PersistentVolumeClaim").name(),
            "persistentvolumeclaim"
        );
        assert_eq!(
            r.plugin_for("", "PersistentVolume").name(),
            "persistentvolume"
        );
        assert_eq!(r.plugin_for("example.com", "Widget").name(), "generic");
    }

    #[test]
    fn selectors() {
        let s: LabelSelector = serde_json::from_value(serde_json::json!({
            "matchLabels": {"app": "web"},
            "matchExpressions": [
                {"key": "tier", "operator": "In", "values": ["a", "b"]},
                {"key": "canary", "operator": "DoesNotExist"}
            ]
        }))
        .unwrap();
        assert_eq!(
            selector_string(&s).as_deref(),
            Some("app=web,tier in (a,b),!canary")
        );
        assert_eq!(selector_string(&LabelSelector::default()), None);
    }
}
