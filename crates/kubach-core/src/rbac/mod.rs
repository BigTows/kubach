//! RBAC inspection: what the current user can do and who can access a resource.
//!
//! [`rules`] evaluates RBAC rules locally, [`review`] asks the API server about
//! the current user, [`who_can`] walks roles and bindings to find subjects,
//! [`graph`] turns its grants into a laid-out subject → binding → role → rule graph.

pub mod graph;
pub mod review;
pub mod rules;
pub mod who_can;

pub use graph::{AccessGraph, GRAPH_COLUMNS, GraphEdge, GraphNode, GraphNodeKind};
pub use review::{AccessDecision, RulesReview, can_i, can_i_all, rules_in_namespace};
pub use rules::{NonResourceRule, Rule};
pub use who_can::{BindingKind, BindingRef, Grant, RbacSnapshot, RoleRefInfo, Subject, who_can};

#[derive(Debug, thiserror::Error)]
pub enum RbacError {
    #[error("API request failed: {0}")]
    Api(#[source] Box<kube::Error>),
    #[error("API server returned no status for {0}")]
    MissingStatus(&'static str),
}

impl From<kube::Error> for RbacError {
    fn from(error: kube::Error) -> Self {
        Self::Api(Box::new(error))
    }
}

/// An action on a resource: "can `verb` `group/resource` in `namespace`".
///
/// `resource` may carry a subresource (`pods/log`), the core API group is `""`.
/// `namespace: None` means a cluster-scoped resource or all namespaces.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceAccess {
    pub verb: String,
    pub group: String,
    pub resource: String,
    pub subresource: Option<String>,
    pub namespace: Option<String>,
    pub name: Option<String>,
}

impl ResourceAccess {
    /// `resource` may be `pods` or `pods/log`.
    pub fn new(verb: impl Into<String>, group: impl Into<String>, resource: &str) -> Self {
        let (resource, subresource) = match resource.split_once('/') {
            Some((resource, sub)) => (resource.to_owned(), Some(sub.to_owned())),
            None => (resource.to_owned(), None),
        };
        Self {
            verb: verb.into(),
            group: group.into(),
            resource,
            subresource,
            namespace: None,
            name: None,
        }
    }

    /// Parses a resource the way `kubectl auth can-i` takes it:
    /// `pods`, `pods/log`, `deployments.apps`, `deployments.apps/scale`.
    /// `None` when the resource or the verb is empty.
    pub fn parse(verb: &str, resource: &str) -> Option<Self> {
        let verb = verb.trim().to_ascii_lowercase();
        let resource = resource.trim().to_ascii_lowercase();
        let (resource, subresource) = match resource.split_once('/') {
            Some((resource, sub)) => (resource, Some(sub.trim().to_owned())),
            None => (resource.as_str(), None),
        };
        let (resource, group) = resource.split_once('.').unwrap_or((resource, ""));
        if verb.is_empty() || resource.is_empty() {
            return None;
        }
        Some(Self {
            verb,
            group: group.to_owned(),
            resource: resource.to_owned(),
            subresource: subresource.filter(|sub| !sub.is_empty()),
            namespace: None,
            name: None,
        })
    }

    pub fn in_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// `resource` or `resource/subresource`, as written in RBAC rules.
    pub fn full_resource(&self) -> String {
        match &self.subresource {
            Some(sub) => format!("{}/{sub}", self.resource),
            None => self.resource.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subresource() {
        let access = ResourceAccess::new("get", "", "pods/log")
            .in_namespace("web")
            .named("api-0");
        assert_eq!(access.resource, "pods");
        assert_eq!(access.subresource.as_deref(), Some("log"));
        assert_eq!(access.full_resource(), "pods/log");
        assert_eq!(access.namespace.as_deref(), Some("web"));
        assert_eq!(access.name.as_deref(), Some("api-0"));
    }

    #[test]
    fn parses_kubectl_style_resources() {
        let access = ResourceAccess::parse(" Get ", "deployments.apps/scale").unwrap();
        assert_eq!(access.verb, "get");
        assert_eq!(access.group, "apps");
        assert_eq!(access.resource, "deployments");
        assert_eq!(access.subresource.as_deref(), Some("scale"));

        let access = ResourceAccess::parse("list", "pods").unwrap();
        assert_eq!((access.group.as_str(), access.subresource), ("", None));

        let access = ResourceAccess::parse("get", "certificates.cert-manager.io").unwrap();
        assert_eq!(access.group, "cert-manager.io");
        assert_eq!(access.resource, "certificates");

        assert_eq!(
            ResourceAccess::parse("get", "pods/").unwrap().subresource,
            None
        );
        assert_eq!(ResourceAccess::parse("", "pods"), None);
        assert_eq!(ResourceAccess::parse("get", " "), None);
        assert_eq!(ResourceAccess::parse("get", ".apps"), None);
    }

    #[test]
    fn plain_resource_has_no_subresource() {
        let access = ResourceAccess::new("list", "apps", "deployments");
        assert_eq!(access.subresource, None);
        assert_eq!(access.full_resource(), "deployments");
    }
}
