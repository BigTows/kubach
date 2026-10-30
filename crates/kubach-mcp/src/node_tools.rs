//! Nodes for the agent (#61, #62): `list_nodes`, `describe_node` and
//! `top_nodes`, read-only.
//!
//! Nodes are cluster-scoped, and a session's own identity is confined to
//! namespaces (ADR 0005): a temporary ServiceAccount has a Role, never a
//! ClusterRole. So once the user allowed nodes (the `nodes` access), these
//! calls read with the user's credentials, and only through the fixed
//! requests below: get and list Node, events of Nodes, Pods on one node,
//! eviction events, NodeMetrics. Nothing else gets the user's client.
//!
//! Pods and evictions on a node are named only in namespaces the session
//! may read; elsewhere they are counted.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use k8s_openapi::api::core::v1::{Event, Node, Pod};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::jiff::Timestamp;
use kubach_core::discovery;
use kubach_core::metrics::{MetricsApi, MetricsError, QuantityError, cpu_nanocores, memory_bytes};
use kube::api::{Api, DynamicObject, ListParams};
use kube::{Client, ResourceExt as _};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::grants::Grant;
use crate::policy::Request;
use crate::tools::{Ctx, MAX_LIST_ITEMS, ToolError, ToolOutput, authorize, finish, parse};

/// Node events and evictions shown at most.
const MAX_EVENTS: usize = 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NodeArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoArgs {}

/// The user's client, after the policy allowed nodes.
async fn nodes_client(ctx: &Ctx<'_>, name: Option<String>) -> Result<Client, ToolError> {
    authorize(ctx, &Request::Nodes { name }).await?;
    Ok(ctx.session.user_client())
}

fn node_api(ctx: &Ctx<'_>, client: &Client) -> Result<Api<DynamicObject>, ToolError> {
    let kind = ctx
        .session
        .catalog()
        .find("", "Node")
        .ok_or_else(|| ToolError::Failed("the cluster serves no Node kind".into()))?;
    Ok(discovery::api(client, kind, None))
}

/// `list_nodes`: the node plugin's table.
pub(crate) async fn list_nodes(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let _: NoArgs = parse(args)?;
    let client = nodes_client(ctx, None).await?;
    let api = node_api(ctx, &client)?;
    let list = api
        .list(&ListParams::default().limit(MAX_LIST_ITEMS))
        .await?;
    let kind = ctx
        .session
        .catalog()
        .find("", "Node")
        .expect("checked above");
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    let now = Timestamp::now();
    let mut out = plugin
        .columns()
        .iter()
        .map(|c| c.title.to_uppercase())
        .collect::<Vec<_>>()
        .join("\t");
    out.push_str("\tPROBLEMS\n");
    for obj in &list.items {
        let cells: Vec<String> = plugin
            .cells(obj)
            .iter()
            .map(|cell| crate::tools::render_cell(cell, now))
            .collect();
        let problems = decode(obj).map(|node| problems(&node)).unwrap_or_default();
        let _ = writeln!(
            out,
            "{}\t{}",
            cells.join("\t"),
            if problems.is_empty() {
                "-".to_owned()
            } else {
                problems.join("; ")
            }
        );
    }
    let _ = write!(
        out,
        "({} nodes; read with the user's credentials. describe_node shows one with events and \
         evictions, top_nodes their usage)",
        list.items.len()
    );
    Ok(ToolOutput::read(finish(&out)))
}

