//! Service and Ingress plugins.

use k8s_openapi::api::core::v1::Service;
use k8s_openapi::api::networking::v1::{Ingress, IngressBackend};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, PORT_FORWARD_ACTION, action, col, creation, decode, delete, fallback_cells,
    join_map, kv, map_pairs, name_ns_cells, name_ns_columns, or_none, pair, push_kv,
    relations_with, selects, text, unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection,
    GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation, RelationKind,
    ResourcePlugin, SelectorRelation,
};

pub struct ServicePlugin;
pub struct IngressPlugin;

// ------------------------------------------------------------------- Service

fn service_type(svc: &Service) -> String {
    svc.spec
        .as_ref()
        .and_then(|s| s.type_.clone())
        .unwrap_or_else(|| "ClusterIP".into())
}

fn lb_addresses(svc: &Service) -> Vec<String> {
    svc.status
        .as_ref()
        .and_then(|s| s.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|i| i.ip.clone().or_else(|| i.hostname.clone()))
        .collect()
}

/// `EXTERNAL-IP` column as kubectl prints it.
pub(crate) fn external_ip(svc: &Service) -> String {
    let spec = svc.spec.clone().unwrap_or_default();
    let external: Vec<String> = spec.external_ips.clone().unwrap_or_default();
    match service_type(svc).as_str() {
        "ExternalName" => or_none(spec.external_name.unwrap_or_default()),
        "LoadBalancer" => {
            let mut all = lb_addresses(svc);
            all.extend(external);
            if all.is_empty() {
                "<pending>".into()
            } else {
                all.join(",")
            }
        }
        _ => or_none(external.join(",")),
    }
}

/// `PORT(S)` column: `80/TCP,443:30443/TCP`.
pub(crate) fn service_ports(svc: &Service) -> String {
    let ports = svc
        .spec
        .as_ref()
        .and_then(|s| s.ports.as_ref())
        .into_iter()
        .flatten()
        .map(|p| {
            let proto = p.protocol.as_deref().unwrap_or("TCP");
            match p.node_port {
                Some(np) => format!("{}:{np}/{proto}", p.port),
                None => format!("{}/{proto}", p.port),
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    or_none(ports)
}

pub(crate) fn service_health(svc: &Service) -> Health {
    if service_type(svc) == "LoadBalancer" && lb_addresses(svc).is_empty() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-service-waiting-lb"));
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-active"))
}

impl ResourcePlugin for ServicePlugin {
    fn name(&self) -> &'static str {
        "service"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "Service")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("type", "Type", ColumnKind::Text),
            col("cluster_ip", "Cluster IP", ColumnKind::Text),
            col("external_ip", "External IP", ColumnKind::Text),
            col("ports", "Ports", ColumnKind::Text),
            col("selector", "Selector", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(svc) = decode::<Service>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = svc.spec.clone().unwrap_or_default();
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(service_type(&svc)),
            text(or_none(spec.cluster_ip.unwrap_or_default())),
            text(external_ip(&svc)),
            text(service_ports(&svc)),
            text(or_none(join_map(spec.selector.as_ref()))),
            Cell::Status(service_health(&svc)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(svc) = decode::<Service>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = svc.spec.clone().unwrap_or_default();
        let list = |v: Option<Vec<String>>| or_none(v.unwrap_or_default().join(", "));
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Type", service_type(&svc)),
                pair("Cluster IPs", list(spec.cluster_ips.clone())),
                pair("External IP", external_ip(&svc)),
                pair("IP families", list(spec.ip_families.clone())),
                pair(
                    "Session affinity",
                    spec.session_affinity
                        .clone()
                        .unwrap_or_else(|| "None".into()),
                ),
                pair(
                    "External traffic policy",
                    or_none(spec.external_traffic_policy.clone().unwrap_or_default()),
                ),
                pair(
                    "Internal traffic policy",
                    or_none(spec.internal_traffic_policy.clone().unwrap_or_default()),
                ),
            ],
        )];
        push_kv(
            &mut sections,
            "Ports",
            spec.ports
                .iter()
                .flatten()
                .map(|p| {
                    let target = p
                        .target_port
                        .as_ref()
                        .map(|t| match t {
                            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(i) => {
                                i.to_string()
                            }
                            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(
                                s,
                            ) => s.clone(),
                        })
                        .unwrap_or_else(|| p.port.to_string());
                    let proto = p.protocol.as_deref().unwrap_or("TCP");
                    let mut value = format!("{} → {target}/{proto}", p.port);
                    if let Some(np) = p.node_port {
                        value.push_str(&format!(" (node port {np})"));
                    }
                    (p.name.clone().unwrap_or_else(|| p.port.to_string()), value)
                })
                .collect(),
        );
        push_kv(&mut sections, "Selector", map_pairs(spec.selector.as_ref()));
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![PORT_FORWARD_ACTION, DELETE_ACTION]
    }

    /// Port forward needs pods behind a selector.
    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        if self.selector_relations(obj).is_empty() {
            vec![DELETE_ACTION]
        } else {
            self.actions()
        }
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            match *action_id {
                action::DELETE => delete::<Service>(client, obj).await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let selectorless_external =
            decode::<Service>(obj).is_some_and(|s| service_type(&s) == "ExternalName");
        let mut extra = Vec::new();
        if !selectorless_external && let Some(name) = &obj.metadata.name {
            extra.push(Relation {
                kind: RelationKind::References,
                target: ObjectRef {
                    api_version: "v1".into(),
                    kind: "Endpoints".into(),
                    name: name.clone(),
                    namespace: obj.metadata.namespace.clone(),
                },
            });
        }
        relations_with(obj, extra)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        decode::<Service>(obj)
            .and_then(|s| s.spec?.selector)
            .filter(|s| !s.is_empty())
            .map(|s| vec![selects(obj, "v1", "Pod", join_map(Some(&s)))])
            .unwrap_or_default()
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Service>(obj) {
            Some(svc) => service_health(&svc),
            None => GenericPlugin.health(obj),
        }
    }
}

