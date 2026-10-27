//! Node plugin: status, roles, capacity, taints and cordon/uncordon.

use k8s_openapi::api::core::v1::Node;
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind, Patch};
use serde_json::{Value, json};

use super::{
    action, col, creation, decode, fallback_cells, kv, or_none, pair, patch, push_kv, text, unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection,
    GenericPlugin, Health, HealthStatus, PluginError, Relation, ResourcePlugin,
};

pub struct NodePlugin;

const CORDON_ACTION: Action = Action::api(action::CORDON, "Cordon", false);

const UNCORDON_ACTION: Action = Action::api(action::UNCORDON, "Uncordon", false);

/// Node conditions that report a problem when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 4] = [
    "MemoryPressure",
    "DiskPressure",
    "PIDPressure",
    "NetworkUnavailable",
];

/// Strategic merge patch for `kubectl cordon` / `uncordon` (uncordon removes
/// the field, as kubectl does).
pub(crate) fn cordon_patch(cordon: bool) -> Value {
    if cordon {
        json!({"spec": {"unschedulable": true}})
    } else {
        json!({"spec": {"unschedulable": null}})
    }
}

fn unschedulable(node: &Node) -> bool {
    node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true)
}

fn ready_condition(node: &Node) -> Option<&k8s_openapi::api::core::v1::NodeCondition> {
    node.status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .find(|c| c.type_ == "Ready")
}

/// `STATUS` column: `Ready`, `NotReady`, `Unknown`, plus `,SchedulingDisabled`.
pub(crate) fn node_status(node: &Node) -> String {
    let mut s = match ready_condition(node).map(|c| c.status.as_str()) {
        Some("True") => "Ready".to_owned(),
        Some("False") => "NotReady".to_owned(),
        _ => "Unknown".to_owned(),
    };
    if unschedulable(node) {
        s.push_str(",SchedulingDisabled");
    }
    s
}

/// Roles from `node-role.kubernetes.io/<role>` and `kubernetes.io/role` labels.
pub(crate) fn node_roles(node: &Node) -> String {
    let mut roles: Vec<String> = Vec::new();
    for (k, v) in node.metadata.labels.iter().flatten() {
        if let Some(role) = k.strip_prefix("node-role.kubernetes.io/") {
            if !role.is_empty() {
                roles.push(role.to_owned());
            }
        } else if k == "kubernetes.io/role" && !v.is_empty() {
            roles.push(v.clone());
        }
    }
    roles.sort();
    roles.dedup();
    or_none(roles.join(","))
}

pub(crate) fn node_health(node: &Node) -> Health {
    let Some(ready) = ready_condition(node) else {
        return Health::unknown();
    };
    if ready.status != "True" {
        let msg = ready
            .message
            .clone()
            .or_else(|| ready.reason.clone())
            .unwrap_or_else(|| "Not ready".into());
        return Health::new(HealthStatus::Failed, msg);
    }
    let pressure: Vec<&str> = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .into_iter()
        .flatten()
        .filter(|c| NEGATIVE_CONDITIONS.contains(&c.type_.as_str()) && c.status == "True")
        .map(|c| c.type_.as_str())
        .collect();
    if !pressure.is_empty() {
        return Health::new(HealthStatus::Degraded, pressure.join(", "));
    }
    if unschedulable(node) {
        return Health::phrase(
            HealthStatus::Healthy,
            msg!("state-node-scheduling-disabled"),
        );
    }
    Health::phrase(HealthStatus::Healthy, msg!("state-ready"))
}

fn address(node: &Node, type_: &str) -> String {
    node.status
        .as_ref()
        .and_then(|s| s.addresses.as_ref())
        .into_iter()
        .flatten()
        .find(|a| a.type_ == type_)
        .map(|a| a.address.clone())
        .unwrap_or_default()
}

fn quantities(
    map: Option<
        &std::collections::BTreeMap<
            String,
            k8s_openapi::apimachinery::pkg::api::resource::Quantity,
        >,
    >,
) -> Vec<(String, String)> {
    map.into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.0.clone()))
        .collect()
}

