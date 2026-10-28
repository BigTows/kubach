//! Topology tools (#327, ADR 0016): `get_topology` and `can_reach`.
//!
//! Read-only and confined like every read: the namespaces pass the policy
//! ([`Request::Topology`]) and need the session's read right each. The
//! graph is built from one list per readable kind in those namespaces,
//! through the session's own client (not the UI cache): kinds the policy or
//! the API refuses are left out and reported, so their nodes show as
//! `unknown` instead of missing. Secrets and ConfigMaps are listed by
//! metadata only; values never enter the graph.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use futures::future::join_all;
use k8s_openapi::api::core::v1::Namespace;
use kubach_core::discovery;
use kubach_core::topology::netpol::{Explanation, PodGroup, PolicySet, Target, Verdict};
use kubach_core::topology::{
    Access, BuildOptions, Detail, EdgeKind, Node, NodeId, NodeKind, PortRef, Presence, Protocol,
    RefVia, ServiceType, SourceKind, TopologyGraph, TopologyInput, build,
};
use kube::api::{Api, DynamicObject, ListParams, TypeMeta};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::policy::{Allowed, Denial, DenialCode, KindRef, Request};

use crate::tools::{Ctx, MAX_OUTPUT_BYTES, ToolError, ToolOutput, authorize, parse};

/// Most objects listed per kind and namespace.
const MAX_OBJECTS: u32 = 2000;
/// Most steps around a resource.
const MAX_DEPTH: usize = 10;

