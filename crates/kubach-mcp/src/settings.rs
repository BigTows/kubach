//! Per-context security settings the MCP layer obeys (ADR 0005, "Настройки
//! безопасности контекста").
//!
//! The app fills [`SecurityPolicy`] from its settings store (#39) through
//! [`SecurityPolicySource`]; this crate never reads settings itself. Defaults
//! are the strict values from the ADR.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Hard upper bound for a session TTL, whatever the settings say.
pub const MAX_SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// Default session TTL.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// What an agent may do in the session's namespace at most, ordered by
/// breadth. Since #227 a session's profile is a ceiling: each right within
/// it is granted by the user when the agent first needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// `get`/`list`/`watch` and events, no Secrets.
    ReadOnly,
    /// Read-only plus logs and exec.
    Debug,
    /// Debug plus apply, scale, rollout restart and delete.
    Deploy,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Debug => "debug",
            Self::Deploy => "deploy",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How far the deploy profile may go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployMode {
    /// Only server-side dry-run and diff; nothing is persisted.
    #[default]
    DryRunOnly,
    /// Real changes after the user confirms each one.
    ApplyAfterConfirmation,
}

/// Exec into containers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecMode {
    #[default]
    Disabled,
    /// One-shot commands, each confirmed by the user.
    OneShot,
    /// One-shot commands plus an interactive shell (the shell is a UI
    /// feature; over MCP only one-shot commands exist).
    Interactive,
}

impl ExecMode {
    pub fn allows_one_shot(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// What the agent may learn about Secrets (#227). Values in other tools'
/// output stay masked in every case.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SecretAccess {
    /// No Secret tools at all.
    None,
    /// Names, types, keys, sizes, references and checks done inside Kubach.
    #[default]
    Metadata,
    /// Metadata, plus revealing one Secret's values after the user confirms
    /// that very Secret.
    Reveal,
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
    /// The broadest profile a session may use.
    pub max_profile: Profile,
    /// Temporary SA + Role + RoleBinding per session.
    pub temporary_objects: bool,
    pub deploy: DeployMode,
    pub exec: ExecMode,
    /// Longest TTL a session may request (capped by [`MAX_SESSION_TTL`]).
    pub max_ttl: Duration,
    /// TTL when the session does not ask for one.
    pub default_ttl: Duration,
    /// Allow more than one namespace per session.
    pub multiple_namespaces: bool,
    /// Identity to impersonate, if an administrator prepared one.
    pub impersonation: Option<ImpersonationTarget>,
    /// Secret tools; already capped by "Hide Secret values".
    pub secrets: SecretAccess,
    /// Refuse to start a session that would fall back to client-only
    /// limits (#197).
    pub require_server_enforced: bool,
    /// Kinds beyond the built-in allowlist (`READABLE`/`WRITABLE` of
    /// [`rules`](crate::rules)): custom resources and the rest (#192).
    /// Secrets, ServiceAccounts and RBAC stay off-limits either way.
    pub other_kinds: bool,
    /// "Protected cluster" (#417): the agent may not ask to raise the
    /// ceiling from a session; such calls are refused with an explanation.
    #[serde(default)]
    pub protected: bool,
}

impl Default for SecurityPolicy {
    /// Strict defaults for a context the user has not configured: agent
    /// access on (#9), but read-only, dry-run only, no exec, Secret metadata only.
    fn default() -> Self {
        Self {
            mcp_enabled: true,
            max_profile: Profile::ReadOnly,
            temporary_objects: false,
            deploy: DeployMode::DryRunOnly,
            exec: ExecMode::Disabled,
            max_ttl: DEFAULT_SESSION_TTL,
            default_ttl: DEFAULT_SESSION_TTL,
            multiple_namespaces: false,
            impersonation: None,
            secrets: SecretAccess::Metadata,
            require_server_enforced: false,
            other_kinds: false,
            protected: false,
        }
    }
}

impl SecurityPolicy {
    /// The "Development" preset the UI offers for local clusters. Never
    /// applied automatically.
    pub fn development() -> Self {
        Self {
            mcp_enabled: true,
            max_profile: Profile::Deploy,
            temporary_objects: true,
            deploy: DeployMode::ApplyAfterConfirmation,
            exec: ExecMode::OneShot,
            max_ttl: MAX_SESSION_TTL,
            default_ttl: DEFAULT_SESSION_TTL,
            multiple_namespaces: false,
            impersonation: None,
            secrets: SecretAccess::Reveal,
            require_server_enforced: false,
            other_kinds: false,
            protected: false,
        }
    }

