//! NetworkPolicy analysis (#325): which pods `networking.k8s.io/v1`
//! NetworkPolicies isolate, and whether traffic between two endpoints is
//! allowed, with the policies that decide it.
//!
//! Pure functions over parsed policies and namespace labels ([`PolicySet`]);
//! nothing is asked of the cluster, and what the CNI actually enforces is
//! not known (a CNI without policy support enforces nothing). CRDs of
//! Cilium, Calico and others are not read.
//!
//! Semantics (Kubernetes docs, "Network Policies"):
//! - a policy selects pods of its own namespace by `podSelector` (empty:
//!   all of them) and restricts the directions in `policyTypes` (when
//!   absent: Ingress, plus Egress if it has egress rules);
//! - a pod no policy selects for a direction is not isolated in it:
//!   everything is allowed; once selected, only the union of the rules of
//!   all selecting policies is allowed (an empty rule list allows nothing);
//! - a connection needs both the source's egress and the destination's
//!   ingress to allow it;
//! - within one peer `podSelector` and `namespaceSelector` both apply (AND),
//!   separate peers are alternatives (OR); a `podSelector` alone means the
//!   policy's namespace; `ipBlock` matches addresses outside the cluster
//!   (pod IPs are not matched: they are implementation-defined);
//! - a port is a number, a range (`endPort`) or a name, resolved against the
//!   destination pod's container ports.
//!
//! [`apply`] is the graph layer: it marks isolated pods and workloads
//! ([`NodeAttrs::network_policy`](super::NodeAttrs::network_policy)), what
//! Service traffic gets to its pods ([`EdgeKind::Selects`] edges) and adds
//! [`EdgeKind::PolicyAllows`] edges for explicitly admitted workloads.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::net::IpAddr;
use std::ops::Range;
use std::rc::Rc;

use kube::api::DynamicObject;
use serde_json::Value;

use super::model::{
    Access, ContainerPort, Edge, EdgeAttrs, EdgeId, EdgeKind, EdgePolicy, Node, NodeClass,
    NodeKind, NodePolicy, PolicyId, PortRef, Protocol, TopologyGraph,
};
use super::source::{SourceKind, TopologyInput};
use crate::namespace::NAME_LABEL;

type Labels = BTreeMap<String, String>;

// --- selectors ---------------------------------------------------------

/// A Kubernetes label selector: `matchLabels` and `matchExpressions`
/// (`In`, `NotIn`, `Exists`, `DoesNotExist`). Empty matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selector {
    match_labels: Labels,
    expressions: Vec<Expression>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Expression {
    key: String,
    operator: Operator,
    values: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
    /// An operator the API would reject: matches nothing.
    Invalid,
}

impl Selector {
    /// Reads a `LabelSelector` object; anything else is the empty selector.
    pub fn parse(value: &Value) -> Self {
        let match_labels = value
            .get("matchLabels")
            .and_then(Value::as_object)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let expressions = value
            .get("matchExpressions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|expression| Expression {
                key: str_at(expression, "key").unwrap_or_default().to_owned(),
                operator: match str_at(expression, "operator") {
                    Some("In") => Operator::In,
                    Some("NotIn") => Operator::NotIn,
                    Some("Exists") => Operator::Exists,
                    Some("DoesNotExist") => Operator::DoesNotExist,
                    _ => Operator::Invalid,
                },
                values: expression
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect(),
            })
            .collect();
        Self {
            match_labels,
            expressions,
        }
    }

    /// A `matchLabels`-only selector.
    pub fn labels<'a>(labels: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            match_labels: labels
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            expressions: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.match_labels.is_empty() && self.expressions.is_empty()
    }

    pub fn matches(&self, labels: &Labels) -> bool {
        self.matches_by(|key| labels.get(key).map(String::as_str))
    }

    /// [`matches`](Self::matches) for labels read through `label`.
    fn matches_by<'a>(&self, label: impl Fn(&str) -> Option<&'a str>) -> bool {
        self.match_labels
            .iter()
            .all(|(key, value)| label(key) == Some(value.as_str()))
            && self.expressions.iter().all(|expression| {
                let value = label(&expression.key);
                let listed = |value: &str| expression.values.iter().any(|listed| listed == value);
                match expression.operator {
                    Operator::In => value.is_some_and(listed),
                    Operator::NotIn => value.is_none_or(|value| !listed(value)),
                    Operator::Exists => value.is_some(),
                    Operator::DoesNotExist => value.is_none(),
                    Operator::Invalid => false,
                }
            })
    }

    /// Labels (`key`, `value`) one of which a matching object must have,
    /// to look candidates up by: the `matchLabels`, or else the values of
    /// an `In`. `None`: nothing narrows it, any object may match.
    fn required(&self) -> Option<Vec<(&str, &str)>> {
        if let Some((key, value)) = self.match_labels.iter().next() {
            return Some(vec![(key.as_str(), value.as_str())]);
        }
        self.expressions
            .iter()
            .find(|expression| expression.operator == Operator::In)
            .map(|expression| {
                expression
                    .values
                    .iter()
                    .map(|value| (expression.key.as_str(), value.as_str()))
                    .collect()
            })
    }

    /// Label keys [`required`](Self::required) may name.
    fn required_keys(&self) -> impl Iterator<Item = &str> {
        let first = self.match_labels.keys().next();
        let listed = self
            .expressions
            .iter()
            .filter(|expression| expression.operator == Operator::In)
            .map(|expression| &expression.key);
        first.into_iter().chain(listed).map(String::as_str)
    }
}

