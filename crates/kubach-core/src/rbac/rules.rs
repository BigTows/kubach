//! Normalized RBAC rules and local evaluation, following the API server's
//! RBAC authorizer matching rules.

use k8s_openapi::api::authorization::v1::{
    NonResourceRule as ApiNonResourceRule, ResourceRule as ApiResourceRule,
};
use k8s_openapi::api::rbac::v1::PolicyRule;

use super::ResourceAccess;

const ALL: &str = "*";

/// A resource rule; empty `resource_names` means any name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rule {
    pub api_groups: Vec<String>,
    pub resources: Vec<String>,
    pub verbs: Vec<String>,
    pub resource_names: Vec<String>,
}

/// A rule for non-resource URLs such as `/healthz`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NonResourceRule {
    pub verbs: Vec<String>,
    pub urls: Vec<String>,
}

impl Rule {
    /// Whether this rule grants `access`.
    pub fn allows(&self, access: &ResourceAccess) -> bool {
        self.matches_verb(&access.verb)
            && self.matches_group(&access.group)
            && self.matches_resource(access)
            && self.matches_name(access.name.as_deref())
    }

    fn matches_verb(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == ALL || v == verb)
    }

    fn matches_group(&self, group: &str) -> bool {
        self.api_groups.iter().any(|g| g == ALL || g == group)
    }

    fn matches_resource(&self, access: &ResourceAccess) -> bool {
        let full = access.full_resource();
        self.resources.iter().any(|r| {
            r == ALL
                || *r == full
                || access
                    .subresource
                    .as_deref()
                    .is_some_and(|sub| r.strip_prefix("*/") == Some(sub))
        })
    }

    fn matches_name(&self, name: Option<&str>) -> bool {
        self.resource_names.is_empty()
            || name.is_some_and(|name| self.resource_names.iter().any(|n| n == name))
    }
}

/// Whether any of `rules` grants `access`.
pub fn allows(rules: &[Rule], access: &ResourceAccess) -> bool {
    rules.iter().any(|rule| rule.allows(access))
}

impl From<&ApiResourceRule> for Rule {
    fn from(rule: &ApiResourceRule) -> Self {
        Self {
            api_groups: rule.api_groups.clone().unwrap_or_default(),
            resources: rule.resources.clone().unwrap_or_default(),
            verbs: rule.verbs.clone(),
            resource_names: rule.resource_names.clone().unwrap_or_default(),
        }
    }
}

/// Non-resource rules (`nonResourceURLs`) become rules without resources and
/// therefore never match a [`ResourceAccess`].
impl From<&PolicyRule> for Rule {
    fn from(rule: &PolicyRule) -> Self {
        Self {
            api_groups: rule.api_groups.clone().unwrap_or_default(),
            resources: rule.resources.clone().unwrap_or_default(),
            verbs: rule.verbs.clone(),
            resource_names: rule.resource_names.clone().unwrap_or_default(),
        }
    }
}

