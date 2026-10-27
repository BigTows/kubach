//! "Who can access X": walks Roles, ClusterRoles and their bindings.
//!
//! Only RBAC is considered: other authorizers (webhook, Node, ABAC) and the
//! implicit superuser group `system:masters` are invisible here. Aggregated
//! ClusterRoles are used as the API returns them, i.e. with `rules` already
//! filled in by the aggregation controller; selectors are not re-evaluated.

use futures::try_join;
use k8s_openapi::api::rbac::v1::{
    ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject as ApiSubject,
};
use kube::Client;
use kube::api::{Api, ListParams};

use super::rules::Rule;
use super::{RbacError, ResourceAccess};

/// All RBAC objects of a cluster.
#[derive(Clone, Debug, Default)]
pub struct RbacSnapshot {
    pub roles: Vec<Role>,
    pub cluster_roles: Vec<ClusterRole>,
    pub role_bindings: Vec<RoleBinding>,
    pub cluster_role_bindings: Vec<ClusterRoleBinding>,
}

impl RbacSnapshot {
    /// Lists roles and bindings in all namespaces. Needs `list` on all four
    /// `rbac.authorization.k8s.io` resources cluster-wide.
    pub async fn fetch(client: &Client) -> Result<Self, RbacError> {
        let params = ListParams::default();
        let roles = Api::<Role>::all(client.clone());
        let cluster_roles = Api::<ClusterRole>::all(client.clone());
        let role_bindings = Api::<RoleBinding>::all(client.clone());
        let cluster_role_bindings = Api::<ClusterRoleBinding>::all(client.clone());
        let (roles, cluster_roles, role_bindings, cluster_role_bindings) = try_join!(
            roles.list(&params),
            cluster_roles.list(&params),
            role_bindings.list(&params),
            cluster_role_bindings.list(&params),
        )?;
        Ok(Self {
            roles: roles.items,
            cluster_roles: cluster_roles.items,
            role_bindings: role_bindings.items,
            cluster_role_bindings: cluster_role_bindings.items,
        })
    }
}

/// Who a binding grants to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    User(String),
    Group(String),
    ServiceAccount { namespace: String, name: String },
}

