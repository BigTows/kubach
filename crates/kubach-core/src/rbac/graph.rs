//! The "who can" answer as a graph: subject → binding → role → rule.
//!
//! Grants that share a binding, role or rule share its node, so a binding
//! with three subjects is one node with three incoming edges. Nodes are laid
//! out in four columns, left to right; rows inside a column are ordered to
//! keep edges short (each node sits near the mean row of its neighbors in
//! the binding column).

use std::collections::{BTreeSet, HashMap};

use super::rules::Rule;
use super::who_can::{BindingRef, Grant, RoleRefInfo, Subject};

/// Column titles, in column order.
pub const GRAPH_COLUMNS: [&str; 4] = ["Subjects", "Bindings", "Roles", "Rules"];

const SUBJECTS: usize = 0;
const BINDINGS: usize = 1;
const ROLES: usize = 2;
const RULES: usize = 3;

/// What a node stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphNodeKind {
    Subject(Subject),
    Binding(BindingRef),
    Role(RoleRefInfo),
    /// The role's first rule that grants the access.
    Rule {
        role: RoleRefInfo,
        rule: Rule,
    },
}

impl GraphNodeKind {
    /// Index into [`GRAPH_COLUMNS`].
    pub fn column(&self) -> usize {
        match self {
            GraphNodeKind::Subject(_) => SUBJECTS,
            GraphNodeKind::Binding(_) => BINDINGS,
            GraphNodeKind::Role(_) => ROLES,
            GraphNodeKind::Rule { .. } => RULES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    pub kind: GraphNodeKind,
    /// Index into [`GRAPH_COLUMNS`].
    pub column: usize,
    /// Position inside the column, from the top, without gaps.
    pub row: usize,
}

/// An edge between node indices, always from a column to the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
}

/// Deduplicated, laid-out graph of grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessGraph {
    pub nodes: Vec<GraphNode>,
    /// Sorted, without duplicates.
    pub edges: Vec<GraphEdge>,
}

