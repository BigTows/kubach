//! Text exports of a [`TopologyGraph`] for documentation and PRs (#327):
//! Mermaid (`flowchart LR`) and Graphviz DOT.
//!
//! Both are pure functions of the graph as built (filters, detail level and
//! the network policy layer already applied): namespaces become subgraphs,
//! edge kinds get their own line style, and nodes with problems, missing
//! targets or policy isolation get a class. Names are escaped, so any object
//! name yields valid output.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use super::model::{
    Access, Edge, EdgeKind, EdgeTraffic, Node, NodeId, NodeKind, Presence, ServiceType, Severity,
    TopologyGraph,
};

/// Label of an edge the policy layer blocks.
pub const BLOCKED_LABEL: &str = "blocked by policy";

/// `Deployment`, `Service · headless`, `Secret · missing`: the kind of a
/// node with its variant and presence.
pub fn kind_text(node: &Node) -> String {
    let kind = if node.api_kind.is_empty() {
        node.kind.label()
    } else {
        &node.api_kind
    };
    let variant = match node.kind {
        NodeKind::Service(ServiceType::Headless) => Some("headless"),
        NodeKind::Service(ServiceType::NodePort) => Some("NodePort"),
        NodeKind::Service(ServiceType::LoadBalancer) => Some("LoadBalancer"),
        NodeKind::Service(ServiceType::ExternalName) => Some("ExternalName"),
        _ => None,
    };
    let presence = match node.presence {
        Presence::Present => None,
        Presence::Missing => Some("missing"),
        Presence::Unknown => Some("unknown"),
    };
    [Some(kind), variant, presence]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `host/path → port`, the first rule with a count of the rest.
fn routes_label(edge: &Edge) -> Option<String> {
    let rule = edge.attrs.routes.first()?;
    let mut text = String::new();
    if let Some(host) = rule.hosts.first() {
        text.push_str(host);
    }
    if let Some(path) = rule.path.as_deref().filter(|path| *path != "/") {
        text.push_str(path);
    }
    if let Some(port) = &rule.port {
        if !text.is_empty() {
            text.push_str(" → ");
        }
        text.push_str(&port.to_string());
    }
    let rest = edge.attrs.routes.len() - 1;
    if rest > 0 {
        let _ = write!(text, " +{rest}");
    }
    (!text.is_empty()).then_some(text)
}

/// Short text for the middle of an edge: route, endpoints, policy verdict.
pub fn edge_label(edge: &Edge) -> Option<String> {
    if let Some(policy) = &edge.attrs.policy {
        match (edge.kind, policy.access) {
            (EdgeKind::PolicyAllows, _) => {
                return Some(if policy.ports.is_empty() {
                    "allowed".to_owned()
                } else {
                    format!("allows {}", policy.ports.join(", "))
                });
            }
            (_, Access::Blocked) => return Some(BLOCKED_LABEL.to_owned()),
            _ => {}
        }
    }
    if let Some(traffic) = &edge.attrs.traffic {
        return Some(traffic_text(traffic));
    }
    match edge.kind {
        EdgeKind::RoutesTo => routes_label(edge),
        EdgeKind::Selects => edge.attrs.endpoints.map(|e| match e.not_ready {
            0 => format!("{} ready", e.ready),
            n => format!("{}/{} ready", e.ready, e.ready + n),
        }),
        _ => None,
    }
}

/// A rate for display: `0.04`, `2.5`, `12`, `1.2k`.
pub fn rate_text(value: f64) -> String {
    let value = value.max(0.);
    if value >= 1000. {
        let text = format!("{:.1}", value / 1000.);
        format!("{}k", text.trim_end_matches(".0"))
    } else if value >= 10. {
        format!("{value:.0}")
    } else if value >= 0.1 {
        let text = format!("{value:.1}");
        text.trim_end_matches(".0").to_owned()
    } else if value > 0. {
        format!("{value:.2}")
    } else {
        "0".to_owned()
    }
}

/// `12 req/s · 2.1% err · p95 84 ms`: the numbers of a traffic edge, the
/// load first.
pub fn traffic_text(traffic: &EdgeTraffic) -> String {
    let mut parts = Vec::new();
    if let Some(requests) = traffic.requests.filter(|r| *r > 0.) {
        parts.push(format!("{} req/s", rate_text(requests)));
    } else if let Some(flows) = traffic.flows.filter(|f| *f > 0.) {
        parts.push(format!("{} flows/s", rate_text(flows)));
    } else if let Some(bytes) = traffic.bytes.filter(|b| *b > 0.) {
        parts.push(format!(
            "{}/s",
            crate::metrics::prometheus::format_bytes(bytes)
        ));
    }
    if let Some(ratio) = traffic.error_ratio().filter(|r| *r > 0.) {
        let percent = ratio * 100.;
        let text = if percent >= 10. {
            format!("{percent:.0}")
        } else if percent >= 0.1 {
            format!("{percent:.1}")
        } else {
            "<0.1".to_owned()
        };
        let what = if traffic.requests.is_some_and(|r| r > 0.) {
            "err"
        } else {
            "dropped"
        };
        parts.push(format!("{text}% {what}"));
    }
    if let Some(p95) = traffic.p95_ms {
        parts.push(if p95 >= 1000. {
            format!("p95 {:.1} s", p95 / 1000.)
        } else {
            format!("p95 {p95:.0} ms")
        });
    }
    if parts.is_empty() {
        "idle".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// `isolated`, `isolated in`, `isolated out`, or `None`.
pub fn isolation_text(node: &Node) -> Option<&'static str> {
    let policy = node.attrs.network_policy.as_ref()?;
    match (policy.isolated_ingress(), policy.isolated_egress()) {
        (true, true) => Some("isolated"),
        (true, false) => Some("isolated in"),
        (false, true) => Some("isolated out"),
        (false, false) => None,
    }
}

/// Class of a node in the exports, the most important first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Error,
    Warning,
    Missing,
    Unknown,
    Isolated,
    Plain,
}

fn look(graph: &TopologyGraph, node: &Node) -> Look {
    match graph.diagnostics_for(&node.id).map(|d| d.severity()).max() {
        Some(Severity::Error) => return Look::Error,
        Some(Severity::Warning) => return Look::Warning,
        None => {}
    }
    match node.presence {
        Presence::Missing => Look::Missing,
        Presence::Unknown => Look::Unknown,
        Presence::Present if isolation_text(node).is_some() => Look::Isolated,
        Presence::Present => Look::Plain,
    }
}

impl Look {
    fn class(self) -> Option<&'static str> {
        match self {
            Look::Error => Some("error"),
            Look::Warning => Some("warning"),
            Look::Missing => Some("missing"),
            Look::Unknown => Some("unknown"),
            Look::Isolated => Some("isolated"),
            Look::Plain => None,
        }
    }
}

