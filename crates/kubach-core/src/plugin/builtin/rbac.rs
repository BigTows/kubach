//! RBAC: Role and ClusterRole (rules as a table, aggregation), RoleBinding
//! and ClusterRoleBinding (the role and the subjects, with links).

use k8s_openapi::api::rbac::v1::{
    AggregationRule, ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef,
    Subject,
};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, col, creation, decode, fallback_cells, name_ns_cells, name_ns_columns,
    object_ref, or_none, references, relations_with, run_delete, section, selector_string, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation, ResourcePlugin,
    SectionBody, Table,
};

const GROUP: &str = "rbac.authorization.k8s.io";
const API_VERSION: &str = "rbac.authorization.k8s.io/v1";

/// Role (namespaced) or ClusterRole.
pub struct RolePlugin {
    cluster: bool,
}

/// RoleBinding (namespaced) or ClusterRoleBinding.
pub struct RoleBindingPlugin {
    cluster: bool,
}

impl RolePlugin {
    pub const ROLE: Self = Self { cluster: false };
    pub const CLUSTER_ROLE: Self = Self { cluster: true };
}

impl RoleBindingPlugin {
    pub const ROLE_BINDING: Self = Self { cluster: false };
    pub const CLUSTER_ROLE_BINDING: Self = Self { cluster: true };
}

/// Columns with a namespace for the namespaced kind only.
fn base_columns(cluster: bool) -> Vec<Column> {
    if cluster {
        vec![col("name", "Name", ColumnKind::Text)]
    } else {
        name_ns_columns()
    }
}

fn base_cells(obj: &DynamicObject, cluster: bool) -> Vec<Cell> {
    if cluster {
        vec![text(obj.metadata.name.clone().unwrap_or_default())]
    } else {
        name_ns_cells(obj)
    }
}

/// A configuration object: present means in effect.
fn active() -> Health {
    Health::phrase(HealthStatus::Healthy, msg!("state-active"))
}

// ----------------------------------------------------------------------- Roles

/// Rules and, for a ClusterRole, its aggregation rule.
fn role_rules(
    obj: &DynamicObject,
    cluster: bool,
) -> Option<(Vec<PolicyRule>, Option<AggregationRule>)> {
    if cluster {
        let role = decode::<ClusterRole>(obj)?;
        Some((role.rules.unwrap_or_default(), role.aggregation_rule))
    } else {
        let role = decode::<Role>(obj)?;
        Some((role.rules.unwrap_or_default(), None))
    }
}

fn list(items: Option<&Vec<String>>) -> String {
    items.map(|v| v.join(", ")).unwrap_or_default()
}

/// One table row per rule: API groups, resources (or non-resource URLs),
/// verbs and resource names. The core group is written as `core`.
fn rule_row(rule: &PolicyRule) -> Vec<String> {
    let groups = rule
        .api_groups
        .iter()
        .flatten()
        .map(|g| if g.is_empty() { "core" } else { g.as_str() })
        .collect::<Vec<_>>()
        .join(", ");
    let resources = match (&rule.resources, &rule.non_resource_urls) {
        (Some(r), _) if !r.is_empty() => r.join(", "),
        (_, Some(urls)) if !urls.is_empty() => urls.join(", "),
        _ => String::new(),
    };
    vec![
        groups,
        resources,
        rule.verbs.join(", "),
        list(rule.resource_names.as_ref()),
    ]
}

fn rules_table(rules: &[PolicyRule]) -> Table {
    Table::new(
        &["API groups", "Resources", "Verbs", "Resource names"],
        rules.iter().map(rule_row).collect(),
    )
}

