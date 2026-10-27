//! Rows of the RBAC tables: the current user's rules and "who can" grants,
//! the nodes of the "who can" graph, plus where each grant's objects are in
//! the resource browser.

use gpui_kit::px;
use kubach_core::rbac::{
    AccessGraph, BindingRef, Grant, GraphNodeKind, RoleRefInfo, Rule, Subject,
};
use kubach_i18n::t;
use kubach_ui::{FlowNode, ResourceCell, ResourceColumn, ResourceRow, Tone};

use super::access::Reveal;

const RBAC_GROUP: &str = "rbac.authorization.k8s.io";

/// `a, b, c`, or a muted `—` when empty.
fn list_cell(items: &[String], empty: &str) -> ResourceCell {
    if items.is_empty() {
        ResourceCell::text(empty.to_owned()).muted()
    } else {
        ResourceCell::text(items.join(", "))
    }
}

/// API groups with the core group (`""`) named.
fn groups(rule: &Rule) -> Vec<String> {
    rule.api_groups
        .iter()
        .map(|group| match group.as_str() {
            "" => "core".to_owned(),
            group => group.to_owned(),
        })
        .collect()
}

/// Headers of the columns of the "who can" graph.
pub fn graph_columns() -> [String; 4] {
    [
        t!("access-graph-subjects"),
        t!("access-graph-bindings"),
        t!("access-graph-roles"),
        t!("access-graph-rules"),
    ]
}

pub fn rule_columns() -> Vec<ResourceColumn> {
    vec![
        ResourceColumn::new("resources", t!("column-resources"))
            .sortable()
            .width(px(280.)),
        ResourceColumn::new("verbs", t!("access-column-verbs"))
            .sortable()
            .width(px(240.)),
        ResourceColumn::new("groups", t!("access-column-groups"))
            .sortable()
            .width(px(180.)),
        ResourceColumn::new("names", t!("access-column-names"))
            .sortable()
            .width(px(160.)),
    ]
}

pub fn rule_rows(rules: &[Rule]) -> Vec<ResourceRow> {
    rules
        .iter()
        .enumerate()
        .map(|(ix, rule)| {
            ResourceRow::new(
                ix.to_string(),
                [
                    list_cell(&rule.resources, "—"),
                    list_cell(&rule.verbs, "—"),
                    list_cell(&groups(rule), "—"),
                    list_cell(&rule.resource_names, &t!("access-any-name")),
                ],
            )
        })
        .collect()
}

/// `RoleBinding web/read-pods`.
pub fn binding_label(grant: &Grant) -> String {
    let binding = &grant.binding;
    match &binding.namespace {
        Some(namespace) => format!("{} {namespace}/{}", binding.kind.as_str(), binding.name),
        None => format!("{} {}", binding.kind.as_str(), binding.name),
    }
}

/// `Role web/pod-reader` or `ClusterRole view`.
pub fn role_label(grant: &Grant) -> String {
    let role = &grant.role;
    match &role.namespace {
        Some(namespace) => format!("{} {namespace}/{}", role.kind, role.name),
        None => format!("{} {}", role.kind, role.name),
    }
}

/// `get, list on pods, pods/log`.
pub fn rule_label(rule: &Rule) -> String {
    let verbs = rule.verbs.join(", ");
    let resources = rule.resources.join(", ");
    if rule.resource_names.is_empty() {
        t!("access-rule-label", verbs = verbs, resources = resources)
    } else {
        t!(
            "access-rule-label-named",
            verbs = verbs,
            resources = resources,
            names = rule.resource_names.join(", ")
        )
    }
}

pub fn grant_columns() -> Vec<ResourceColumn> {
    vec![
        ResourceColumn::new("subject", t!("access-subject"))
            .sortable()
            .width(px(240.)),
        ResourceColumn::new("kind", t!("column-kind"))
            .sortable()
            .width(px(130.)),
        ResourceColumn::new("binding", t!("access-binding"))
            .sortable()
            .width(px(300.)),
        ResourceColumn::new("role", t!("access-role"))
            .sortable()
            .width(px(260.)),
    ]
}

