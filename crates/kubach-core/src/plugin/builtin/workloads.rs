//! Deployment, StatefulSet, DaemonSet and ReplicaSet plugins.

use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::core::v1::{PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kubach_i18n::msg;
use kube::api::{DynamicObject, GroupVersionKind, Patch};
use kube::{Client, Resource};
use serde_json::{Value, json};

use super::pod::{probes, scheduling, security_notes};
use super::{
    DELETE_ACTION, OVERVIEW, PORT_FORWARD_ACTION, action, api_for, col, creation, decode, delete,
    fallback_cells, kv, map_pairs, name_ns_cells, name_ns_columns, name_of, now_rfc3339, or_none,
    pair, patch, patch_params, push_kv, push_notes, section, selector_string, selects, text,
    unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParam, ActionParamKind, ActionParams, BoxFuture, Brief, Cell, Column,
    ColumnKind, DetailSection, Field, FieldValue, GenericPlugin, Health, HealthStatus, PluginError,
    RelatedFilter, RelatedObjects, RelatedOrder, Relation, ReplicaCounts, ResourcePlugin,
    SectionBody, SelectorRelation, revision,
};
use crate::security;

pub struct DeploymentPlugin;
pub struct StatefulSetPlugin;
pub struct DaemonSetPlugin;
pub struct ReplicaSetPlugin;

/// Deployment, ReplicaSet and ReplicationController conditions that report
/// a problem when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 1] = ["ReplicaFailure"];

const SCALE_ACTION: Action = Action::api(action::SCALE, "Scale", false);

const RESTART_ACTION: Action = Action::api(action::RESTART, "Rollout restart", false);

/// Strategic merge patch used by `kubectl rollout restart`.
pub(crate) fn restart_patch(at: &str) -> Value {
    json!({"spec": {"template": {"metadata": {"annotations": {
        "kubectl.kubernetes.io/restartedAt": at
    }}}}})
}

/// Merge patch for the scale subresource, as `kubectl scale` sends.
pub(crate) fn scale_patch(replicas: i32) -> Value {
    json!({"spec": {"replicas": replicas}})
}

/// Reads the `replicas` parameter.
pub(crate) fn replicas_param(params: &ActionParams) -> Result<i32, PluginError> {
    let invalid = |reason: &str| PluginError::InvalidParam {
        name: action::REPLICAS.into(),
        reason: reason.into(),
    };
    let value = params
        .get(action::REPLICAS)
        .ok_or_else(|| invalid("required"))?;
    let n = value
        .as_i64()
        .ok_or_else(|| invalid("must be an integer"))?;
    i32::try_from(n)
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| invalid("must be between 0 and 2147483647"))
}