impl ResourcePlugin for RolePlugin {
    fn name(&self) -> &'static str {
        if self.cluster { "clusterrole" } else { "role" }
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        let kind = if self.cluster { "ClusterRole" } else { "Role" };
        vec![GroupVersionKind::gvk(GROUP, "v1", kind)]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = base_columns(self.cluster);
        cols.extend([
            col("rules", "Rules", ColumnKind::Number),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some((rules, _)) = role_rules(obj, self.cluster) else {
            return fallback_cells(obj, &self.columns());
        };
        let mut cells = base_cells(obj, self.cluster);
        cells.extend([Cell::Number(rules.len() as i64), Cell::Age(creation(obj))]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some((rules, aggregation)) = role_rules(obj, self.cluster) else {
            return GenericPlugin.details(obj);
        };
        let mut fields = vec![Field::text("Rules", rules.len().to_string())];
        if let Some(aggregation) = &aggregation {
            let selectors: Vec<String> = aggregation
                .cluster_role_selectors
                .iter()
                .flatten()
                .filter_map(selector_string)
                .collect();
            fields.push(Field::text("Aggregates", or_none(selectors.join("; "))));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        if !rules.is_empty() {
            let title = if aggregation.is_some() {
                "Rules (aggregated)"
            } else {
                "Rules"
            };
            sections.push(section(title, SectionBody::Table(rules_table(&rules))));
        }
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
        if self.cluster {
            run_delete::<ClusterRole>(client, obj, action_id)
        } else {
            run_delete::<Role>(client, obj, action_id)
        }
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        active()
    }
}

// -------------------------------------------------------------------- Bindings

fn binding_parts(obj: &DynamicObject, cluster: bool) -> Option<(RoleRef, Vec<Subject>)> {
    if cluster {
        let b = decode::<ClusterRoleBinding>(obj)?;
        Some((b.role_ref, b.subjects.unwrap_or_default()))
    } else {
        let b = decode::<RoleBinding>(obj)?;
        Some((b.role_ref, b.subjects.unwrap_or_default()))
    }
}

/// The bound role: a ClusterRole, or a Role in the binding's namespace.
fn role_target(obj: &DynamicObject, role: &RoleRef) -> ObjectRef {
    let ns = (role.kind == "Role")
        .then_some(obj.metadata.namespace.as_deref())
        .flatten();
    object_ref(API_VERSION, &role.kind, &role.name, ns)
}

/// A ServiceAccount subject as an object; its namespace defaults to the
/// binding's.
fn service_account(obj: &DynamicObject, subject: &Subject) -> Option<ObjectRef> {
    (subject.kind == "ServiceAccount").then(|| {
        let ns = subject
            .namespace
            .as_deref()
            .or(obj.metadata.namespace.as_deref());
        object_ref("v1", "ServiceAccount", &subject.name, ns)
    })
}

/// `SA kube-system/default`, `User alice`, `Group system:masters`.
fn subject_label(obj: &DynamicObject, subject: &Subject) -> String {
    match service_account(obj, subject) {
        Some(sa) => match sa.namespace {
            Some(ns) => format!("SA {ns}/{}", sa.name),
            None => format!("SA {}", sa.name),
        },
        None => format!("{} {}", subject.kind, subject.name),
    }
}

/// The first two subjects and how many more: `User alice, SA ns/ci +3`.
fn subjects_brief(obj: &DynamicObject, subjects: &[Subject]) -> String {
    let mut out: Vec<String> = subjects
        .iter()
        .take(2)
        .map(|s| subject_label(obj, s))
        .collect();
    if subjects.len() > 2 {
        out.push(format!("+{}", subjects.len() - 2));
    }
    match out.split_last() {
        Some((last, rest)) if last.starts_with('+') => format!("{} {last}", rest.join(", ")),
        _ => out.join(", "),
    }
}

impl ResourcePlugin for RoleBindingPlugin {
    fn name(&self) -> &'static str {
        if self.cluster {
            "clusterrolebinding"
        } else {
            "rolebinding"
        }
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        let kind = if self.cluster {
            "ClusterRoleBinding"
        } else {
            "RoleBinding"
        };
        vec![GroupVersionKind::gvk(GROUP, "v1", kind)]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = base_columns(self.cluster);
        cols.extend([
            col("role", "Role", ColumnKind::Text),
            col("subjects", "Subjects", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some((role, subjects)) = binding_parts(obj, self.cluster) else {
            return fallback_cells(obj, &self.columns());
        };
        let mut cells = base_cells(obj, self.cluster);
        cells.extend([
            text(format!("{}/{}", role.kind, role.name)),
            text(subjects_brief(obj, &subjects)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some((role, subjects)) = binding_parts(obj, self.cluster) else {
            return GenericPlugin.details(obj);
        };
        let overview = vec![
            Field::object_link(
                role.kind.clone(),
                role.name.clone(),
                role_target(obj, &role),
            ),
            Field::text("Subjects", subjects.len().to_string()),
        ];
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(overview))];
        let rows: Vec<Field> = subjects
            .iter()
            .map(|s| match service_account(obj, s) {
                Some(sa) => {
                    let label = match &sa.namespace {
                        Some(ns) => format!("{ns}/{}", sa.name),
                        None => sa.name.clone(),
                    };
                    Field::object_link("ServiceAccount", label, sa)
                }
                None => Field::text(s.kind.clone(), s.name.clone()),
            })
            .collect();
        if !rows.is_empty() {
            sections.push(section("Subjects", SectionBody::Fields(rows)));
        }
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
        if self.cluster {
            run_delete::<ClusterRoleBinding>(client, obj, action_id)
        } else {
            run_delete::<RoleBinding>(client, obj, action_id)
        }
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let Some((role, subjects)) = binding_parts(obj, self.cluster) else {
            return GenericPlugin.relations(obj);
        };
        let mut extra = vec![references(role_target(obj, &role))];
        extra.extend(
            subjects
                .iter()
                .filter_map(|s| service_account(obj, s))
                .map(references),
        );
        relations_with(obj, extra)
    }

    fn health(&self, _obj: &DynamicObject) -> Health {
        active()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::FieldValue;
    use crate::plugin::builtin::test_util::{obj, section as body};

    #[test]
    fn role_rules_table() {
        let p = RolePlugin::ROLE;
        let o = obj(json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {"name": "reader", "namespace": "team"},
            "rules": [
                {"apiGroups": [""], "resources": ["pods", "pods/log"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get"],
                 "resourceNames": ["web", "api"]}
            ]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[..3], [text("reader"), text("team"), Cell::Number(2)]);
        let sections = p.details(&o);
        let SectionBody::Table(table) = body(&sections, "Rules") else {
            panic!("rules is not a table");
        };
        assert_eq!(
            table.columns,
            ["API groups", "Resources", "Verbs", "Resource names"]
        );
        assert_eq!(
            table.rows,
            [
                ["core", "pods, pods/log", "get, list, watch", ""],
                ["apps", "deployments", "get", "web, api"],
            ]
        );
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
    }

    #[test]
    fn cluster_role_aggregation_and_urls() {
        let p = RolePlugin::CLUSTER_ROLE;
        let o = obj(json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
            "metadata": {"name": "monitoring"},
            "aggregationRule": {"clusterRoleSelectors": [
                {"matchLabels": {"rbac.example.com/aggregate-to-monitoring": "true"}}
            ]},
            "rules": [
                {"nonResourceURLs": ["/metrics", "/healthz"], "verbs": ["get"]},
                {"apiGroups": ["*"], "resources": ["*"], "verbs": ["*"]}
            ]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[..2], [text("monitoring"), Cell::Number(2)]);
        let sections = p.details(&o);
        let SectionBody::Fields(fields) = body(&sections, OVERVIEW) else {
            panic!()
        };
        assert_eq!(
            fields[1],
            Field::text(
                "Aggregates",
                "rbac.example.com/aggregate-to-monitoring=true"
            )
        );
        let SectionBody::Table(table) = body(&sections, "Rules (aggregated)") else {
            panic!()
        };
        assert_eq!(table.rows[0], ["", "/metrics, /healthz", "get", ""]);
        assert_eq!(table.rows[1], ["*", "*", "*", ""]);
    }

    #[test]
    fn role_binding_role_and_subjects() {
        let p = RoleBindingPlugin::ROLE_BINDING;
        let o = obj(json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": {"name": "readers", "namespace": "team"},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "reader"},
            "subjects": [
                {"kind": "User", "apiGroup": "rbac.authorization.k8s.io", "name": "alice"},
                {"kind": "ServiceAccount", "name": "ci"},
                {"kind": "ServiceAccount", "name": "bot", "namespace": "tools"},
                {"kind": "Group", "apiGroup": "rbac.authorization.k8s.io", "name": "devs"}
            ]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("Role/reader"));
        assert_eq!(cells[3], text("User alice, SA team/ci +2"));
        let sections = p.details(&o);
        let SectionBody::Fields(overview) = body(&sections, OVERVIEW) else {
            panic!()
        };
        assert_eq!(
            overview[0],
            Field::object_link(
                "Role",
                "reader",
                object_ref(API_VERSION, "Role", "reader", Some("team"))
            )
        );
        let SectionBody::Fields(subjects) = body(&sections, "Subjects") else {
            panic!()
        };
        assert_eq!(subjects[0], Field::text("User", "alice"));
        assert_eq!(
            subjects[1].value,
            FieldValue::Link {
                phrase: None,
                label: "team/ci".into(),
                target: object_ref("v1", "ServiceAccount", "ci", Some("team")),
            }
        );
        assert_eq!(subjects[2].value.text(), "tools/bot");
        assert_eq!(subjects[3], Field::text("Group", "devs"));
        let targets: Vec<String> = p
            .relations(&o)
            .into_iter()
            .map(|r| format!("{}/{}", r.target.kind, r.target.name))
            .collect();
        assert_eq!(
            targets,
            ["Role/reader", "ServiceAccount/ci", "ServiceAccount/bot"]
        );
    }

    #[test]
    fn cluster_role_binding_links_cluster_role() {
        let p = RoleBindingPlugin::CLUSTER_ROLE_BINDING;
        let o = obj(json!({
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
            "metadata": {"name": "admins"},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "cluster-admin"},
            "subjects": [{"kind": "Group", "apiGroup": "rbac.authorization.k8s.io", "name": "system:masters"}]
        }));
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[..3],
            [
                text("admins"),
                text("ClusterRole/cluster-admin"),
                text("Group system:masters")
            ]
        );
        let sections = p.details(&o);
        let SectionBody::Fields(overview) = body(&sections, OVERVIEW) else {
            panic!()
        };
        assert_eq!(
            overview[0],
            Field::object_link(
                "ClusterRole",
                "cluster-admin",
                object_ref(API_VERSION, "ClusterRole", "cluster-admin", None)
            )
        );
    }
}