pub fn grant_rows(grants: &[Grant]) -> Vec<ResourceRow> {
    grants
        .iter()
        .enumerate()
        .map(|(ix, grant)| {
            ResourceRow::new(
                ix.to_string(),
                [
                    ResourceCell::text(grant.subject.display_name()),
                    ResourceCell::text(grant.subject.kind()),
                    ResourceCell::text(binding_label(grant)),
                    ResourceCell::text(role_label(grant)),
                ],
            )
        })
        .collect()
}

fn binding_reveal(binding: &BindingRef) -> Reveal {
    Reveal {
        group: RBAC_GROUP.into(),
        kind: binding.kind.as_str().into(),
        namespace: binding.namespace.clone(),
        name: binding.name.clone(),
    }
}

fn role_reveal(role: &RoleRefInfo) -> Reveal {
    Reveal {
        group: RBAC_GROUP.into(),
        kind: role.kind.clone(),
        namespace: role.namespace.clone(),
        name: role.name.clone(),
    }
}

/// The ServiceAccount object; users and groups are not objects.
fn subject_reveal(subject: &Subject) -> Option<Reveal> {
    match subject {
        Subject::ServiceAccount { namespace, name } => Some(Reveal {
            group: String::new(),
            kind: "ServiceAccount".into(),
            namespace: Some(namespace.clone()),
            name: name.clone(),
        }),
        Subject::User(_) | Subject::Group(_) => None,
    }
}

/// The objects of a grant that can be opened: binding, role and, for a
/// ServiceAccount subject, the account. Users and groups are not objects.
pub fn grant_objects(grant: &Grant) -> Vec<(String, Reveal)> {
    let mut objects = vec![
        (t!("access-open-binding"), binding_reveal(&grant.binding)),
        (t!("access-open-role"), role_reveal(&grant.role)),
    ];
    if let Some(account) = subject_reveal(&grant.subject) {
        objects.push((t!("access-open-service-account"), account));
    }
    objects
}

/// What clicking a graph node opens; a rule opens its role.
pub fn node_reveal(kind: &GraphNodeKind) -> Option<Reveal> {
    match kind {
        GraphNodeKind::Subject(subject) => subject_reveal(subject),
        GraphNodeKind::Binding(binding) => Some(binding_reveal(binding)),
        GraphNodeKind::Role(role) | GraphNodeKind::Rule { role, .. } => Some(role_reveal(role)),
    }
}

/// `namespace/name`, or `name` for cluster-scoped objects.
fn qualified(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => format!("{namespace}/{name}"),
        None => name.to_owned(),
    }
}