/// `app=web,tier in (a,b),canary,!legacy`; empty for the empty selector.
impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self
            .match_labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        for expression in &self.expressions {
            let key = &expression.key;
            let values = expression.values.join(",");
            parts.push(match expression.operator {
                Operator::In => format!("{key} in ({values})"),
                Operator::NotIn => format!("{key} notin ({values})"),
                Operator::Exists => key.clone(),
                Operator::DoesNotExist => format!("!{key}"),
                Operator::Invalid => format!("{key} ?"),
            });
        }
        f.write_str(&parts.join(","))
    }
}

// --- addresses ---------------------------------------------------------

/// An IPv4 or IPv6 range, `10.0.0.0/8`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `10.0.0.0/8`, `2001:db8::/32`; a bare address is a single host.
    pub fn parse(text: &str) -> Option<Self> {
        let (address, prefix) = match text.trim().split_once('/') {
            Some((address, prefix)) => (address, Some(prefix.parse::<u8>().ok()?)),
            None => (text.trim(), None),
        };
        let network: IpAddr = address.parse().ok()?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Self { network, prefix })
    }

    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

// --- policies ----------------------------------------------------------

/// Direction of traffic relative to a pod.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    Ingress,
    Egress,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Direction::Ingress => "ingress",
            Direction::Egress => "egress",
        })
    }
}

/// A parsed NetworkPolicy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub id: PolicyId,
    pub pod_selector: Selector,
    /// `Some` when the policy restricts ingress (`policyTypes`); an empty
    /// list denies all ingress.
    pub ingress: Option<Vec<Rule>>,
    pub egress: Option<Vec<Rule>>,
}

/// One ingress (`from`) or egress (`to`) rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rule {
    /// Empty: any peer.
    pub peers: Vec<Peer>,
    /// Empty: any port.
    pub ports: Vec<PolicyPort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peer {
    /// Pods by `podSelector` (`None`: all pods) in namespaces by
    /// `namespaceSelector` (`None`: the policy's own namespace).
    Pods {
        pods: Option<Selector>,
        namespaces: Option<Selector>,
    },
    /// Addresses outside the cluster.
    IpBlock { cidr: Cidr, except: Vec<Cidr> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyPort {
    pub protocol: Protocol,
    /// `None`: every port of the protocol.
    pub port: Option<PortRef>,
    /// Last port of a range starting at a numeric `port`.
    pub end_port: Option<i32>,
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

impl Policy {
    /// Reads a NetworkPolicy object; `None` without a namespace or name.
    pub fn parse(object: &DynamicObject) -> Option<Self> {
        let id = PolicyId {
            namespace: object.metadata.namespace.clone()?,
            name: object.metadata.name.clone()?,
        };
        Some(Self::from_spec(
            id,
            object.data.get("spec").unwrap_or(&Value::Null),
        ))
    }

    /// Reads the `spec` of a NetworkPolicy.
    pub fn from_spec(id: PolicyId, spec: &Value) -> Self {
        let mut types: BTreeSet<&str> = array(spec, "policyTypes")
            .filter_map(Value::as_str)
            .collect();
        if types.is_empty() {
            // What the API server defaults on create.
            types.insert("Ingress");
            if array(spec, "egress").next().is_some() {
                types.insert("Egress");
            }
        }
        let rules = |key: &str, peers: &str| -> Vec<Rule> {
            array(spec, key)
                .map(|rule| Rule {
                    peers: array(rule, peers).filter_map(parse_peer).collect(),
                    ports: array(rule, "ports").filter_map(parse_port).collect(),
                })
                .collect()
        };
        Self {
            id,
            pod_selector: spec
                .get("podSelector")
                .map(Selector::parse)
                .unwrap_or_default(),
            ingress: types.contains("Ingress").then(|| rules("ingress", "from")),
            egress: types.contains("Egress").then(|| rules("egress", "to")),
        }
    }

    /// Rules of a direction; `None` when the policy doesn't restrict it.
    pub fn rules(&self, direction: Direction) -> Option<&[Rule]> {
        match direction {
            Direction::Ingress => self.ingress.as_deref(),
            Direction::Egress => self.egress.as_deref(),
        }
    }

    pub fn selects(&self, pod: &PodGroup) -> bool {
        self.id.namespace == pod.namespace && self.pod_selector.matches(&pod.labels)
    }
}

fn parse_peer(value: &Value) -> Option<Peer> {
    if let Some(block) = value.get("ipBlock") {
        return Some(Peer::IpBlock {
            cidr: Cidr::parse(str_at(block, "cidr")?)?,
            except: array(block, "except")
                .filter_map(Value::as_str)
                .filter_map(Cidr::parse)
                .collect(),
        });
    }
    let pods = value.get("podSelector").map(Selector::parse);
    let namespaces = value.get("namespaceSelector").map(Selector::parse);
    (pods.is_some() || namespaces.is_some()).then_some(Peer::Pods { pods, namespaces })
}

fn parse_port(value: &Value) -> Option<PolicyPort> {
    let protocol = match str_at(value, "protocol") {
        Some(text) => Protocol::parse(text)?,
        None => Protocol::Tcp,
    };
    let port = match value.get("port") {
        Some(Value::Number(number)) => Some(PortRef::Number(i32::try_from(number.as_i64()?).ok()?)),
        Some(Value::String(name)) => Some(match name.parse::<i32>() {
            Ok(number) => PortRef::Number(number),
            Err(_) => PortRef::Name(name.clone()),
        }),
        _ => None,
    };
    let end_port = value
        .get("endPort")
        .and_then(Value::as_i64)
        .and_then(|end| i32::try_from(end).ok());
    Some(PolicyPort {
        protocol,
        port,
        end_port,
    })
}

impl Peer {
    /// `pods app=web`, `pods app=web in namespaces team=a`,
    /// `10.0.0.0/8 except 10.1.0.0/16`.
    pub fn describe(&self) -> String {
        match self {
            Peer::IpBlock { cidr, except } if except.is_empty() => cidr.to_string(),
            Peer::IpBlock { cidr, except } => {
                let except: Vec<String> = except.iter().map(ToString::to_string).collect();
                format!("{cidr} except {}", except.join(", "))
            }
            Peer::Pods { pods, namespaces } => {
                let pods = match pods {
                    Some(selector) if !selector.is_empty() => format!("pods {selector}"),
                    _ => "all pods".to_owned(),
                };
                match namespaces {
                    None => format!("{pods} (same namespace)"),
                    Some(selector) if selector.is_empty() => format!("{pods} in all namespaces"),
                    Some(selector) => format!("{pods} in namespaces {selector}"),
                }
            }
        }
    }
}

impl fmt::Display for PolicyPort {
    /// `TCP 8080`, `TCP 8000-8080`, `TCP http`, `UDP any port`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.port, self.end_port) {
            (Some(port), Some(end)) => write!(f, "{} {port}-{end}", self.protocol),
            (Some(port), None) => write!(f, "{} {port}", self.protocol),
            (None, _) => write!(f, "{} any port", self.protocol),
        }
    }
}

impl Rule {
    /// `from pods app=web (same namespace) · TCP 8080`.
    pub fn describe(&self, direction: Direction) -> String {
        let word = match direction {
            Direction::Ingress => "from",
            Direction::Egress => "to",
        };
        let peers = if self.peers.is_empty() {
            "anywhere".to_owned()
        } else {
            self.peers
                .iter()
                .map(Peer::describe)
                .collect::<Vec<_>>()
                .join("; ")
        };
        let ports = if self.ports.is_empty() {
            "all ports".to_owned()
        } else {
            self.ports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!("{word} {peers} · {ports}")
    }
}

// --- endpoints ---------------------------------------------------------

/// Pods that policies can't tell apart: same namespace, labels and ports.
/// A workload's pods are one group (its pod template).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PodGroup {
    pub namespace: String,
    pub labels: Labels,
    pub ports: Vec<ContainerPort>,
}

impl PodGroup {
    /// The pods a pod or workload node stands for.
    pub fn of(node: &Node) -> Option<Self> {
        let pods = Pods::of(node)?;
        Some(Self {
            namespace: pods.namespace.to_owned(),
            labels: pods.labels.clone(),
            ports: pods.ports.to_vec(),
        })
    }

    /// The number of a named port of `protocol`.
    pub fn port_named(&self, name: &str, protocol: Protocol) -> Option<i32> {
        self.view().port_named(name, protocol)
    }

    fn view(&self) -> Pods<'_> {
        Pods {
            namespace: &self.namespace,
            labels: &self.labels,
            ports: &self.ports,
        }
    }
}