impl Subject {
    /// `User`, `Group` or `ServiceAccount`.
    pub fn kind(&self) -> &'static str {
        match self {
            Subject::User(_) => "User",
            Subject::Group(_) => "Group",
            Subject::ServiceAccount { .. } => "ServiceAccount",
        }
    }

    /// The name, `namespace/name` for a ServiceAccount.
    pub fn display_name(&self) -> String {
        match self {
            Subject::User(name) | Subject::Group(name) => name.clone(),
            Subject::ServiceAccount { namespace, name } => format!("{namespace}/{name}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingKind {
    RoleBinding,
    ClusterRoleBinding,
}

impl BindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingKind::RoleBinding => "RoleBinding",
            BindingKind::ClusterRoleBinding => "ClusterRoleBinding",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BindingRef {
    pub kind: BindingKind,
    pub name: String,
    /// `None` for a ClusterRoleBinding.
    pub namespace: Option<String>,
}

/// The role a binding points to. `namespace` is set only for a `Role`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoleRefInfo {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

/// One path that grants access: subject ← binding → role → rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub subject: Subject,
    pub binding: BindingRef,
    pub role: RoleRefInfo,
    pub rule: Rule,
}

/// Every subject granted `access` by RBAC, with the chain that grants it.
///
/// RoleBindings only count when `access.namespace` equals their namespace;
/// with `namespace: None` (cluster-scoped or all namespaces) only
/// ClusterRoleBindings apply. A subject appears once per granting binding.
pub fn who_can(snapshot: &RbacSnapshot, access: &ResourceAccess) -> Vec<Grant> {
    let mut grants = Vec::new();

    for binding in &snapshot.cluster_role_bindings {
        let binding_ref = BindingRef {
            kind: BindingKind::ClusterRoleBinding,
            name: binding.metadata.name.clone().unwrap_or_default(),
            namespace: None,
        };
        collect(
            snapshot,
            access,
            &binding.role_ref,
            binding.subjects.as_deref(),
            binding_ref,
            &mut grants,
        );
    }

    if let Some(namespace) = &access.namespace {
        for binding in &snapshot.role_bindings {
            if binding.metadata.namespace.as_ref() != Some(namespace) {
                continue;
            }
            let binding_ref = BindingRef {
                kind: BindingKind::RoleBinding,
                name: binding.metadata.name.clone().unwrap_or_default(),
                namespace: Some(namespace.clone()),
            };
            collect(
                snapshot,
                access,
                &binding.role_ref,
                binding.subjects.as_deref(),
                binding_ref,
                &mut grants,
            );
        }
    }

    grants
}

fn collect(
    snapshot: &RbacSnapshot,
    access: &ResourceAccess,
    role_ref: &RoleRef,
    subjects: Option<&[ApiSubject]>,
    binding: BindingRef,
    grants: &mut Vec<Grant>,
) {
    let Some((role, rules)) = resolve_role(snapshot, role_ref, binding.namespace.as_deref()) else {
        return;
    };
    let Some(rule) = rules
        .iter()
        .map(Rule::from)
        .find(|rule| rule.allows(access))
    else {
        return;
    };
    for subject in subjects.unwrap_or_default() {
        if let Some(subject) = subject_from(subject, binding.namespace.as_deref()) {
            grants.push(Grant {
                subject,
                binding: binding.clone(),
                role: role.clone(),
                rule: rule.clone(),
            });
        }
    }
}

/// Finds the referenced role and its rules; `None` when it does not exist.
fn resolve_role<'a>(
    snapshot: &'a RbacSnapshot,
    role_ref: &RoleRef,
    binding_namespace: Option<&str>,
) -> Option<(RoleRefInfo, &'a [PolicyRule])> {
    match role_ref.kind.as_str() {
        "ClusterRole" => {
            let role = snapshot
                .cluster_roles
                .iter()
                .find(|r| r.metadata.name.as_ref() == Some(&role_ref.name))?;
            let info = RoleRefInfo {
                kind: role_ref.kind.clone(),
                name: role_ref.name.clone(),
                namespace: None,
            };
            Some((info, role.rules.as_deref().unwrap_or_default()))
        }
        "Role" => {
            let namespace = binding_namespace?;
            let role = snapshot.roles.iter().find(|r| {
                r.metadata.name.as_ref() == Some(&role_ref.name)
                    && r.metadata.namespace.as_deref() == Some(namespace)
            })?;
            let info = RoleRefInfo {
                kind: role_ref.kind.clone(),
                name: role_ref.name.clone(),
                namespace: Some(namespace.to_owned()),
            };
            Some((info, role.rules.as_deref().unwrap_or_default()))
        }
        _ => None,
    }
}