/// Reads the `delta` and `allow_zero` parameters of a relative scale.
pub(crate) fn delta_params(params: &ActionParams) -> Result<(i32, bool), PluginError> {
    let delta = params
        .get(action::DELTA)
        .and_then(Value::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .filter(|n| *n != 0)
        .ok_or_else(|| PluginError::InvalidParam {
            name: action::DELTA.into(),
            reason: "must be a non-zero integer".into(),
        })?;
    let allow_zero = params
        .get(action::ALLOW_ZERO)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok((delta, allow_zero))
}

fn scale_params(current: i32) -> Vec<ActionParam> {
    vec![ActionParam {
        id: action::REPLICAS,
        title: "Replicas",
        kind: ActionParamKind::Integer { min: 0, max: None },
        default: Some(json!(current)),
    }]
}

fn spec_replicas(obj: &DynamicObject) -> i32 {
    obj.data
        .pointer("/spec/replicas")
        .and_then(Value::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(1)
}

/// Runs scale/restart/delete for a workload type.
async fn run_workload<K>(
    client: &Client,
    obj: &DynamicObject,
    action_id: &ActionId,
    params: &ActionParams,
    allowed: &[ActionId],
) -> Result<(), PluginError>
where
    K: Resource<DynamicType = ()>,
{
    if !allowed.contains(action_id) {
        return Err(unknown(action_id));
    }
    match *action_id {
        action::SCALE if params.contains_key(action::DELTA) => {
            let (delta, allow_zero) = delta_params(params)?;
            crate::scale::scale_by(&api_for::<K>(client, obj), name_of(obj)?, delta, allow_zero)
                .await?;
            Ok(())
        }
        action::SCALE => {
            let replicas = replicas_param(params)?;
            api_for::<K>(client, obj)
                .patch_scale(
                    name_of(obj)?,
                    &patch_params(),
                    &Patch::Merge(scale_patch(replicas)),
                )
                .await?;
            Ok(())
        }
        action::RESTART => {
            patch::<K>(
                client,
                obj,
                &Patch::Strategic(restart_patch(&now_rfc3339())),
            )
            .await
        }
        action::DELETE => delete::<K>(client, obj).await,
        _ => Err(unknown(action_id)),
    }
}

fn images(template: Option<&PodTemplateSpec>) -> String {
    template
        .and_then(|t| t.spec.as_ref())
        .map(|s| {
            s.containers
                .iter()
                .filter_map(|c| c.image.clone())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// A selector as chips when it only matches labels, else in kubectl syntax.
pub(crate) fn selector_field(selector: Option<&LabelSelector>) -> Field {
    let only_labels = selector.is_some_and(|s| {
        s.match_expressions.as_ref().is_none_or(Vec::is_empty)
            && s.match_labels.as_ref().is_some_and(|l| !l.is_empty())
    });
    match selector {
        Some(s) if only_labels => Field::new(
            "Selector",
            FieldValue::Labels(map_pairs(s.match_labels.as_ref())),
        ),
        _ => Field::text(
            "Selector",
            or_none(selector.and_then(selector_string).unwrap_or_default()),
        ),
    }
}

/// Objects of `kind` matching `selector` (and owned by `obj` when
/// `owned`), as a related list.
pub(crate) fn related(
    obj: &DynamicObject,
    api_version: &str,
    kind: &str,
    selector: Option<&LabelSelector>,
    owned: bool,
    order: RelatedOrder,
) -> Option<DetailSection> {
    let labels = selector.and_then(selector_string)?;
    let title = match kind {
        "Pod" => "Pods".to_owned(),
        other => format!("{other}s"),
    };
    Some(section(
        title,
        SectionBody::Related(RelatedObjects {
            api_version: api_version.into(),
            kind: kind.into(),
            namespace: obj.metadata.namespace.clone(),
            filter: RelatedFilter::Selector {
                labels: Some(labels),
                owner_uid: obj.metadata.uid.clone().filter(|_| owned),
            },
            order,
        }),
    ))
}

/// The main block, containers, security, `related` lists, then labels,
/// owners and conditions.
fn workload_details(
    obj: &DynamicObject,
    overview: Vec<Field>,
    selector: Option<&LabelSelector>,
    template: Option<&PodTemplateSpec>,
    related: Vec<DetailSection>,
) -> Vec<DetailSection> {
    let mut overview = overview;
    overview.push(selector_field(selector));
    let mut sections = vec![section(OVERVIEW, SectionBody::Fields(overview))];
    let spec = template.and_then(|t| t.spec.as_ref());
    push_kv(
        &mut sections,
        "Containers",
        spec.map(|s| {
            s.init_containers
                .iter()
                .flatten()
                .chain(&s.containers)
                .map(|c| pair(c.name.clone(), c.image.clone().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default(),
    );
    push_kv(
        &mut sections,
        "Probes",
        spec.map(template_probes).unwrap_or_default(),
    );
    push_notes(
        &mut sections,
        "Security",
        spec.map(security::check_pod_spec)
            .map(security_notes)
            .unwrap_or_default(),
    );
    if let Some(spec) = spec {
        let scheduling = scheduling(spec);
        if !scheduling.is_empty() {
            sections.push(section("Scheduling", SectionBody::Fields(scheduling)).collapsed());
        }
    }
    sections.extend(related);
    sections.extend(GenericPlugin.details(obj));
    sections
}

/// Probes of every container of a pod template, `(app liveness, http-get
/// :80/ · delay 0s · …)`; containers without probes say `none`, init
/// containers (they cannot have probes) are left out.
fn template_probes(spec: &PodSpec) -> Vec<(String, String)> {
    spec.init_containers
        .iter()
        .flatten()
        .filter(|c| c.restart_policy.as_deref() == Some("Always"))
        .chain(&spec.containers)
        .flat_map(|c| {
            let probes = probes(c);
            if probes.is_empty() {
                return vec![(c.name.clone(), "none".to_owned())];
            }
            probes
                .into_iter()
                .map(|(name, probe)| (format!("{} {}", c.name, name.to_lowercase()), probe))
                .collect()
        })
        .collect()
}

/// `RollingUpdate · max surge 25% · max unavailable 25%`.
fn rolling(
    kind: Option<&str>,
    surge: Option<&IntOrString>,
    unavailable: Option<&IntOrString>,
) -> String {
    let text = |v: &IntOrString| match v {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s.clone(),
    };
    let kind = kind.unwrap_or("RollingUpdate");
    let mut parts = vec![kind.to_owned()];
    if kind == "RollingUpdate" {
        if let Some(surge) = surge {
            parts.push(format!("max surge {}", text(surge)));
        }
        if let Some(unavailable) = unavailable {
            parts.push(format!("max unavailable {}", text(unavailable)));
        }
    }
    parts.join(" · ")
}

fn revision_field(obj: &DynamicObject) -> Option<Field> {
    revision(obj).map(|r| Field::text("Revision", r.to_string()))
}

fn pod_selector_relations(
    obj: &DynamicObject,
    selector: Option<&LabelSelector>,
    with_replicasets: bool,
) -> Vec<SelectorRelation> {
    let Some(sel) = selector.and_then(selector_string) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if with_replicasets {
        out.push(selects(obj, "apps/v1", "ReplicaSet", sel.clone()));
    }
    out.push(selects(obj, "v1", "Pod", sel));
    out
}

fn generation_pending(obj: &DynamicObject, observed: Option<i64>) -> bool {
    matches!((obj.metadata.generation, observed), (Some(g), Some(o)) if o < g)
}

fn n(v: Option<i32>) -> i32 {
    v.unwrap_or(0)
}

// ---------------------------------------------------------------- Deployment

pub(crate) fn deployment_health(obj: &DynamicObject, d: &Deployment) -> Health {
    let spec = d.spec.as_ref();
    let status = d.status.clone().unwrap_or_default();
    let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
    let conditions = status.conditions.clone().unwrap_or_default();
    let cond = |t: &str| conditions.iter().find(|c| c.type_ == t);

    if let Some(c) =
        cond("Progressing").filter(|c| c.reason.as_deref() == Some("ProgressDeadlineExceeded"))
    {
        let msg = c
            .message
            .clone()
            .unwrap_or_else(|| "Rollout exceeded its progress deadline".into());
        return Health::new(HealthStatus::Failed, msg);
    }
    if spec.and_then(|s| s.paused) == Some(true) {
        return Health::phrase(HealthStatus::Unknown, msg!("state-rollout-paused"));
    }
    if generation_pending(obj, status.observed_generation) {
        return Health::phrase(HealthStatus::Progressing, msg!("state-waiting-rollout"));
    }
    let updated = n(status.updated_replicas);
    let replicas = n(status.replicas);
    let available = n(status.available_replicas);
    if updated < desired {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!(
                "state-replicas-updated",
                updated = updated,
                desired = desired
            ),
        );
    }
    if replicas > updated {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!("state-old-replicas", count = replicas - updated),
        );
    }
    if let Some(c) = cond("Available").filter(|c| c.status == "False") {
        let msg = c.message.clone().unwrap_or_else(|| "Unavailable".into());
        return Health::new(HealthStatus::Degraded, msg);
    }
    if available < updated {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!(
                "state-updated-available",
                available = available,
                updated = updated
            ),
        );
    }
    if desired == 0 {
        return Health::phrase(HealthStatus::Healthy, msg!("state-scaled-to-zero"));
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-available"))
}

impl ResourcePlugin for DeploymentPlugin {
    fn name(&self) -> &'static str {
        "deployment"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("apps", "v1", "Deployment")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("ready", "Ready", ColumnKind::Text),
            col("up_to_date", "Up-to-date", ColumnKind::Number),
            col("available", "Available", ColumnKind::Number),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
            col("images", "Images", ColumnKind::Text),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(d) = decode::<Deployment>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = d.spec.as_ref();
        let status = d.status.clone().unwrap_or_default();
        let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(format!("{}/{desired}", n(status.ready_replicas))),
            Cell::Number(n(status.updated_replicas).into()),
            Cell::Number(n(status.available_replicas).into()),
            Cell::Status(deployment_health(obj, &d)),
            Cell::Age(creation(obj)),
            text(images(spec.map(|s| &s.template))),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(d) = decode::<Deployment>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = d.spec.as_ref();
        let status = d.status.clone().unwrap_or_default();
        let strategy = spec.and_then(|s| s.strategy.as_ref());
        let rolling_update = strategy.and_then(|s| s.rolling_update.as_ref());
        let mut overview = vec![
            Field::new(
                "Replicas",
                FieldValue::Replicas(ReplicaCounts {
                    desired: spec.and_then(|s| s.replicas).unwrap_or(1),
                    current: n(status.replicas),
                    ready: n(status.ready_replicas),
                    updated: Some(n(status.updated_replicas)),
                    available: Some(n(status.available_replicas)),
                }),
            ),
            Field::text(
                "Strategy",
                rolling(
                    strategy.and_then(|s| s.type_.as_deref()),
                    rolling_update.and_then(|r| r.max_surge.as_ref()),
                    rolling_update.and_then(|r| r.max_unavailable.as_ref()),
                ),
            ),
        ];
        overview.extend(revision_field(obj));
        if spec.and_then(|s| s.paused) == Some(true) {
            overview.push(Field::text("Paused", "Yes"));
        }
        let selector = spec.map(|s| &s.selector);
        let related_lists = [
            related(
                obj,
                "apps/v1",
                "ReplicaSet",
                selector,
                true,
                RelatedOrder::Revision,
            ),
            related(obj, "v1", "Pod", selector, false, RelatedOrder::Name),
        ];
        workload_details(
            obj,
            overview,
            selector,
            spec.map(|s| &s.template),
            related_lists.into_iter().flatten().collect(),
        )
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            SCALE_ACTION,
            RESTART_ACTION,
            PORT_FORWARD_ACTION,
            DELETE_ACTION,
        ]
    }

    fn action_params(&self, obj: &DynamicObject, action_id: &ActionId) -> Vec<ActionParam> {
        if *action_id == action::SCALE {
            scale_params(spec_replicas(obj))
        } else {
            Vec::new()
        }
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(run_workload::<Deployment>(
            client,
            obj,
            action_id,
            params,
            &[action::SCALE, action::RESTART, action::DELETE],
        ))
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        let d = decode::<Deployment>(obj);
        pod_selector_relations(
            obj,
            d.as_ref()
                .and_then(|d| d.spec.as_ref())
                .map(|s| &s.selector),
            true,
        )
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Deployment>(obj) {
            Some(d) => deployment_health(obj, &d),
            None => GenericPlugin.health(obj),
        }
    }
}

// --------------------------------------------------------------- StatefulSet

pub(crate) fn statefulset_health(obj: &DynamicObject, s: &StatefulSet) -> Health {
    let spec = s.spec.as_ref();
    let status = s.status.clone().unwrap_or_default();
    let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
    if generation_pending(obj, status.observed_generation) {
        return Health::phrase(HealthStatus::Progressing, msg!("state-waiting-rollout"));
    }
    let ready = n(status.ready_replicas);
    if ready < desired {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!("state-replicas-ready", ready = ready, desired = desired),
        );
    }
    let on_delete = spec
        .and_then(|s| s.update_strategy.as_ref())
        .and_then(|u| u.type_.as_deref())
        == Some("OnDelete");
    if !on_delete
        && status.update_revision.is_some()
        && status.current_revision != status.update_revision
    {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!(
                "state-replicas-updated",
                updated = n(status.updated_replicas),
                desired = desired
            ),
        );
    }
    if desired == 0 {
        return Health::phrase(HealthStatus::Healthy, msg!("state-scaled-to-zero"));
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-ready"))
}

impl ResourcePlugin for StatefulSetPlugin {
    fn name(&self) -> &'static str {
        "statefulset"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("apps", "v1", "StatefulSet")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("ready", "Ready", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
            col("images", "Images", ColumnKind::Text),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(s) = decode::<StatefulSet>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = s.spec.as_ref();
        let status = s.status.clone().unwrap_or_default();
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(format!(
                "{}/{}",
                n(status.ready_replicas),
                spec.and_then(|s| s.replicas).unwrap_or(1)
            )),
            Cell::Status(statefulset_health(obj, &s)),
            Cell::Age(creation(obj)),
            text(images(spec.map(|s| &s.template))),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(s) = decode::<StatefulSet>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = s.spec.as_ref();
        let status = s.status.clone().unwrap_or_default();
        let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
        let ns = obj.metadata.namespace.as_deref();
        let name = obj.metadata.name.clone().unwrap_or_default();
        let mut overview = vec![Field::new(
            "Replicas",
            FieldValue::Replicas(ReplicaCounts {
                desired,
                current: status.replicas,
                ready: n(status.ready_replicas),
                updated: Some(n(status.updated_replicas)),
                available: status.available_replicas,
            }),
        )];
        if let Some(service) = spec
            .and_then(|s| s.service_name.as_deref())
            .filter(|s| !s.is_empty())
        {
            overview.push(Field::link("Service", "Service", service, ns));
        }
        let update = spec.and_then(|s| s.update_strategy.as_ref());
        let mut strategy = update
            .and_then(|u| u.type_.clone())
            .unwrap_or_else(|| "RollingUpdate".into());
        if let Some(rolling) = update.and_then(|u| u.rolling_update.as_ref()) {
            if let Some(partition) = rolling.partition.filter(|p| *p > 0) {
                strategy.push_str(&format!(" · partition {partition}"));
            }
            if let Some(max) = &rolling.max_unavailable {
                let max = match max {
                    IntOrString::Int(n) => n.to_string(),
                    IntOrString::String(s) => s.clone(),
                };
                strategy.push_str(&format!(" · max unavailable {max}"));
            }
        }
        overview.push(Field::text("Update strategy", strategy));
        overview.push(Field::text(
            "Pod management",
            spec.and_then(|s| s.pod_management_policy.clone())
                .unwrap_or_else(|| "OrderedReady".into()),
        ));
        match (&status.current_revision, &status.update_revision) {
            (Some(current), Some(update)) if current != update => {
                overview.push(Field::text("Revision", format!("{current} → {update}")));
            }
            (Some(current), _) => overview.push(Field::text("Revision", current.clone())),
            _ => {}
        }

        let templates = spec
            .and_then(|s| s.volume_claim_templates.as_ref())
            .cloned()
            .unwrap_or_default();
        let mut related_lists = Vec::new();
        let claims: Vec<(String, String)> = templates
            .iter()
            .map(|t| {
                let spec = t.spec.as_ref();
                let mut parts = Vec::new();
                if let Some(size) = spec
                    .and_then(|s| s.resources.as_ref())
                    .and_then(|r| r.requests.as_ref())
                    .and_then(|r| r.get("storage"))
                {
                    parts.push(size.0.clone());
                }
                if let Some(modes) = spec.and_then(|s| s.access_modes.as_ref()) {
                    parts.push(modes.join(","));
                }
                if let Some(class) = spec.and_then(|s| s.storage_class_name.as_ref()) {
                    parts.push(class.clone());
                }
                (
                    t.metadata.name.clone().unwrap_or_default(),
                    or_none(parts.join(" · ")),
                )
            })
            .collect();
        if !claims.is_empty() {
            related_lists.push(kv("Volume claim templates", claims));
            // `<template>-<statefulset>-<ordinal>`, for every ordinal.
            let names = templates
                .iter()
                .filter_map(|t| t.metadata.name.as_deref())
                .flat_map(|template| {
                    let name = &name;
                    (0..desired.max(0)).map(move |ix| format!("{template}-{name}-{ix}"))
                })
                .collect();
            related_lists.push(section(
                "Persistent volume claims",
                SectionBody::Related(RelatedObjects {
                    api_version: "v1".into(),
                    kind: "PersistentVolumeClaim".into(),
                    namespace: obj.metadata.namespace.clone(),
                    filter: RelatedFilter::Names(names),
                    order: RelatedOrder::Name,
                }),
            ));
        }
        let selector = spec.map(|s| &s.selector);
        related_lists.extend(related(
            obj,
            "v1",
            "Pod",
            selector,
            true,
            RelatedOrder::Ordinal,
        ));
        workload_details(
            obj,
            overview,
            selector,
            spec.map(|s| &s.template),
            related_lists,
        )
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            SCALE_ACTION,
            RESTART_ACTION,
            PORT_FORWARD_ACTION,
            DELETE_ACTION,
        ]
    }

    fn action_params(&self, obj: &DynamicObject, action_id: &ActionId) -> Vec<ActionParam> {
        if *action_id == action::SCALE {
            scale_params(spec_replicas(obj))
        } else {
            Vec::new()
        }
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(run_workload::<StatefulSet>(
            client,
            obj,
            action_id,
            params,
            &[action::SCALE, action::RESTART, action::DELETE],
        ))
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        let s = decode::<StatefulSet>(obj);
        pod_selector_relations(
            obj,
            s.as_ref()
                .and_then(|s| s.spec.as_ref())
                .map(|s| &s.selector),
            false,
        )
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<StatefulSet>(obj) {
            Some(s) => statefulset_health(obj, &s),
            None => GenericPlugin.health(obj),
        }
    }
}

// ----------------------------------------------------------------- DaemonSet

pub(crate) fn daemonset_health(obj: &DynamicObject, d: &DaemonSet) -> Health {
    let Some(status) = &d.status else {
        return Health::phrase(HealthStatus::Progressing, msg!("state-waiting-status"));
    };
    if generation_pending(obj, status.observed_generation) {
        return Health::phrase(HealthStatus::Progressing, msg!("state-waiting-rollout"));
    }
    let desired = status.desired_number_scheduled;
    let updated = n(status.updated_number_scheduled);
    let available = n(status.number_available);
    if updated < desired {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!("state-pods-updated", updated = updated, desired = desired),
        );
    }
    if available < desired {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!(
                "state-pods-available",
                available = available,
                desired = desired
            ),
        );
    }
    if status.number_misscheduled > 0 {
        return Health::phrase(
            HealthStatus::Degraded,
            msg!(
                "state-pods-misscheduled",
                count = status.number_misscheduled
            ),
        );
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-available"))
}

impl ResourcePlugin for DaemonSetPlugin {
    fn name(&self) -> &'static str {
        "daemonset"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("apps", "v1", "DaemonSet")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("desired", "Desired", ColumnKind::Number),
            col("current", "Current", ColumnKind::Number),
            col("ready", "Ready", ColumnKind::Number),
            col("up_to_date", "Up-to-date", ColumnKind::Number),
            col("available", "Available", ColumnKind::Number),
            col("node_selector", "Node selector", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(d) = decode::<DaemonSet>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let status = d.status.clone().unwrap_or_default();
        let node_selector = d
            .spec
            .as_ref()
            .and_then(|s| s.template.spec.as_ref())
            .and_then(|s| s.node_selector.as_ref());
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Number(status.desired_number_scheduled.into()),
            Cell::Number(status.current_number_scheduled.into()),
            Cell::Number(status.number_ready.into()),
            Cell::Number(n(status.updated_number_scheduled).into()),
            Cell::Number(n(status.number_available).into()),
            text(or_none(super::join_map(node_selector))),
            Cell::Status(daemonset_health(obj, &d)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(d) = decode::<DaemonSet>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = d.spec.as_ref();
        let status = d.status.clone().unwrap_or_default();
        let update = spec.and_then(|s| s.update_strategy.as_ref());
        let rolling_update = update.and_then(|u| u.rolling_update.as_ref());
        let mut overview = vec![
            Field::new(
                "Pods",
                FieldValue::Replicas(ReplicaCounts {
                    desired: status.desired_number_scheduled,
                    current: status.current_number_scheduled,
                    ready: status.number_ready,
                    updated: Some(n(status.updated_number_scheduled)),
                    available: Some(n(status.number_available)),
                }),
            ),
            Field::text(
                "Update strategy",
                rolling(
                    update.and_then(|u| u.type_.as_deref()),
                    rolling_update.and_then(|r| r.max_surge.as_ref()),
                    rolling_update.and_then(|r| r.max_unavailable.as_ref()),
                ),
            ),
        ];
        if status.number_misscheduled > 0 {
            overview.push(Field::text(
                "Misscheduled",
                status.number_misscheduled.to_string(),
            ));
        }
        let selector = spec.map(|s| &s.selector);
        workload_details(
            obj,
            overview,
            selector,
            spec.map(|s| &s.template),
            related(obj, "v1", "Pod", selector, true, RelatedOrder::Name)
                .into_iter()
                .collect(),
        )
    }

    fn actions(&self) -> Vec<Action> {
        vec![RESTART_ACTION, PORT_FORWARD_ACTION, DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(run_workload::<DaemonSet>(
            client,
            obj,
            action_id,
            params,
            &[action::RESTART, action::DELETE],
        ))
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        let d = decode::<DaemonSet>(obj);
        pod_selector_relations(
            obj,
            d.as_ref()
                .and_then(|d| d.spec.as_ref())
                .map(|s| &s.selector),
            false,
        )
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<DaemonSet>(obj) {
            Some(d) => daemonset_health(obj, &d),
            None => GenericPlugin.health(obj),
        }
    }
}

// ---------------------------------------------------------------- ReplicaSet

pub(crate) fn replicaset_health(obj: &DynamicObject, r: &ReplicaSet) -> Health {
    let status = r.status.clone().unwrap_or_default();
    let desired = r.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
    if let Some(c) = status
        .conditions
        .iter()
        .flatten()
        .find(|c| c.type_ == "ReplicaFailure" && c.status == "True")
    {
        let msg = c
            .message
            .clone()
            .unwrap_or_else(|| "Replica failure".into());
        return Health::new(HealthStatus::Failed, msg);
    }
    if generation_pending(obj, status.observed_generation) {
        return Health::phrase(HealthStatus::Progressing, msg!("state-waiting-controller"));
    }
    let ready = n(status.ready_replicas);
    if ready < desired {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!("state-replicas-ready", ready = ready, desired = desired),
        );
    }
    if desired == 0 {
        return Health::phrase(HealthStatus::Healthy, msg!("state-scaled-to-zero"));
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-ready"))
}

impl ResourcePlugin for ReplicaSetPlugin {
    fn name(&self) -> &'static str {
        "replicaset"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("apps", "v1", "ReplicaSet")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("desired", "Desired", ColumnKind::Number),
            col("current", "Current", ColumnKind::Number),
            col("ready", "Ready", ColumnKind::Number),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(r) = decode::<ReplicaSet>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let status = r.status.clone().unwrap_or_default();
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Number(r.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1).into()),
            Cell::Number(status.replicas.into()),
            Cell::Number(n(status.ready_replicas).into()),
            Cell::Status(replicaset_health(obj, &r)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(r) = decode::<ReplicaSet>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = r.spec.as_ref();
        let status = r.status.clone().unwrap_or_default();
        let mut overview = vec![Field::new(
            "Replicas",
            FieldValue::Replicas(ReplicaCounts {
                desired: spec.and_then(|s| s.replicas).unwrap_or(1),
                current: status.replicas,
                ready: n(status.ready_replicas),
                updated: None,
                available: Some(n(status.available_replicas)),
            }),
        )];
        overview.extend(revision_field(obj));
        let selector = spec.map(|s| &s.selector);
        workload_details(
            obj,
            overview,
            selector,
            spec.and_then(|s| s.template.as_ref()),
            related(obj, "v1", "Pod", selector, true, RelatedOrder::Name)
                .into_iter()
                .collect(),
        )
    }

    fn brief(&self, obj: &DynamicObject) -> Brief {
        let health = self.health(obj);
        let ready = obj
            .data
            .pointer("/status/readyReplicas")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let desired = spec_replicas(obj);
        let note = match revision(obj) {
            Some(r) => msg!(
                "brief-revision-ready",
                revision = r,
                ready = ready,
                desired = desired
            ),
            None => msg!("brief-ready", ready = ready, desired = desired),
        };
        Brief::phrased(health, note, Vec::new())
    }

    fn actions(&self) -> Vec<Action> {
        vec![SCALE_ACTION, PORT_FORWARD_ACTION, DELETE_ACTION]
    }

    fn action_params(&self, obj: &DynamicObject, action_id: &ActionId) -> Vec<ActionParam> {
        if *action_id == action::SCALE {
            scale_params(spec_replicas(obj))
        } else {
            Vec::new()
        }
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(run_workload::<ReplicaSet>(
            client,
            obj,
            action_id,
            params,
            &[action::SCALE, action::DELETE],
        ))
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        let r = decode::<ReplicaSet>(obj);
        pod_selector_relations(
            obj,
            r.as_ref()
                .and_then(|r| r.spec.as_ref())
                .map(|s| &s.selector),
            false,
        )
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<ReplicaSet>(obj) {
            Some(r) => replicaset_health(obj, &r),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as section_body};

    #[test]
    fn statefulset_details_list_claims_and_pods_by_ordinal() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "StatefulSet",
            "metadata": {"name": "db", "namespace": "default", "uid": "s-uid"},
            "spec": {
                "replicas": 2, "serviceName": "db-headless",
                "selector": {"matchLabels": {"app": "db"}},
                "updateStrategy": {"type": "RollingUpdate", "rollingUpdate": {"partition": 1}},
                "template": {"spec": {"containers": [{"name": "pg", "image": "postgres:16"}]}},
                "volumeClaimTemplates": [{
                    "metadata": {"name": "data"},
                    "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}
                }]
            },
            "status": {"replicas": 2, "readyReplicas": 2, "currentRevision": "db-1", "updateRevision": "db-2"}
        }));
        let sections = StatefulSetPlugin.details(&o);
        let SectionBody::Fields(overview) = &sections[0].body else {
            panic!()
        };
        assert!(overview.contains(&Field::link(
            "Service",
            "Service",
            "db-headless",
            Some("default")
        )));
        assert!(overview.contains(&Field::text(
            "Update strategy",
            "RollingUpdate · partition 1"
        )));
        assert!(overview.contains(&Field::text("Revision", "db-1 → db-2")));
        assert_eq!(
            pairs(&sections, "Volume claim templates"),
            [pair("data", "1Gi · ReadWriteOnce")]
        );
        let SectionBody::Related(claims) = section_body(&sections, "Persistent volume claims")
        else {
            panic!()
        };
        assert_eq!(
            claims.filter,
            RelatedFilter::Names(vec!["data-db-0".into(), "data-db-1".into()])
        );
        let SectionBody::Related(pods) = section_body(&sections, "Pods") else {
            panic!()
        };
        assert_eq!(pods.order, RelatedOrder::Ordinal);
    }

    fn deployment(spec_replicas: i32, status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {
                "name": "web", "namespace": "default", "generation": 3,
                "creationTimestamp": "2026-10-22T10:00:00Z",
                "annotations": {"deployment.kubernetes.io/revision": "3"}
            },
            "spec": {
                "replicas": spec_replicas,
                "selector": {"matchLabels": {"app": "web"}},
                "template": {
                    "metadata": {"labels": {"app": "web"}},
                    "spec": {"containers": [{"name": "app", "image": "nginx:latest"}]}
                }
            },
            "status": status
        }))
    }

    fn healthy_status() -> Value {
        json!({
            "observedGeneration": 3, "replicas": 3, "updatedReplicas": 3,
            "readyReplicas": 3, "availableReplicas": 3,
            "conditions": [{"type": "Available", "status": "True"}]
        })
    }

    #[test]
    fn deployment_cells() {
        let p = DeploymentPlugin;
        let cells = p.cells(&deployment(3, healthy_status()));
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("3/3"));
        assert_eq!(cells[3], Cell::Number(3));
        assert_eq!(cells[4], Cell::Number(3));
        assert_eq!(
            cells[5],
            Cell::Status(Health::new(HealthStatus::Healthy, "Available"))
        );
        assert_eq!(cells[7], text("nginx:latest"));
    }

    #[test]
    fn deployment_health_states() {
        let h = |replicas, status| DeploymentPlugin.health(&deployment(replicas, status));
        assert_eq!(h(3, healthy_status()).status, HealthStatus::Healthy);
        assert_eq!(
            h(5, healthy_status()),
            Health::new(HealthStatus::Progressing, "3 of 5 replicas updated")
        );
        assert_eq!(
            h(3, json!({"observedGeneration": 2})).status,
            HealthStatus::Progressing
        );
        let failed = h(
            3,
            json!({
                "observedGeneration": 3, "replicas": 3, "updatedReplicas": 1,
                "conditions": [{
                    "type": "Progressing", "status": "False",
                    "reason": "ProgressDeadlineExceeded",
                    "message": "ReplicaSet \"web-1\" has timed out progressing."
                }]
            }),
        );
        assert_eq!(failed.status, HealthStatus::Failed);
        assert!(failed.message.contains("timed out"));
        let degraded = h(
            3,
            json!({
                "observedGeneration": 3, "replicas": 3, "updatedReplicas": 3,
                "availableReplicas": 1,
                "conditions": [{"type": "Available", "status": "False",
                                "message": "Deployment does not have minimum availability."}]
            }),
        );
        assert_eq!(degraded.status, HealthStatus::Degraded);
        assert_eq!(
            h(0, json!({"observedGeneration": 3})),
            Health::new(HealthStatus::Healthy, "Scaled to zero")
        );
    }

    #[test]
    fn deployment_details_show_template_probes() {
        let mut o = deployment(3, healthy_status());
        o.data["spec"]["template"]["spec"] = json!({
            "initContainers": [
                {"name": "migrate", "image": "x"},
                {"name": "proxy", "image": "x", "restartPolicy": "Always",
                 "startupProbe": {"tcpSocket": {"port": 15000}, "failureThreshold": 30}}
            ],
            "containers": [{
                "name": "app", "image": "x",
                "livenessProbe": {"httpGet": {"path": "/healthz", "port": "http"}, "timeoutSeconds": 1},
                "readinessProbe": {"exec": {"command": ["cat", "/tmp/ready"]}, "periodSeconds": 5}
            }]
        });
        let sections = DeploymentPlugin.details(&o);
        assert_eq!(
            pairs(&sections, "Probes"),
            [
                pair(
                    "proxy startup",
                    "tcp :15000 · delay 0s · timeout 1s · every 10s · failure 30 · success 1"
                ),
                pair(
                    "app liveness",
                    "http-get :http/healthz · delay 0s · timeout 1s · every 10s · failure 3 · success 1"
                ),
                pair(
                    "app readiness",
                    "exec cat /tmp/ready · delay 0s · timeout 1s · every 5s · failure 3 · success 1"
                ),
            ]
        );
    }

    #[test]
    fn deployment_details_and_selectors() {
        let mut o = deployment(3, healthy_status());
        o.metadata.uid = Some("d-uid".into());
        let sections = DeploymentPlugin.details(&o);
        let SectionBody::Fields(overview) = &sections[0].body else {
            panic!("overview fields expected");
        };
        assert!(overview.contains(&Field::text("Revision", "3")));
        assert!(overview.contains(&Field::new(
            "Selector",
            FieldValue::Labels(vec![pair("app", "web")])
        )));
        assert!(overview.contains(&Field::new(
            "Replicas",
            FieldValue::Replicas(ReplicaCounts {
                desired: 3,
                current: 3,
                ready: 3,
                updated: Some(3),
                available: Some(3),
            })
        )));
        assert!(overview.contains(&Field::text("Strategy", "RollingUpdate")));
        let SectionBody::Related(sets) = section_body(&sections, "ReplicaSets") else {
            panic!()
        };
        assert_eq!(sets.order, RelatedOrder::Revision);
        assert_eq!(
            sets.filter,
            RelatedFilter::Selector {
                labels: Some("app=web".into()),
                owner_uid: Some("d-uid".into())
            }
        );
        assert!(matches!(
            section_body(&sections, "Pods"),
            SectionBody::Related(pods) if pods.kind == "Pod"
        ));
        assert_eq!(
            pairs(&sections, "Containers"),
            [pair("app", "nginx:latest")]
        );
        assert_eq!(pairs(&sections, "Probes"), [pair("app", "none")]);
        // `:latest` and missing limits are reported.
        assert!(
            matches!(section_body(&sections, "Security"), SectionBody::Notes(n) if !n.is_empty())
        );

        let rels = DeploymentPlugin.selector_relations(&o);
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].target_kind, "ReplicaSet");
        assert_eq!(rels[1].target_kind, "Pod");
        assert_eq!(rels[1].label_selector, "app=web");
        assert_eq!(rels[1].namespace.as_deref(), Some("default"));
    }

    #[test]
    fn scale_params_and_patches() {
        let o = deployment(4, healthy_status());
        let params = DeploymentPlugin.action_params(&o, &action::SCALE);
        assert_eq!(params[0].id, "replicas");
        assert_eq!(params[0].default, Some(json!(4)));
        assert!(
            DeploymentPlugin
                .action_params(&o, &action::RESTART)
                .is_empty()
        );

        let mut p = ActionParams::new();
        assert!(matches!(
            replicas_param(&p),
            Err(PluginError::InvalidParam { .. })
        ));
        p.insert("replicas".into(), json!(-1));
        assert!(replicas_param(&p).is_err());
        p.insert("replicas".into(), json!("2"));
        assert!(replicas_param(&p).is_err());
        p.insert("replicas".into(), json!(2));
        assert_eq!(replicas_param(&p).unwrap(), 2);

        assert_eq!(scale_patch(2), json!({"spec": {"replicas": 2}}));
        assert_eq!(
            restart_patch("2026-10-22T12:00:00Z"),
            json!({"spec": {"template": {"metadata": {"annotations": {
                "kubectl.kubernetes.io/restartedAt": "2026-10-22T12:00:00Z"
            }}}}})
        );
    }

    #[test]
    fn statefulset_cells_and_health() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "StatefulSet",
            "metadata": {"name": "db", "namespace": "default", "generation": 1},
            "spec": {
                "replicas": 3, "serviceName": "db",
                "selector": {"matchLabels": {"app": "db"}},
                "template": {"spec": {"containers": [{"name": "pg", "image": "postgres:16"}]}}
            },
            "status": {"observedGeneration": 1, "replicas": 3, "readyReplicas": 2,
                       "currentRevision": "db-1", "updateRevision": "db-1"}
        }));
        let p = StatefulSetPlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("2/3"));
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Progressing, "2 of 3 replicas ready")
        );
        assert_eq!(p.selector_relations(&o).len(), 1);
    }

    #[test]
    fn daemonset_cells_and_health() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "DaemonSet",
            "metadata": {"name": "agent", "namespace": "kube-system", "generation": 1},
            "spec": {
                "selector": {"matchLabels": {"app": "agent"}},
                "template": {"spec": {
                    "nodeSelector": {"kubernetes.io/os": "linux"},
                    "containers": [{"name": "a", "image": "agent:1"}]
                }}
            },
            "status": {"observedGeneration": 1, "desiredNumberScheduled": 2,
                       "currentNumberScheduled": 2, "numberReady": 2, "numberMisscheduled": 0,
                       "updatedNumberScheduled": 2, "numberAvailable": 2}
        }));
        let p = DaemonSetPlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], Cell::Number(2));
        assert_eq!(cells[7], text("kubernetes.io/os=linux"));
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
        let ids: Vec<_> = p.actions().iter().map(|a| a.id).collect();
        assert_eq!(ids, ["restart", "port-forward", "delete"]);
    }

    #[test]
    fn replicaset_cells_and_health() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "ReplicaSet",
            "metadata": {"name": "web-abc", "namespace": "default",
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment", "name": "web", "uid": "u"}]},
            "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "web"}}},
            "status": {"replicas": 2, "readyReplicas": 2,
                "conditions": [{"type": "ReplicaFailure", "status": "True", "message": "quota exceeded"}]}
        }));
        let p = ReplicaSetPlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], Cell::Number(2));
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Failed, "quota exceeded")
        );
        assert_eq!(p.relations(&o)[0].target.name, "web");
    }

    #[test]
    fn undecodable_falls_back() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "x", "namespace": "n"},
            "spec": {"replicas": "many"}
        }));
        let cells = DeploymentPlugin.cells(&o);
        assert_eq!(cells.len(), DeploymentPlugin.columns().len());
        assert_eq!(cells[0], text("x"));
        assert_eq!(DeploymentPlugin.health(&o), Health::unknown());
    }
}