/// A [`PodGroup`] read in place: of a node of the graph, nothing copied.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Pods<'a> {
    namespace: &'a str,
    labels: &'a Labels,
    ports: &'a [ContainerPort],
}

impl<'a> Pods<'a> {
    fn of(node: &'a Node) -> Option<Self> {
        matches!(node.kind, NodeKind::Pod | NodeKind::Workload(_)).then_some(())?;
        Some(Self {
            namespace: node.namespace.as_deref()?,
            labels: &node.attrs.pod_labels,
            ports: &node.attrs.container_ports,
        })
    }

    fn port_named(&self, name: &str, protocol: Protocol) -> Option<i32> {
        self.ports
            .iter()
            .find(|port| port.name.as_deref() == Some(name) && port.protocol == protocol)
            .map(|port| port.port)
    }
}

/// One end of a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Pods(PodGroup),
    /// An address outside the cluster.
    Address(IpAddr),
}

impl Target {
    fn pods(&self) -> Option<&PodGroup> {
        match self {
            Target::Pods(pods) => Some(pods),
            Target::Address(_) => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            Target::Pods(pods) if pods.labels.is_empty() => {
                format!("pods in {}", pods.namespace)
            }
            Target::Pods(pods) => {
                let labels: Vec<String> = pods
                    .labels
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                format!("pods {} in {}", labels.join(","), pods.namespace)
            }
            Target::Address(address) => address.to_string(),
        }
    }
}

// --- verdicts ----------------------------------------------------------

/// What the policies of one side (the source's egress or the
/// destination's ingress) say about a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explanation {
    pub direction: Direction,
    pub allowed: bool,
    /// Policies selecting this side's pods for the direction: when there
    /// are any, the side is isolated.
    pub isolated_by: Vec<PolicyId>,
    /// Isolating policies with a rule that admits the connection.
    pub allowed_by: Vec<PolicyId>,
    /// Sentences explaining the decision.
    pub notes: Vec<String>,
}

impl Explanation {
    pub fn isolated(&self) -> bool {
        !self.isolated_by.is_empty()
    }
}

/// Answer of [`PolicySet::can_reach`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub allowed: bool,
    /// The destination port, a named one resolved; `None` when the name
    /// doesn't resolve.
    pub port: Option<i32>,
    pub protocol: Protocol,
    pub egress_side: Explanation,
    pub ingress_side: Explanation,
}

/// kube-dns as the target of the DNS hint.
fn kube_dns() -> Target {
    Target::Pods(PodGroup {
        namespace: "kube-system".into(),
        labels: [("k8s-app".to_owned(), "kube-dns".to_owned())].into(),
        ports: vec![
            ContainerPort {
                name: Some("dns".into()),
                port: 53,
                protocol: Protocol::Udp,
            },
            ContainerPort {
                name: Some("dns-tcp".into()),
                port: 53,
                protocol: Protocol::Tcp,
            },
        ],
    })
}