impl AccessGraph {
    pub fn from_grants(grants: &[Grant]) -> Self {
        let mut builder = Builder::default();
        for grant in grants {
            let subject = builder.node(GraphNodeKind::Subject(grant.subject.clone()));
            let binding = builder.node(GraphNodeKind::Binding(grant.binding.clone()));
            let role = builder.node(GraphNodeKind::Role(grant.role.clone()));
            let rule = builder.node(GraphNodeKind::Rule {
                role: grant.role.clone(),
                rule: grant.rule.clone(),
            });
            builder.edges.insert(GraphEdge {
                from: subject,
                to: binding,
            });
            builder.edges.insert(GraphEdge {
                from: binding,
                to: role,
            });
            builder.edges.insert(GraphEdge {
                from: role,
                to: rule,
            });
        }
        let mut graph = AccessGraph {
            nodes: builder.nodes,
            edges: builder.edges.into_iter().collect(),
        };
        graph.layout();
        graph
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Number of nodes in `column`.
    pub fn column_len(&self, column: usize) -> usize {
        self.nodes.iter().filter(|n| n.column == column).count()
    }

    /// Rows of the tallest column.
    pub fn height(&self) -> usize {
        (0..GRAPH_COLUMNS.len())
            .map(|c| self.column_len(c))
            .max()
            .unwrap_or(0)
    }

    /// Node indices of `column`, top to bottom.
    pub fn column(&self, column: usize) -> Vec<usize> {
        let mut ixs: Vec<usize> = (0..self.nodes.len())
            .filter(|&ix| self.nodes[ix].column == column)
            .collect();
        ixs.sort_by_key(|&ix| self.nodes[ix].row);
        ixs
    }

    /// Every node on a chain through `ix`: all it reaches to the right and
    /// everything that reaches it from the left, `ix` included. Sorted.
    pub fn chain(&self, ix: usize) -> Vec<usize> {
        let mut found = BTreeSet::from([ix]);
        for forward in [true, false] {
            let mut stack = vec![ix];
            while let Some(current) = stack.pop() {
                for edge in &self.edges {
                    let (here, next) = if forward {
                        (edge.from, edge.to)
                    } else {
                        (edge.to, edge.from)
                    };
                    if here == current && found.insert(next) {
                        stack.push(next);
                    }
                }
            }
        }
        found.into_iter().collect()
    }

    /// Indices of the edges with both ends in `nodes` (sorted).
    pub fn edges_within(&self, nodes: &[usize]) -> Vec<usize> {
        (0..self.edges.len())
            .filter(|&e| {
                let edge = self.edges[e];
                nodes.binary_search(&edge.from).is_ok() && nodes.binary_search(&edge.to).is_ok()
            })
            .collect()
    }

    /// Bindings keep the order of the grants (ClusterRoleBindings first);
    /// subjects and roles sit at the mean row of their bindings, rules at
    /// the row of their role.
    fn layout(&mut self) {
        // Bindings: first appearance, which is node creation order.
        let bindings = self.column(BINDINGS);
        for (row, ix) in bindings.iter().enumerate() {
            self.nodes[*ix].row = row;
        }
        let rows: Vec<usize> = self.nodes.iter().map(|n| n.row).collect();
        self.order_by_neighbors(SUBJECTS, &rows, false);
        self.order_by_neighbors(ROLES, &rows, true);
        let rows: Vec<usize> = self.nodes.iter().map(|n| n.row).collect();
        self.order_by_neighbors(RULES, &rows, true);
    }

    /// Sorts `column` by the mean row of its neighbors in the adjacent
    /// column (left when `incoming`, right otherwise), ties by label.
    fn order_by_neighbors(&mut self, column: usize, rows: &[usize], incoming: bool) {
        let mut keyed: Vec<(f64, String, usize)> = self
            .column(column)
            .into_iter()
            .map(|ix| {
                let neighbors: Vec<usize> = self
                    .edges
                    .iter()
                    .filter_map(|e| {
                        if incoming && e.to == ix {
                            Some(rows[e.from])
                        } else if !incoming && e.from == ix {
                            Some(rows[e.to])
                        } else {
                            None
                        }
                    })
                    .collect();
                let mean = if neighbors.is_empty() {
                    f64::MAX
                } else {
                    neighbors.iter().sum::<usize>() as f64 / neighbors.len() as f64
                };
                (mean, sort_label(&self.nodes[ix].kind), ix)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for (row, (_, _, ix)) in keyed.into_iter().enumerate() {
            self.nodes[ix].row = row;
        }
    }
}

fn sort_label(kind: &GraphNodeKind) -> String {
    match kind {
        GraphNodeKind::Subject(subject) => format!("{} {}", subject.kind(), subject.display_name()),
        GraphNodeKind::Binding(binding) => binding.name.clone(),
        GraphNodeKind::Role(role) | GraphNodeKind::Rule { role, .. } => {
            format!("{} {}", role.kind, role.name)
        }
    }
}

#[derive(Default)]
struct Builder {
    nodes: Vec<GraphNode>,
    index: HashMap<NodeKey, usize>,
    edges: BTreeSet<GraphEdge>,
}

/// Identity of a node. A rule is identified by its role: a role contributes
/// one granting rule.
#[derive(PartialEq, Eq, Hash)]
enum NodeKey {
    Subject(Subject),
    Binding(BindingRef),
    Role(RoleRefInfo),
    Rule(RoleRefInfo),
}

impl Builder {
    fn node(&mut self, kind: GraphNodeKind) -> usize {
        let key = match &kind {
            GraphNodeKind::Subject(s) => NodeKey::Subject(s.clone()),
            GraphNodeKind::Binding(b) => NodeKey::Binding(b.clone()),
            GraphNodeKind::Role(r) => NodeKey::Role(r.clone()),
            GraphNodeKind::Rule { role, .. } => NodeKey::Rule(role.clone()),
        };
        if let Some(&ix) = self.index.get(&key) {
            return ix;
        }
        let ix = self.nodes.len();
        self.nodes.push(GraphNode {
            column: kind.column(),
            kind,
            row: 0,
        });
        self.index.insert(key, ix);
        ix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbac::BindingKind;

    fn rule(resource: &str) -> Rule {
        Rule {
            api_groups: vec![String::new()],
            resources: vec![resource.into()],
            verbs: vec!["get".into()],
            resource_names: vec![],
        }
    }

    fn binding(kind: BindingKind, name: &str) -> BindingRef {
        BindingRef {
            kind,
            name: name.into(),
            namespace: (kind == BindingKind::RoleBinding).then(|| "web".into()),
        }
    }

    fn role(kind: &str, name: &str) -> RoleRefInfo {
        RoleRefInfo {
            kind: kind.into(),
            name: name.into(),
            namespace: (kind == "Role").then(|| "web".into()),
        }
    }

    fn grant(subject: Subject, binding: BindingRef, role: RoleRefInfo, rule: Rule) -> Grant {
        Grant {
            subject,
            binding,
            role,
            rule,
        }
    }

    fn user(name: &str) -> Subject {
        Subject::User(name.into())
    }

    /// admins (CRB) → cluster-admin; read-pods (RB) → pod-reader;
    /// view (RB) → view (ClusterRole); alice is in two bindings.
    fn grants() -> Vec<Grant> {
        let admins = binding(BindingKind::ClusterRoleBinding, "admins");
        let read = binding(BindingKind::RoleBinding, "read-pods");
        let view = binding(BindingKind::RoleBinding, "view");
        let admin_role = role("ClusterRole", "cluster-admin");
        let reader = role("Role", "pod-reader");
        let view_role = role("ClusterRole", "view");
        vec![
            grant(
                Subject::Group("ops".into()),
                admins.clone(),
                admin_role.clone(),
                rule("*"),
            ),
            grant(user("zed"), admins, admin_role, rule("*")),
            grant(user("alice"), read.clone(), reader.clone(), rule("pods")),
            grant(
                Subject::ServiceAccount {
                    namespace: "web".into(),
                    name: "ci".into(),
                },
                read,
                reader,
                rule("pods"),
            ),
            grant(user("alice"), view, view_role, rule("pods")),
        ]
    }

    fn labels(graph: &AccessGraph, column: usize) -> Vec<String> {
        graph
            .column(column)
            .into_iter()
            .map(|ix| sort_label(&graph.nodes[ix].kind))
            .collect()
    }

    #[test]
    fn deduplicates_nodes_and_edges() {
        let graph = AccessGraph::from_grants(&grants());
        assert_eq!(
            [0, 1, 2, 3].map(|c| graph.column_len(c)),
            [4, 3, 3, 3],
            "ops, zed, alice, ci; three bindings, roles and rules"
        );
        // 5 subject edges (alice twice), 3 binding → role, 3 role → rule.
        assert_eq!(graph.edges.len(), 11);
        let mut sorted = graph.edges.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, graph.edges);
        assert_eq!(graph.height(), 4);

        let doubled = [grants(), grants()].concat();
        assert_eq!(AccessGraph::from_grants(&doubled), graph);
    }

    #[test]
    fn edges_go_one_column_right() {
        let graph = AccessGraph::from_grants(&grants());
        for edge in &graph.edges {
            assert_eq!(
                graph.nodes[edge.from].column + 1,
                graph.nodes[edge.to].column
            );
        }
    }

    #[test]
    fn rows_are_dense_per_column() {
        let graph = AccessGraph::from_grants(&grants());
        for column in 0..GRAPH_COLUMNS.len() {
            let rows: Vec<usize> = graph
                .column(column)
                .into_iter()
                .map(|ix| graph.nodes[ix].row)
                .collect();
            assert_eq!(rows, (0..rows.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn layout_follows_bindings() {
        let graph = AccessGraph::from_grants(&grants());
        assert_eq!(labels(&graph, BINDINGS), ["admins", "read-pods", "view"]);
        // ops and zed sit at row 0 (by label), ci at 1, alice at 1.5.
        assert_eq!(
            labels(&graph, SUBJECTS),
            [
                "Group ops",
                "User zed",
                "ServiceAccount web/ci",
                "User alice"
            ]
        );
        assert_eq!(
            labels(&graph, ROLES),
            [
                "ClusterRole cluster-admin",
                "Role pod-reader",
                "ClusterRole view"
            ]
        );
        assert_eq!(labels(&graph, RULES), labels(&graph, ROLES));
    }

    #[test]
    fn rule_node_keeps_the_rule() {
        let graph = AccessGraph::from_grants(&grants());
        let ix = graph.column(RULES)[1];
        let GraphNodeKind::Rule { role, rule: r } = &graph.nodes[ix].kind else {
            panic!("not a rule");
        };
        assert_eq!(role.name, "pod-reader");
        assert_eq!(r, &rule("pods"));
    }

    #[test]
    fn shared_role_has_one_rule_node() {
        let shared = role("ClusterRole", "view");
        let grants = vec![
            grant(
                user("a"),
                binding(BindingKind::RoleBinding, "one"),
                shared.clone(),
                rule("pods"),
            ),
            grant(
                user("b"),
                binding(BindingKind::RoleBinding, "two"),
                shared,
                rule("pods"),
            ),
        ];
        let graph = AccessGraph::from_grants(&grants);
        assert_eq!([0, 1, 2, 3].map(|c| graph.column_len(c)), [2, 2, 1, 1]);
    }

    #[test]
    fn chain_through_a_node() {
        let graph = AccessGraph::from_grants(&grants());
        let find = |label: &str| {
            (0..graph.nodes.len())
                .find(|&ix| sort_label(&graph.nodes[ix].kind) == label)
                .unwrap()
        };
        let chain = graph.chain(find("User alice"));
        let names: Vec<String> = chain
            .iter()
            .map(|&ix| sort_label(&graph.nodes[ix].kind))
            .collect();
        assert_eq!(names.len(), 7, "{names:?}");
        assert!(!names.contains(&"admins".to_owned()));
        assert!(!names.contains(&"ServiceAccount web/ci".to_owned()));
        // alice → 2 bindings → 2 roles → 2 rules.
        assert_eq!(graph.edges_within(&chain).len(), 6);

        // From a role: every subject that reaches it, and its rule.
        let chain = graph.chain(find("Role pod-reader"));
        assert_eq!(chain.len(), 5);
    }

    #[test]
    fn empty() {
        let graph = AccessGraph::from_grants(&[]);
        assert!(graph.is_empty());
        assert_eq!(graph.height(), 0);
        assert!(graph.column(0).is_empty());
    }
}
