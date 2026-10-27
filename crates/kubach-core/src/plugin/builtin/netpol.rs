//! NetworkPolicy plugin: the pods it isolates, policy types and its ingress
//! and egress rules in words.

use k8s_openapi::api::networking::v1::{NetworkPolicy, NetworkPolicyPeer, NetworkPolicyPort};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, col, creation, decode, fallback_cells, int_or_string, map_pairs,
    name_ns_cells, name_ns_columns, push_kv, run_delete, section, selector_string, selects, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, PluginError, RelatedFilter, RelatedObjects,
    RelatedOrder, Relation, ResourcePlugin, SectionBody, SelectorRelation,
};

pub struct NetworkPolicyPlugin;

/// `all pods` for an empty selector, kubectl syntax otherwise.
fn pods_text(selector: &k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector) -> String {
    selector_string(selector).unwrap_or_else(|| "all pods".into())
}

/// `pods app=web in namespaces team=a`, `10.0.0.0/8 except 10.1.0.0/16`.
fn peer_text(peer: &NetworkPolicyPeer) -> String {
    if let Some(block) = &peer.ip_block {
        let except = block.except.clone().unwrap_or_default();
        return if except.is_empty() {
            block.cidr.clone()
        } else {
            format!("{} except {}", block.cidr, except.join(", "))
        };
    }
    let namespaces = peer.namespace_selector.as_ref().map(|s| {
        selector_string(s)
            .map(|s| format!("namespaces {s}"))
            .unwrap_or_else(|| "all namespaces".into())
    });
    match (&peer.pod_selector, namespaces) {
        (Some(pods), Some(ns)) => format!("{} in {ns}", pods_text(pods)),
        (Some(pods), None) => format!("{} (this namespace)", pods_text(pods)),
        (None, Some(ns)) => format!("pods in {ns}"),
        (None, None) => "anywhere".into(),
    }
}

/// `TCP 80`, `TCP 8000-8080`, `UDP` (any port).
fn port_text(port: &NetworkPolicyPort) -> String {
    let protocol = port.protocol.as_deref().unwrap_or("TCP");
    match (&port.port, port.end_port) {
        (Some(p), Some(end)) => format!("{protocol} {}-{end}", int_or_string(p)),
        (Some(p), None) => format!("{protocol} {}", int_or_string(p)),
        (None, _) => format!("{protocol} any port"),
    }
}

/// `from pods app=web (this namespace); 10.0.0.0/8 · ports TCP 80, TCP 443`.
fn rule_text(
    direction: &str,
    peers: Option<&Vec<NetworkPolicyPeer>>,
    ports: Option<&Vec<NetworkPolicyPort>>,
) -> String {
    let peers: Vec<String> = peers.into_iter().flatten().map(peer_text).collect();
    let ports: Vec<String> = ports.into_iter().flatten().map(port_text).collect();
    let who = if peers.is_empty() {
        format!("{direction} anywhere")
    } else {
        format!("{direction} {}", peers.join("; "))
    };
    if ports.is_empty() {
        format!("{who} · all ports")
    } else {
        format!("{who} · ports {}", ports.join(", "))
    }
}

/// The directions the policy restricts: `policyTypes`, or what the
/// API server defaults (Ingress, plus Egress when egress rules exist).
fn policy_types(np: &NetworkPolicy) -> Vec<String> {
    let spec = np.spec.clone().unwrap_or_default();
    if let Some(types) = spec.policy_types.filter(|t| !t.is_empty()) {
        return types;
    }
    let mut types = vec!["Ingress".to_owned()];
    if spec.egress.is_some() {
        types.push("Egress".into());
    }
    types
}

/// `2 rules`, `deny all` (restricted, no rule), `-` (not restricted).
fn rules_summary(restricted: bool, count: usize) -> String {
    match (restricted, count) {
        (false, _) => "-".into(),
        (true, 0) => "deny all".into(),
        (true, 1) => "1 rule".into(),
        (true, n) => format!("{n} rules"),
    }
}

