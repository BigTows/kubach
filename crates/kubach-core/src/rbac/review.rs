//! Questions to the API server about the current user: `SelfSubjectRulesReview`
//! and `SelfSubjectAccessReview`.

use futures::future::join_all;
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
    SelfSubjectRulesReview, SelfSubjectRulesReviewSpec, SubjectAccessReviewStatus,
    SubjectRulesReviewStatus,
};
use kube::Client;
use kube::api::{Api, PostParams};

use super::rules::{self, NonResourceRule, Rule};
use super::{RbacError, ResourceAccess};

/// What the current user can do in a namespace.
///
/// Rules may be partial: `incomplete` is set when an authorizer (webhook, ABAC)
/// cannot enumerate rules, and `evaluation_error` explains why. A missing rule
/// then does not mean "denied": use [`can_i`] for a definitive answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RulesReview {
    pub namespace: String,
    pub rules: Vec<Rule>,
    pub non_resource_rules: Vec<NonResourceRule>,
    pub incomplete: bool,
    pub evaluation_error: Option<String>,
}

impl RulesReview {
    pub fn from_status(namespace: impl Into<String>, status: &SubjectRulesReviewStatus) -> Self {
        Self {
            namespace: namespace.into(),
            rules: status.resource_rules.iter().map(Rule::from).collect(),
            non_resource_rules: status
                .non_resource_rules
                .iter()
                .map(NonResourceRule::from)
                .collect(),
            incomplete: status.incomplete,
            evaluation_error: status.evaluation_error.clone(),
        }
    }

    /// Local check against the reviewed rules. `false` may be wrong when
    /// [`incomplete`](Self::incomplete) is set.
    pub fn allows(&self, access: &ResourceAccess) -> bool {
        rules::allows(&self.rules, access)
    }
}

/// The API server's answer to "can I".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessDecision {
    pub allowed: bool,
    /// Explicitly denied by an authorizer, not just "no rule allows it".
    pub denied: bool,
    pub reason: Option<String>,
    pub evaluation_error: Option<String>,
}

impl From<&SubjectAccessReviewStatus> for AccessDecision {
    fn from(status: &SubjectAccessReviewStatus) -> Self {
        Self {
            allowed: status.allowed,
            denied: status.denied.unwrap_or(false),
            reason: status.reason.clone().filter(|r| !r.is_empty()),
            evaluation_error: status.evaluation_error.clone().filter(|e| !e.is_empty()),
        }
    }
}

/// Lists the current user's rules in `namespace` via `SelfSubjectRulesReview`.
pub async fn rules_in_namespace(
    client: &Client,
    namespace: &str,
) -> Result<RulesReview, RbacError> {
    let review = SelfSubjectRulesReview {
        spec: SelfSubjectRulesReviewSpec {
            namespace: Some(namespace.to_owned()),
        },
        ..Default::default()
    };
    let api: Api<SelfSubjectRulesReview> = Api::all(client.clone());
    let response = api.create(&PostParams::default(), &review).await?;
    let status = response
        .status
        .ok_or(RbacError::MissingStatus("SelfSubjectRulesReview"))?;
    Ok(RulesReview::from_status(namespace, &status))
}

/// Asks the API server whether the current user may perform `access` via
/// `SelfSubjectAccessReview`.
pub async fn can_i(client: &Client, access: &ResourceAccess) -> Result<AccessDecision, RbacError> {
    let review = SelfSubjectAccessReview {
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(resource_attributes(access)),
            ..Default::default()
        },
        ..Default::default()
    };
    let api: Api<SelfSubjectAccessReview> = Api::all(client.clone());
    let response = api.create(&PostParams::default(), &review).await?;
    let status = response
        .status
        .ok_or(RbacError::MissingStatus("SelfSubjectAccessReview"))?;
    Ok(AccessDecision::from(&status))
}

/// Runs [`can_i`] for every item concurrently; results keep the input order.
pub async fn can_i_all(
    client: &Client,
    accesses: &[ResourceAccess],
) -> Vec<Result<AccessDecision, RbacError>> {
    join_all(accesses.iter().map(|access| can_i(client, access))).await
}

fn resource_attributes(access: &ResourceAccess) -> ResourceAttributes {
    ResourceAttributes {
        verb: Some(access.verb.clone()),
        group: Some(access.group.clone()),
        resource: Some(access.resource.clone()),
        subresource: access.subresource.clone(),
        namespace: access.namespace.clone(),
        name: access.name.clone(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_review_from_status() {
        let status: SubjectRulesReviewStatus = serde_json::from_value(serde_json::json!({
            "resourceRules": [
                { "apiGroups": [""], "resources": ["pods", "pods/log"], "verbs": ["get", "list"] },
                { "apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["*"],
                  "resourceNames": ["api"] },
            ],
            "nonResourceRules": [{ "nonResourceURLs": ["/healthz"], "verbs": ["get"] }],
            "incomplete": true,
            "evaluationError": "webhook authorizer cannot list rules",
        }))
        .unwrap();
        let review = RulesReview::from_status("web", &status);

        assert_eq!(review.namespace, "web");
        assert_eq!(review.rules.len(), 2);
        assert_eq!(review.non_resource_rules[0].urls, ["/healthz"]);
        assert!(review.incomplete);
        assert_eq!(
            review.evaluation_error.as_deref(),
            Some("webhook authorizer cannot list rules")
        );

        assert!(review.allows(&ResourceAccess::new("get", "", "pods/log")));
        assert!(!review.allows(&ResourceAccess::new("delete", "", "pods")));
        let deploy = ResourceAccess::new("delete", "apps", "deployments");
        assert!(review.allows(&deploy.clone().named("api")));
        assert!(!review.allows(&deploy));
    }

    #[test]
    fn decision_from_status() {
        let status: SubjectAccessReviewStatus = serde_json::from_value(serde_json::json!({
            "allowed": true,
            "reason": "RBAC: allowed by RoleBinding \"dev\"",
        }))
        .unwrap();
        let decision = AccessDecision::from(&status);
        assert!(decision.allowed);
        assert!(!decision.denied);
        assert!(decision.reason.unwrap().contains("dev"));
        assert_eq!(decision.evaluation_error, None);
    }

    #[test]
    fn attributes_split_subresource() {
        let access = ResourceAccess::new("get", "", "pods/log")
            .in_namespace("web")
            .named("api-0");
        let attrs = resource_attributes(&access);
        assert_eq!(attrs.resource.as_deref(), Some("pods"));
        assert_eq!(attrs.subresource.as_deref(), Some("log"));
        assert_eq!(attrs.group.as_deref(), Some(""));
        assert_eq!(attrs.namespace.as_deref(), Some("web"));
        assert_eq!(attrs.name.as_deref(), Some("api-0"));
    }
}