/// Second line of a node: the kind and its state.
fn details(node: &Node) -> String {
    let mut parts = vec![kind_text(node)];
    if let (NodeKind::Workload(_), Some(pods)) = (node.kind, node.attrs.pods) {
        let total = node
            .attrs
            .desired_replicas
            .map_or(pods.total, |desired| desired.max(0) as u32);
        parts.push(format!("{}/{total} ready", pods.ready));
    }
    if let Some(isolation) = isolation_text(node) {
        parts.push(isolation.to_owned());
    }
    parts.join(" · ")
}

/// How an edge looks: whether the policy layer blocks it, otherwise by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    /// Traffic: routes and selections.
    Traffic,
    Owns,
    /// References to configuration.
    Reference,
    Exposes,
    Allows,
    Blocked,
}

fn line(graph: &TopologyGraph, edge: &Edge) -> Line {
    if edge
        .attrs
        .policy
        .as_ref()
        .is_some_and(|policy| policy.access == Access::Blocked)
    {
        return Line::Blocked;
    }
    if graph
        .node(&edge.to)
        .is_some_and(|node| node.presence == Presence::Missing)
    {
        return Line::Blocked;
    }
    match edge.kind {
        EdgeKind::RoutesTo | EdgeKind::Selects | EdgeKind::Traffic => Line::Traffic,
        EdgeKind::Owns => Line::Owns,
        EdgeKind::Exposes => Line::Exposes,
        EdgeKind::PolicyAllows => Line::Allows,
        _ => Line::Reference,
    }
}

// Export colors: the files leave the app, so they carry their own neutral
// palette instead of the UI theme.
const RED: &str = "#e5484d";
const AMBER: &str = "#f5a524";
const GREEN: &str = "#30a46c";
const BLUE: &str = "#3e63dd";
const GRAY: &str = "#8b8d98";

/// Nodes by namespace (`None`: outside any, i.e. external addresses), in
/// graph order.
fn by_namespace(graph: &TopologyGraph) -> BTreeMap<Option<&str>, Vec<(usize, &Node)>> {
    let mut groups: BTreeMap<Option<&str>, Vec<(usize, &Node)>> = BTreeMap::new();
    for (ix, node) in graph.nodes().iter().enumerate() {
        groups
            .entry(node.namespace.as_deref())
            .or_default()
            .push((ix, node));
    }
    groups
}

// ---------------------------------------------------------------- Mermaid