impl From<&ApiNonResourceRule> for NonResourceRule {
    fn from(rule: &ApiNonResourceRule) -> Self {
        Self {
            verbs: rule.verbs.clone(),
            urls: rule.non_resource_urls.clone().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(groups: &[&str], resources: &[&str], verbs: &[&str], names: &[&str]) -> Rule {
        let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
        Rule {
            api_groups: owned(groups),
            resources: owned(resources),
            verbs: owned(verbs),
            resource_names: owned(names),
        }
    }

    fn get_pod() -> ResourceAccess {
        ResourceAccess::new("get", "", "pods").in_namespace("web")
    }

    #[test]
    fn exact_match() {
        assert!(rule(&[""], &["pods"], &["get", "list"], &[]).allows(&get_pod()));
    }

    #[test]
    fn verb_mismatch() {
        assert!(!rule(&[""], &["pods"], &["list"], &[]).allows(&get_pod()));
    }

    #[test]
    fn group_mismatch() {
        assert!(!rule(&["apps"], &["pods"], &["get"], &[]).allows(&get_pod()));
    }

    #[test]
    fn resource_mismatch() {
        assert!(!rule(&[""], &["services"], &["get"], &[]).allows(&get_pod()));
    }

    #[test]
    fn empty_lists_match_nothing() {
        assert!(!rule(&[], &["pods"], &["get"], &[]).allows(&get_pod()));
        assert!(!rule(&[""], &[], &["get"], &[]).allows(&get_pod()));
        assert!(!rule(&[""], &["pods"], &[], &[]).allows(&get_pod()));
    }

    #[test]
    fn wildcards() {
        assert!(rule(&["*"], &["*"], &["*"], &[]).allows(&get_pod()));
        assert!(rule(&["*"], &["pods"], &["get"], &[]).allows(&get_pod()));
        assert!(rule(&[""], &["*"], &["get"], &[]).allows(&get_pod()));
        assert!(rule(&[""], &["pods"], &["*"], &[]).allows(&get_pod()));
    }

    #[test]
    fn resource_wildcard_covers_subresources() {
        let logs = ResourceAccess::new("get", "", "pods/log");
        assert!(rule(&[""], &["*"], &["get"], &[]).allows(&logs));
    }

    #[test]
    fn subresource_needs_explicit_rule() {
        let logs = ResourceAccess::new("get", "", "pods/log");
        assert!(!rule(&[""], &["pods"], &["get"], &[]).allows(&logs));
        assert!(rule(&[""], &["pods/log"], &["get"], &[]).allows(&logs));
        assert!(!rule(&[""], &["pods/exec"], &["get"], &[]).allows(&logs));
    }

    #[test]
    fn subresource_wildcard_prefix() {
        let logs = ResourceAccess::new("get", "", "pods/log");
        assert!(rule(&[""], &["*/log"], &["get"], &[]).allows(&logs));
        assert!(!rule(&[""], &["*/exec"], &["get"], &[]).allows(&logs));
        assert!(!rule(&[""], &["*/log"], &["get"], &[]).allows(&get_pod()));
    }

    #[test]
    fn resource_suffix_wildcard_is_not_supported() {
        let logs = ResourceAccess::new("get", "", "pods/log");
        assert!(!rule(&[""], &["pods/*"], &["get"], &[]).allows(&logs));
    }

    #[test]
    fn subresource_rule_does_not_grant_parent() {
        assert!(!rule(&[""], &["pods/log"], &["get"], &[]).allows(&get_pod()));
    }

    #[test]
    fn resource_names() {
        let named = rule(&[""], &["pods"], &["get"], &["api-0"]);
        assert!(named.allows(&get_pod().named("api-0")));
        assert!(!named.allows(&get_pod().named("api-1")));
        assert!(!named.allows(&get_pod()));
        assert!(rule(&[""], &["pods"], &["get"], &[]).allows(&get_pod().named("x")));
    }

    #[test]
    fn any_rule_allows() {
        let rules = [
            rule(&["apps"], &["deployments"], &["get"], &[]),
            rule(&[""], &["pods"], &["get"], &[]),
        ];
        assert!(allows(&rules, &get_pod()));
        assert!(!allows(&rules[..1], &get_pod()));
        assert!(!allows(&[], &get_pod()));
    }

    #[test]
    fn from_api_resource_rule() {
        let api: ApiResourceRule = serde_json::from_value(serde_json::json!({
            "apiGroups": ["apps"],
            "resources": ["deployments", "deployments/scale"],
            "verbs": ["get", "patch"],
        }))
        .unwrap();
        let rule = Rule::from(&api);
        assert_eq!(rule.api_groups, ["apps"]);
        assert!(rule.resource_names.is_empty());
        assert!(rule.allows(&ResourceAccess::new("patch", "apps", "deployments/scale")));
    }

    #[test]
    fn non_resource_policy_rule_matches_nothing() {
        let api: PolicyRule = serde_json::from_value(serde_json::json!({
            "nonResourceURLs": ["/healthz"],
            "verbs": ["*"],
        }))
        .unwrap();
        assert!(!Rule::from(&api).allows(&get_pod()));
    }
}