fn join(ids: &[PolicyId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// NetworkPolicies with the labels of the namespaces they may refer to.
#[derive(Clone, Debug, Default)]
pub struct PolicySet {
    policies: Vec<Policy>,
    /// Where the policies of a namespace are in `policies` (sorted by
    /// namespace first): only they can select its pods.
    by_namespace: HashMap<String, Range<usize>>,
    namespaces: HashMap<String, Labels>,
    /// Namespace labels were loaded; without them a `namespaceSelector`
    /// can only match `kubernetes.io/metadata.name`.
    namespaces_known: bool,
}

impl PolicySet {
    pub fn new(
        policies: Vec<Policy>,
        namespaces: impl IntoIterator<Item = (String, Labels)>,
    ) -> Self {
        let mut policies = policies;
        policies.sort_by(|a, b| a.id.cmp(&b.id));
        let mut by_namespace: HashMap<String, Range<usize>> = HashMap::new();
        for (ix, policy) in policies.iter().enumerate() {
            match by_namespace.get_mut(&policy.id.namespace) {
                Some(range) => range.end = ix + 1,
                None => {
                    by_namespace.insert(policy.id.namespace.clone(), ix..ix + 1);
                }
            }
        }
        Self {
            policies,
            by_namespace,
            namespaces: namespaces.into_iter().collect(),
            namespaces_known: true,
        }
    }

    /// Policies and namespaces of a topology input
    /// ([`SourceKind::NetworkPolicy`], [`SourceKind::Namespace`]).
    pub fn from_input(input: &TopologyInput) -> Self {
        let policies = input
            .objects(SourceKind::NetworkPolicy)
            .iter()
            .filter_map(|object| Policy::parse(object))
            .collect();
        let objects = input.objects(SourceKind::Namespace);
        let namespaces: Vec<(String, Labels)> = objects
            .iter()
            .filter_map(|object| {
                Some((
                    object.metadata.name.clone()?,
                    object
                        .metadata
                        .labels
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                ))
            })
            .collect();
        let mut set = Self::new(policies, namespaces);
        set.namespaces_known = input.is_complete(SourceKind::Namespace) || !objects.is_empty();
        set
    }

    /// Sorted by namespace and name.
    pub fn policies(&self) -> &[Policy] {
        &self.policies
    }

    pub fn policy(&self, id: &PolicyId) -> Option<&Policy> {
        self.policies.iter().find(|policy| &policy.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Labels of a namespace, always with `kubernetes.io/metadata.name`.
    pub fn namespace_labels(&self, namespace: &str) -> Labels {
        let mut labels = self.namespaces.get(namespace).cloned().unwrap_or_default();
        labels.insert(NAME_LABEL.to_owned(), namespace.to_owned());
        labels
    }

    /// Whether `selector` matches the namespace: its labels, always with
    /// `kubernetes.io/metadata.name`.
    fn namespace_matches(&self, selector: &Selector, namespace: &str) -> bool {
        let labels = self.namespaces.get(namespace);
        selector.matches_by(|key| {
            if key == NAME_LABEL {
                Some(namespace)
            } else {
                labels?.get(key).map(String::as_str)
            }
        })
    }

    /// Positions in [`policies`](Self::policies) of those of a namespace.
    fn in_namespace(&self, namespace: &str) -> Range<usize> {
        self.by_namespace.get(namespace).cloned().unwrap_or(0..0)
    }

    /// Policies selecting the pods (any direction).
    pub fn selecting<'a>(&'a self, pods: &'a PodGroup) -> impl Iterator<Item = &'a Policy> + 'a {
        self.selecting_pods(pods.view()).map(|(_, policy)| policy)
    }

    /// [`selecting`](Self::selecting), with the position of each policy.
    fn selecting_pods<'a>(&'a self, pods: Pods<'a>) -> impl Iterator<Item = (usize, &'a Policy)> {
        self.in_namespace(pods.namespace)
            .map(|ix| (ix, &self.policies[ix]))
            .filter(move |(_, policy)| policy.pod_selector.matches(pods.labels))
    }

    /// Policies isolating the pods, by direction.
    pub fn node_policy(&self, pods: &PodGroup) -> NodePolicy {
        self.node_policy_of(pods.view())
    }

    fn node_policy_of(&self, pods: Pods<'_>) -> NodePolicy {
        let mut out = NodePolicy::default();
        for (_, policy) in self.selecting_pods(pods) {
            if policy.ingress.is_some() {
                out.ingress.push(policy.id.clone());
            }
            if policy.egress.is_some() {
                out.egress.push(policy.id.clone());
            }
        }
        out
    }

    fn peer_matches(&self, policy: &Policy, peer: &Peer, other: &Target) -> bool {
        match (peer, other) {
            (Peer::Pods { .. }, Target::Pods(group)) => {
                self.peer_matches_pods(policy, peer, group.view())
            }
            (Peer::IpBlock { cidr, except }, Target::Address(address)) => {
                cidr.contains(*address) && !except.iter().any(|except| except.contains(*address))
            }
            _ => false,
        }
    }

    /// Whether a peer of `policy` names these pods (an `ipBlock` never
    /// does).
    fn peer_matches_pods(&self, policy: &Policy, peer: &Peer, group: Pods<'_>) -> bool {
        let Peer::Pods { pods, namespaces } = peer else {
            return false;
        };
        let namespace = match namespaces {
            None => group.namespace == policy.id.namespace,
            Some(selector) => self.namespace_matches(selector, group.namespace),
        };
        namespace
            && pods
                .as_ref()
                .is_none_or(|selector| selector.matches(group.labels))
    }

    /// Whether the egress policies of `source` let it open a connection to
    /// `port` of `destination`: what [`side`](Self::side) answers for
    /// egress, without the explanation.
    fn egress_allows(
        &self,
        source: Pods<'_>,
        destination: Pods<'_>,
        port: i32,
        protocol: Protocol,
    ) -> bool {
        let mut isolated = false;
        for (_, policy) in self.selecting_pods(source) {
            let Some(rules) = policy.egress.as_deref() else {
                continue;
            };
            isolated = true;
            let admits = rules.iter().any(|rule| {
                ports_match(&rule.ports, port, protocol, Some(destination))
                    && (rule.peers.is_empty()
                        || rule
                            .peers
                            .iter()
                            .any(|peer| self.peer_matches_pods(policy, peer, destination)))
            });
            if admits {
                return true;
            }
        }
        !isolated
    }

    /// Whether a rule names this peer (or any peer).
    fn rule_peers(&self, policy: &Policy, rule: &Rule, other: &Target) -> bool {
        rule.peers.is_empty()
            || rule
                .peers
                .iter()
                .any(|peer| self.peer_matches(policy, peer, other))
    }

    /// Evaluates one side: `subject`'s policies of `direction` about
    /// traffic with `other` to `port` of `destination`.
    fn side(
        &self,
        direction: Direction,
        subject: &Target,
        other: &Target,
        port: i32,
        protocol: Protocol,
        destination: Option<&PodGroup>,
    ) -> Explanation {
        let mut out = Explanation {
            direction,
            allowed: true,
            isolated_by: Vec::new(),
            allowed_by: Vec::new(),
            notes: Vec::new(),
        };
        let Target::Pods(pods) = subject else {
            out.notes
                .push("Not a pod: no NetworkPolicy applies on this side.".into());
            return out;
        };
        let isolating: Vec<&Policy> = self
            .selecting(pods)
            .filter(|policy| policy.rules(direction).is_some())
            .collect();
        if isolating.is_empty() {
            out.notes.push(format!(
                "No NetworkPolicy selects {} for {direction}: all {direction} traffic is allowed.",
                subject.describe()
            ));
            return out;
        }
        out.isolated_by = isolating.iter().map(|policy| policy.id.clone()).collect();
        let mut ipblock_rules = false;
        for policy in &isolating {
            let rules = policy.rules(direction).unwrap_or_default();
            let admits = rules.iter().any(|rule| {
                ipblock_rules |= rule
                    .peers
                    .iter()
                    .any(|peer| matches!(peer, Peer::IpBlock { .. }));
                ports_match(&rule.ports, port, protocol, destination.map(PodGroup::view))
                    && self.rule_peers(policy, rule, other)
            });
            if admits {
                out.allowed_by.push(policy.id.clone());
            }
        }
        out.allowed = !out.allowed_by.is_empty();
        if out.allowed {
            out.notes
                .push(format!("Allowed by {}.", join(&out.allowed_by)));
        } else {
            let deny_all: Vec<PolicyId> = isolating
                .iter()
                .filter(|policy| policy.rules(direction).is_some_and(<[Rule]>::is_empty))
                .map(|policy| policy.id.clone())
                .collect();
            if !deny_all.is_empty() {
                out.notes
                    .push(format!("{} deny all {direction} traffic.", join(&deny_all)));
            }
            let word = match direction {
                Direction::Ingress => "from",
                Direction::Egress => "to",
            };
            out.notes.push(format!(
                "Isolated by {}; no rule allows {direction} {word} {} on {protocol} {port}.",
                join(&out.isolated_by),
                other.describe()
            ));
            if ipblock_rules && matches!(other, Target::Pods(_)) {
                out.notes.push(
                    "ipBlock rules are not matched against pods: their IPs are \
                     implementation-defined."
                        .into(),
                );
            }
        }
        if !self.namespaces_known
            && isolating.iter().any(|policy| {
                policy.rules(direction).unwrap_or_default().iter().any(|rule| {
                    rule.peers.iter().any(|peer| {
                        matches!(peer, Peer::Pods { namespaces: Some(selector), .. } if !selector.is_empty())
                    })
                })
            })
        {
            out.notes.push(
                "Namespace labels are not loaded: namespaceSelectors only matched \
                 kubernetes.io/metadata.name."
                    .into(),
            );
        }
        out
    }

    /// Whether `from` can open a connection to `port` of `to`: both the
    /// egress of `from` and the ingress of `to` must allow it. A named
    /// port resolves against the container ports of `to`.
    pub fn can_reach(
        &self,
        from: &Target,
        to: &Target,
        port: &PortRef,
        protocol: Protocol,
    ) -> Verdict {
        let destination = to.pods();
        let number = match port {
            PortRef::Number(number) => Some(*number),
            PortRef::Name(name) => destination.and_then(|pods| pods.port_named(name, protocol)),
        };
        let Some(number) = number else {
            let note = format!("{} has no {protocol} port named {port}.", to.describe());
            let side = |direction| Explanation {
                direction,
                allowed: false,
                isolated_by: Vec::new(),
                allowed_by: Vec::new(),
                notes: vec![note.clone()],
            };
            return Verdict {
                allowed: false,
                port: None,
                protocol,
                egress_side: side(Direction::Egress),
                ingress_side: side(Direction::Ingress),
            };
        };
        let mut egress = self.side(Direction::Egress, from, to, number, protocol, destination);
        let ingress = self.side(Direction::Ingress, to, from, number, protocol, destination);
        if egress.isolated() && to != &kube_dns() {
            let dns = kube_dns();
            let resolves = self
                .side(Direction::Egress, from, &dns, 53, Protocol::Udp, dns.pods())
                .allowed;
            if !resolves {
                egress.notes.push(
                    "Egress is restricted and DNS (UDP 53 to kube-dns in kube-system) is not \
                     allowed: Service names won't resolve."
                        .into(),
                );
            }
        }
        Verdict {
            allowed: egress.allowed && ingress.allowed,
            port: Some(number),
            protocol,
            egress_side: egress,
            ingress_side: ingress,
        }
    }

    /// What gets in to `pods` on `ports` (`(number, protocol)`; empty: any
    /// port) from sources not known in advance (a Service's clients).
    pub fn admission(&self, pods: &PodGroup, ports: &[(i32, Protocol)]) -> EdgePolicy {
        self.admission_of(pods.view(), ports)
    }

    fn admission_of(&self, pods: Pods<'_>, ports: &[(i32, Protocol)]) -> EdgePolicy {
        let isolating: Vec<&Policy> = self
            .selecting_pods(pods)
            .map(|(_, policy)| policy)
            .filter(|policy| policy.ingress.is_some())
            .collect();
        let port_texts: Vec<String> = ports
            .iter()
            .map(|(port, protocol)| format!("{protocol} {port}"))
            .collect();
        if isolating.is_empty() {
            return EdgePolicy {
                access: Access::Open,
                policies: Vec::new(),
                ports: port_texts,
            };
        }
        let mut admitting: BTreeSet<PolicyId> = BTreeSet::new();
        let mut accesses: BTreeSet<Access> = BTreeSet::new();
        let checks: Vec<Option<(i32, Protocol)>> = if ports.is_empty() {
            vec![None]
        } else {
            ports.iter().copied().map(Some).collect()
        };
        for check in checks {
            let mut access = Access::Blocked;
            for policy in &isolating {
                for rule in policy.ingress.as_deref().unwrap_or_default() {
                    let port_ok = match check {
                        Some((port, protocol)) => {
                            ports_match(&rule.ports, port, protocol, Some(pods))
                        }
                        None => true,
                    };
                    if !port_ok {
                        continue;
                    }
                    admitting.insert(policy.id.clone());
                    access = if rule.peers.is_empty() && (check.is_some() || rule.ports.is_empty())
                    {
                        Access::Allowed
                    } else {
                        access.min(Access::Restricted)
                    };
                    if access == Access::Allowed {
                        break;
                    }
                }
                if access == Access::Allowed {
                    break;
                }
            }
            accesses.insert(access);
        }
        let access = if accesses.len() == 1 {
            accesses.into_iter().next().unwrap_or(Access::Blocked)
        } else {
            Access::Restricted
        };
        let policies = if access == Access::Blocked {
            isolating.iter().map(|policy| policy.id.clone()).collect()
        } else {
            admitting.into_iter().collect()
        };
        EdgePolicy {
            access,
            policies,
            ports: port_texts,
        }
    }
}

/// Whether `port`/`protocol` of `destination` is in a rule's port list.
fn ports_match(
    ports: &[PolicyPort],
    port: i32,
    protocol: Protocol,
    destination: Option<Pods<'_>>,
) -> bool {
    ports.is_empty()
        || ports.iter().any(|rule| {
            rule.protocol == protocol
                && match &rule.port {
                    None => true,
                    Some(PortRef::Number(first)) => match rule.end_port {
                        Some(end) => (*first..=end).contains(&port),
                        None => *first == port,
                    },
                    Some(PortRef::Name(name)) => {
                        destination.and_then(|pods| pods.port_named(name, protocol)) == Some(port)
                    }
                }
        })
}

// --- graph layer -------------------------------------------------------

/// Sources drawn per isolated target at most ([`EdgeKind::PolicyAllows`]).
const MAX_ALLOWED_SOURCES: usize = 24;

/// Target port of a Service port on `pods`: a number, a resolved name, or
/// the port itself.
fn service_target(port: &super::model::ServicePort, pods: Pods<'_>) -> Option<(i32, Protocol)> {
    let protocol = port
        .protocol
        .as_deref()
        .and_then(Protocol::parse)
        .unwrap_or_default();
    let number = match &port.target_port {
        Some(PortRef::Number(number)) => *number,
        Some(PortRef::Name(name)) => pods.port_named(name, protocol)?,
        None => port.port,
    };
    Some((number, protocol))
}

/// The ports traffic to `destination` under `rule` may use, resolved.
fn rule_ports(rule: &Rule, destination: Pods<'_>) -> Vec<(i32, Protocol)> {
    let mut out = Vec::new();
    if rule.ports.is_empty() {
        out.extend(
            destination
                .ports
                .iter()
                .map(|port| (port.port, port.protocol)),
        );
    }
    for port in &rule.ports {
        match &port.port {
            Some(PortRef::Number(number)) => out.push((*number, port.protocol)),
            Some(PortRef::Name(name)) => {
                if let Some(number) = destination.port_named(name, port.protocol) {
                    out.push((number, port.protocol));
                }
            }
            None => out.extend(
                destination
                    .ports
                    .iter()
                    .filter(|own| own.protocol == port.protocol)
                    .map(|own| (own.port, own.protocol)),
            ),
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Adds the network policy layer to a graph: [`NodePolicy`] on pods and
/// workloads, [`EdgePolicy`] on Service → pods edges (the Service's target
/// ports) and [`EdgeKind::PolicyAllows`] edges from workloads a rule
/// explicitly admits (a non-empty `podSelector`) to isolated ones, when the
/// source's egress lets the traffic out too.
///
/// The work is in proportion to the graph and to what the policies select:
/// a pod is tried against the policies of its namespace only, and the
/// sources a peer admits are looked up by namespace and label, once per
/// peer and kind of target (pods of one workload are one kind).
pub fn apply(graph: TopologyGraph, policies: &PolicySet) -> TopologyGraph {
    let Layer {
        nodes,
        edges,
        allows,
    } = Layer::of(&graph, policies);
    let mut nodes = nodes.into_iter();
    let mut edges = edges.into_iter();
    // Both run over the graph in order, as the layer was made.
    graph.layered(
        |node| {
            if let Some(policy) = nodes.next().flatten() {
                node.attrs.network_policy = Some(policy);
            }
        },
        |edge| {
            if let Some(policy) = edges.next().flatten() {
                edge.attrs.policy = Some(policy);
            }
        },
        allows,
    )
}

/// What the layer adds to a graph.
struct Layer {
    /// By node, in the order of the graph.
    nodes: Vec<Option<NodePolicy>>,
    /// By edge, in the order of the graph.
    edges: Vec<Option<EdgePolicy>>,
    allows: Vec<Edge>,
}

/// Drawn pods and workloads a peer admits to a kind of target that can
/// also get out to it, in the order of the graph, each with the ports open
/// to it; at most [`MAX_ALLOWED_SOURCES`] + 1 (one may be the target itself).
type Admitted = Rc<Vec<(usize, Vec<String>)>>;

impl Layer {
    fn of(graph: &TopologyGraph, policies: &PolicySet) -> Self {
        let nodes = graph.nodes();
        let groups: Vec<Option<Pods<'_>>> = nodes.iter().map(Pods::of).collect();
        let node_policies: Vec<Option<NodePolicy>> = groups
            .iter()
            .map(|pods| pods.map(|pods| policies.node_policy_of(pods)))
            .collect();

        // Workloads whose pods are drawn: the pods carry the layer's edges.
        let mut with_pods = vec![false; nodes.len()];
        let mut edge_policies: Vec<Option<EdgePolicy>> = Vec::with_capacity(graph.edges().len());
        for edge in graph.edges() {
            let mut policy = None;
            match edge.kind {
                EdgeKind::Owns => {
                    if let (Some(from), Some(to)) =
                        (graph.node_index(&edge.from), graph.node_index(&edge.to))
                        && nodes[to].kind == NodeKind::Pod
                    {
                        with_pods[from] = true;
                    }
                }
                EdgeKind::Selects => {
                    if let Some(pods) = graph.node_index(&edge.to).and_then(|to| groups[to])
                        && let Some(service) = graph
                            .node(&edge.from)
                            .and_then(|node| node.attrs.service.as_ref())
                    {
                        let mut ports: Vec<(i32, Protocol)> = service
                            .ports
                            .iter()
                            .filter_map(|port| service_target(port, pods))
                            .collect();
                        ports.sort();
                        ports.dedup();
                        policy = Some(policies.admission_of(pods, &ports));
                    }
                }
                _ => {}
            }
            edge_policies.push(policy);
        }

        let drawn: Vec<usize> = (0..nodes.len())
            .filter(|&ix| groups[ix].is_some() && !with_pods[ix])
            .collect();
        let allows = Allows {
            policies,
            nodes,
            groups: &groups,
            node_policies: &node_policies,
            drawn: &drawn,
            sources: None,
            kinds: HashMap::new(),
            admitted: HashMap::new(),
        }
        .edges();
        Self {
            nodes: node_policies,
            edges: edge_policies,
            allows,
        }
    }
}

/// Drawn nodes by namespace and by label, in the order of the graph.
struct Sources<'g> {
    namespaces: Vec<&'g str>,
    in_namespace: HashMap<&'g str, Vec<usize>>,
    /// Only for the label keys peers look sources up by.
    by_label: HashMap<(&'g str, &'g str, &'g str), Vec<usize>>,
}

/// Makes the [`EdgeKind::PolicyAllows`] edges.
struct Allows<'g> {
    policies: &'g PolicySet,
    nodes: &'g [Node],
    groups: &'g [Option<Pods<'g>>],
    node_policies: &'g [Option<NodePolicy>],
    /// Nodes that may be an end of an edge, as positions in `nodes`.
    drawn: &'g [usize],
    /// Made for the first peer that needs it.
    sources: Option<Sources<'g>>,
    /// Pods policies can't tell apart are one kind: its number.
    kinds: HashMap<Pods<'g>, usize>,
    /// By policy, rule, peer (positions) and kind of target.
    admitted: HashMap<(usize, usize, usize, usize), Admitted>,
}

impl<'g> Allows<'g> {
    fn edges(mut self) -> Vec<Edge> {
        let mut allows = Vec::new();
        for &target_ix in self.drawn {
            let isolated = self.node_policies[target_ix]
                .as_ref()
                .is_some_and(NodePolicy::isolated_ingress);
            let Some(target) = self.groups[target_ix].filter(|_| isolated) else {
                continue;
            };
            let target_id = &self.nodes[target_ix].id;
            // The edges of this target, in the order their sources came.
            let mut edges: Vec<(usize, Edge)> = Vec::new();
            let policies = self.policies;
            'target: for (policy_ix, policy) in policies.selecting_pods(target) {
                let rules = policy.ingress.as_deref().unwrap_or_default();
                for (rule_ix, rule) in rules.iter().enumerate() {
                    for (peer_ix, peer) in rule.peers.iter().enumerate() {
                        let Peer::Pods {
                            pods: Some(selector),
                            ..
                        } = peer
                        else {
                            continue;
                        };
                        if selector.is_empty() {
                            continue;
                        }
                        let admitted = self.admitted(
                            (policy_ix, rule_ix, peer_ix),
                            policy,
                            rule,
                            peer,
                            target,
                        );
                        for (source_ix, open) in admitted.iter() {
                            if *source_ix == target_ix {
                                continue;
                            }
                            let position = match edges.iter().position(|(ix, _)| ix == source_ix) {
                                Some(position) => position,
                                None => {
                                    let source_id = &self.nodes[*source_ix].id;
                                    edges.push((
                                        *source_ix,
                                        Edge {
                                            id: EdgeId::new(
                                                EdgeKind::PolicyAllows,
                                                source_id,
                                                target_id,
                                            ),
                                            from: source_id.clone(),
                                            to: target_id.clone(),
                                            kind: EdgeKind::PolicyAllows,
                                            attrs: EdgeAttrs {
                                                policy: Some(EdgePolicy {
                                                    access: Access::Allowed,
                                                    policies: Vec::new(),
                                                    ports: Vec::new(),
                                                }),
                                                ..EdgeAttrs::default()
                                            },
                                        },
                                    ));
                                    edges.len() - 1
                                }
                            };
                            if let Some(edge_policy) = &mut edges[position].1.attrs.policy {
                                if !edge_policy.policies.contains(&policy.id) {
                                    edge_policy.policies.push(policy.id.clone());
                                }
                                for port in open {
                                    if !edge_policy.ports.contains(port) {
                                        edge_policy.ports.push(port.clone());
                                    }
                                }
                            }
                            // Enough sources drawn: the rest is left out.
                            if edges.len() >= MAX_ALLOWED_SOURCES {
                                break 'target;
                            }
                        }
                    }
                }
            }
            allows.extend(edges.into_iter().map(|(_, edge)| edge));
        }
        allows
    }

    fn kind_of(&mut self, pods: Pods<'g>) -> usize {
        let next = self.kinds.len();
        *self.kinds.entry(pods).or_insert(next)
    }

    fn sources(&mut self) -> &Sources<'g> {
        let (policies, groups, drawn) = (self.policies, self.groups, self.drawn);
        self.sources.get_or_insert_with(|| {
            let keys: HashSet<&str> = policies
                .policies
                .iter()
                .flat_map(|policy| policy.ingress.iter().flatten())
                .flat_map(|rule| &rule.peers)
                .filter_map(|peer| match peer {
                    Peer::Pods {
                        pods: Some(selector),
                        ..
                    } => Some(selector),
                    _ => None,
                })
                .flat_map(Selector::required_keys)
                .collect();
            let mut sources = Sources {
                namespaces: Vec::new(),
                in_namespace: HashMap::new(),
                by_label: HashMap::new(),
            };
            for &ix in drawn {
                let Some(pods) = groups[ix] else {
                    continue;
                };
                let all = sources.in_namespace.entry(pods.namespace).or_default();
                if all.is_empty() {
                    sources.namespaces.push(pods.namespace);
                }
                all.push(ix);
                for (key, value) in pods.labels {
                    if keys.contains(key.as_str()) {
                        sources
                            .by_label
                            .entry((pods.namespace, key.as_str(), value.as_str()))
                            .or_default()
                            .push(ix);
                    }
                }
            }
            sources
        })
    }

    /// Drawn nodes `peer` of `policy` names, in the order of the graph.
    fn matching(&mut self, policy: &'g Policy, peer: &'g Peer) -> Vec<usize> {
        let Peer::Pods {
            pods: Some(selector),
            namespaces,
        } = peer
        else {
            return Vec::new();
        };
        let (policies, groups) = (self.policies, self.groups);
        let sources = self.sources();
        let admitted: Vec<&str> = match namespaces {
            None => vec![policy.id.namespace.as_str()],
            Some(namespaces) => sources
                .namespaces
                .iter()
                .copied()
                .filter(|namespace| policies.namespace_matches(namespaces, namespace))
                .collect(),
        };
        let required = selector.required();
        let mut matching = Vec::new();
        for namespace in admitted {
            let mut candidates: Vec<&[usize]> = Vec::new();
            match &required {
                Some(labels) => candidates.extend(
                    labels
                        .iter()
                        .filter_map(|(key, value)| sources.by_label.get(&(namespace, *key, *value)))
                        .map(Vec::as_slice),
                ),
                None => candidates.extend(sources.in_namespace.get(namespace).map(Vec::as_slice)),
            }
            matching.extend(
                candidates
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&ix| groups[ix].is_some_and(|pods| selector.matches(pods.labels))),
            );
        }
        // Namespaces and labels were gone through one after another.
        matching.sort_unstable();
        matching.dedup();
        matching
    }

    /// The sources `peer` admits to targets like `target` that also get
    /// out to them, with the open ports.
    fn admitted(
        &mut self,
        (policy_ix, rule_ix, peer_ix): (usize, usize, usize),
        policy: &'g Policy,
        rule: &'g Rule,
        peer: &'g Peer,
        target: Pods<'g>,
    ) -> Admitted {
        let key = (policy_ix, rule_ix, peer_ix, self.kind_of(target));
        if let Some(admitted) = self.admitted.get(&key) {
            return Rc::clone(admitted);
        }
        let ports = rule_ports(rule, target);
        let mut admitted = Vec::new();
        // Whether a kind of source gets out to the target, port by port.
        let mut open_to: HashMap<usize, Option<Vec<String>>> = HashMap::new();
        for source_ix in self.matching(policy, peer) {
            let Some(source) = self.groups[source_ix] else {
                continue;
            };
            let kind = self.kind_of(source);
            let (policies, node_policies) = (self.policies, self.node_policies);
            let open = open_to.entry(kind).or_insert_with(|| {
                // The source's egress must let it out on some port.
                let open: Vec<String> = ports
                    .iter()
                    .filter(|(port, protocol)| {
                        policies.egress_allows(source, target, *port, *protocol)
                    })
                    .map(|(port, protocol)| format!("{protocol} {port}"))
                    .collect();
                let egress_open = !node_policies[source_ix]
                    .as_ref()
                    .is_some_and(NodePolicy::isolated_egress);
                (!open.is_empty() || ports.is_empty() && egress_open).then_some(open)
            });
            if let Some(open) = open {
                admitted.push((source_ix, open.clone()));
                if admitted.len() > MAX_ALLOWED_SOURCES {
                    break;
                }
            }
        }
        let admitted = Rc::new(admitted);
        self.admitted.insert(key, Rc::clone(&admitted));
        admitted
    }
}

#[cfg(test)]
mod reference;
#[cfg(test)]
pub(super) use reference::apply as reference_apply;

/// Whether a node is something NetworkPolicies select (pod or workload).
pub fn applies_to(node: &Node) -> bool {
    matches!(node.kind.class(), NodeClass::Pod | NodeClass::Workload) && node.namespace.is_some()
}

#[cfg(test)]
mod tests;