/// Text inside a Mermaid `"…"` label: quotes, `#`, `<`, `>` and `&` as
/// entity codes, line breaks as `<br/>`.
pub fn mermaid_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("#quot;"),
            '#' => out.push_str("#35;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '&' => out.push_str("#amp;"),
            '|' => out.push_str("#124;"),
            '`' => out.push_str("#96;"),
            '\n' => out.push_str("<br/>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// A Mermaid `flowchart LR` of the graph. Node ids are `n<index>` (the
/// graph is sorted by id, so they are stable for the same graph); each
/// namespace is a subgraph.
pub fn to_mermaid(graph: &TopologyGraph) -> String {
    let mut out = String::from("flowchart LR\n");
    let _ = writeln!(
        out,
        "  %% Kubach topology: {} nodes, {} edges",
        graph.nodes().len(),
        graph.edges().len()
    );
    let ids: HashMap<&NodeId, usize> = graph
        .nodes()
        .iter()
        .enumerate()
        .map(|(ix, node)| (&node.id, ix))
        .collect();
    let node_line = |out: &mut String, indent: &str, ix: usize, node: &Node| {
        let label = format!(
            "<b>{}</b><br/><small>{}</small>",
            mermaid_escape(&node.name),
            mermaid_escape(&details(node))
        );
        let (open, close) = match node.kind {
            NodeKind::External(_) => ("([", "])"),
            NodeKind::Service(_) => ("[[", "]]"),
            NodeKind::ConfigMap
            | NodeKind::Secret
            | NodeKind::PersistentVolumeClaim
            | NodeKind::ServiceAccount => ("[(", ")]"),
            _ => ("[", "]"),
        };
        let _ = writeln!(out, "{indent}n{ix}{open}\"{label}\"{close}");
    };
    for (group, (namespace, nodes)) in by_namespace(graph).into_iter().enumerate() {
        match namespace {
            Some(namespace) => {
                let _ = writeln!(
                    out,
                    "  subgraph ns{group}[\"namespace {}\"]",
                    mermaid_escape(namespace)
                );
                out.push_str("    direction LR\n");
                for (ix, node) in nodes {
                    node_line(&mut out, "    ", ix, node);
                }
                out.push_str("  end\n");
            }
            None => {
                for (ix, node) in nodes {
                    node_line(&mut out, "  ", ix, node);
                }
            }
        }
    }
    // Link styles go by declaration order.
    let mut styles: Vec<Line> = Vec::new();
    for edge in graph.edges() {
        let (Some(from), Some(to)) = (ids.get(&edge.from), ids.get(&edge.to)) else {
            continue;
        };
        let look = line(graph, edge);
        let arrow = match look {
            Line::Traffic | Line::Allows => "==>",
            Line::Owns | Line::Exposes => "-->",
            Line::Reference | Line::Blocked => "-.->",
        };
        let label = edge_label(edge)
            .map(|label| format!("|\"{}\"|", mermaid_escape(&label)))
            .unwrap_or_default();
        let _ = writeln!(out, "  n{from} {arrow}{label} n{to}");
        styles.push(look);
    }
    let _ = writeln!(out, "  classDef error stroke:{RED},stroke-width:2px");
    let _ = writeln!(out, "  classDef warning stroke:{AMBER},stroke-width:2px");
    let _ = writeln!(
        out,
        "  classDef missing stroke:{RED},stroke-width:2px,stroke-dasharray:5 3"
    );
    let _ = writeln!(out, "  classDef unknown stroke:{GRAY},stroke-dasharray:3 3");
    let _ = writeln!(out, "  classDef isolated stroke:{BLUE},stroke-width:2px");
    let mut classes: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (ix, node) in graph.nodes().iter().enumerate() {
        if let Some(class) = look(graph, node).class() {
            classes.entry(class).or_default().push(format!("n{ix}"));
        }
    }
    for (class, nodes) in classes {
        let _ = writeln!(out, "  class {} {class}", nodes.join(","));
    }
    for (color, which) in [
        (RED, Line::Blocked),
        (GREEN, Line::Allows),
        (GRAY, Line::Owns),
    ] {
        let edges: Vec<String> = styles
            .iter()
            .enumerate()
            .filter(|(_, look)| **look == which)
            .map(|(position, _)| position.to_string())
            .collect();
        if !edges.is_empty() {
            let _ = writeln!(out, "  linkStyle {} stroke:{color}", edges.join(","));
        }
    }
    out
}

// ---------------------------------------------------------------- DOT

/// A DOT double-quoted string: backslashes and quotes escaped, line breaks
/// as `\n`.
pub fn dot_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Graphviz digraph of the graph (`dot -Tsvg`). Node ids are the stable
/// topology ids; each namespace is a `cluster_` subgraph.
pub fn to_dot(graph: &TopologyGraph) -> String {
    let mut out = String::from("digraph topology {\n");
    let _ = writeln!(
        out,
        "  // Kubach topology: {} nodes, {} edges",
        graph.nodes().len(),
        graph.edges().len()
    );
    out.push_str("  rankdir=LR;\n");
    out.push_str("  compound=true;\n");
    out.push_str("  fontname=\"Helvetica\";\n");
    out.push_str("  node [shape=box, style=\"rounded\", fontname=\"Helvetica\", fontsize=11];\n");
    out.push_str(&format!(
        "  edge [fontname=\"Helvetica\", fontsize=9, color=\"{GRAY}\", fontcolor=\"{GRAY}\"];\n"
    ));
    let node_line = |out: &mut String, indent: &str, node: &Node| {
        let label = format!("{}\n{}", node.name, details(node));
        let mut attrs = vec![format!("label={}", dot_quote(&label))];
        let shape = match node.kind {
            NodeKind::External(_) => Some("ellipse"),
            NodeKind::ConfigMap | NodeKind::Secret | NodeKind::ServiceAccount => Some("note"),
            NodeKind::PersistentVolumeClaim => Some("cylinder"),
            _ => None,
        };
        if let Some(shape) = shape {
            attrs.push(format!("shape={shape}"));
        }
        let mut style = vec!["rounded"];
        match look(graph, node) {
            Look::Error => attrs.push(format!("color=\"{RED}\", penwidth=2")),
            Look::Warning => attrs.push(format!("color=\"{AMBER}\", penwidth=2")),
            Look::Missing => {
                attrs.push(format!("color=\"{RED}\", penwidth=2"));
                style.push("dashed");
            }
            Look::Unknown => {
                attrs.push(format!("color=\"{GRAY}\""));
                style.push("dashed");
            }
            Look::Isolated => attrs.push(format!("color=\"{BLUE}\", penwidth=2")),
            Look::Plain => {}
        }
        if shape.is_none() || style.len() > 1 {
            attrs.push(format!("style={}", dot_quote(&style.join(","))));
        }
        let _ = writeln!(
            out,
            "{indent}{} [{}];",
            dot_quote(node.id.as_str()),
            attrs.join(", ")
        );
    };
    for (namespace, nodes) in by_namespace(graph) {
        match namespace {
            Some(namespace) => {
                let _ = writeln!(
                    out,
                    "  subgraph {} {{",
                    dot_quote(&format!("cluster_{namespace}"))
                );
                let _ = writeln!(
                    out,
                    "    label={};",
                    dot_quote(&format!("namespace {namespace}"))
                );
                out.push_str("    style=\"rounded,dashed\";\n");
                let _ = writeln!(out, "    color=\"{GRAY}\";");
                for (_, node) in nodes {
                    node_line(&mut out, "    ", node);
                }
                out.push_str("  }\n");
            }
            None => {
                for (_, node) in nodes {
                    node_line(&mut out, "  ", node);
                }
            }
        }
    }
    for edge in graph.edges() {
        let mut attrs = Vec::new();
        if let Some(label) = edge_label(edge) {
            attrs.push(format!("label={}", dot_quote(&label)));
        }
        match line(graph, edge) {
            Line::Traffic => attrs.push("penwidth=1.6".to_owned()),
            Line::Owns => attrs.push("arrowhead=odiamond".to_owned()),
            Line::Reference => attrs.push("style=dashed".to_owned()),
            Line::Exposes => {}
            Line::Allows => attrs.push(format!(
                "color=\"{GREEN}\", fontcolor=\"{GREEN}\", penwidth=1.6"
            )),
            Line::Blocked => attrs.push(format!(
                "color=\"{RED}\", fontcolor=\"{RED}\", style=dashed"
            )),
        }
        attrs.push(format!("tooltip={}", dot_quote(edge.kind.as_str())));
        let _ = writeln!(
            out,
            "  {} -> {} [{}];",
            dot_quote(edge.from.as_str()),
            dot_quote(edge.to.as_str()),
            attrs.join(", ")
        );
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::{dot_quote, mermaid_escape};

    #[test]
    fn mermaid_labels_are_escaped() {
        assert_eq!(
            mermaid_escape("a\"b#c<d>&|`\nx"),
            "a#quot;b#35;c#lt;d#gt;#amp;#124;#96;<br/>x"
        );
    }

    #[test]
    fn dot_strings_are_quoted() {
        assert_eq!(dot_quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }
}
