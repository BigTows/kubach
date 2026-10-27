//! Namespace policies: ResourceQuota (used against hard, per resource),
//! LimitRange (constraints per type) and PodDisruptionBudget (disruptions
//! allowed, healthy pods, the pods it selects).

use kubach_i18n::msg;
use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{LimitRange, ResourceQuota};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::workloads::related;
use super::{
    DELETE_ACTION, OVERVIEW, col, creation, decode, fallback_cells, int_or_string, kv, map_pairs,
    name_ns_cells, name_ns_columns, or_none, pair, push_kv, run_delete, section, selector_string,
    selects, text,
};
use crate::metrics::parse_quantity;
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, PluginError, RelatedOrder, Relation,
    ResourcePlugin, SectionBody, SelectorRelation,
};

pub struct ResourceQuotaPlugin;
pub struct LimitRangePlugin;
pub struct PodDisruptionBudgetPlugin;

type Quantities = BTreeMap<String, Quantity>;

// -------------------------------------------------------------- ResourceQuota

/// One resource of a quota: name, used, hard and the share used (`0..`,
/// above 1 when over the quota; `None` when a side does not parse).
struct QuotaUse {
    resource: String,
    used: String,
    hard: String,
    share: Option<f64>,
}

fn quota_uses(quota: &ResourceQuota) -> Vec<QuotaUse> {
    let empty = Quantities::new();
    let status = quota.status.as_ref();
    // The status repeats the spec once the controller has seen it.
    let hard = status
        .and_then(|s| s.hard.as_ref())
        .or_else(|| quota.spec.as_ref().and_then(|s| s.hard.as_ref()))
        .unwrap_or(&empty);
    let used = status.and_then(|s| s.used.as_ref()).unwrap_or(&empty);
    hard.iter()
        .map(|(resource, hard)| {
            let used = used
                .get(resource)
                .map_or_else(|| "0".to_owned(), |q| q.0.clone());
            let share = parse_quantity(&used)
                .ok()
                .zip(parse_quantity(&hard.0).ok())
                .and_then(|(u, h)| {
                    if h > 0.0 {
                        Some(u / h)
                    } else {
                        (u == 0.0).then_some(0.0)
                    }
                });
            QuotaUse {
                resource: resource.clone(),
                used,
                hard: hard.0.clone(),
                share,
            }
        })
        .collect()
}

fn percent(share: Option<f64>) -> String {
    share
        .map(|s| format!(" ({:.0}%)", s * 100.0))
        .unwrap_or_default()
}

