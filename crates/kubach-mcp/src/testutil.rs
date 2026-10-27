//! Fixtures shared by unit tests.

use kubach_core::discovery::ApiCatalog;
use kube::api::ApiResource;
use kube::discovery::{ApiCapabilities, Scope};

use crate::scope::{SessionScope, SessionSpec};
use crate::settings::{DeployMode, ExecMode, Profile, SecurityPolicy};

pub const NS: &str = "kubach-test-mcp";
pub const OTHER_NS: &str = "kubach-test-mcp-other";

fn kind(
    group: &str,
    version: &str,
    kind: &str,
    plural: &str,
    namespaced: bool,
) -> (ApiResource, ApiCapabilities) {
    let api_version = if group.is_empty() {
        version.to_owned()
    } else {
        format!("{group}/{version}")
    };
    (
        ApiResource {
            group: group.into(),
            version: version.into(),
            api_version,
            kind: kind.into(),
            plural: plural.into(),
        },
        ApiCapabilities {
            scope: if namespaced {
                Scope::Namespaced
            } else {
                Scope::Cluster
            },
            subresources: Vec::new(),
            operations: [
                "get", "list", "watch", "create", "update", "patch", "delete",
            ]
            .map(String::from)
            .to_vec(),
        },
    )
}

/// A catalog with the kinds the tests touch.
pub fn catalog() -> ApiCatalog {
    ApiCatalog::from_resources([
        kind("", "v1", "Pod", "pods", true),
        kind("", "v1", "Service", "services", true),
        kind("", "v1", "ConfigMap", "configmaps", true),
        kind("", "v1", "Secret", "secrets", true),
        kind("", "v1", "ServiceAccount", "serviceaccounts", true),
        kind("", "v1", "Event", "events", true),
        kind("", "v1", "Namespace", "namespaces", false),
        kind("", "v1", "Node", "nodes", false),
        kind("", "v1", "PersistentVolume", "persistentvolumes", false),
        kind("apps", "v1", "Deployment", "deployments", true),
        kind("apps", "v1", "StatefulSet", "statefulsets", true),
        kind("apps", "v1", "DaemonSet", "daemonsets", true),
        kind("batch", "v1", "Job", "jobs", true),
        kind("batch", "v1", "CronJob", "cronjobs", true),
        kind("events.k8s.io", "v1", "Event", "events", true),
        kind("rbac.authorization.k8s.io", "v1", "Role", "roles", true),
        kind(
            "rbac.authorization.k8s.io",
            "v1",
            "RoleBinding",
            "rolebindings",
            true,
        ),
        kind(
            "rbac.authorization.k8s.io",
            "v1",
            "ClusterRole",
            "clusterroles",
            false,
        ),
        kind(
            "rbac.authorization.k8s.io",
            "v1",
            "ClusterRoleBinding",
            "clusterrolebindings",
            false,
        ),
        kind("example.com", "v1", "Widget", "widgets", true),
        kind("apps", "v1", "ReplicaSet", "replicasets", true),
        kind(
            "kustomize.toolkit.fluxcd.io",
            "v1",
            "Kustomization",
            "kustomizations",
            true,
        ),
        kind(
            "helm.toolkit.fluxcd.io",
            "v2",
            "HelmRelease",
            "helmreleases",
            true,
        ),
        kind(
            "argoproj.io",
            "v1alpha1",
            "Application",
            "applications",
            true,
        ),
        kind(
            "argoproj.io",
            "v1alpha1",
            "ApplicationSet",
            "applicationsets",
            true,
        ),
        kind("argoproj.io", "v1alpha1", "AppProject", "appprojects", true),
        kind("other.example.com", "v1", "Widget", "widgets", true),
        kind("networking.k8s.io", "v1", "Ingress", "ingresses", true),
        kind(
            "networking.k8s.io",
            "v1",
            "NetworkPolicy",
            "networkpolicies",
            true,
        ),
    ])
}

/// Everything enabled, so tests can narrow one setting at a time.
pub fn permissive() -> SecurityPolicy {
    SecurityPolicy {
        deploy: DeployMode::ApplyAfterConfirmation,
        exec: ExecMode::OneShot,
        other_kinds: true,
        ..SecurityPolicy::development()
    }
}

pub fn scope_with(profile: Profile, policy: &SecurityPolicy, namespaces: &[&str]) -> SessionScope {
    SessionScope::new(
        &SessionSpec {
            context: "minikube".into(),
            server_url: "https://127.0.0.1:8443".into(),
            namespaces: namespaces.iter().map(|n| n.to_string()).collect(),
            profile,
            ttl: None,
            hints: Vec::new(),
        },
        policy,
    )
    .expect("valid scope")
}

pub fn scope(profile: Profile) -> SessionScope {
    scope_with(profile, &permissive(), &[NS])
}

/// A grant table with `list` granted.
pub fn grants(list: &[crate::grants::Grant]) -> crate::grants::Grants {
    let mut grants = crate::grants::Grants::new();
    for grant in list {
        grants.set(grant.clone(), crate::grants::GrantState::Granted);
    }
    grants
}