// ------------------------------------------------------------------- Ingress

fn backend_text(b: &IngressBackend) -> String {
    if let Some(s) = &b.service {
        let port = s
            .port
            .as_ref()
            .and_then(|p| p.name.clone().or_else(|| p.number.map(|n| n.to_string())))
            .unwrap_or_default();
        return format!("{}:{port}", s.name);
    }
    if let Some(r) = &b.resource {
        return format!("{}/{}", r.kind, r.name);
    }
    String::new()
}

fn ingress_addresses(ing: &Ingress) -> Vec<String> {
    ing.status
        .as_ref()
        .and_then(|s| s.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|i| i.ip.clone().or_else(|| i.hostname.clone()))
        .collect()
}

fn ingress_hosts(ing: &Ingress) -> String {
    let hosts: Vec<String> = ing
        .spec
        .as_ref()
        .and_then(|s| s.rules.as_ref())
        .into_iter()
        .flatten()
        .map(|r| r.host.clone().unwrap_or_else(|| "*".into()))
        .collect();
    if hosts.is_empty() {
        "*".into()
    } else {
        hosts.join(",")
    }
}

fn ingress_class(ing: &Ingress) -> String {
    ing.spec
        .as_ref()
        .and_then(|s| s.ingress_class_name.clone())
        .or_else(|| {
            ing.metadata
                .annotations
                .as_ref()?
                .get("kubernetes.io/ingress.class")
                .cloned()
        })
        .unwrap_or_default()
}

fn has_tls(ing: &Ingress) -> bool {
    ing.spec
        .as_ref()
        .and_then(|s| s.tls.as_ref())
        .is_some_and(|t| !t.is_empty())
}

pub(crate) fn ingress_health(ing: &Ingress) -> Health {
    if ingress_addresses(ing).is_empty() {
        Health::phrase(
            HealthStatus::Progressing,
            msg!("state-ingress-waiting-address"),
        )
    } else {
        Health::phrase(HealthStatus::Healthy, msg!("state-active"))
    }
}