impl ResourcePlugin for NetworkPolicyPlugin {
    fn name(&self) -> &'static str {
        "networkpolicy"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            "networking.k8s.io",
            "v1",
            "NetworkPolicy",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("pod_selector", "Pod selector", ColumnKind::Text),
            col("policy_types", "Policy types", ColumnKind::Text),
            col("ingress", "Ingress", ColumnKind::Text),
            col("egress", "Egress", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(np) = decode::<NetworkPolicy>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = np.spec.clone().unwrap_or_default();
        let types = policy_types(&np);
        let restricts = |t: &str| types.iter().any(|x| x == t);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(pods_text(&spec.pod_selector.clone().unwrap_or_default())),
            text(types.join(", ")),
            text(rules_summary(
                restricts("Ingress"),
                spec.ingress.as_ref().map_or(0, Vec::len),
            )),
            text(rules_summary(
                restricts("Egress"),
                spec.egress.as_ref().map_or(0, Vec::len),
            )),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(np) = decode::<NetworkPolicy>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = np.spec.clone().unwrap_or_default();
        let selector = spec.pod_selector.clone().unwrap_or_default();
        let labels = map_pairs(selector.match_labels.as_ref());
        let types = policy_types(&np);
        let pod_field = if labels.is_empty() || selector.match_expressions.is_some() {
            Field::text("Pod selector", pods_text(&selector))
        } else {
            Field::new("Pod selector", FieldValue::Labels(labels))
        };
        let mut sections = vec![section(
            OVERVIEW,
            SectionBody::Fields(vec![
                pod_field,
                Field::text("Policy types", types.join(", ")),
            ]),
        )];
        let numbered = |rules: Vec<String>| -> Vec<(String, String)> {
            rules
                .into_iter()
                .enumerate()
                .map(|(i, r)| (format!("Rule {}", i + 1), r))
                .collect()
        };
        let restricts = |t: &str| types.iter().any(|x| x == t);
        let mut ingress = numbered(
            spec.ingress
                .iter()
                .flatten()
                .map(|r| rule_text("from", r.from.as_ref(), r.ports.as_ref()))
                .collect(),
        );
        if restricts("Ingress") && ingress.is_empty() {
            ingress.push(("Rules".into(), "No rule: all ingress is denied".into()));
        }
        let mut egress = numbered(
            spec.egress
                .iter()
                .flatten()
                .map(|r| rule_text("to", r.to.as_ref(), r.ports.as_ref()))
                .collect(),
        );
        if restricts("Egress") && egress.is_empty() {
            egress.push(("Rules".into(), "No rule: all egress is denied".into()));
        }
        push_kv(&mut sections, "Ingress rules", ingress);
        push_kv(&mut sections, "Egress rules", egress);
        sections.push(section(
            "Pods",
            SectionBody::Related(RelatedObjects {
                api_version: "v1".into(),
                kind: "Pod".into(),
                namespace: obj.metadata.namespace.clone(),
                filter: RelatedFilter::Selector {
                    labels: selector_string(&selector),
                    owner_uid: None,
                },
                order: RelatedOrder::Name,
            }),
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
        run_delete::<NetworkPolicy>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        decode::<NetworkPolicy>(obj)
            .and_then(|np| np.spec?.pod_selector)
            .and_then(|s| selector_string(&s))
            .map(|labels| vec![selects(obj, "v1", "Pod", labels)])
            .unwrap_or_default()
    }

    /// A policy is enforced by the network plugin and has no status.
    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::phrase(HealthStatus::Healthy, msg!("state-active"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::pair;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    #[test]
    fn rules_in_words() {
        let p = NetworkPolicyPlugin;
        let o = obj(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": {"name": "api", "namespace": "default"},
            "spec": {
                "podSelector": {"matchLabels": {"app": "api"}},
                "policyTypes": ["Ingress", "Egress"],
                "ingress": [{
                    "from": [
                        {"podSelector": {"matchLabels": {"app": "web"}}},
                        {"namespaceSelector": {"matchLabels": {"team": "ops"}},
                         "podSelector": {}},
                        {"ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.1.0.0/16"]}}
                    ],
                    "ports": [{"protocol": "TCP", "port": 80}, {"port": 8000, "endPort": 8080}]
                }]
            }
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("app=api"));
        assert_eq!(cells[3], text("Ingress, Egress"));
        assert_eq!(cells[4], text("1 rule"));
        assert_eq!(cells[5], text("deny all"));
        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, "Ingress rules"),
            [pair(
                "Rule 1",
                "from app=web (this namespace); all pods in namespaces team=ops; \
                 10.0.0.0/8 except 10.1.0.0/16 · ports TCP 80, TCP 8000-8080"
            )]
        );
        assert_eq!(
            pairs(&sections, "Egress rules"),
            [pair("Rules", "No rule: all egress is denied")]
        );
        let SectionBody::Related(pods) = body(&sections, "Pods") else {
            panic!("no pods");
        };
        assert_eq!(pods.labels(), Some("app=api"));
        assert_eq!(p.selector_relations(&o)[0].label_selector, "app=api");
    }

    #[test]
    fn empty_selector_and_default_types() {
        let p = NetworkPolicyPlugin;
        let o = obj(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": {"name": "allow-dns", "namespace": "default"},
            "spec": {"podSelector": {}, "egress": [{"ports": [{"protocol": "UDP", "port": 53}]}]}
        }));
        let cells = p.cells(&o);
        assert_eq!(cells[2], text("all pods"));
        assert_eq!(cells[3], text("Ingress, Egress"));
        assert_eq!(cells[4], text("deny all"));
        assert_eq!(cells[5], text("1 rule"));
        assert_eq!(
            pairs(&p.details(&o), "Egress rules"),
            [pair("Rule 1", "to anywhere · ports UDP 53")]
        );
        assert!(p.selector_relations(&o).is_empty());
    }
}
