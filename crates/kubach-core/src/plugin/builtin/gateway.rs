//! Gateway API (`gateway.networking.k8s.io`): GatewayClass, Gateway and
//! the routes (HTTPRoute, GRPCRoute, TLSRoute, TCPRoute, UDPRoute).
//!
//! These are CRDs, served only when installed, and have no `k8s-openapi`
//! types: the plugins read the JSON. Delete goes through discovery, like
//! any CRD.

use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind, TypeMeta};
use serde_json::Value;

use super::{
    DELETE_ACTION, OVERVIEW, action, col, condition_polarity, creation, delete_discovered, joined,
    kv, name_ns_cells, name_ns_columns, object_ref, or_none, pair, push_kv, references,
    relations_with, section, text, unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, ConditionInfo,
    DetailSection, Field, GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation,
    ResourcePlugin, SectionBody,
};

pub struct GatewayClassPlugin;
pub struct GatewayPlugin;
/// HTTPRoute, GRPCRoute, TLSRoute, TCPRoute and UDPRoute.
pub struct RoutePlugin;

/// API group of the Gateway API.
pub const GROUP: &str = "gateway.networking.k8s.io";
/// Version used for links; links resolve by group and kind.
const API_VERSION: &str = "gateway.networking.k8s.io/v1";

/// Route kinds, in the order the sidebar lists them.
pub const ROUTE_KINDS: [&str; 5] = ["HTTPRoute", "GRPCRoute", "TLSRoute", "TCPRoute", "UDPRoute"];

/// Gateway API conditions that report a problem when `True` (a listener
/// that conflicts with another one).
pub(crate) const NEGATIVE_CONDITIONS: [&str; 1] = ["Conflicted"];

pub(crate) fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

pub(crate) fn arr_at<'a>(v: &'a Value, pointer: &str) -> &'a [Value] {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Conditions of a JSON `conditions` array, with Gateway API polarity.
fn conditions_of(types: Option<&TypeMeta>, list: &[Value]) -> Vec<ConditionInfo> {
    list.iter()
        .filter_map(|c| {
            let type_ = str_at(c, "/type")?.to_owned();
            let status = str_at(c, "/status")?.to_owned();
            Some(condition_polarity(types, &type_).condition(
                type_,
                status,
                str_at(c, "/reason").map(str::to_owned),
                str_at(c, "/message").map(str::to_owned),
                str_at(c, "/lastTransitionTime").and_then(|t| t.parse().ok()),
            ))
        })
        .collect()
}

fn condition_text(c: &ConditionInfo) -> String {
    c.message
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| c.reason.clone())
        .unwrap_or_else(|| c.type_.clone())
}

fn find<'a>(conditions: &'a [ConditionInfo], type_: &str) -> Option<&'a ConditionInfo> {
    conditions.iter().find(|c| c.type_ == type_)
}

pub(crate) fn delete_action<'a>(
    client: &'a Client,
    obj: &'a DynamicObject,
    action_id: &'a ActionId,
) -> BoxFuture<'a, Result<(), PluginError>> {
    Box::pin(async move {
        match *action_id {
            action::DELETE => delete_discovered(client, obj).await,
            _ => Err(unknown(action_id)),
        }
    })
}

pub(crate) fn terminating(obj: &DynamicObject) -> Option<Health> {
    obj.metadata
        .deletion_timestamp
        .is_some()
        .then(|| Health::new(HealthStatus::Progressing, "Terminating"))
}

// ---------------------------------------------------------------- GatewayClass

fn class_health(obj: &DynamicObject) -> Health {
    if let Some(h) = terminating(obj) {
        return h;
    }
    let conditions = conditions_of(obj.types.as_ref(), arr_at(&obj.data, "/status/conditions"));
    match find(&conditions, "Accepted") {
        Some(c) if c.status == "True" => {
            Health::phrase(HealthStatus::Healthy, msg!("state-gateway-accepted"))
        }
        Some(c) if c.status == "False" => Health::new(HealthStatus::Degraded, condition_text(c)),
        _ => Health::phrase(HealthStatus::Progressing, msg!("state-waiting-controller")),
    }
}

