//! RBAC rules of each profile: the Role of a temporary ServiceAccount and the
//! [`Capability`] accesses tests use for the user's RBAC (real sessions review
//! the user's rights in the namespace of each call, #417).
//!
//! Resources are listed explicitly (never `*`) so that `secrets` can never be
//! granted by accident: Secrets appear only through [`grant_rules`], when the
//! session has Secret access in a namespace, or the user allowed revealing
//! one Secret (#227, #15).

use k8s_openapi::api::rbac::v1::PolicyRule;
use kubach_core::rbac::ResourceAccess;

use crate::access::Access;
use crate::grants::{Grant, Grants};
use crate::settings::Profile;

const READ_VERBS: &[&str] = &["get", "list", "watch"];
const WRITE_VERBS: &[&str] = &["create", "update", "patch", "delete"];

/// Readable resources by API group. No `secrets`.
const READABLE: &[(&str, &[&str])] = &[
    (
        "",
        &[
            "pods",
            "services",
            "endpoints",
            "configmaps",
            "events",
            "persistentvolumeclaims",
            "replicationcontrollers",
            "serviceaccounts",
            "limitranges",
            "resourcequotas",
        ],
    ),
    (
        "apps",
        &[
            "deployments",
            "statefulsets",
            "daemonsets",
            "replicasets",
            "controllerrevisions",
        ],
    ),
    ("batch", &["jobs", "cronjobs"]),
    ("autoscaling", &["horizontalpodautoscalers"]),
    ("networking.k8s.io", &["ingresses", "networkpolicies"]),
    ("policy", &["poddisruptionbudgets"]),
    ("discovery.k8s.io", &["endpointslices"]),
    ("events.k8s.io", &["events"]),
    ("metrics.k8s.io", &["pods"]),
];

/// Writable resources for the deploy profile. No Secrets, ServiceAccounts or
/// RBAC objects: those would let the agent widen its own rights.
const WRITABLE: &[(&str, &[&str])] = &[
    (
        "",
        &["pods", "services", "configmaps", "persistentvolumeclaims"],
    ),
    (
        "apps",
        &[
            "deployments",
            "statefulsets",
            "daemonsets",
            "replicasets",
            "deployments/scale",
            "statefulsets/scale",
            "replicasets/scale",
        ],
    ),
    ("batch", &["jobs", "cronjobs"]),
    ("autoscaling", &["horizontalpodautoscalers"]),
    ("networking.k8s.io", &["ingresses", "networkpolicies"]),
    ("policy", &["poddisruptionbudgets"]),
];

fn listed(table: &[(&str, &[&str])], group: &str, plural: &str) -> bool {
    table
        .iter()
        .any(|(g, resources)| *g == group && resources.contains(&plural))
}

/// Whether the built-in allowlist lets the agent read `plural` of `group`.
/// The client-side policy uses the same list as the temporary
/// ServiceAccount's Role, in every session mode (#192).
pub fn is_readable(group: &str, plural: &str) -> bool {
    listed(READABLE, group, plural)
}

/// Whether the built-in allowlist lets the deploy profile change `plural`
/// of `group`.
pub fn is_writable(group: &str, plural: &str) -> bool {
    !plural.contains('/') && listed(WRITABLE, group, plural)
}

fn rule(group: &str, resources: &[&str], verbs: &[&str]) -> PolicyRule {
    PolicyRule {
        api_groups: Some(vec![group.to_owned()]),
        resources: Some(resources.iter().map(|r| (*r).to_owned()).collect()),
        verbs: verbs.iter().map(|v| (*v).to_owned()).collect(),
        ..Default::default()
    }
}

/// What `read(ns)` grants: the readable resources, plus logs from the
/// debug profile on.
pub fn read_rules(profile: Profile) -> Vec<PolicyRule> {
    let mut rules: Vec<PolicyRule> = READABLE
        .iter()
        .map(|(group, resources)| rule(group, resources, READ_VERBS))
        .collect();
    if profile >= Profile::Debug {
        rules.push(rule("", &["pods/log"], &["get"]));
    }
    rules
}

/// What a running port-forward needs (#17). Port-forward over WebSocket is
/// authorized as `get` before 1.30, `create` after, like exec.
pub fn forward_rule() -> PolicyRule {
    rule("", &["pods/portforward"], &["get", "create"])
}