const SCOPE_NOTE: &str = "Analysis of NetworkPolicy objects (networking.k8s.io/v1); whether \
                          the cluster's CNI enforces them is not known. Cilium/Calico CRDs are \
                          not read.";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceArg {
    kind: String,
    name: String,
    namespace: Option<String>,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DetailArg {
    #[default]
    Workloads,
    Pods,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopologyArgs {
    namespace: Option<String>,
    #[serde(default)]
    namespaces: Vec<String>,
    resource: Option<ResourceArg>,
    depth: Option<usize>,
    #[serde(default)]
    detail: DetailArg,
    network_policies: Option<bool>,
}

/// One end of a `can_reach` question: an object or an address.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointArg {
    kind: Option<String>,
    name: Option<String>,
    namespace: Option<String>,
    address: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PortArg {
    Number(i32),
    Name(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReachArgs {
    from: EndpointArg,
    to: EndpointArg,
    port: PortArg,
    protocol: Option<String>,
}

fn invalid(message: impl Into<String>) -> ToolError {
    Denial::new(DenialCode::InvalidInput, message).into()
}

pub(crate) async fn get_topology(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: TopologyArgs = parse(args)?;
    let mut namespaces = a.namespaces.clone();
    if let Some(namespace) = &a.namespace {
        namespaces.insert(0, namespace.clone());
    }
    if let Some(resource) = &a.resource {
        if !plain_name(&resource.kind) || !plain_name(&resource.name) {
            return Err(invalid("resource kind and name must be plain names"));
        }
        if let Some(namespace) = &resource.namespace
            && !namespaces.contains(namespace)
        {
            namespaces.push(namespace.clone());
        }
    }
    if a.depth.is_some_and(|depth| depth == 0 || depth > MAX_DEPTH) {
        return Err(invalid(format!("depth must be 1..={MAX_DEPTH}")));
    }
    let allowed = authorize(ctx, &Request::Topology { namespaces }).await?;
    let policies = a.network_policies.unwrap_or(true);
    let detail = match a.detail {
        DetailArg::Workloads => Detail::Workloads,
        DetailArg::Pods => Detail::Pods,
    };
    let loaded = load(ctx, &allowed, policies).await;
    let mut graph = build(
        &loaded.input,
        &BuildOptions {
            detail,
            network_policies: policies,
            ..BuildOptions::default()
        },
    );
    let mut full = None;
    if let Some(resource) = &a.resource {
        let namespace = resource
            .namespace
            .clone()
            .unwrap_or_else(|| allowed.namespace.clone());
        let Some(seed) = find_node(&graph, &resource.kind, &namespace, &resource.name) else {
            return Err(ToolError::Failed(not_found(
                &resource.kind,
                &namespace,
                &resource.name,
                detail,
            )));
        };
        let keep = graph.neighborhood_within([&seed], a.depth);
        full = Some(graph.clone());
        graph = graph.retain(|node| keep.contains(&node.id));
    }
    // Messages name targets cut away by `resource` too.
    let described = full.as_ref().unwrap_or(&graph);
    let out = json!({
        "namespaces": allowed.namespaces,
        "detail": match detail { Detail::Workloads => "workloads", Detail::Pods => "pods" },
        "networkPolicies": policies,
        "nodes": graph.nodes().iter().map(node_json).collect::<Vec<_>>(),
        "edges": graph.edges().iter().map(edge_json).collect::<Vec<_>>(),
        "diagnostics": graph.diagnostics().iter().map(|d| json!({
            "severity": match d.severity() {
                kubach_core::topology::Severity::Warning => "warning",
                kubach_core::topology::Severity::Error => "error",
            },
            "node": d.node.as_str(),
            "target": d.target.as_ref().map(NodeId::as_str),
            "message": d.message(described),
        })).collect::<Vec<_>>(),
        "unavailable": loaded.unavailable,
        "truncated": loaded.truncated,
        "note": if policies { Some(SCOPE_NOTE) } else { None },
    });
    let text = serde_json::to_string(&strip_nulls(out)).unwrap_or_default();
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(ToolError::Failed(format!(
            "the topology has {} nodes and {} edges, too large to return; pass `resource` \
             (with `depth`) or fewer namespaces",
            graph.nodes().len(),
            graph.edges().len()
        )));
    }
    Ok(ToolOutput::read(text))
}

pub(crate) async fn can_reach(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: ReachArgs = parse(args)?;
    let protocol = match a.protocol.as_deref() {
        None => Protocol::Tcp,
        Some(text) => Protocol::parse(text)
            .ok_or_else(|| invalid(format!("protocol {text:?} is not TCP, UDP or SCTP")))?,
    };
    let port = match a.port {
        PortArg::Number(number) if (1..=65535).contains(&number) => PortRef::Number(number),
        PortArg::Number(number) => return Err(invalid(format!("port {number} is out of range"))),
        PortArg::Name(name) if plain_name(&name) && name.len() <= 15 => match name.parse::<i32>() {
            Ok(number) if (1..=65535).contains(&number) => PortRef::Number(number),
            _ => PortRef::Name(name),
        },
        PortArg::Name(name) => return Err(invalid(format!("{name:?} is not a port"))),
    };
    let mut from = endpoint(&a.from)?;
    let mut to = endpoint(&a.to)?;
    let mut namespaces = Vec::new();
    for end in [&mut from, &mut to] {
        if let End::Object { namespace, .. } = end {
            let resolved = match namespace.take() {
                Some(namespace) => namespace,
                None => ctx.session.policy().namespace(None)?,
            };
            namespaces.push(resolved.clone());
            *namespace = Some(resolved);
        }
    }
    if namespaces.is_empty() {
        return Err(invalid("at least one end must be a workload or a pod"));
    }
    let allowed = authorize(ctx, &Request::Topology { namespaces }).await?;
    let pods = matches!(&from, End::Object { kind, .. } if is_pod(kind))
        || matches!(&to, End::Object { kind, .. } if is_pod(kind));
    let detail = if pods {
        Detail::Pods
    } else {
        Detail::Workloads
    };
    let loaded = load(ctx, &allowed, true).await;
    let graph = build(
        &loaded.input,
        &BuildOptions {
            detail,
            network_policies: true,
            ..BuildOptions::default()
        },
    );
    let policies = PolicySet::from_input(&loaded.input);
    let source = resolve(&graph, &from, &allowed.namespace, detail, None)?;
    let targets = resolve(&graph, &to, &allowed.namespace, detail, Some(&port))?;
    let Some((source_label, source, _)) = source.into_iter().next() else {
        return Err(ToolError::Failed("the source has no pods to check".into()));
    };
    let verdicts: Vec<(String, Verdict)> = targets
        .into_iter()
        .map(|(label, target, service_port)| {
            let port = service_port.unwrap_or_else(|| port.clone());
            let verdict = policies.can_reach(&source, &target, &port, protocol);
            (label, verdict)
        })
        .collect();
    if verdicts.is_empty() {
        return Err(ToolError::Failed(
            "the target selects no workloads or pods to check".into(),
        ));
    }
    let out = json!({
        "from": source_label,
        "to": end_label(&to, &allowed.namespace),
        "port": port.to_string(),
        "protocol": protocol.as_str(),
        "allowed": verdicts.iter().all(|(_, verdict)| verdict.allowed),
        "verdicts": verdicts.iter().map(|(label, verdict)| json!({
            "target": label,
            "allowed": verdict.allowed,
            "port": verdict.port,
            "egress": explanation_json(&verdict.egress_side),
            "ingress": explanation_json(&verdict.ingress_side),
        })).collect::<Vec<_>>(),
        "unavailable": loaded.unavailable,
        "note": SCOPE_NOTE,
    });
    Ok(ToolOutput::read(
        serde_json::to_string(&strip_nulls(out)).unwrap_or_default(),
    ))
}

// ------------------------------------------------------------- loading

struct Loaded {
    input: TopologyInput,
    /// `{kind, namespace, reason}` of kinds left out.
    unavailable: Vec<Value>,
    /// `{kind, namespace}` of lists cut at [`MAX_OBJECTS`].
    truncated: Vec<Value>,
}

enum Fetched {
    Complete(Vec<Arc<DynamicObject>>),
    Partial(Vec<Arc<DynamicObject>>),
    Unavailable(String),
}

/// One list per kind and namespace, all at once.
async fn load(ctx: &Ctx<'_>, allowed: &Allowed, policies: bool) -> Loaded {
    let policy = ctx.session.policy();
    let secrets = ctx.session.access().secrets;
    let mut kinds: Vec<SourceKind> = SourceKind::ALL.to_vec();
    if policies {
        kinds.push(SourceKind::NetworkPolicy);
    }
    let mut unavailable = Vec::new();
    let mut jobs = Vec::new();
    for kind in kinds {
        let resolved = ctx
            .session
            .catalog()
            .find(kind.group(), kind.kind())
            .cloned();
        let Some(resolved) = resolved else {
            // Not served (no Gateway API CRDs): nothing to say.
            continue;
        };
        let readable = if kind == SourceKind::Secret {
            if secrets {
                Ok(())
            } else {
                Err("Secrets need the user's approval first (list_secrets asks)".to_owned())
            }
        } else {
            policy
                .read_kind(&KindRef {
                    kind: resolved.api.kind.clone(),
                    api_version: Some(resolved.api.api_version.clone()),
                })
                .map(|_| ())
                .map_err(|denial| denial.message)
        };
        if let Err(reason) = readable {
            unavailable.push(json!({"kind": kind.kind(), "reason": reason}));
            continue;
        }
        for namespace in &allowed.namespaces {
            // Each namespace under its own identity (#401).
            let client = match ctx.session.client_in(namespace) {
                Ok(client) => client,
                Err(error) => {
                    unavailable.push(json!({
                        "kind": kind.kind(),
                        "namespace": namespace,
                        "reason": error.to_string(),
                    }));
                    continue;
                }
            };
            let api = discovery::api(&client, &resolved, Some(namespace));
            let types = TypeMeta {
                api_version: resolved.api.api_version.clone(),
                kind: resolved.api.kind.clone(),
            };
            let namespace = namespace.clone();
            jobs.push(async move {
                let params = ListParams::default().limit(MAX_OBJECTS);
                let metadata_only = kind.metadata_only();
                let result = if metadata_only {
                    api.list_metadata(&params).await.map(|list| {
                        let more = list
                            .metadata
                            .continue_
                            .as_deref()
                            .is_some_and(|c| !c.is_empty());
                        let items: Vec<Arc<DynamicObject>> = list
                            .items
                            .into_iter()
                            .map(|item| {
                                Arc::new(DynamicObject {
                                    types: Some(types.clone()),
                                    metadata: item.metadata,
                                    data: json!({}),
                                })
                            })
                            .collect();
                        (items, more)
                    })
                } else {
                    api.list(&params).await.map(|list| {
                        let more = list
                            .metadata
                            .continue_
                            .as_deref()
                            .is_some_and(|c| !c.is_empty());
                        let items: Vec<Arc<DynamicObject>> = list
                            .items
                            .into_iter()
                            .map(|mut item| {
                                item.types.get_or_insert_with(|| types.clone());
                                Arc::new(item)
                            })
                            .collect();
                        (items, more)
                    })
                };
                let fetched = match result {
                    Ok((items, false)) => Fetched::Complete(only_in(items, &namespace)),
                    Ok((items, true)) => Fetched::Partial(only_in(items, &namespace)),
                    Err(error) => Fetched::Unavailable(ToolError::from(error).message()),
                };
                (kind, namespace, fetched)
            });
        }
    }
    let mut namespace_objects = Vec::new();
    if policies {
        // Labels of the session's own namespaces, for namespaceSelectors;
        // without them only `kubernetes.io/metadata.name` matches.
        for namespace in &allowed.namespaces {
            let Ok(client) = ctx.session.client_in(namespace) else {
                continue;
            };
            let api: Api<Namespace> = Api::all(client);
            if let Ok(object) = api.get(namespace).await {
                namespace_objects.push(Arc::new(DynamicObject {
                    types: Some(TypeMeta {
                        api_version: "v1".into(),
                        kind: "Namespace".into(),
                    }),
                    metadata: object.metadata,
                    data: json!({}),
                }));
            }
        }
    }
    let results = join_all(jobs).await;
    let mut input = TopologyInput::new();
    let mut truncated = Vec::new();
    let mut by_kind: BTreeMap<SourceKind, (Vec<Arc<DynamicObject>>, bool)> = BTreeMap::new();
    for (kind, namespace, fetched) in results {
        let entry = by_kind.entry(kind).or_insert_with(|| (Vec::new(), true));
        match fetched {
            Fetched::Complete(items) => entry.0.extend(items),
            Fetched::Partial(items) => {
                entry.0.extend(items);
                entry.1 = false;
                truncated.push(json!({"kind": kind.kind(), "namespace": namespace}));
            }
            Fetched::Unavailable(reason) => {
                entry.1 = false;
                unavailable
                    .push(json!({"kind": kind.kind(), "namespace": namespace, "reason": reason}));
            }
        }
    }
    for (kind, (objects, complete)) in by_kind {
        if complete {
            input.set(kind, objects);
        } else {
            input.set_partial(kind, objects);
        }
    }
    if !namespace_objects.is_empty() {
        input.set_partial(SourceKind::Namespace, namespace_objects);
    }
    Loaded {
        input,
        unavailable,
        truncated,
    }
}

/// The API answered for the namespace asked; anything else is dropped.
fn only_in(items: Vec<Arc<DynamicObject>>, namespace: &str) -> Vec<Arc<DynamicObject>> {
    items
        .into_iter()
        .filter(|item| item.metadata.namespace.as_deref() == Some(namespace))
        .collect()
}

// ------------------------------------------------------------- lookup

/// A DNS-label-ish name: no slashes, no spaces, bounded.
fn plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 253
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

fn is_pod(kind: &str) -> bool {
    matches!(kind.to_ascii_lowercase().as_str(), "pod" | "pods" | "po")
}

/// Whether `kind` (as the agent names it: `Deployment`, `deployments`,
/// `svc`) names the node's kind.
fn kind_matches(node: &Node, kind: &str) -> bool {
    let wanted = kind.to_ascii_lowercase();
    let names = [node.api_kind.as_str(), node.kind.label()];
    names.iter().filter(|name| !name.is_empty()).any(|name| {
        let lower = name.to_ascii_lowercase();
        wanted == lower || wanted == format!("{lower}s")
    }) || matches!(
        (wanted.as_str(), node.kind),
        ("svc", NodeKind::Service(_))
            | ("po", NodeKind::Pod)
            | ("deploy", NodeKind::Workload(_))
            | ("sts", NodeKind::Workload(_))
            | ("ds", NodeKind::Workload(_))
            | ("cm", NodeKind::ConfigMap)
            | ("pvc", NodeKind::PersistentVolumeClaim)
            | ("sa", NodeKind::ServiceAccount)
            | ("ing", NodeKind::Ingress)
    ) && short_kind_ok(&wanted, node)
}

fn short_kind_ok(wanted: &str, node: &Node) -> bool {
    use kubach_core::topology::WorkloadKind;
    match (wanted, node.kind) {
        ("deploy", NodeKind::Workload(kind)) => kind == WorkloadKind::Deployment,
        ("sts", NodeKind::Workload(kind)) => kind == WorkloadKind::StatefulSet,
        ("ds", NodeKind::Workload(kind)) => kind == WorkloadKind::DaemonSet,
        _ => true,
    }
}

fn find_node(graph: &TopologyGraph, kind: &str, namespace: &str, name: &str) -> Option<NodeId> {
    graph
        .nodes()
        .iter()
        .find(|node| {
            node.name == name
                && node.namespace.as_deref() == Some(namespace)
                && kind_matches(node, kind)
        })
        .map(|node| node.id.clone())
}

fn not_found(kind: &str, namespace: &str, name: &str, detail: Detail) -> String {
    let mut text = format!("{kind} {namespace}/{name} is not on the topology graph");
    if detail == Detail::Workloads && is_pod(kind) {
        text.push_str("; pods are collapsed into their workload, pass detail=pods");
    }
    text
}

enum End {
    Object {
        kind: String,
        name: String,
        namespace: Option<String>,
    },
    Address(IpAddr),
}

fn endpoint(arg: &EndpointArg) -> Result<End, ToolError> {
    match (&arg.kind, &arg.name, &arg.address) {
        (Some(kind), Some(name), None) => {
            if !plain_name(kind) || !plain_name(name) {
                return Err(invalid("kind and name must be plain names"));
            }
            Ok(End::Object {
                kind: kind.clone(),
                name: name.clone(),
                namespace: arg.namespace.clone(),
            })
        }
        (None, None, Some(address)) if arg.namespace.is_none() => address
            .parse()
            .map(End::Address)
            .map_err(|_| invalid(format!("{address:?} is not an IP address"))),
        _ => Err(invalid(
            "each end is {kind, name, namespace?} of a workload, pod or Service, or {address} \
             of an IP outside the cluster",
        )),
    }
}

fn end_label(end: &End, default: &str) -> String {
    match end {
        End::Object {
            kind,
            name,
            namespace,
        } => format!("{kind} {}/{name}", namespace.as_deref().unwrap_or(default)),
        End::Address(address) => address.to_string(),
    }
}

/// Pod groups an end stands for, with a label and, for a Service target
/// (`port` is set for targets), the target port its port maps to. A
/// Service as the source is refused: traffic comes from pods.
fn resolve(
    graph: &TopologyGraph,
    end: &End,
    default: &str,
    detail: Detail,
    port: Option<&PortRef>,
) -> Result<Vec<(String, Target, Option<PortRef>)>, ToolError> {
    let target = port.is_some();
    let (kind, name, namespace) = match end {
        End::Address(address) => {
            return Ok(vec![(address.to_string(), Target::Address(*address), None)]);
        }
        End::Object {
            kind,
            name,
            namespace,
        } => (kind, name, namespace.as_deref().unwrap_or(default)),
    };
    let Some(id) = find_node(graph, kind, namespace, name) else {
        return Err(ToolError::Failed(not_found(kind, namespace, name, detail)));
    };
    let node = graph.node(&id).expect("found above");
    if node.presence != Presence::Present {
        return Err(ToolError::Failed(format!(
            "{} does not exist",
            node.describe()
        )));
    }
    if let Some(pods) = PodGroup::of(node) {
        return Ok(vec![(node.describe(), Target::Pods(pods), None)]);
    }
    if matches!(node.kind, NodeKind::Service(_)) && target {
        // The Service port names or numbers the pods' port.
        let mapped = node.attrs.service.as_ref().and_then(|service| {
            service
                .ports
                .iter()
                .find(|p| match port {
                    Some(PortRef::Number(number)) => p.port == *number,
                    Some(PortRef::Name(name)) => p.name.as_deref() == Some(name.as_str()),
                    None => false,
                })
                .map(|p| p.target_port.clone().unwrap_or(PortRef::Number(p.port)))
        });
        let mut out = Vec::new();
        for edge in graph
            .edges_from(&id)
            .filter(|edge| edge.kind == EdgeKind::Selects)
        {
            let Some(backend) = graph.node(&edge.to) else {
                continue;
            };
            let Some(pods) = PodGroup::of(backend) else {
                continue;
            };
            out.push((backend.describe(), Target::Pods(pods), mapped.clone()));
        }
        return Ok(out);
    }
    Err(invalid(format!(
        "{} is not a workload or pod{}",
        node.describe(),
        if target { " or Service" } else { "" }
    )))
}

// ------------------------------------------------------------- JSON

fn node_json(node: &Node) -> Value {
    let a = &node.attrs;
    let kind = if node.api_kind.is_empty() {
        node.kind.label().to_owned()
    } else {
        node.api_kind.clone()
    };
    let service = a.service.as_ref().map(|service| {
        json!({
            "type": match node.kind {
                NodeKind::Service(ServiceType::Headless) => "Headless",
                NodeKind::Service(ServiceType::NodePort) => "NodePort",
                NodeKind::Service(ServiceType::LoadBalancer) => "LoadBalancer",
                NodeKind::Service(ServiceType::ExternalName) => "ExternalName",
                _ => "ClusterIP",
            },
            "clusterIP": service.cluster_ip,
            "externalName": service.external_name,
            "ports": service.ports.iter().map(|port| json!({
                "name": port.name,
                "port": port.port,
                "protocol": port.protocol,
                "targetPort": port.target_port.as_ref().map(ToString::to_string),
                "nodePort": port.node_port,
            })).collect::<Vec<_>>(),
        })
    });
    let isolation = a.network_policy.as_ref().and_then(|policy| {
        (policy.isolated_ingress() || policy.isolated_egress()).then(|| {
            json!({
                "ingress": policy.ingress.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "egress": policy.egress.iter().map(ToString::to_string).collect::<Vec<_>>(),
            })
        })
    });
    json!({
        "id": node.id.as_str(),
        "kind": kind,
        "namespace": node.namespace,
        "name": node.name,
        "presence": match node.presence {
            Presence::Present => None,
            Presence::Missing => Some("missing"),
            Presence::Unknown => Some("unknown"),
        },
        "pods": a.pods.map(|pods| json!({"ready": pods.ready, "total": pods.total})),
        "replicas": a.desired_replicas,
        "phase": a.phase,
        "ready": a.ready,
        "service": service,
        "hosts": (!a.hosts.is_empty()).then_some(&a.hosts),
        "className": a.class_name,
        "isolation": isolation,
    })
}

fn edge_json(edge: &kubach_core::topology::Edge) -> Value {
    let a = &edge.attrs;
    json!({
        "from": edge.from.as_str(),
        "to": edge.to.as_str(),
        "kind": edge.kind.as_str(),
        "routes": (!a.routes.is_empty()).then(|| a.routes.iter().map(|rule| json!({
            "hosts": (!rule.hosts.is_empty()).then_some(&rule.hosts),
            "path": rule.path,
            "port": rule.port.as_ref().map(ToString::to_string),
            "section": rule.section,
        })).collect::<Vec<_>>()),
        "endpoints": a.endpoints.map(|e| json!({"ready": e.ready, "notReady": e.not_ready})),
        "refs": (!a.refs.is_empty()).then(|| a.refs.iter().map(|r| json!({
            "via": match r.via {
                RefVia::Env => "env",
                RefVia::EnvFrom => "envFrom",
                RefVia::Volume => "volume",
                RefVia::Projected => "projected",
                RefVia::ImagePullSecret => "imagePullSecret",
                RefVia::ClaimTemplate => "volumeClaimTemplate",
            },
            "at": r.at,
            "key": r.key,
            "optional": r.optional.then_some(true),
        })).collect::<Vec<_>>()),
        "policy": a.policy.as_ref().map(|policy| json!({
            "access": match policy.access {
                Access::Open => "open",
                Access::Allowed => "allowed",
                Access::Restricted => "restricted",
                Access::Blocked => "blocked",
            },
            "policies": policy.policies.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "ports": (!policy.ports.is_empty()).then_some(&policy.ports),
        })),
    })
}

fn explanation_json(side: &Explanation) -> Value {
    json!({
        "allowed": side.allowed,
        "isolatedBy": side.isolated_by.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "allowedBy": side.allowed_by.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "notes": side.notes,
    })
}

/// Drops `null` fields and empty arrays of objects, recursively: the agent
/// pays for every byte.
fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, strip_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::strip_nulls;

    #[test]
    fn nulls_are_dropped() {
        assert_eq!(
            strip_nulls(json!({"a": null, "b": [{"c": null, "d": 1}], "e": []})),
            json!({"b": [{"d": 1}], "e": []})
        );
    }
}