/// Boxes of the "who can" graph, in the graph's node order.
pub fn graph_nodes(graph: &AccessGraph) -> Vec<FlowNode> {
    graph
        .nodes
        .iter()
        .map(|node| {
            let (title, subtitle, tone) = match &node.kind {
                GraphNodeKind::Subject(subject) => {
                    let tone = match subject {
                        Subject::User(_) => Tone::Info,
                        Subject::Group(_) => Tone::Warning,
                        Subject::ServiceAccount { .. } => Tone::Success,
                    };
                    (subject.display_name(), subject.kind().to_owned(), tone)
                }
                GraphNodeKind::Binding(binding) => (
                    qualified(binding.namespace.as_deref(), &binding.name),
                    binding.kind.as_str().to_owned(),
                    Tone::Neutral,
                ),
                GraphNodeKind::Role(role) => (
                    qualified(role.namespace.as_deref(), &role.name),
                    role.kind.clone(),
                    Tone::Neutral,
                ),
                GraphNodeKind::Rule { rule, .. } => (
                    rule_label(rule),
                    t!("access-rule-groups", groups = groups(rule).join(", ")),
                    Tone::Neutral,
                ),
            };
            FlowNode::new(node.column, node.row, title)
                .subtitle(subtitle)
                .tone(tone)
                .clickable(node_reveal(&node.kind).is_some())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use kubach_core::rbac::{BindingKind, BindingRef, Grant, RoleRefInfo, Rule, Subject};

    use kubach_core::rbac::{AccessGraph, GraphNodeKind};

    use super::{
        binding_label, grant_objects, grant_rows, graph_nodes, node_reveal, role_label,
        rule_columns, rule_label, rule_rows,
    };

    fn rule() -> Rule {
        Rule {
            api_groups: vec!["".into(), "apps".into()],
            resources: vec!["pods".into(), "deployments".into()],
            verbs: vec!["get".into(), "list".into()],
            resource_names: vec![],
        }
    }

    fn grant(subject: Subject) -> Grant {
        Grant {
            subject,
            binding: BindingRef {
                kind: BindingKind::RoleBinding,
                name: "read".into(),
                namespace: Some("web".into()),
            },
            role: RoleRefInfo {
                kind: "ClusterRole".into(),
                name: "view".into(),
                namespace: None,
            },
            rule: rule(),
        }
    }

    #[test]
    fn rule_rows_name_the_core_group() {
        let rows = rule_rows(&[rule()]);
        let labels: Vec<_> = rows[0]
            .cells
            .iter()
            .map(|c| c.label().to_string())
            .collect();
        assert_eq!(
            labels,
            ["pods, deployments", "get, list", "core, apps", "any"]
        );
        assert_eq!(rows[0].cells.len(), rule_columns().len());
    }

    #[test]
    fn grant_labels() {
        let grant = grant(Subject::User("alice".into()));
        assert_eq!(binding_label(&grant), "RoleBinding web/read");
        assert_eq!(role_label(&grant), "ClusterRole view");
        assert_eq!(rule_label(&grant.rule), "get, list on pods, deployments");
        let row = &grant_rows(std::slice::from_ref(&grant))[0];
        assert_eq!(row.cells[0].label().as_ref(), "alice");
        assert_eq!(row.cells[1].label().as_ref(), "User");
    }

    #[test]
    fn objects_of_a_grant() {
        let user = grant(Subject::User("alice".into()));
        let objects = grant_objects(&user);
        assert_eq!(objects.len(), 2);
        let (_, binding) = &objects[0];
        assert_eq!(binding.kind, "RoleBinding");
        assert_eq!(binding.namespace.as_deref(), Some("web"));
        let (_, role) = &objects[1];
        assert_eq!(
            (role.kind.as_str(), role.namespace.as_deref()),
            ("ClusterRole", None)
        );

        let sa = grant(Subject::ServiceAccount {
            namespace: "ci".into(),
            name: "runner".into(),
        });
        let (_, account) = &grant_objects(&sa)[2];
        assert_eq!(account.group, "");
        assert_eq!(account.kind, "ServiceAccount");
        assert_eq!(account.namespace.as_deref(), Some("ci"));
        assert_eq!(account.name, "runner");
    }

    #[test]
    fn graph_nodes_and_clicks() {
        let grants = [
            grant(Subject::User("alice".into())),
            grant(Subject::ServiceAccount {
                namespace: "ci".into(),
                name: "runner".into(),
            }),
        ];
        let graph = AccessGraph::from_grants(&grants);
        let nodes = graph_nodes(&graph);
        assert_eq!(nodes.len(), graph.nodes.len());
        let titles: Vec<(&str, &str, bool)> = nodes
            .iter()
            .map(|n| {
                (
                    n.title.as_ref(),
                    n.subtitle.as_ref().map_or("", |s| s.as_ref()),
                    n.clickable,
                )
            })
            .collect();
        assert_eq!(
            titles,
            [
                ("alice", "User", false),
                ("web/read", "RoleBinding", true),
                ("view", "ClusterRole", true),
                (
                    "get, list on pods, deployments",
                    "API groups: core, apps",
                    true
                ),
                ("ci/runner", "ServiceAccount", true),
            ]
        );
        for (node, flow) in graph.nodes.iter().zip(&nodes) {
            assert_eq!((node.column, node.row), (flow.column, flow.row));
        }

        let rule = graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, GraphNodeKind::Rule { .. }))
            .unwrap();
        let reveal = node_reveal(&rule.kind).unwrap();
        assert_eq!(
            (reveal.kind.as_str(), reveal.name.as_str()),
            ("ClusterRole", "view")
        );
    }
}