fn ingress_refs(ing: &Ingress) -> Vec<Relation> {
    let ns = ing.metadata.namespace.clone();
    let mut out: Vec<Relation> = Vec::new();
    let mut add = |api_version: &str, kind: &str, name: &str, namespace: Option<String>| {
        let r = Relation {
            kind: RelationKind::References,
            target: ObjectRef {
                api_version: api_version.into(),
                kind: kind.into(),
                name: name.into(),
                namespace,
            },
        };
        if !out.contains(&r) {
            out.push(r);
        }
    };
    let Some(spec) = &ing.spec else {
        return out;
    };
    if let Some(class) = &spec.ingress_class_name {
        add("networking.k8s.io/v1", "IngressClass", class, None);
    }
    let backends = spec.default_backend.iter().chain(
        spec.rules
            .iter()
            .flatten()
            .filter_map(|r| r.http.as_ref())
            .flat_map(|h| h.paths.iter().map(|p| &p.backend)),
    );
    for b in backends {
        if let Some(s) = &b.service {
            add("v1", "Service", &s.name, ns.clone());
        }
    }
    for t in spec.tls.iter().flatten() {
        if let Some(secret) = &t.secret_name {
            add("v1", "Secret", secret, ns.clone());
        }
    }
    out
}

impl ResourcePlugin for IngressPlugin {
    fn name(&self) -> &'static str {
        "ingress"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("class", "Class", ColumnKind::Text),
            col("hosts", "Hosts", ColumnKind::Text),
            col("address", "Address", ColumnKind::Text),
            col("ports", "Ports", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(ing) = decode::<Ingress>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(or_none(ingress_class(&ing))),
            text(ingress_hosts(&ing)),
            text(ingress_addresses(&ing).join(",")),
            text(if has_tls(&ing) { "80, 443" } else { "80" }),
            Cell::Status(ingress_health(&ing)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(ing) = decode::<Ingress>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = ing.spec.clone().unwrap_or_default();
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Class", or_none(ingress_class(&ing))),
                pair("Address", or_none(ingress_addresses(&ing).join(", "))),
                pair(
                    "Default backend",
                    or_none(
                        spec.default_backend
                            .as_ref()
                            .map(backend_text)
                            .unwrap_or_default(),
                    ),
                ),
            ],
        )];
        let mut rules = Vec::new();
        for rule in spec.rules.iter().flatten() {
            let host = rule.host.clone().unwrap_or_else(|| "*".into());
            for p in rule.http.iter().flat_map(|h| &h.paths) {
                let path = p.path.clone().unwrap_or_else(|| "/".into());
                rules.push(pair(
                    format!("{host}{path}"),
                    format!("{} ({})", backend_text(&p.backend), p.path_type),
                ));
            }
        }
        push_kv(&mut sections, "Rules", rules);
        push_kv(
            &mut sections,
            "TLS",
            spec.tls
                .iter()
                .flatten()
                .map(|t| {
                    pair(
                        or_none(t.hosts.clone().unwrap_or_default().join(", ")),
                        or_none(t.secret_name.clone().unwrap_or_default()),
                    )
                })
                .collect(),
        );
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
        Box::pin(async move {
            match *action_id {
                action::DELETE => delete::<Ingress>(client, obj).await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        match decode::<Ingress>(obj) {
            Some(ing) => relations_with(obj, ingress_refs(&ing)),
            None => GenericPlugin.relations(obj),
        }
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Ingress>(obj) {
            Some(ing) => ingress_health(&ing),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs};

    fn service(spec: Value, status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": spec, "status": status
        }))
    }

    #[test]
    fn cluster_ip_service() {
        let o = service(
            json!({
                "type": "ClusterIP", "clusterIP": "10.96.0.10", "clusterIPs": ["10.96.0.10"],
                "selector": {"app": "web"},
                "ports": [{"name": "http", "port": 80, "targetPort": 8080, "protocol": "TCP"}]
            }),
            json!({}),
        );
        let p = ServicePlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("ClusterIP"));
        assert_eq!(cells[3], text("10.96.0.10"));
        assert_eq!(cells[4], text("<none>"));
        assert_eq!(cells[5], text("80/TCP"));
        assert_eq!(cells[6], text("app=web"));
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);

        let sel = p.selector_relations(&o);
        assert_eq!(sel[0].target_kind, "Pod");
        assert_eq!(sel[0].label_selector, "app=web");
        assert_eq!(p.relations(&o)[0].target.kind, "Endpoints");
        assert_eq!(
            pairs(&p.details(&o), "Ports"),
            [pair("http", "80 → 8080/TCP")]
        );
        let ids: Vec<_> = p.actions_for(&o).iter().map(|a| a.id).collect();
        assert_eq!(ids, ["port-forward", "delete"]);
    }

    #[test]
    fn load_balancer_service() {
        let spec = json!({
            "type": "LoadBalancer", "clusterIP": "10.96.0.11",
            "ports": [{"port": 443, "nodePort": 30443, "protocol": "TCP"}]
        });
        let p = ServicePlugin;
        let pending = service(spec.clone(), json!({}));
        let cells = p.cells(&pending);
        assert_eq!(cells[4], text("<pending>"));
        assert_eq!(cells[5], text("443:30443/TCP"));
        assert_eq!(p.health(&pending).status, HealthStatus::Progressing);
        assert!(p.selector_relations(&pending).is_empty());
        let ids: Vec<_> = p.actions_for(&pending).iter().map(|a| a.id).collect();
        assert_eq!(ids, ["delete"]);

        let ready = service(
            spec,
            json!({"loadBalancer": {"ingress": [{"ip": "203.0.113.7"}]}}),
        );
        assert_eq!(p.cells(&ready)[4], text("203.0.113.7"));
        assert_eq!(p.health(&ready).status, HealthStatus::Healthy);
    }

    #[test]
    fn external_name_service() {
        let o = service(
            json!({"type": "ExternalName", "externalName": "db.example.com"}),
            json!({}),
        );
        assert_eq!(ServicePlugin.cells(&o)[4], text("db.example.com"));
        assert!(ServicePlugin.relations(&o).is_empty());
    }

    fn ingress(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": {
                "ingressClassName": "nginx",
                "tls": [{"hosts": ["web.example.com"], "secretName": "web-tls"}],
                "rules": [{
                    "host": "web.example.com",
                    "http": {"paths": [
                        {"path": "/", "pathType": "Prefix",
                         "backend": {"service": {"name": "web", "port": {"number": 80}}}},
                        {"path": "/api", "pathType": "Prefix",
                         "backend": {"service": {"name": "api", "port": {"name": "http"}}}}
                    ]}
                }]
            },
            "status": status
        }))
    }

    #[test]
    fn ingress_cells_details_relations() {
        let p = IngressPlugin;
        let o = ingress(json!({"loadBalancer": {"ingress": [{"hostname": "lb.example.com"}]}}));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("nginx"));
        assert_eq!(cells[3], text("web.example.com"));
        assert_eq!(cells[4], text("lb.example.com"));
        assert_eq!(cells[5], text("80, 443"));
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
        assert_eq!(
            p.health(&ingress(json!({}))).status,
            HealthStatus::Progressing
        );

        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Rules"),
            [
                pair("web.example.com/", "web:80 (Prefix)"),
                pair("web.example.com/api", "api:http (Prefix)"),
            ]
        );
        assert_eq!(
            pairs(&sections, "TLS"),
            [pair("web.example.com", "web-tls")]
        );

        let targets: Vec<_> = p
            .relations(&o)
            .iter()
            .map(|r| format!("{}/{}", r.target.kind, r.target.name))
            .collect();
        assert_eq!(
            targets,
            [
                "IngressClass/nginx",
                "Service/web",
                "Service/api",
                "Secret/web-tls"
            ]
        );
    }
}