fn decode(obj: &DynamicObject) -> Option<Node> {
    serde_json::to_value(obj)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Conditions that are off: not Ready, or a pressure that is on.
pub(crate) fn problems(node: &Node) -> Vec<String> {
    let mut out = Vec::new();
    for condition in node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .into_iter()
        .flatten()
    {
        let bad = if condition.type_ == "Ready" {
            condition.status != "True"
        } else {
            condition.status == "True"
        };
        if bad {
            let reason = condition
                .reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            out.push(format!("{}={}{reason}", condition.type_, condition.status));
        }
    }
    if node
        .spec
        .as_ref()
        .and_then(|s| s.unschedulable)
        .unwrap_or(false)
    {
        out.push("cordoned".into());
    }
    out
}

/// `describe_node`: the node's details, conditions with messages, what
/// its pods request, its events and evictions from it.
pub(crate) async fn describe_node(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: NodeArgs = parse(args)?;
    let client = nodes_client(ctx, Some(a.name.clone())).await?;
    let api = node_api(ctx, &client)?;
    let obj = api.get(&a.name).await?;
    let kind = ctx
        .session
        .catalog()
        .find("", "Node")
        .expect("checked above");
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    let health = plugin.health(&obj);
    let mut out = format!(
        "Node {}\nHealth: {:?}{}\n",
        obj.name_any(),
        health.status,
        if health.message.is_empty() {
            String::new()
        } else {
            format!(" ({})", health.message)
        }
    );
    let node = decode(&obj);
    if let Some(node) = &node {
        out.push_str("\nConditions:\n");
        for condition in node
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .into_iter()
            .flatten()
        {
            let _ = writeln!(
                out,
                "  {} {} {}{}",
                condition.type_,
                condition.status,
                condition.reason.as_deref().unwrap_or(""),
                condition
                    .message
                    .as_deref()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            );
        }
    }
    for section in plugin.details(&obj) {
        if section.title == "Conditions" {
            continue;
        }
        let _ = writeln!(out, "\n{}:", section.title);
        for line in section.text_lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    let pods = Api::<Pod>::all(client.clone())
        .list(&ListParams::default().fields(&format!("spec.nodeName={}", a.name)))
        .await;
    match pods {
        Ok(pods) => out.push_str(&pods_text(
            ctx,
            &pods.items,
            node.as_ref()
                .and_then(|n| n.status.as_ref()?.allocatable.clone()),
        )),
        Err(error) => {
            let _ = writeln!(
                out,
                "\nPods: not readable ({})",
                ToolError::from(error).message()
            );
        }
    }
    out.push_str(&node_events(&client, &a.name).await);
    out.push_str(&evictions(ctx, &client, &a.name).await);
    Ok(ToolOutput::read(finish(&out)))
}

/// Requests and limits of the node's pods against its allocatable, like
/// `kubectl describe node`, and the pods not running; named only in the
/// namespaces the session may read.
fn pods_text(ctx: &Ctx<'_>, pods: &[Pod], allocatable: Option<Quantities>) -> String {
    let (mut cpu, mut memory, mut cpu_limit, mut memory_limit) = (0u64, 0u64, 0u64, 0u64);
    let mut by_namespace: BTreeMap<String, usize> = BTreeMap::new();
    let mut unhealthy = Vec::new();
    let mut hidden_unhealthy = 0;
    for pod in pods {
        let phase = pod
            .status
            .as_ref()
            .and_then(|s| s.phase.as_deref())
            .unwrap_or("Unknown");
        if matches!(phase, "Succeeded" | "Failed") {
            continue;
        }
        let namespace = pod.namespace().unwrap_or_default();
        *by_namespace.entry(namespace.clone()).or_default() += 1;
        for container in pod.spec.iter().flat_map(|s| &s.containers) {
            let Some(resources) = &container.resources else {
                continue;
            };
            cpu += amount(resources.requests.as_ref(), "cpu", cpu_nanocores).unwrap_or(0);
            memory += amount(resources.requests.as_ref(), "memory", memory_bytes).unwrap_or(0);
            cpu_limit += amount(resources.limits.as_ref(), "cpu", cpu_nanocores).unwrap_or(0);
            memory_limit += amount(resources.limits.as_ref(), "memory", memory_bytes).unwrap_or(0);
        }
        if phase != "Running" {
            if readable(ctx, &namespace) {
                unhealthy.push(format!("{namespace}/{} {phase}", pod.name_any()));
            } else {
                hidden_unhealthy += 1;
            }
        }
    }
    let percent = |used: u64, of: Option<u64>| match of {
        Some(total) if total > 0 => format!(" ({}%)", used * 100 / total),
        _ => String::new(),
    };
    let alloc_cpu = amount(allocatable.as_ref(), "cpu", cpu_nanocores);
    let alloc_memory = amount(allocatable.as_ref(), "memory", memory_bytes);
    let mut out = format!(
        "\nAllocated (pods' requests / limits against allocatable):\n  cpu {}{} / {}{}\n  memory \
         {}{} / {}{}\n",
        cores(cpu),
        percent(cpu, alloc_cpu),
        cores(cpu_limit),
        percent(cpu_limit, alloc_cpu),
        mib(memory),
        percent(memory, alloc_memory),
        mib(memory_limit),
        percent(memory_limit, alloc_memory),
    );
    let total: usize = by_namespace.values().sum();
    let _ = writeln!(
        out,
        "\nPods: {total} ({})",
        by_namespace
            .iter()
            .map(|(ns, n)| format!("{ns} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for line in unhealthy.iter().take(MAX_EVENTS) {
        let _ = writeln!(out, "  not running: {line}");
    }
    if hidden_unhealthy > 0 {
        let _ = writeln!(
            out,
            "  not running in namespaces this session does not read: {hidden_unhealthy}"
        );
    }
    out
}

type Quantities = BTreeMap<String, Quantity>;

/// `key` of `map` (requests, limits, allocatable) parsed by `parse`.
fn amount(
    map: Option<&Quantities>,
    key: &str,
    parse: fn(&str) -> Result<u64, QuantityError>,
) -> Option<u64> {
    map.and_then(|m| m.get(key)).and_then(|q| parse(&q.0).ok())
}

fn cores(nanocores: u64) -> String {
    format!("{}m", nanocores / 1_000_000)
}

fn mib(bytes: u64) -> String {
    format!("{}Mi", bytes / (1024 * 1024))
}

/// Whether the session may read `namespace`: pinned to it, or the user
/// allowed reading it.
fn readable(ctx: &Ctx<'_>, namespace: &str) -> bool {
    let scope = ctx.session.scope();
    if scope.pinned() {
        return scope.namespaces.iter().any(|n| n == namespace);
    }
    ctx.session
        .grants()
        .is_granted(&Grant::read(namespace.to_owned()))
}

/// The node's own events, newest first.
async fn node_events(client: &Client, node: &str) -> String {
    let params = ListParams::default().fields(&format!(
        "involvedObject.kind=Node,involvedObject.name={node}"
    ));
    let mut out = String::from("\nEvents:\n");
    match Api::<Event>::all(client.clone()).list(&params).await {
        Ok(list) => {
            let mut events = list.items;
            sort_newest_first(&mut events);
            if events.is_empty() {
                out.push_str("  (none)\n");
            }
            for event in events.iter().take(MAX_EVENTS) {
                let _ = writeln!(out, "  {}", event_line(event));
            }
        }
        Err(error) => {
            let _ = writeln!(out, "  not readable: {}", ToolError::from(error).message());
        }
    }
    out
}

/// Pods evicted from the node (kubelet `Evicted` events: memory, disk or
/// PID pressure), newest first.
async fn evictions(ctx: &Ctx<'_>, client: &Client, node: &str) -> String {
    let params = ListParams::default().fields("reason=Evicted");
    let mut out = String::from("\nEvictions:\n");
    let list = match Api::<Event>::all(client.clone()).list(&params).await {
        Ok(list) => list.items,
        Err(error) => {
            let _ = writeln!(out, "  not readable: {}", ToolError::from(error).message());
            return out;
        }
    };
    let mut mine: Vec<Event> = list
        .into_iter()
        .filter(|event| on_node(event, node))
        .collect();
    sort_newest_first(&mut mine);
    let (shown, hidden): (Vec<&Event>, Vec<&Event>) = mine.iter().partition(|event| {
        readable(
            ctx,
            event
                .involved_object
                .namespace
                .as_deref()
                .unwrap_or_default(),
        )
    });
    if mine.is_empty() {
        out.push_str("  (none in the events the API server still keeps, about an hour)\n");
    }
    for event in shown.iter().take(MAX_EVENTS) {
        let _ = writeln!(
            out,
            "  {}/{}: {}",
            event
                .involved_object
                .namespace
                .as_deref()
                .unwrap_or_default(),
            event.involved_object.name.as_deref().unwrap_or_default(),
            event_line(event)
        );
    }
    if !hidden.is_empty() {
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for event in &hidden {
            *reasons.entry(pressure(event)).or_default() += 1;
        }
        let _ = writeln!(
            out,
            "  in namespaces this session does not read: {}",
            reasons
                .iter()
                .map(|(reason, n)| format!("{n} ({reason})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out
}

/// Whether the kubelet of `node` reported `event`.
pub(crate) fn on_node(event: &Event, node: &str) -> bool {
    event.source.as_ref().and_then(|s| s.host.as_deref()) == Some(node)
        || event.reporting_instance.as_deref() == Some(node)
}

/// The resource an eviction message names: "The node was low on
/// resource: memory. …" → `memory`.
pub(crate) fn pressure(event: &Event) -> String {
    let message = event.message.as_deref().unwrap_or_default();
    message
        .split_once("low on resource: ")
        .map(|(_, rest)| {
            rest.split(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|resource| !resource.is_empty())
        .unwrap_or_else(|| "other".into())
}

fn last_seen(event: &Event) -> Option<Timestamp> {
    event
        .last_timestamp
        .as_ref()
        .map(|t| t.0)
        .or_else(|| event.event_time.as_ref().map(|t| t.0))
        .or_else(|| event.metadata.creation_timestamp.as_ref().map(|t| t.0))
}

fn sort_newest_first(events: &mut [Event]) {
    events.sort_by_key(|event| std::cmp::Reverse(last_seen(event)));
}

fn event_line(event: &Event) -> String {
    let count = event
        .count
        .filter(|n| *n > 1)
        .map(|n| format!(" x{n}"))
        .unwrap_or_default();
    format!(
        "{} {} {}{count}: {}",
        last_seen(event).map(|t| t.to_string()).unwrap_or_default(),
        event.type_.as_deref().unwrap_or(""),
        event.reason.as_deref().unwrap_or(""),
        event.message.as_deref().unwrap_or("").trim()
    )
}

/// `top_nodes`: CPU and memory used per node, from metrics-server, against
/// what the node can give to pods.
pub(crate) async fn top_nodes(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let _: NoArgs = parse(args)?;
    let client = nodes_client(ctx, None).await?;
    let metrics = MetricsApi::new(&client, ctx.session.catalog()).map_err(metrics_error)?;
    let usage = metrics.nodes().await.map_err(metrics_error)?;
    let nodes = Api::<Node>::all(client)
        .list(&ListParams::default().limit(MAX_LIST_ITEMS))
        .await?;
    let allocatable: BTreeMap<String, (Option<u64>, Option<u64>)> = nodes
        .items
        .iter()
        .map(|node| {
            let alloc = node.status.as_ref().and_then(|s| s.allocatable.as_ref());
            (
                node.name_any(),
                (
                    amount(alloc, "cpu", cpu_nanocores),
                    amount(alloc, "memory", memory_bytes),
                ),
            )
        })
        .collect();
    let mut out = String::from("NODE\tCPU\tCPU%\tMEMORY\tMEMORY%\n");
    for node in &usage {
        let (cpu, memory) = allocatable.get(&node.name).copied().unwrap_or_default();
        let percent = |used: u64, of: Option<u64>| match of {
            Some(total) if total > 0 => format!("{}%", used * 100 / total),
            _ => "-".into(),
        };
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            node.name,
            cores(node.usage.cpu),
            percent(node.usage.cpu, cpu),
            mib(node.usage.memory),
            percent(node.usage.memory, memory),
        );
    }
    out.push_str("(% of allocatable; read with the user's credentials)");
    Ok(ToolOutput::read(finish(&out)))
}

fn metrics_error(error: MetricsError) -> ToolError {
    match error {
        MetricsError::NotInstalled => {
            ToolError::Failed("metrics-server is not installed in this cluster".into())
        }
        other => ToolError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Event, Node};
    use serde_json::json;

    use super::{on_node, pressure, problems};

    #[test]
    fn a_nodes_problems_are_its_off_conditions() {
        let node: Node = serde_json::from_value(json!({
            "metadata": {"name": "n1"},
            "spec": {"unschedulable": true},
            "status": {"conditions": [
                {"type": "MemoryPressure", "status": "True", "reason": "KubeletHasInsufficientMemory"},
                {"type": "DiskPressure", "status": "False"},
                {"type": "Ready", "status": "False", "reason": "KubeletNotReady"},
            ]},
        }))
        .unwrap();
        assert_eq!(
            problems(&node),
            [
                "MemoryPressure=True (KubeletHasInsufficientMemory)",
                "Ready=False (KubeletNotReady)",
                "cordoned",
            ]
        );
    }

    #[test]
    fn evictions_are_matched_to_their_node_and_resource() {
        let event: Event = serde_json::from_value(json!({
            "metadata": {"name": "e", "namespace": "ns"},
            "involvedObject": {"kind": "Pod", "name": "web-1", "namespace": "ns"},
            "reason": "Evicted",
            "message": "The node was low on resource: memory. Threshold quantity: 100Mi, available: 50Mi.",
            "source": {"component": "kubelet", "host": "n1"},
        }))
        .unwrap();
        assert!(on_node(&event, "n1"));
        assert!(!on_node(&event, "n2"));
        assert_eq!(pressure(&event), "memory");
        let other: Event = serde_json::from_value(json!({
            "metadata": {"name": "e2"},
            "involvedObject": {},
            "reportingInstance": "n2",
            "message": "something else",
        }))
        .unwrap();
        assert!(on_node(&other, "n2"));
        assert_eq!(pressure(&other), "other");
    }
}