/// A ServiceAccount without a namespace defaults to the binding's namespace.
fn subject_from(subject: &ApiSubject, binding_namespace: Option<&str>) -> Option<Subject> {
    let name = subject.name.clone();
    match subject.kind.as_str() {
        "User" => Some(Subject::User(name)),
        "Group" => Some(Subject::Group(name)),
        "ServiceAccount" => {
            let namespace = subject.namespace.as_deref().or(binding_namespace)?;
            Some(Subject::ServiceAccount {
                namespace: namespace.to_owned(),
                name,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn from<T: k8s_openapi::serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    fn snapshot() -> RbacSnapshot {
        RbacSnapshot {
            roles: vec![
                from(json!({
                    "metadata": { "name": "pod-reader", "namespace": "web" },
                    "rules": [{ "apiGroups": [""], "resources": ["pods", "pods/log"],
                                "verbs": ["get", "list"] }],
                })),
                from(json!({
                    "metadata": { "name": "pod-reader", "namespace": "db" },
                    "rules": [{ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }],
                })),
            ],
            cluster_roles: vec![
                from(json!({
                    "metadata": { "name": "cluster-admin" },
                    "rules": [{ "apiGroups": ["*"], "resources": ["*"], "verbs": ["*"] }],
                })),
                from(json!({
                    "metadata": { "name": "deployer" },
                    "rules": [{ "apiGroups": ["apps"], "resources": ["deployments"],
                                "verbs": ["get", "update"], "resourceNames": ["api"] }],
                })),
                from(json!({
                    "metadata": { "name": "aggregate-view" },
                    "aggregationRule": { "clusterRoleSelectors": [
                        { "matchLabels": { "rbac.example.com/aggregate-to-view": "true" } },
                    ] },
                    "rules": [{ "apiGroups": [""], "resources": ["configmaps"], "verbs": ["get"] }],
                })),
                from(json!({
                    "metadata": { "name": "aggregate-empty" },
                    "aggregationRule": { "clusterRoleSelectors": [] },
                })),
            ],
            role_bindings: vec![
                from(json!({
                    "metadata": { "name": "read-pods", "namespace": "web" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role",
                                 "name": "pod-reader" },
                    "subjects": [
                        { "kind": "User", "name": "alice" },
                        { "kind": "ServiceAccount", "name": "ci" },
                    ],
                })),
                from(json!({
                    "metadata": { "name": "deploy-api", "namespace": "web" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole",
                                 "name": "deployer" },
                    "subjects": [{ "kind": "Group", "name": "devs" }],
                })),
                from(json!({
                    "metadata": { "name": "view", "namespace": "web" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole",
                                 "name": "aggregate-view" },
                    "subjects": [{ "kind": "User", "name": "carol" }],
                })),
                from(json!({
                    "metadata": { "name": "dangling", "namespace": "web" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role",
                                 "name": "missing" },
                    "subjects": [{ "kind": "User", "name": "mallory" }],
                })),
            ],
            cluster_role_bindings: vec![
                from(json!({
                    "metadata": { "name": "admins" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole",
                                 "name": "cluster-admin" },
                    "subjects": [
                        { "kind": "Group", "name": "system:masters",
                          "apiGroup": "rbac.authorization.k8s.io" },
                        { "kind": "ServiceAccount", "name": "argo", "namespace": "argocd" },
                    ],
                })),
                from(json!({
                    "metadata": { "name": "empty-agg" },
                    "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole",
                                 "name": "aggregate-empty" },
                    "subjects": [{ "kind": "User", "name": "nobody" }],
                })),
            ],
        }
    }

    fn subjects(grants: &[Grant]) -> Vec<&Subject> {
        grants.iter().map(|g| &g.subject).collect()
    }

    fn admins() -> [Subject; 2] {
        [
            Subject::Group("system:masters".into()),
            Subject::ServiceAccount {
                namespace: "argocd".into(),
                name: "argo".into(),
            },
        ]
    }

    #[test]
    fn role_binding_in_namespace() {
        let access = ResourceAccess::new("get", "", "pods").in_namespace("web");
        let grants = who_can(&snapshot(), &access);
        let [masters, argo] = admins();
        assert_eq!(
            subjects(&grants),
            [
                &masters,
                &argo,
                &Subject::User("alice".into()),
                &Subject::ServiceAccount {
                    namespace: "web".into(),
                    name: "ci".into()
                },
            ]
        );

        let alice = &grants[2];
        assert_eq!(
            alice.binding,
            BindingRef {
                kind: BindingKind::RoleBinding,
                name: "read-pods".into(),
                namespace: Some("web".into()),
            }
        );
        assert_eq!(
            alice.role,
            RoleRefInfo {
                kind: "Role".into(),
                name: "pod-reader".into(),
                namespace: Some("web".into()),
            }
        );
        assert_eq!(alice.rule.resources, ["pods", "pods/log"]);
    }

    #[test]
    fn cluster_role_binding_grants_everywhere() {
        let grants = who_can(&snapshot(), &ResourceAccess::new("delete", "", "nodes"));
        assert_eq!(subjects(&grants), admins().iter().collect::<Vec<_>>());
        assert!(
            grants
                .iter()
                .all(|g| g.binding.kind == BindingKind::ClusterRoleBinding)
        );
    }

    #[test]
    fn role_bindings_ignored_without_namespace() {
        let grants = who_can(&snapshot(), &ResourceAccess::new("get", "", "pods"));
        assert_eq!(grants.len(), 2);
    }

    #[test]
    fn role_binding_does_not_leak_to_other_namespaces() {
        let access = ResourceAccess::new("get", "", "pods").in_namespace("db");
        let grants = who_can(&snapshot(), &access);
        assert!(!subjects(&grants).contains(&&Subject::User("alice".into())));
    }

    #[test]
    fn role_lookup_uses_binding_namespace() {
        // `read-pods` in `web` must use `web/pod-reader`, not `db/pod-reader`.
        let access = ResourceAccess::new("get", "", "secrets").in_namespace("web");
        let grants = who_can(&snapshot(), &access);
        assert!(!subjects(&grants).contains(&&Subject::User("alice".into())));
    }

    #[test]
    fn cluster_role_via_role_binding_is_namespaced() {
        let access = ResourceAccess::new("update", "apps", "deployments")
            .in_namespace("web")
            .named("api");
        let grants = who_can(&snapshot(), &access);
        let devs = grants
            .iter()
            .find(|g| g.subject == Subject::Group("devs".into()))
            .unwrap();
        assert_eq!(devs.role.kind, "ClusterRole");
        assert_eq!(devs.role.namespace, None);
        assert_eq!(devs.binding.namespace.as_deref(), Some("web"));

        let elsewhere = access.clone().in_namespace("db");
        assert!(
            !who_can(&snapshot(), &elsewhere)
                .iter()
                .any(|g| g.subject == Subject::Group("devs".into()))
        );
    }

    #[test]
    fn resource_names_respected() {
        let access = ResourceAccess::new("update", "apps", "deployments")
            .in_namespace("web")
            .named("worker");
        let grants = who_can(&snapshot(), &access);
        assert!(!subjects(&grants).contains(&&Subject::Group("devs".into())));
    }

    #[test]
    fn subresource() {
        let access = ResourceAccess::new("get", "", "pods/log").in_namespace("web");
        assert!(subjects(&who_can(&snapshot(), &access)).contains(&&Subject::User("alice".into())));
        let exec = ResourceAccess::new("create", "", "pods/exec").in_namespace("web");
        assert!(!subjects(&who_can(&snapshot(), &exec)).contains(&&Subject::User("alice".into())));
    }

    #[test]
    fn aggregated_cluster_role_uses_populated_rules() {
        let access = ResourceAccess::new("get", "", "configmaps").in_namespace("web");
        let grants = who_can(&snapshot(), &access);
        assert!(subjects(&grants).contains(&&Subject::User("carol".into())));
        assert!(!subjects(&grants).contains(&&Subject::User("nobody".into())));
    }

    #[test]
    fn dangling_role_ref_grants_nothing() {
        let access = ResourceAccess::new("get", "", "pods").in_namespace("web");
        let grants = who_can(&snapshot(), &access);
        assert!(!subjects(&grants).contains(&&Subject::User("mallory".into())));
    }

    #[test]
    fn labels() {
        let sa = Subject::ServiceAccount {
            namespace: "web".into(),
            name: "ci".into(),
        };
        assert_eq!(
            (sa.kind(), sa.display_name().as_str()),
            ("ServiceAccount", "web/ci")
        );
        let user = Subject::User("alice".into());
        assert_eq!(
            (user.kind(), user.display_name().as_str()),
            ("User", "alice")
        );
        assert_eq!(Subject::Group("devs".into()).kind(), "Group");
        assert_eq!(BindingKind::RoleBinding.as_str(), "RoleBinding");
        assert_eq!(
            BindingKind::ClusterRoleBinding.as_str(),
            "ClusterRoleBinding"
        );
    }

    #[test]
    fn empty_snapshot() {
        let access = ResourceAccess::new("get", "", "pods").in_namespace("web");
        assert!(who_can(&RbacSnapshot::default(), &access).is_empty());
    }
}
