//! Per-context security settings the MCP layer obeys (ADR 0005, "Настройки
//! безопасности контекста", "Минимальный доступ вместо потолка" #15).
//!
//! The app fills [`SecurityPolicy`] from its settings store (#39) through
//! [`SecurityPolicySource`]; this crate never reads settings itself.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::access::Access;

/// Longest a session lives: until Kubach (or `kubach mcp`) exits, but no
/// longer than this (#15).
pub const MAX_SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// A session without `--ttl` lives as long as it may.
pub const DEFAULT_SESSION_TTL: Duration = MAX_SESSION_TTL;

/// Levels of the agent's access, ordered by breadth. Since #15 the
/// context's minimum is one of these, and more is asked for on request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// `get`/`list`/`watch` and events, no Secrets.
    ReadOnly,
    /// Read-only plus logs and exec.
    Debug,
    /// Debug plus apply, scale, rollout restart, delete and Helm.
    Deploy,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Self::ReadOnly, Self::Debug, Self::Deploy];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Debug => "debug",
            Self::Deploy => "deploy",
        }
    }

    /// Parses `read-only`, `debug` or `deploy`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.as_str() == text)
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<kubach_core::settings::AgentLevel> for Profile {
    fn from(level: kubach_core::settings::AgentLevel) -> Self {
        use kubach_core::settings::AgentLevel;
        match level {
            AgentLevel::Read => Self::ReadOnly,
            AgentLevel::Debug => Self::Debug,
            AgentLevel::Changes => Self::Deploy,
        }
    }
}

impl From<Profile> for kubach_core::settings::AgentLevel {
    fn from(profile: Profile) -> Self {
        match profile {
            Profile::ReadOnly => Self::Read,
            Profile::Debug => Self::Debug,
            Profile::Deploy => Self::Changes,
        }
    }
}

/// An identity prepared by an administrator for impersonation (ADR 0005,
/// option 1). `{namespace}` and `{profile}` in names are substituted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpersonationTarget {
    /// For example `kubach:mcp:{namespace}:{profile}`.
    pub user: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl ImpersonationTarget {
    /// User and groups with placeholders filled in.
    pub fn render(&self, namespace: &str, profile: Profile) -> (String, Vec<String>) {
        let fill = |s: &str| {
            s.replace("{namespace}", namespace)
                .replace("{profile}", profile.as_str())
        };
        (
            fill(&self.user),
            self.groups.iter().map(|g| fill(g)).collect(),
        )
    }
}

/// The "Security" settings of one context, keyed by context name **and**
/// API server URL so that a renamed context does not inherit permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityPolicy {
    /// MCP access for this context at all. On by default (#9); the user
    /// turns it off per context in Settings → Security.
    pub mcp_enabled: bool,
    /// What the agent gets without asking (#15); more is asked for.
    pub minimum: Access,
    /// "Hide Secret values" of the user (#96): then the agent cannot ask to
    /// reveal a value either.
    pub hide_secret_values: bool,
    /// A temporary ServiceAccount per namespace where the user's RBAC
    /// allows creating one. Always on from the settings since #15 (the
    /// identity is chosen automatically); tests turn it off to get
    /// client-only namespaces.
    pub temporary_objects: bool,
    /// Identity to impersonate, if an administrator prepared one.
    pub impersonation: Option<ImpersonationTarget>,
}

impl Default for SecurityPolicy {
    /// A context the user has not configured: agent access on (#9),
    /// reading built-in kinds without asking, Secret values hidden.
    fn default() -> Self {
        Self {
            mcp_enabled: true,
            minimum: Access::NONE,
            hide_secret_values: true,
            temporary_objects: true,
            impersonation: None,
        }
    }
}

impl SecurityPolicy {
    /// The default with `minimum` and Secret values shown.
    pub fn with_minimum(minimum: Access) -> Self {
        Self {
            minimum,
            hide_secret_values: false,
            ..Self::default()
        }
    }

    /// Whether the agent may ask to reveal one Secret's values.
    pub fn reveal_allowed(&self) -> bool {
        !self.hide_secret_values
    }
}

impl From<&kubach_core::settings::ContextPolicy> for SecurityPolicy {
    /// The context's stored settings (#39) as the MCP layer reads them.
    fn from(stored: &kubach_core::settings::ContextPolicy) -> Self {
        let minimum = stored.minimum();
        Self {
            mcp_enabled: stored.mcp_enabled,
            minimum: Access {
                profile: Profile::from(minimum.level),
                secrets: minimum.secrets,
                crds: minimum.crds,
            },
            hide_secret_values: stored.hide_secret_values,
            temporary_objects: true,
            impersonation: None,
        }
    }
}

/// Where the app looks up the settings of a cluster. Implemented by the app
/// over its settings module (#39); this crate only consumes it.
pub trait SecurityPolicySource: Send + Sync {
    fn policy(&self, context: &str, server_url: &str) -> SecurityPolicy;
}

/// The same policy for every cluster; handy for tests and the CLI launcher.
impl SecurityPolicySource for SecurityPolicy {
    fn policy(&self, _context: &str, _server_url: &str) -> SecurityPolicy {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_read_without_asking() {
        let policy = SecurityPolicy::default();
        assert!(policy.mcp_enabled, "agent access is on by default (#9)");
        assert_eq!(policy.minimum, Access::NONE);
        assert!(policy.hide_secret_values);
        assert!(!policy.reveal_allowed());
        assert!(policy.temporary_objects, "the identity is automatic (#15)");
        assert!(policy.impersonation.is_none());
        assert_eq!(DEFAULT_SESSION_TTL, Duration::from_secs(8 * 3600));
    }

    #[test]
    fn from_stored_settings() {
        use kubach_core::settings::{AgentLevel, ContextPolicy};
        let default = SecurityPolicy::from(&ContextPolicy::default());
        assert_eq!(default, SecurityPolicy::default());
        let raised = SecurityPolicy::from(&ContextPolicy {
            minimum_access: AgentLevel::Changes,
            minimum_secrets: true,
            minimum_crds: true,
            hide_secret_values: false,
            mcp_enabled: false,
        });
        assert!(!raised.mcp_enabled);
        assert_eq!(raised.minimum, Access::ALL);
        assert!(raised.reveal_allowed());
        let debug = SecurityPolicy::from(&ContextPolicy {
            minimum_access: AgentLevel::Debug,
            ..ContextPolicy::default()
        });
        assert_eq!(debug.minimum, Access::profile(Profile::Debug));
        for profile in Profile::ALL {
            assert_eq!(Profile::from(AgentLevel::from(profile)), profile);
        }
        assert_eq!(Profile::parse("debug"), Some(Profile::Debug));
        assert_eq!(Profile::parse("admin"), None);
    }

    #[test]
    fn profiles_are_ordered() {
        assert!(Profile::ReadOnly < Profile::Debug);
        assert!(Profile::Debug < Profile::Deploy);
    }

    #[test]
    fn impersonation_placeholders() {
        let target = ImpersonationTarget {
            user: "kubach:mcp:{namespace}:{profile}".into(),
            groups: vec!["kubach:mcp:{profile}".into()],
        };
        let (user, groups) = target.render("web", Profile::Debug);
        assert_eq!(user, "kubach:mcp:web:debug");
        assert_eq!(groups, vec!["kubach:mcp:debug".to_string()]);
    }
}