impl ResourcePlugin for NodePlugin {
    fn name(&self) -> &'static str {
        "node"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "Node")]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("roles", "Roles", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
            col("version", "Version", ColumnKind::Text),
            col("internal_ip", "Internal IP", ColumnKind::Text),
            col("runtime", "Container runtime", ColumnKind::Text),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(node) = decode::<Node>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let info = node.status.as_ref().and_then(|s| s.node_info.as_ref());
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            Cell::Status(Health::new(node_health(&node).status, node_status(&node))),
            text(node_roles(&node)),
            Cell::Age(creation(obj)),
            text(info.map(|i| i.kubelet_version.clone()).unwrap_or_default()),
            text(or_none(address(&node, "InternalIP"))),
            text(
                info.map(|i| i.container_runtime_version.clone())
                    .unwrap_or_default(),
            ),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(node) = decode::<Node>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = node.spec.clone().unwrap_or_default();
        let status = node.status.clone().unwrap_or_default();
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Status", node_status(&node)),
                pair("Roles", node_roles(&node)),
                pair("Unschedulable", unschedulable(&node).to_string()),
                pair(
                    "Pod CIDR",
                    or_none(spec.pod_cidr.clone().unwrap_or_default()),
                ),
                pair(
                    "Provider ID",
                    or_none(spec.provider_id.clone().unwrap_or_default()),
                ),
            ],
        )];
        push_kv(
            &mut sections,
            "Addresses",
            status
                .addresses
                .iter()
                .flatten()
                .map(|a| pair(a.type_.clone(), a.address.clone()))
                .collect(),
        );
        if let Some(i) = &status.node_info {
            sections.push(kv(
                "System info",
                vec![
                    pair("OS image", i.os_image.clone()),
                    pair("Kernel", i.kernel_version.clone()),
                    pair("Architecture", i.architecture.clone()),
                    pair("Container runtime", i.container_runtime_version.clone()),
                    pair("Kubelet", i.kubelet_version.clone()),
                ],
            ));
        }
        push_kv(
            &mut sections,
            "Capacity",
            quantities(status.capacity.as_ref()),
        );
        push_kv(
            &mut sections,
            "Allocatable",
            quantities(status.allocatable.as_ref()),
        );
        push_kv(
            &mut sections,
            "Taints",
            spec.taints
                .iter()
                .flatten()
                .map(|t| {
                    let value = t
                        .value
                        .as_deref()
                        .map(|v| format!("={v}"))
                        .unwrap_or_default();
                    pair(format!("{}{value}", t.key), t.effect.clone())
                })
                .collect(),
        );
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![CORDON_ACTION, UNCORDON_ACTION]
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        let cordoned = obj
            .data
            .pointer("/spec/unschedulable")
            .and_then(Value::as_bool)
            == Some(true);
        let toggle = if cordoned {
            UNCORDON_ACTION
        } else {
            CORDON_ACTION
        };
        vec![toggle]
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
                action::CORDON | action::UNCORDON => {
                    let p = cordon_patch(*action_id == action::CORDON);
                    patch::<Node>(client, obj, &Patch::Strategic(p)).await
                }
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Node>(obj) {
            Some(node) => node_health(&node),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs};

    fn node(unschedulable: bool, conditions: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Node",
            "metadata": {
                "name": "minikube",
                "creationTimestamp": "2026-10-01T00:00:00Z",
                "labels": {
                    "node-role.kubernetes.io/control-plane": "",
                    "kubernetes.io/hostname": "minikube"
                }
            },
            "spec": {
                "unschedulable": unschedulable,
                "podCIDR": "10.244.0.0/24",
                "taints": [{"key": "dedicated", "value": "gpu", "effect": "NoSchedule"}]
            },
            "status": {
                "conditions": conditions,
                "addresses": [{"type": "InternalIP", "address": "192.168.49.2"}],
                "capacity": {"cpu": "4", "memory": "8Gi"},
                "allocatable": {"cpu": "3800m", "memory": "7Gi"},
                "nodeInfo": {
                    "machineID": "", "systemUUID": "", "bootID": "",
                    "kernelVersion": "6.6", "osImage": "Ubuntu 22.04",
                    "containerRuntimeVersion": "docker://27.0", "kubeletVersion": "v1.34.0",
                    "kubeProxyVersion": "", "operatingSystem": "linux", "architecture": "arm64"
                }
            }
        }))
    }

    fn ready() -> Value {
        json!([{"type": "Ready", "status": "True"}, {"type": "MemoryPressure", "status": "False"}])
    }

    #[test]
    fn node_cells() {
        let p = NodePlugin;
        let o = node(false, ready());
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[1],
            Cell::Status(Health::new(HealthStatus::Healthy, "Ready"))
        );
        assert_eq!(cells[2], text("control-plane"));
        assert_eq!(cells[4], text("v1.34.0"));
        assert_eq!(cells[5], text("192.168.49.2"));
        assert_eq!(cells[6], text("docker://27.0"));

        let cordoned = p.cells(&node(true, ready()));
        assert_eq!(
            cordoned[1],
            Cell::Status(Health::new(
                HealthStatus::Healthy,
                "Ready,SchedulingDisabled"
            ))
        );
    }

    #[test]
    fn node_health_states() {
        let p = NodePlugin;
        assert_eq!(
            p.health(&node(false, ready())).status,
            HealthStatus::Healthy
        );
        let pressure = p.health(&node(
            false,
            json!([{"type": "Ready", "status": "True"}, {"type": "DiskPressure", "status": "True"}]),
        ));
        assert_eq!(
            pressure,
            Health::new(HealthStatus::Degraded, "DiskPressure")
        );
        let not_ready = p.health(&node(
            false,
            json!([{"type": "Ready", "status": "Unknown", "reason": "NodeStatusUnknown",
                    "message": "Kubelet stopped posting node status."}]),
        ));
        assert_eq!(not_ready.status, HealthStatus::Failed);
        assert_eq!(
            p.cells(&node(false, json!([{"type": "Ready", "status": "False"}])))[1],
            Cell::Status(Health::new(HealthStatus::Failed, "NotReady"))
        );
    }

    #[test]
    fn node_details() {
        let sections = NodePlugin.details(&node(false, ready()));
        assert_eq!(
            pairs(&sections, "Taints"),
            [pair("dedicated=gpu", "NoSchedule")]
        );
        assert!(pairs(&sections, "Capacity").contains(&pair("cpu", "4")));
        assert!(pairs(&sections, "Allocatable").contains(&pair("cpu", "3800m")));
        assert!(pairs(&sections, "System info").contains(&pair("OS image", "Ubuntu 22.04")));
        assert!(pairs(&sections, "Addresses").contains(&pair("InternalIP", "192.168.49.2")));
    }

    #[test]
    fn cordon_actions_and_patches() {
        let p = NodePlugin;
        let ids = |o: &DynamicObject| p.actions_for(o).iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(ids(&node(false, ready())), ["cordon"]);
        assert_eq!(ids(&node(true, ready())), ["uncordon"]);
        assert_eq!(cordon_patch(true), json!({"spec": {"unschedulable": true}}));
        assert_eq!(
            cordon_patch(false),
            json!({"spec": {"unschedulable": null}})
        );
    }
}