impl ResourcePlugin for GatewayClassPlugin {
    fn name(&self) -> &'static str {
        "gatewayclass"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1", "GatewayClass")]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("controller", "Controller", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            text(str_at(&obj.data, "/spec/controllerName").unwrap_or_default()),
            Cell::Status(class_health(obj)),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let spec = obj.data.get("spec").cloned().unwrap_or_default();
        let mut overview = vec![pair(
            "Controller",
            or_none(str_at(&spec, "/controllerName").unwrap_or_default()),
        )];
        if let Some(description) = str_at(&spec, "/description") {
            overview.push(pair("Description", description));
        }
        if let Some(params) = spec.get("parametersRef") {
            let kind = str_at(params, "/kind").unwrap_or_default();
            let name = str_at(params, "/name").unwrap_or_default();
            let value = match str_at(params, "/namespace") {
                Some(ns) => format!("{kind} {ns}/{name}"),
                None => format!("{kind} {name}"),
            };
            overview.push(pair("Parameters", value));
        }
        let mut sections = vec![kv(OVERVIEW, overview)];
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
        delete_action(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        class_health(obj)
    }
}

// --------------------------------------------------------------------- Gateway

/// `http:80/HTTP, https:443/HTTPS`.
fn listeners_text(obj: &DynamicObject) -> String {
    arr_at(&obj.data, "/spec/listeners")
        .iter()
        .map(|l| {
            format!(
                "{}:{}/{}",
                str_at(l, "/name").unwrap_or_default(),
                l.get("port").and_then(Value::as_i64).unwrap_or_default(),
                str_at(l, "/protocol").unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Addresses the controller assigned, or the requested ones until then.
fn gateway_addresses(obj: &DynamicObject) -> Vec<String> {
    let assigned = arr_at(&obj.data, "/status/addresses");
    let list = if assigned.is_empty() {
        arr_at(&obj.data, "/spec/addresses")
    } else {
        assigned
    };
    list.iter()
        .filter_map(|a| str_at(a, "/value").map(str::to_owned))
        .collect()
}

fn listener_status<'a>(obj: &'a DynamicObject, name: &str) -> Option<&'a Value> {
    arr_at(&obj.data, "/status/listeners")
        .iter()
        .find(|l| str_at(l, "/name") == Some(name))
}

fn gateway_health(obj: &DynamicObject) -> Health {
    if let Some(h) = terminating(obj) {
        return h;
    }
    let types = obj.types.as_ref();
    let conditions = conditions_of(types, arr_at(&obj.data, "/status/conditions"));
    if let Some(c) = find(&conditions, "Accepted").filter(|c| c.status == "False") {
        return Health::new(HealthStatus::Degraded, condition_text(c));
    }
    for listener in arr_at(&obj.data, "/status/listeners") {
        let name = str_at(listener, "/name").unwrap_or_default();
        let bad = conditions_of(types, arr_at(listener, "/conditions"))
            .into_iter()
            .find(|c| c.is_good() == Some(false));
        if let Some(c) = bad {
            return Health::phrase(
                HealthStatus::Degraded,
                msg!(
                    "state-gateway-listener",
                    name = name,
                    reason = condition_text(&c)
                ),
            );
        }
    }
    match find(&conditions, "Programmed") {
        Some(c) if c.status == "True" => {
            Health::phrase(HealthStatus::Healthy, msg!("state-gateway-programmed"))
        }
        Some(c) if c.status == "False" => Health::new(HealthStatus::Degraded, condition_text(c)),
        _ => Health::phrase(HealthStatus::Progressing, msg!("state-waiting-controller")),
    }
}

/// Secrets (or other objects) a listener's TLS refers to.
fn certificate_refs(obj: &DynamicObject) -> Vec<ObjectRef> {
    let ns = obj.metadata.namespace.as_deref();
    arr_at(&obj.data, "/spec/listeners")
        .iter()
        .flat_map(|l| arr_at(l, "/tls/certificateRefs"))
        .filter_map(|r| {
            let name = str_at(r, "/name")?;
            let group = str_at(r, "/group").unwrap_or_default();
            let kind = str_at(r, "/kind").unwrap_or("Secret");
            let api_version = if group.is_empty() {
                "v1".to_owned()
            } else {
                format!("{group}/v1")
            };
            Some(object_ref(
                &api_version,
                kind,
                name,
                str_at(r, "/namespace").or(ns),
            ))
        })
        .collect()
}

impl ResourcePlugin for GatewayPlugin {
    fn name(&self) -> &'static str {
        "gateway"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1", "Gateway")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("class", "Class", ColumnKind::Text),
            col("addresses", "Addresses", ColumnKind::Text),
            col("listeners", "Listeners", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(str_at(&obj.data, "/spec/gatewayClassName").unwrap_or_default()),
            text(gateway_addresses(obj).join(",")),
            text(listeners_text(obj)),
            Cell::Status(gateway_health(obj)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let types = obj.types.as_ref();
        let mut fields = Vec::new();
        if let Some(class) = str_at(&obj.data, "/spec/gatewayClassName") {
            fields.push(Field::object_link(
                "Class",
                class,
                object_ref(API_VERSION, "GatewayClass", class, None),
            ));
        }
        fields.push(Field::text(
            "Addresses",
            or_none(gateway_addresses(obj).join(", ")),
        ));
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        let mut listeners = Vec::new();
        let mut listener_conditions = Vec::new();
        for l in arr_at(&obj.data, "/spec/listeners") {
            let name = str_at(l, "/name").unwrap_or_default();
            let mut parts = vec![msg!(
                "gateway-listener-port",
                protocol = str_at(l, "/protocol").unwrap_or_default(),
                port = l
                    .get("port")
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    .to_string()
            )];
            if let Some(host) = str_at(l, "/hostname") {
                parts.push(msg!("gateway-listener-host", host = host));
            }
            if let Some(mode) = str_at(l, "/tls/mode") {
                parts.push(msg!("gateway-listener-tls", mode = mode));
            }
            let from = str_at(l, "/allowedRoutes/namespaces/from");
            if let Some(from) = from {
                parts.push(msg!("gateway-listener-routes-from", from = from));
            }
            if let Some(status) = listener_status(obj, name) {
                if let Some(n) = status.get("attachedRoutes").and_then(Value::as_i64) {
                    parts.push(msg!("gateway-listener-routes-attached", count = n));
                }
                let conditions = conditions_of(types, arr_at(status, "/conditions"));
                if !conditions.is_empty() {
                    listener_conditions.push(section(
                        format!("Listener {name}"),
                        SectionBody::Conditions(conditions),
                    ));
                }
            }
            listeners.push(Field::phrase(name, joined(parts)));
        }
        if !listeners.is_empty() {
            sections.push(section("Listeners", SectionBody::Fields(listeners)));
        }
        sections.extend(GenericPlugin.details(obj));
        sections.extend(listener_conditions);
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
        delete_action(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra: Vec<Relation> = str_at(&obj.data, "/spec/gatewayClassName")
            .map(|class| references(object_ref(API_VERSION, "GatewayClass", class, None)))
            .into_iter()
            .collect();
        extra.extend(certificate_refs(obj).into_iter().map(references));
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        gateway_health(obj)
    }
}

// ---------------------------------------------------------------------- Routes

/// A parent (`spec.parentRefs[]`) as an object and a label: `public`,
/// `infra/public`, `public/https` (a listener), `public:443`.
fn parent_ref(route_ns: Option<&str>, r: &Value) -> Option<(ObjectRef, String)> {
    let name = str_at(r, "/name")?;
    let group = str_at(r, "/group").unwrap_or(GROUP);
    let kind = str_at(r, "/kind").unwrap_or("Gateway");
    let ns = str_at(r, "/namespace");
    let mut label = match ns.filter(|ns| Some(*ns) != route_ns) {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_owned(),
    };
    if let Some(section) = str_at(r, "/sectionName") {
        label.push('/');
        label.push_str(section);
    }
    if let Some(port) = r.get("port").and_then(Value::as_i64) {
        label.push_str(&format!(":{port}"));
    }
    let api_version = if group.is_empty() {
        "v1".to_owned()
    } else if group == GROUP {
        API_VERSION.to_owned()
    } else {
        format!("{group}/v1")
    };
    Some((object_ref(&api_version, kind, name, ns.or(route_ns)), label))
}

fn parents(obj: &DynamicObject) -> Vec<(ObjectRef, String)> {
    let ns = obj.metadata.namespace.as_deref();
    arr_at(&obj.data, "/spec/parentRefs")
        .iter()
        .filter_map(|r| parent_ref(ns, r))
        .collect()
}

/// A backend (`backendRefs[]`, a Service by default) as an object, a label
/// (`api:8080`) and its weight.
fn backend_ref(route_ns: Option<&str>, b: &Value) -> Option<(ObjectRef, String, Option<i64>)> {
    let name = str_at(b, "/name")?;
    let group = str_at(b, "/group").unwrap_or_default();
    let kind = str_at(b, "/kind").unwrap_or("Service");
    let ns = str_at(b, "/namespace");
    let mut label = match ns.filter(|ns| Some(*ns) != route_ns) {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_owned(),
    };
    if kind != "Service" {
        label = format!("{kind} {label}");
    }
    if let Some(port) = b.get("port").and_then(Value::as_i64) {
        label.push_str(&format!(":{port}"));
    }
    let api_version = if group.is_empty() {
        "v1".to_owned()
    } else {
        format!("{group}/v1")
    };
    Some((
        object_ref(&api_version, kind, name, ns.or(route_ns)),
        label,
        b.get("weight").and_then(Value::as_i64),
    ))
}

fn rules(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/spec/rules")
}

fn backends(obj: &DynamicObject) -> Vec<(ObjectRef, String, Option<i64>)> {
    let ns = obj.metadata.namespace.as_deref();
    rules(obj)
        .iter()
        .flat_map(|r| arr_at(r, "/backendRefs"))
        .filter_map(|b| backend_ref(ns, b))
        .collect()
}

/// `PathPrefix /api GET header x-env=canary` (HTTPRoute),
/// `grpc.health.v1.Health/Check` (GRPCRoute).
fn match_text(m: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(path) = m.get("path") {
        parts.push(format!(
            "{} {}",
            str_at(path, "/type").unwrap_or("PathPrefix"),
            str_at(path, "/value").unwrap_or("/")
        ));
    }
    if let Some(method) = m.get("method") {
        match method {
            Value::String(verb) => parts.push(verb.clone()),
            grpc => parts.push(format!(
                "{}/{}",
                str_at(grpc, "/service").unwrap_or("*"),
                str_at(grpc, "/method").unwrap_or("*")
            )),
        }
    }
    for h in arr_at(m, "/headers") {
        parts.push(format!(
            "header {}={}",
            str_at(h, "/name").unwrap_or_default(),
            str_at(h, "/value").unwrap_or_default()
        ));
    }
    for q in arr_at(m, "/queryParams") {
        parts.push(format!(
            "query {}={}",
            str_at(q, "/name").unwrap_or_default(),
            str_at(q, "/value").unwrap_or_default()
        ));
    }
    parts.join(" ")
}

/// `PathPrefix /api → api:8080 (weight 90), api-v2:8080 (weight 10) ·
/// filters URLRewrite`.
fn rule_text(route_ns: Option<&str>, rule: &Value) -> String {
    let matches: Vec<String> = arr_at(rule, "/matches").iter().map(match_text).collect();
    let backends: Vec<String> = arr_at(rule, "/backendRefs")
        .iter()
        .filter_map(|b| backend_ref(route_ns, b))
        .map(|(_, label, weight)| match weight {
            Some(w) => format!("{label} (weight {w})"),
            None => label,
        })
        .collect();
    let mut text = format!(
        "{} → {}",
        if matches.is_empty() {
            "all requests".to_owned()
        } else {
            matches.join(" | ")
        },
        if backends.is_empty() {
            "no backend".to_owned()
        } else {
            backends.join(", ")
        }
    );
    let filters: Vec<&str> = arr_at(rule, "/filters")
        .iter()
        .filter_map(|f| str_at(f, "/type"))
        .collect();
    if !filters.is_empty() {
        text.push_str(&format!(" · filters {}", filters.join(", ")));
    }
    text
}

/// `status.parents[]` with the label of its parent and its conditions.
fn parent_statuses(obj: &DynamicObject) -> Vec<(String, Vec<ConditionInfo>)> {
    let ns = obj.metadata.namespace.as_deref();
    arr_at(&obj.data, "/status/parents")
        .iter()
        .map(|p| {
            let label = p
                .get("parentRef")
                .and_then(|r| parent_ref(ns, r))
                .map(|(_, label)| label)
                .unwrap_or_default();
            (
                label,
                conditions_of(obj.types.as_ref(), arr_at(p, "/conditions")),
            )
        })
        .collect()
}

fn route_health(obj: &DynamicObject) -> Health {
    if let Some(h) = terminating(obj) {
        return h;
    }
    let statuses = parent_statuses(obj);
    if statuses.is_empty() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-route-not-accepted"));
    }
    for (parent, conditions) in &statuses {
        if let Some(c) = conditions.iter().find(|c| c.is_good() == Some(false)) {
            return Health::new(
                HealthStatus::Degraded,
                format!("{parent}: {}", condition_text(c)),
            );
        }
    }
    let accepted = statuses
        .iter()
        .all(|(_, c)| find(c, "Accepted").is_some_and(|c| c.status == "True"));
    if accepted {
        Health::phrase(HealthStatus::Healthy, msg!("state-gateway-accepted"))
    } else {
        Health::phrase(
            HealthStatus::Progressing,
            msg!("state-route-waiting-gateways"),
        )
    }
}

fn hostnames(obj: &DynamicObject) -> Vec<String> {
    arr_at(&obj.data, "/spec/hostnames")
        .iter()
        .filter_map(|h| h.as_str().map(str::to_owned))
        .collect()
}

impl ResourcePlugin for RoutePlugin {
    fn name(&self) -> &'static str {
        "gateway-route"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        ROUTE_KINDS
            .iter()
            .map(|kind| GroupVersionKind::gvk(GROUP, "v1", kind))
            .collect()
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("parents", "Parents", ColumnKind::Text),
            col("hostnames", "Hostnames", ColumnKind::Text),
            col("backends", "Backends", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut backend_labels: Vec<String> = Vec::new();
        for (_, label, _) in backends(obj) {
            if !backend_labels.contains(&label) {
                backend_labels.push(label);
            }
        }
        let hosts = hostnames(obj);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(
                parents(obj)
                    .into_iter()
                    .map(|(_, label)| label)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            text(if hosts.is_empty() {
                "*".to_owned()
            } else {
                hosts.join(", ")
            }),
            text(backend_labels.join(", ")),
            Cell::Status(route_health(obj)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let ns = obj.metadata.namespace.as_deref();
        let mut fields: Vec<Field> = parents(obj)
            .into_iter()
            .map(|(target, label)| Field::object_link("Parent", label, target))
            .collect();
        let hosts = hostnames(obj);
        if !hosts.is_empty() {
            fields.push(Field::text("Hostnames", hosts.join(", ")));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        push_kv(
            &mut sections,
            "Rules",
            rules(obj)
                .iter()
                .enumerate()
                .map(|(i, rule)| {
                    let key = str_at(rule, "/name")
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Rule {}", i + 1));
                    (key, rule_text(ns, rule))
                })
                .collect(),
        );
        let mut seen: Vec<ObjectRef> = Vec::new();
        let backend_links: Vec<Field> = backends(obj)
            .into_iter()
            .filter(|(target, _, _)| {
                let new = !seen.contains(target);
                if new {
                    seen.push(target.clone());
                }
                new
            })
            .map(|(target, label, _)| Field::object_link(target.kind.clone(), label, target))
            .collect();
        if !backend_links.is_empty() {
            sections.push(section("Backends", SectionBody::Fields(backend_links)));
        }
        sections.extend(GenericPlugin.details(obj));
        for (parent, conditions) in parent_statuses(obj) {
            if !conditions.is_empty() {
                sections.push(section(
                    format!("Parent {parent}"),
                    SectionBody::Conditions(conditions),
                ));
            }
        }
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
        delete_action(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let extra = parents(obj)
            .into_iter()
            .map(|(target, _)| target)
            .chain(backends(obj).into_iter().map(|(target, _, _)| target))
            .map(references)
            .collect();
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        route_health(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::FieldValue;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    fn class(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "gateway.networking.k8s.io/v1", "kind": "GatewayClass",
            "metadata": {"name": "envoy"},
            "spec": {"controllerName": "gateway.envoyproxy.io/gatewayclass-controller",
                     "description": "Envoy Gateway"},
            "status": status
        }))
    }

    #[test]
    fn gateway_class() {
        let p = GatewayClassPlugin;
        let accepted = class(json!({"conditions": [
            {"type": "Accepted", "status": "True", "reason": "Accepted"}
        ]}));
        let cells = p.cells(&accepted);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[1],
            text("gateway.envoyproxy.io/gatewayclass-controller")
        );
        assert_eq!(
            cells[2],
            Cell::Status(Health::new(HealthStatus::Healthy, "Accepted"))
        );
        let rejected = class(json!({"conditions": [
            {"type": "Accepted", "status": "False", "reason": "InvalidParameters",
             "message": "parametersRef not found"}
        ]}));
        assert_eq!(
            p.health(&rejected),
            Health::new(HealthStatus::Degraded, "parametersRef not found")
        );
        assert_eq!(
            p.health(&class(json!({}))).status,
            HealthStatus::Progressing
        );
        assert_eq!(
            pairs(&p.details(&accepted), OVERVIEW)[1],
            pair("Description", "Envoy Gateway")
        );
    }

    fn gateway(listener_conditions: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "gateway.networking.k8s.io/v1", "kind": "Gateway",
            "metadata": {"name": "public", "namespace": "infra"},
            "spec": {
                "gatewayClassName": "envoy",
                "listeners": [
                    {"name": "http", "port": 80, "protocol": "HTTP",
                     "allowedRoutes": {"namespaces": {"from": "All"}}},
                    {"name": "https", "port": 443, "protocol": "HTTPS", "hostname": "*.example.com",
                     "tls": {"mode": "Terminate", "certificateRefs": [{"name": "wildcard-tls"}]}}
                ]
            },
            "status": {
                "addresses": [{"type": "IPAddress", "value": "203.0.113.10"}],
                "conditions": [
                    {"type": "Accepted", "status": "True"},
                    {"type": "Programmed", "status": "True"}
                ],
                "listeners": [
                    {"name": "http", "attachedRoutes": 2, "conditions": [
                        {"type": "Programmed", "status": "True"}]},
                    {"name": "https", "attachedRoutes": 1, "conditions": listener_conditions}
                ]
            }
        }))
    }

    #[test]
    fn gateway_cells_details_relations() {
        let p = GatewayPlugin;
        let o = gateway(json!([
            {"type": "Programmed", "status": "True"},
            {"type": "Conflicted", "status": "False"}
        ]));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("envoy"));
        assert_eq!(cells[3], text("203.0.113.10"));
        assert_eq!(cells[4], text("http:80/HTTP, https:443/HTTPS"));
        assert_eq!(
            p.health(&o),
            Health::new(HealthStatus::Healthy, "Programmed")
        );

        let sections = p.details(&o);
        let SectionBody::Fields(fields) = body(&sections, OVERVIEW) else {
            panic!("overview is not fields");
        };
        let FieldValue::Link { target, .. } = &fields[0].value else {
            panic!("class is not a link");
        };
        assert_eq!(
            (target.kind.as_str(), target.namespace.as_deref()),
            ("GatewayClass", None)
        );
        let listeners = sections.iter().find(|s| s.title == "Listeners").unwrap();
        assert_eq!(
            listeners.text_lines(),
            [
                "http: HTTP :80 · routes from All · 2 routes attached",
                "https: HTTPS :443 · host *.example.com · TLS Terminate · 1 routes attached",
            ]
        );
        let SectionBody::Conditions(https) = body(&sections, "Listener https") else {
            panic!("no listener conditions");
        };
        let conflicted = https.iter().find(|c| c.type_ == "Conflicted").unwrap();
        assert!(conflicted.negative);
        assert_eq!(conflicted.is_good(), Some(true));

        let targets: Vec<_> = p
            .relations(&o)
            .iter()
            .map(|r| format!("{}/{}", r.target.kind, r.target.name))
            .collect();
        assert_eq!(targets, ["GatewayClass/envoy", "Secret/wildcard-tls"]);

        let conflicting = gateway(json!([
            {"type": "Conflicted", "status": "True", "reason": "HostnameConflict"}
        ]));
        assert_eq!(
            p.health(&conflicting),
            Health::new(HealthStatus::Degraded, "Listener https: HostnameConflict")
        );
    }

    fn http_route(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "gateway.networking.k8s.io/v1", "kind": "HTTPRoute",
            "metadata": {"name": "shop", "namespace": "web"},
            "spec": {
                "parentRefs": [{"name": "public", "namespace": "infra", "sectionName": "https"}],
                "hostnames": ["shop.example.com"],
                "rules": [
                    {"matches": [{"path": {"type": "PathPrefix", "value": "/api"}, "method": "GET"}],
                     "backendRefs": [
                        {"name": "api", "port": 8080, "weight": 90},
                        {"name": "api-v2", "port": 8080, "weight": 10}],
                     "filters": [{"type": "URLRewrite"}]},
                    {"backendRefs": [{"name": "frontend", "port": 80}]}
                ]
            },
            "status": status
        }))
    }

    #[test]
    fn http_route_cells_details_relations() {
        let p = RoutePlugin;
        let o = http_route(json!({"parents": [{
            "parentRef": {"name": "public", "namespace": "infra", "sectionName": "https"},
            "controllerName": "gateway.envoyproxy.io/gatewayclass-controller",
            "conditions": [
                {"type": "Accepted", "status": "True"},
                {"type": "ResolvedRefs", "status": "True"}
            ]
        }]}));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("infra/public/https"));
        assert_eq!(cells[3], text("shop.example.com"));
        assert_eq!(cells[4], text("api:8080, api-v2:8080, frontend:80"));
        assert_eq!(p.health(&o), Health::new(HealthStatus::Healthy, "Accepted"));

        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Rules"),
            [
                pair(
                    "Rule 1",
                    "PathPrefix /api GET → api:8080 (weight 90), api-v2:8080 (weight 10) · filters URLRewrite"
                ),
                pair("Rule 2", "all requests → frontend:80"),
            ]
        );
        let SectionBody::Fields(backends) = body(&sections, "Backends") else {
            panic!("no backends");
        };
        let FieldValue::Link { target, .. } = &backends[0].value else {
            panic!("backend is not a link");
        };
        assert_eq!(
            (
                target.api_version.as_str(),
                target.kind.as_str(),
                target.namespace.as_deref()
            ),
            ("v1", "Service", Some("web"))
        );
        let SectionBody::Conditions(parent) = body(&sections, "Parent infra/public/https") else {
            panic!("no parent conditions");
        };
        assert!(parent.iter().all(|c| c.is_good() == Some(true)));

        let targets: Vec<_> = p
            .relations(&o)
            .iter()
            .map(|r| {
                format!(
                    "{}/{}/{}",
                    r.target.kind,
                    r.target.namespace.as_deref().unwrap_or_default(),
                    r.target.name
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                "Gateway/infra/public",
                "Service/web/api",
                "Service/web/api-v2",
                "Service/web/frontend"
            ]
        );
    }

    #[test]
    fn route_health_states() {
        let p = RoutePlugin;
        assert_eq!(
            p.health(&http_route(json!({}))).status,
            HealthStatus::Progressing
        );
        let unresolved = http_route(json!({"parents": [{
            "parentRef": {"name": "public", "namespace": "infra"},
            "conditions": [
                {"type": "Accepted", "status": "True"},
                {"type": "ResolvedRefs", "status": "False", "reason": "BackendNotFound",
                 "message": "service web/api-v2 not found"}
            ]
        }]}));
        assert_eq!(
            p.health(&unresolved),
            Health::new(
                HealthStatus::Degraded,
                "infra/public: service web/api-v2 not found"
            )
        );
    }

    #[test]
    fn grpc_route_matches() {
        let o = obj(json!({
            "apiVersion": "gateway.networking.k8s.io/v1", "kind": "GRPCRoute",
            "metadata": {"name": "health", "namespace": "web"},
            "spec": {
                "parentRefs": [{"name": "public"}],
                "rules": [{"matches": [{"method": {"service": "grpc.health.v1.Health", "method": "Check"}}],
                           "backendRefs": [{"name": "grpc", "port": 9000}]}]
            }
        }));
        let p = RoutePlugin;
        assert_eq!(p.cells(&o)[2], text("public"));
        assert_eq!(p.cells(&o)[3], text("*"));
        assert_eq!(
            pairs(&p.details(&o), "Rules"),
            [pair("Rule 1", "grpc.health.v1.Health/Check → grpc:9000")]
        );
    }

    #[test]
    fn registry_routes_every_route_kind() {
        let r = crate::plugin::PluginRegistry::with_builtins();
        for kind in ROUTE_KINDS {
            assert_eq!(r.plugin_for(GROUP, kind).name(), "gateway-route");
        }
        assert_eq!(r.plugin_for(GROUP, "Gateway").name(), "gateway");
        assert_eq!(r.plugin_for(GROUP, "GatewayClass").name(), "gatewayclass");
        assert_eq!(
            r.plugin_for(GROUP, "ReferenceGrant").name(),
            "referencegrant"
        );
    }
}