/// What `modify(ns)` grants: exec (debug) and the write verbs (deploy).
pub fn modify_rules(profile: Profile) -> Vec<PolicyRule> {
    let mut rules = Vec::new();
    if profile >= Profile::Debug {
        // Exec over WebSocket is authorized as `get` before 1.30, `create` after.
        rules.push(rule("", &["pods/exec"], &["get", "create"]));
    }
    if profile >= Profile::Deploy {
        // Server-side dry-run needs the write verbs too: the API server
        // authorizes a dry-run like the real request.
        for (group, resources) in WRITABLE {
            rules.push(rule(group, resources, WRITE_VERBS));
        }
    }
    rules
}

/// Role rules of a whole profile. Never Secrets.
pub fn role_rules(profile: Profile) -> Vec<PolicyRule> {
    let mut rules = read_rules(profile);
    rules.extend(modify_rules(profile));
    rules
}

/// The Role of a temporary ServiceAccount: exactly the rights the session
/// has in `namespace` so far within its `access` (#227 variant A, #15).
/// Empty until the agent first works there.
///
/// Secret metadata needs `get`/`list` on all Secrets of the namespace, so
/// their values pass through Kubach as well: that "the agent sees no values"
/// is held by Kubach alone (ADR 0005). Revealing a Secret adds a rule for
/// that name only.
pub fn grant_rules(grants: &Grants, namespace: &str, access: Access) -> Vec<PolicyRule> {
    let profile = access.profile;
    let mut rules = Vec::new();
    let mut revealed = Vec::new();
    let mut modify = false;
    for grant in grants.granted().filter(|g| g.namespace() == namespace) {
        match grant {
            Grant::Read { .. } => {
                rules.extend(read_rules(profile));
                if access.secrets {
                    rules.push(rule("", &["secrets"], &["get", "list"]));
                }
                // Only while a forward the user approved runs (#17).
                if profile >= Profile::Debug && grants.is_forwarding(namespace) {
                    rules.push(forward_rule());
                }
            }
            Grant::Secret { name, .. } if access.secrets => {
                revealed.push(name.clone());
            }
            Grant::Secret { .. } => {}
            Grant::Modify { .. } => {
                modify = true;
                rules.extend(modify_rules(profile));
            }
        }
    }
    if !revealed.is_empty() {
        rules.push(PolicyRule {
            resource_names: Some(revealed),
            ..rule("", &["secrets"], &["get"])
        });
    }
    // Secrets written through the user (#230): RBAC cannot narrow `create`
    // by name, so creating is allowed with `modify(ns)`; patching and
    // deleting only the Secrets the user approved a change of.
    if modify && writes_secrets(access) {
        rules.push(rule("", &["secrets"], &["create"]));
        let written = grants.secret_writes_in(namespace);
        if !written.is_empty() {
            rules.push(PolicyRule {
                resource_names: Some(written),
                ..rule("", &["secrets"], &["patch", "delete"])
            });
        }
    }
    rules
}

/// Whether the agent may write Secrets through the user (#230): the deploy
/// profile and Secret access.
pub fn writes_secrets(access: Access) -> bool {
    access.profile >= Profile::Deploy && access.secrets
}

/// An access a tool needs; tests list the ones the user has (#417).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    ListPods,
    ListEvents,
    PodLogs,
    PodExec,
    PodPortForward,
    PatchDeployments,
    ScaleDeployments,
    DeleteDeployments,
    PodMetrics,
    CreateSecrets,
    PatchSecrets,
    DeleteSecrets,
}

impl Capability {
    pub const ALL: [Capability; 12] = [
        Self::ListPods,
        Self::ListEvents,
        Self::PodLogs,
        Self::PodExec,
        Self::PodPortForward,
        Self::PatchDeployments,
        Self::ScaleDeployments,
        Self::DeleteDeployments,
        Self::PodMetrics,
        Self::CreateSecrets,
        Self::PatchSecrets,
        Self::DeleteSecrets,
    ];

    /// The access to review in `namespace`.
    pub fn access(self, namespace: &str) -> ResourceAccess {
        let access = match self {
            Self::ListPods => ResourceAccess::new("list", "", "pods"),
            Self::ListEvents => ResourceAccess::new("list", "", "events"),
            Self::PodLogs => ResourceAccess::new("get", "", "pods/log"),
            Self::PodExec => ResourceAccess::new("create", "", "pods/exec"),
            Self::PodPortForward => ResourceAccess::new("create", "", "pods/portforward"),
            Self::PatchDeployments => ResourceAccess::new("patch", "apps", "deployments"),
            Self::ScaleDeployments => ResourceAccess::new("patch", "apps", "deployments/scale"),
            Self::DeleteDeployments => ResourceAccess::new("delete", "apps", "deployments"),
            Self::PodMetrics => ResourceAccess::new("list", "metrics.k8s.io", "pods"),
            Self::CreateSecrets => ResourceAccess::new("create", "", "secrets"),
            Self::PatchSecrets => ResourceAccess::new("patch", "", "secrets"),
            Self::DeleteSecrets => ResourceAccess::new("delete", "", "secrets"),
        };
        access.in_namespace(namespace)
    }
}