    /// The effective TTL cap.
    pub fn ttl_cap(&self) -> Duration {
        self.max_ttl.min(MAX_SESSION_TTL)
    }
}

impl From<&kubach_core::settings::ContextPolicy> for SecurityPolicy {
    /// The context's stored settings (#39) as the MCP layer reads them.
    fn from(stored: &kubach_core::settings::ContextPolicy) -> Self {
        use kubach_core::settings as core;
        let stored = stored.clone().normalized();
        let max_ttl = Duration::from_secs(u64::from(stored.session_ttl_minutes) * 60);
        Self {
            mcp_enabled: stored.mcp_enabled,
            max_profile: match stored.max_profile {
                core::Profile::ReadOnly => Profile::ReadOnly,
                core::Profile::Debug => Profile::Debug,
                core::Profile::Deploy => Profile::Deploy,
            },
            temporary_objects: stored.temporary_objects,
            deploy: match stored.deploy {
                core::DeployMode::DryRun => DeployMode::DryRunOnly,
                core::DeployMode::ApplyWithConfirmation => DeployMode::ApplyAfterConfirmation,
            },
            exec: match stored.exec {
                core::ExecMode::Disabled => ExecMode::Disabled,
                core::ExecMode::ConfirmEach => ExecMode::OneShot,
                core::ExecMode::InteractiveShell => ExecMode::Interactive,
            },
            max_ttl,
            default_ttl: DEFAULT_SESSION_TTL.min(max_ttl),
            multiple_namespaces: stored.namespaces == core::NamespaceLimit::Multiple,
            impersonation: None,
            secrets: match stored.effective_agent_secrets() {
                core::AgentSecrets::None => SecretAccess::None,
                core::AgentSecrets::MetadataAndChecks => SecretAccess::Metadata,
                core::AgentSecrets::RevealOnRequest => SecretAccess::Reveal,
            },
            require_server_enforced: stored.require_server_enforced,
            other_kinds: stored.agent_other_kinds,
            protected: stored.protected,
        }
    }
}

impl Profile {
    pub const ALL: [Profile; 3] = [Self::ReadOnly, Self::Debug, Self::Deploy];

    /// Parses `read-only`, `debug` or `deploy`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.as_str() == text)
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
    fn defaults_are_strict() {
        let policy = SecurityPolicy::default();
        assert!(policy.mcp_enabled, "agent access is on by default (#9)");
        assert_eq!(policy.max_profile, Profile::ReadOnly);
        assert!(!policy.temporary_objects);
        assert_eq!(policy.deploy, DeployMode::DryRunOnly);
        assert_eq!(policy.exec, ExecMode::Disabled);
        assert_eq!(policy.max_ttl, DEFAULT_SESSION_TTL);
        assert!(!policy.multiple_namespaces);
        assert!(policy.impersonation.is_none());
        assert!(!policy.other_kinds);
    }

    #[test]
    fn ttl_is_capped() {
        let policy = SecurityPolicy {
            max_ttl: Duration::from_secs(100 * 3600),
            ..SecurityPolicy::development()
        };
        assert_eq!(policy.ttl_cap(), MAX_SESSION_TTL);
    }

    #[test]
    fn from_stored_settings() {
        use kubach_core::settings::ContextPolicy;
        let strict = SecurityPolicy::from(&ContextPolicy::strict());
        assert_eq!(strict, SecurityPolicy::default());
        let dev = SecurityPolicy::from(&ContextPolicy::development());
        assert!(dev.mcp_enabled);
        assert_eq!(dev.max_profile, Profile::Deploy);
        assert_eq!(dev.deploy, DeployMode::ApplyAfterConfirmation);
        assert_eq!(dev.exec, ExecMode::OneShot);
        assert_eq!(dev.max_ttl, Duration::from_secs(2 * 3600));
        assert_eq!(dev.default_ttl, DEFAULT_SESSION_TTL);
        assert!(dev.multiple_namespaces);
        assert_eq!(dev.secrets, SecretAccess::Reveal);
        assert_eq!(strict.secrets, SecretAccess::Metadata);
        // Hidden values cap the agent at metadata.
        let hidden = SecurityPolicy::from(&ContextPolicy {
            hide_secret_values: true,
            ..ContextPolicy::development()
        });
        assert_eq!(hidden.secrets, SecretAccess::Metadata);
        assert!(!dev.other_kinds);
        let crds = SecurityPolicy::from(&ContextPolicy {
            agent_other_kinds: true,
            ..ContextPolicy::strict()
        });
        assert!(crds.other_kinds);
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