/// The resource closest to its limit: `pods 8/10 (80%)`.
fn most_used(uses: &[QuotaUse]) -> String {
    uses.iter()
        .filter(|u| u.share.is_some())
        .max_by(|a, b| {
            a.share
                .partial_cmp(&b.share)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .or(uses.first())
        .map(|u| format!("{} {}/{}{}", u.resource, u.used, u.hard, percent(u.share)))
        .unwrap_or_else(|| "<none>".into())
}

/// Resources at or over their hard limit make the quota degraded: new
/// objects using them are refused.
pub(crate) fn quota_health(quota: &ResourceQuota) -> Health {
    if quota
        .status
        .as_ref()
        .and_then(|s| s.used.as_ref())
        .is_none()
    {
        return Health::phrase(HealthStatus::Progressing, msg!("state-quota-not-computed"));
    }
    let names: Vec<String> = quota_uses(quota)
        .into_iter()
        .filter(|u| u.share.is_some_and(|s| s >= 1.0))
        .map(|u| u.resource)
        .collect();
    if names.is_empty() {
        return Health::phrase(HealthStatus::Healthy, msg!("state-quota-within"));
    }
    Health::phrase(
        HealthStatus::Degraded,
        msg!("state-quota-at-limit", resources = names.join(", ")),
    )
}

impl ResourcePlugin for ResourceQuotaPlugin {
    fn name(&self) -> &'static str {
        "resourcequota"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "ResourceQuota")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("resources", "Resources", ColumnKind::Number),
            col("most_used", "Most used", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(quota) = decode::<ResourceQuota>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let uses = quota_uses(&quota);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Number(uses.len() as i64),
            text(most_used(&uses)),
            Cell::Status(quota_health(&quota)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(quota) = decode::<ResourceQuota>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = quota.spec.clone().unwrap_or_default();
        let mut overview = vec![pair(
            "Scopes",
            or_none(spec.scopes.clone().unwrap_or_default().join(", ")),
        )];
        let selector: Vec<String> = spec
            .scope_selector
            .iter()
            .flat_map(|s| s.match_expressions.iter().flatten())
            .map(|e| {
                let values = e.values.clone().unwrap_or_default().join(",");
                format!("{} {} ({values})", e.scope_name, e.operator)
            })
            .collect();
        if !selector.is_empty() {
            overview.push(pair("Scope selector", selector.join("; ")));
        }
        let mut sections = vec![kv(OVERVIEW, overview)];
        let usage: Vec<Field> = quota_uses(&quota)
            .into_iter()
            .map(|u| Field::new(u.resource, FieldValue::usage(u.used, u.hard)))
            .collect();
        if !usage.is_empty() {
            sections.push(section("Usage", SectionBody::Fields(usage)));
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_delete::<ResourceQuota>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<ResourceQuota>(obj) {
            Some(quota) => quota_health(&quota),
            None => GenericPlugin.health(obj),
        }
    }
}

// ----------------------------------------------------------------- LimitRange

/// `min 100m · max 2 · default 500m · default request 250m · max ratio 4`
/// for each resource that any constraint of the item names.
fn limit_rows(item: &k8s_openapi::api::core::v1::LimitRangeItem) -> Vec<(String, String)> {
    let parts: [(&str, Option<&Quantities>); 5] = [
        ("min", item.min.as_ref()),
        ("max", item.max.as_ref()),
        ("default request", item.default_request.as_ref()),
        ("default limit", item.default.as_ref()),
        ("max limit/request", item.max_limit_request_ratio.as_ref()),
    ];
    let mut resources: Vec<&String> = parts
        .iter()
        .flat_map(|(_, m)| m.iter().flat_map(|m| m.keys()))
        .collect();
    resources.sort();
    resources.dedup();
    resources
        .into_iter()
        .map(|resource| {
            let value = parts
                .iter()
                .filter_map(|(label, map)| {
                    map.and_then(|m| m.get(resource))
                        .map(|q| format!("{label} {}", q.0))
                })
                .collect::<Vec<_>>()
                .join(" · ");
            (resource.clone(), value)
        })
        .collect()
}

fn limit_types(lr: &LimitRange) -> Vec<String> {
    let mut types: Vec<String> = lr
        .spec
        .iter()
        .flat_map(|s| &s.limits)
        .map(|l| l.type_.clone())
        .collect();
    types.dedup();
    types
}

impl ResourcePlugin for LimitRangePlugin {
    fn name(&self) -> &'static str {
        "limitrange"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "LimitRange")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("types", "Types", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(lr) = decode::<LimitRange>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(or_none(limit_types(&lr).join(", "))),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(lr) = decode::<LimitRange>(obj) else {
            return GenericPlugin.details(obj);
        };
        let mut sections = vec![kv(
            OVERVIEW,
            vec![pair("Types", or_none(limit_types(&lr).join(", ")))],
        )];
        for item in lr.spec.iter().flat_map(|s| &s.limits) {
            push_kv(
                &mut sections,
                format!("{} limits", item.type_),
                limit_rows(item),
            );
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_delete::<LimitRange>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    /// A LimitRange has no status: it is a constraint, always in effect.
    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::phrase(HealthStatus::Healthy, msg!("state-active"))
    }
}

// -------------------------------------------------------- PodDisruptionBudget

struct Budget {
    allowed: i32,
    current: i32,
    desired: i32,
    expected: i32,
}

fn budget(pdb: &PodDisruptionBudget) -> Budget {
    let status = pdb.status.clone().unwrap_or_default();
    Budget {
        allowed: status.disruptions_allowed.unwrap_or(0),
        current: status.current_healthy.unwrap_or(0),
        desired: status.desired_healthy.unwrap_or(0),
        expected: status.expected_pods.unwrap_or(0),
    }
}

fn min_available(pdb: &PodDisruptionBudget) -> String {
    pdb.spec
        .as_ref()
        .and_then(|s| s.min_available.as_ref())
        .map(int_or_string)
        .unwrap_or_else(|| "N/A".into())
}

fn max_unavailable(pdb: &PodDisruptionBudget) -> String {
    pdb.spec
        .as_ref()
        .and_then(|s| s.max_unavailable.as_ref())
        .map(int_or_string)
        .unwrap_or_else(|| "N/A".into())
}

/// Fewer healthy pods than the budget wants is degraded; a budget that
/// allows no disruption is healthy but says that evictions (drains) wait.
pub(crate) fn pdb_health(pdb: &PodDisruptionBudget) -> Health {
    let Some(status) = &pdb.status else {
        return Health::phrase(HealthStatus::Progressing, msg!("state-pdb-not-observed"));
    };
    let b = budget(pdb);
    if status.observed_generation.is_none() && status.conditions.is_none() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-pdb-not-observed"));
    }
    if b.expected == 0 {
        return Health::phrase(HealthStatus::Healthy, msg!("state-pdb-no-pods"));
    }
    if b.current < b.desired {
        return Health::phrase(
            HealthStatus::Degraded,
            msg!(
                "state-pdb-unhealthy",
                current = b.current,
                desired = b.desired
            ),
        );
    }
    if b.allowed == 0 {
        return Health::phrase(HealthStatus::Healthy, msg!("state-pdb-no-disruptions"));
    }
    Health::phrase(
        HealthStatus::Healthy,
        msg!("state-pdb-disruptions", count = b.allowed),
    )
}

impl ResourcePlugin for PodDisruptionBudgetPlugin {
    fn name(&self) -> &'static str {
        "poddisruptionbudget"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("policy", "v1", "PodDisruptionBudget")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("min_available", "Min available", ColumnKind::Text),
            col("max_unavailable", "Max unavailable", ColumnKind::Text),
            col("allowed", "Allowed disruptions", ColumnKind::Number),
            col("healthy", "Healthy", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(pdb) = decode::<PodDisruptionBudget>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let b = budget(&pdb);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(min_available(&pdb)),
            text(max_unavailable(&pdb)),
            Cell::Number(b.allowed.into()),
            text(format!("{}/{}", b.current, b.desired)),
            Cell::Status(pdb_health(&pdb)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(pdb) = decode::<PodDisruptionBudget>(obj) else {
            return GenericPlugin.details(obj);
        };
        let b = budget(&pdb);
        let spec = pdb.spec.clone().unwrap_or_default();
        let mut fields = vec![
            Field::text("Min available", min_available(&pdb)),
            Field::text("Max unavailable", max_unavailable(&pdb)),
            Field::text("Allowed disruptions", b.allowed.to_string()),
            Field::text(
                "Healthy pods",
                format!(
                    "{} current · {} desired · {} expected",
                    b.current, b.desired, b.expected
                ),
            ),
            Field::text(
                "Unhealthy pod eviction",
                spec.unhealthy_pod_eviction_policy
                    .clone()
                    .unwrap_or_else(|| "IfHealthyBudget".into()),
            ),
        ];
        let labels = spec
            .selector
            .as_ref()
            .map(|s| map_pairs(s.match_labels.as_ref()))
            .unwrap_or_default();
        let selector = spec.selector.as_ref().and_then(selector_string);
        fields.push(match (labels.is_empty(), &selector) {
            (false, _) => Field::new("Selector", FieldValue::Labels(labels)),
            (true, Some(s)) => Field::text("Selector", s.clone()),
            (true, None) => Field::text("Selector", "<none>"),
        });
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        push_kv(
            &mut sections,
            "Disrupted pods",
            pdb.status
                .as_ref()
                .and_then(|s| s.disrupted_pods.as_ref())
                .into_iter()
                .flatten()
                .map(|(pod, at)| pair(pod.clone(), at.0.to_string()))
                .collect(),
        );
        sections.extend(related(
            obj,
            "v1",
            "Pod",
            spec.selector.as_ref(),
            false,
            RelatedOrder::Name,
        ));
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_delete::<PodDisruptionBudget>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        decode::<PodDisruptionBudget>(obj)
            .and_then(|pdb| pdb.spec?.selector)
            .and_then(|s| selector_string(&s))
            .map(|labels| vec![selects(obj, "v1", "Pod", labels)])
            .unwrap_or_default()
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<PodDisruptionBudget>(obj) {
            Some(pdb) => pdb_health(&pdb),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};
    use crate::plugin::{RelatedFilter, SectionBody};

    fn quota(used: serde_json::Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "ResourceQuota",
            "metadata": {"name": "compute", "namespace": "team"},
            "spec": {"hard": {"pods": "10", "requests.cpu": "2", "requests.memory": "4Gi"},
                     "scopes": ["NotTerminating"]},
            "status": {"hard": {"pods": "10", "requests.cpu": "2", "requests.memory": "4Gi"},
                       "used": used}
        }))
    }

    #[test]
    fn quota_cells_and_usage() {
        let p = ResourceQuotaPlugin;
        let o = quota(json!({"pods": "3", "requests.cpu": "1500m", "requests.memory": "1Gi"}));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], Cell::Number(3));
        assert_eq!(cells[3], text("requests.cpu 1500m/2 (75%)"));
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);

        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, OVERVIEW),
            [pair("Scopes", "NotTerminating")]
        );
        let SectionBody::Fields(usage) = body(&sections, "Usage") else {
            panic!("usage is not fields");
        };
        assert_eq!(usage[0].key, "pods");
        assert_eq!(
            usage[0].value,
            FieldValue::Usage {
                used: "3".into(),
                hard: "10".into(),
                permille: Some(300)
            }
        );
        assert_eq!(usage[0].value.text(), "3/10");
        let FieldValue::Usage { permille, .. } = &usage[2].value else {
            panic!()
        };
        assert_eq!(*permille, Some(250));
    }

    #[test]
    fn quota_at_limit_is_degraded() {
        let p = ResourceQuotaPlugin;
        let full = quota(json!({"pods": "10", "requests.cpu": "500m", "requests.memory": "0"}));
        assert_eq!(
            p.health(&full),
            Health::new(HealthStatus::Degraded, "At the limit: pods")
        );
        assert_eq!(p.cells(&full)[3], text("pods 10/10 (100%)"));
        let fresh = obj(json!({
            "apiVersion": "v1", "kind": "ResourceQuota",
            "metadata": {"name": "q", "namespace": "team"},
            "spec": {"hard": {"pods": "1"}}
        }));
        assert_eq!(p.health(&fresh).status, HealthStatus::Progressing);
        assert_eq!(p.cells(&fresh)[3], text("pods 0/1 (0%)"));
    }

    #[test]
    fn limit_range_rows_per_type() {
        let p = LimitRangePlugin;
        let o = obj(json!({
            "apiVersion": "v1", "kind": "LimitRange",
            "metadata": {"name": "limits", "namespace": "team"},
            "spec": {"limits": [
                {"type": "Container",
                 "min": {"cpu": "50m"}, "max": {"cpu": "2", "memory": "1Gi"},
                 "default": {"cpu": "500m", "memory": "256Mi"},
                 "defaultRequest": {"cpu": "100m"},
                 "maxLimitRequestRatio": {"cpu": "4"}},
                {"type": "PersistentVolumeClaim", "max": {"storage": "10Gi"}}
            ]}
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("Container, PersistentVolumeClaim"));
        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Container limits"),
            [
                pair(
                    "cpu",
                    "min 50m · max 2 · default request 100m · default limit 500m · max limit/request 4"
                ),
                pair("memory", "max 1Gi · default limit 256Mi"),
            ]
        );
        assert_eq!(
            pairs(&sections, "PersistentVolumeClaim limits"),
            [pair("storage", "max 10Gi")]
        );
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
    }

    fn pdb(status: serde_json::Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "policy/v1", "kind": "PodDisruptionBudget",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": {"minAvailable": 2, "selector": {"matchLabels": {"app": "web"}}},
            "status": status
        }))
    }

    #[test]
    fn pdb_cells_details_and_pods() {
        let p = PodDisruptionBudgetPlugin;
        let o = pdb(json!({
            "observedGeneration": 1, "disruptionsAllowed": 1,
            "currentHealthy": 3, "desiredHealthy": 2, "expectedPods": 3,
            "conditions": [{"type": "DisruptionAllowed", "status": "True", "reason": "SufficientPods",
                            "lastTransitionTime": "2026-10-01T10:00:00Z"}]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("2"));
        assert_eq!(cells[3], text("N/A"));
        assert_eq!(cells[4], Cell::Number(1));
        assert_eq!(cells[5], text("3/2"));
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Healthy, "1 disruptions allowed")
        );
        let sel = p.selector_relations(&o);
        assert_eq!(
            (sel[0].target_kind.as_str(), sel[0].label_selector.as_str()),
            ("Pod", "app=web")
        );
        let sections = p.details(&o);
        let SectionBody::Related(pods) = body(&sections, "Pods") else {
            panic!("no pods section");
        };
        assert_eq!(
            pods.filter,
            RelatedFilter::Selector {
                labels: Some("app=web".into()),
                owner_uid: None
            }
        );
        let SectionBody::Conditions(conditions) = body(&sections, "Conditions") else {
            panic!("no conditions");
        };
        assert_eq!(conditions[0].is_good(), Some(true));
    }

    #[test]
    fn pdb_health_states() {
        let p = PodDisruptionBudgetPlugin;
        let short = pdb(json!({
            "observedGeneration": 1, "disruptionsAllowed": 0,
            "currentHealthy": 1, "desiredHealthy": 2, "expectedPods": 3
        }));
        assert_eq!(
            p.health(&short),
            Health::new(HealthStatus::Degraded, "1 of 2 required pods healthy")
        );
        let strict = pdb(json!({
            "observedGeneration": 1, "disruptionsAllowed": 0,
            "currentHealthy": 2, "desiredHealthy": 2, "expectedPods": 2
        }));
        assert_eq!(p.health(&strict).status, HealthStatus::Healthy);
        assert!(p.health(&strict).message.contains("No disruptions"));
        assert_eq!(
            p.health(&pdb(json!(null))).status,
            HealthStatus::Progressing
        );
    }
}