/// Accesses the user needs to create a temporary ServiceAccount session.
pub fn temporary_sa_requirements(namespace: &str) -> Vec<ResourceAccess> {
    [
        ("create", "", "serviceaccounts"),
        ("delete", "", "serviceaccounts"),
        ("create", "", "serviceaccounts/token"),
        ("create", "rbac.authorization.k8s.io", "roles"),
        // Every grant and revocation patches the session's Role (#227).
        ("patch", "rbac.authorization.k8s.io", "roles"),
        ("create", "rbac.authorization.k8s.io", "rolebindings"),
    ]
    .into_iter()
    .map(|(verb, group, resource)| {
        ResourceAccess::new(verb, group, resource).in_namespace(namespace)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_resources(rules: &[PolicyRule]) -> Vec<String> {
        rules
            .iter()
            .flat_map(|r| r.resources.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn no_profile_grants_secrets_or_wildcards() {
        for profile in [Profile::ReadOnly, Profile::Debug, Profile::Deploy] {
            let rules = role_rules(profile);
            for rule in &rules {
                let resources = rule.resources.clone().unwrap_or_default();
                assert!(!resources.iter().any(|r| r == "secrets" || r.contains('*')));
                assert!(
                    !rule
                        .verbs
                        .iter()
                        .any(|v| v == "*" || v == "escalate" || v == "bind")
                );
                assert!(
                    !rule
                        .api_groups
                        .clone()
                        .unwrap_or_default()
                        .iter()
                        .any(|g| { g == "*" || g == "rbac.authorization.k8s.io" })
                );
                assert!(rule.resource_names.is_none());
            }
        }
    }

    #[test]
    fn port_forward_is_in_the_role_only_while_a_forward_runs() {
        let debug = Access {
            profile: Profile::Debug,
            secrets: false,
            crds: false,
        };
        let read_only = Access {
            profile: Profile::ReadOnly,
            ..debug
        };
        let has_forward = |grants: &Grants, access| {
            all_resources(&grant_rules(grants, "web", access))
                .contains(&"pods/portforward".to_string())
        };
        let mut grants = crate::testutil::grants(&[Grant::read("web")]);
        assert!(!has_forward(&grants, debug));
        assert!(grants.set_forwarding("web", true));
        assert!(has_forward(&grants, debug));
        // Never below the debug level, never in another namespace.
        assert!(!has_forward(&grants, read_only));
        assert!(
            !all_resources(&grant_rules(&grants, "other", debug))
                .contains(&"pods/portforward".to_string())
        );
        // Revoking reading the namespace takes it away.
        grants.revoke(&Grant::read("web"));
        assert!(!has_forward(&grants, debug));
        assert!(!grants.is_forwarding("web"));
        // Not part of any profile's Role by itself.
        for profile in [Profile::ReadOnly, Profile::Debug, Profile::Deploy] {
            assert!(!all_resources(&role_rules(profile)).contains(&"pods/portforward".to_string()));
        }
    }

    #[test]
    fn read_only_has_no_logs_exec_or_writes() {
        let rules = role_rules(Profile::ReadOnly);
        let resources = all_resources(&rules);
        assert!(!resources.iter().any(|r| r.contains('/')));
        for rule in &rules {
            assert!(rule.verbs.iter().all(|v| READ_VERBS.contains(&v.as_str())));
        }
    }

    #[test]
    fn debug_has_logs_and_exec_but_no_writes() {
        let resources = all_resources(&role_rules(Profile::Debug));
        assert!(resources.contains(&"pods/log".to_string()));
        assert!(resources.contains(&"pods/exec".to_string()));
        assert!(
            role_rules(Profile::Debug)
                .iter()
                .all(|r| !r.verbs.iter().any(|v| v == "delete"))
        );
    }

    #[test]
    fn deploy_never_writes_identity_objects() {
        let rules = role_rules(Profile::Deploy);
        for rule in rules
            .iter()
            .filter(|r| r.verbs.iter().any(|v| v == "create"))
        {
            let resources = rule.resources.clone().unwrap_or_default();
            for forbidden in ["serviceaccounts", "secrets", "roles", "rolebindings"] {
                assert!(!resources.iter().any(|r| r == forbidden), "{forbidden}");
            }
        }
    }

    fn grants(list: &[Grant]) -> Grants {
        let mut grants = Grants::new();
        for grant in list {
            grants.set(grant.clone(), crate::grants::GrantState::Granted);
        }
        grants
    }

    #[test]
    fn role_starts_empty_and_follows_grants() {
        let access = Access::ALL;
        assert!(grant_rules(&Grants::new(), "web", access).is_empty());

        let read = grant_rules(&grants(&[Grant::read("web")]), "web", access);
        assert!(
            read.iter()
                .all(|r| r.verbs.iter().all(|v| READ_VERBS.contains(&v.as_str())))
        );
        let secrets: Vec<_> = read
            .iter()
            .filter(|r| r.resources.as_deref() == Some(&["secrets".to_owned()][..]))
            .collect();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].verbs, ["get", "list"]);
        // Another namespace's grants are not this Role's.
        assert!(grant_rules(&grants(&[Grant::read("api")]), "web", access).is_empty());

        let all = grant_rules(
            &grants(&[
                Grant::read("web"),
                Grant::secret("web", "db"),
                Grant::modify("web"),
            ]),
            "web",
            access,
        );
        let named: Vec<_> = all.iter().filter(|r| r.resource_names.is_some()).collect();
        assert_eq!(named.len(), 1);
        assert_eq!(
            named[0].resource_names.as_deref(),
            Some(&["db".to_owned()][..])
        );
        assert_eq!(named[0].verbs, ["get"]);
        assert!(all.iter().any(|r| r.verbs.iter().any(|v| v == "delete")));
        assert!(all.iter().all(|r| !r.verbs.iter().any(|v| v == "*")));
    }

    #[test]
    fn secret_writes_are_narrowed_by_name() {
        let secret_rules = |grants: &Grants, access: Access| {
            grant_rules(grants, "web", access)
                .into_iter()
                .filter(|r| r.resources.as_deref() == Some(&["secrets".to_owned()][..]))
                .filter(|r| r.verbs.iter().any(|v| v != "get" && v != "list"))
                .collect::<Vec<_>>()
        };
        let mut granted = grants(&[Grant::read("web"), Grant::modify("web")]);
        let rules = secret_rules(&granted, Access::ALL);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].verbs, ["create"]);
        assert!(rules[0].resource_names.is_none());

        granted.add_secret_write("web", "db");
        let rules = secret_rules(&granted, Access::ALL);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].verbs, ["patch", "delete"]);
        assert_eq!(
            rules[1].resource_names.as_deref(),
            Some(&["db".to_owned()][..])
        );
        // Never without modify, the deploy profile or Secret access.
        let read_only = grants(&[Grant::read("web")]);
        assert!(secret_rules(&read_only, Access::ALL).is_empty());
        assert!(secret_rules(&granted, Access::ALL.capped(Profile::Debug)).is_empty());
        let none = Access {
            secrets: false,
            ..Access::ALL
        };
        assert!(secret_rules(&granted, none).is_empty());
    }

    #[test]
    fn secret_rules_follow_secret_access() {
        // Reading only: no rule on Secrets at all, not even metadata (#15).
        let rules = grant_rules(
            &grants(&[Grant::read("web"), Grant::secret("web", "db")]),
            "web",
            Access::NONE,
        );
        assert!(!all_resources(&rules).contains(&"secrets".to_owned()));
        let metadata = Access {
            secrets: true,
            ..Access::NONE
        };
        let rules = grant_rules(&grants(&[Grant::read("web")]), "web", metadata);
        assert!(rules.iter().all(|r| r.resource_names.is_none()));
        assert!(all_resources(&rules).contains(&"secrets".to_owned()));
    }

    #[test]
    fn allowlist_lookups() {
        assert!(is_readable("apps", "deployments"));
        assert!(is_readable("", "serviceaccounts"));
        assert!(!is_readable("", "secrets"));
        assert!(!is_readable(
            "kustomize.toolkit.fluxcd.io",
            "kustomizations"
        ));
        assert!(is_writable("batch", "jobs"));
        assert!(!is_writable("", "serviceaccounts"));
        assert!(!is_writable("apps", "deployments/scale"));
        assert!(!is_writable("argoproj.io", "applications"));
        // Everything writable is readable.
        for (group, resources) in WRITABLE {
            for resource in resources.iter().filter(|r| !r.contains('/')) {
                assert!(is_readable(group, resource), "{group}/{resource}");
            }
        }
    }

    #[test]
    fn sa_requirements_are_namespaced() {
        assert!(
            temporary_sa_requirements("web")
                .iter()
                .all(|a| a.namespace.as_deref() == Some("web"))
        );
    }
}
