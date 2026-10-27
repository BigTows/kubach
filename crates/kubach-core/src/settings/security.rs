//! Per-context security settings (ADR 0005, #39): what the MCP agent may
//! do, and whether the user sees Secret values in the UI (#96).
//!
//! A new context is strict: MCP off, read-only, nothing created in the
//! cluster, Secret values hidden. Settings only restrict; a session never
//! gets more than the context allows nor more than the user's own RBAC.

use serde::{Deserialize, Serialize};

/// Default session TTL.
pub const DEFAULT_SESSION_TTL_MINUTES: u32 = 30;
/// Longest allowed session TTL (8 h).
pub const MAX_SESSION_TTL_MINUTES: u32 = 8 * 60;
/// Shortest allowed session TTL.
pub const MIN_SESSION_TTL_MINUTES: u32 = 15;
/// The session ends when the window closes. Not configurable.
pub const SESSION_ENDS_ON_WINDOW_CLOSE: bool = true;

/// What the agent may do at most (ADR 0005 profiles), ordered by power.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// `get`/`list`/`watch` and events, without Secrets.
    #[default]
    ReadOnly,
    /// Read-only plus `pods/log` and `pods/exec`.
    Debug,
    /// Read-only plus apply, rollout, scale and delete.
    Deploy,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::ReadOnly, Profile::Debug, Profile::Deploy];
}

/// How changes from the agent reach the cluster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployMode {
    /// Server-side dry run and diff only.
    #[default]
    DryRun,
    /// Applied after the user confirms each change.
    ApplyWithConfirmation,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ExecMode {
    #[default]
    Disabled,
    /// One-off commands, each confirmed by the user.
    ConfirmEach,
    InteractiveShell,
}

/// What the agent may learn about Secrets (#227). Values in
/// `get_resource`, `describe_resource`, logs and exec stay masked whatever
/// this says; only the reveal tool hands out values, one Secret at a time
/// and after the user confirmed that very Secret.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum AgentSecrets {
    /// Nothing about Secrets.
    None,
    /// Names, types, keys, sizes, references from workloads and checks done
    /// inside Kubach (empty, valid JSON, certificate dates…). No values and
    /// no hashes of values.
    #[default]
    MetadataAndChecks,
    /// Metadata and checks, plus revealing one Secret's values after the
    /// user confirms that very Secret.
    RevealOnRequest,
}

impl AgentSecrets {
    pub const ALL: [AgentSecrets; 3] = [
        AgentSecrets::None,
        AgentSecrets::MetadataAndChecks,
        AgentSecrets::RevealOnRequest,
    ];
}

/// How many namespaces one session may cover.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum NamespaceLimit {
    #[default]
    Single,
    Multiple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// The defaults.
    Strict,
    /// Everything allowed, every change confirmed. For local dev clusters.
    Development,
}

/// Security settings of one context. `Default` is the strict preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextPolicy {
    /// Whether an MCP agent may open sessions on this context at all.
    pub mcp_enabled: bool,
    pub max_profile: Profile,
    /// Temporary ServiceAccount + Role + RoleBinding for the session.
    pub temporary_objects: bool,
    pub deploy: DeployMode,
    pub exec: ExecMode,
    pub session_ttl_minutes: u32,
    pub namespaces: NamespaceLimit,
    /// Secret values stay hidden from the user in the UI as well: no
    /// show, copy or load of a value, no reveal in Helm and Kustomize.
    /// Keys can still be replaced blindly. See [`ContextPolicy::allows_secret`].
    /// Also caps [`agent_secrets`](Self::agent_secrets): the agent never gets
    /// a value the user may not see.
    pub hide_secret_values: bool,
    /// What the agent may learn about Secrets; see
    /// [`ContextPolicy::effective_agent_secrets`].
    pub agent_secrets: AgentSecrets,
    /// A session starts only when the API server enforces its limits too
    /// (impersonation or a temporary ServiceAccount). Without it a session
    /// whose server-side identity fails falls back to limits only in Kubach,
    /// with a warning (#197). Stricter than the default, never loosened.
    pub require_server_enforced: bool,
    /// The agent may read and change kinds beyond the built-in allowlist
    /// (custom resources such as Flux or Argo CD objects, #192). Secrets,
    /// ServiceAccounts and RBAC stay off-limits either way.
    pub agent_other_kinds: bool,
    /// "Protected cluster" (#417): the agent cannot ask to raise the
    /// ceiling (profile, applying changes, exec) from a session; the request
    /// is refused with an explanation and the user is not asked. Turned off
    /// only here, in the settings. Stricter than the default, never loosened.
    pub protected: bool,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self::strict()
    }
}

/// A setting that is looser than the strict default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loosened {
    McpEnabled,
    MaxProfile,
    TemporaryObjects,
    Deploy,
    Exec,
    SessionTtl,
    Namespaces,
    /// Secret values are shown in the UI.
    SecretValues,
    /// The agent may ask to reveal Secret values.
    AgentSecretValues,
    /// The agent may use kinds beyond the built-in allowlist (#192).
    AgentOtherKinds,
}

impl Loosened {
    /// What is looser, for lists like "Looser than strict: exec, …".
    pub fn label(self) -> &'static str {
        match self {
            Self::McpEnabled => "AI agent access",
            Self::MaxProfile => "profile",
            Self::TemporaryObjects => "separate identity in the cluster",
            Self::Deploy => "applying changes",
            Self::Exec => "exec",
            Self::SessionTtl => "session length",
            Self::Namespaces => "several namespaces",
            Self::SecretValues => "Secret values shown",
            Self::AgentSecretValues => "Secret values revealed to the agent on request",
            Self::AgentOtherKinds => "custom resources for the agent",
        }
    }
}

/// Something the UI does with a Secret, checked against
/// [`ContextPolicy::allows_secret`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretAction {
    /// Show one value in the details panel.
    Show,
    /// Copy a shown value.
    Copy,
    /// Fetch a value into the editor to change it.
    LoadValue,
    /// Reveal data that may hold Secret values: Helm release values and
    /// manifests, Kustomize build output and its diff with the cluster.
    Reveal,
    /// Typed summary (registries and user names, certificate dates); never
    /// passwords or private keys.
    Summary,
    /// Type a new value without loading the current one.
    ReplaceBlind,
    AddKey,
    RemoveKey,
    /// Create a new Secret from values the user types.
    Create,
}

impl SecretAction {
    /// Whether the action puts an existing value on screen or in the
    /// clipboard.
    pub fn exposes_value(self) -> bool {
        matches!(
            self,
            Self::Show | Self::Copy | Self::LoadValue | Self::Reveal
        )
    }
}

/// Why a value action is unavailable, for tooltips and notes.
pub const SECRET_VALUES_HIDDEN: &str =
    "Secret values are hidden on this context by its security settings.";

/// A combination of values that is invalid; see [`ContextPolicy::normalized`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PolicyIssue {
    /// TTL outside `MIN_SESSION_TTL_MINUTES..=MAX_SESSION_TTL_MINUTES`.
    TtlOutOfRange,
    /// Exec is allowed but the profile is below debug.
    ExecAboveProfile,
    /// Apply is allowed but the profile is below deploy.
    DeployAboveProfile,
    /// Revealing values to the agent is allowed while they are hidden from
    /// the user.
    RevealWithHiddenValues,
}

impl ContextPolicy {
    pub fn strict() -> Self {
        Self {
            mcp_enabled: false,
            max_profile: Profile::ReadOnly,
            temporary_objects: false,
            deploy: DeployMode::DryRun,
            exec: ExecMode::Disabled,
            session_ttl_minutes: DEFAULT_SESSION_TTL_MINUTES,
            namespaces: NamespaceLimit::Single,
            hide_secret_values: true,
            agent_secrets: AgentSecrets::MetadataAndChecks,
            require_server_enforced: false,
            agent_other_kinds: false,
            protected: false,
        }
    }

    /// Everything allowed, with confirmation for each change or command.
    pub fn development() -> Self {
        Self {
            mcp_enabled: true,
            max_profile: Profile::Deploy,
            temporary_objects: true,
            deploy: DeployMode::ApplyWithConfirmation,
            exec: ExecMode::ConfirmEach,
            session_ttl_minutes: 2 * 60,
            namespaces: NamespaceLimit::Multiple,
            hide_secret_values: false,
            agent_secrets: AgentSecrets::RevealOnRequest,
            require_server_enforced: false,
            // Custom resources stay an explicit choice even on dev clusters:
            // a GitOps object makes a controller act with its own rights.
            agent_other_kinds: false,
            protected: false,
        }
    }

    pub fn preset(preset: Preset) -> Self {
        match preset {
            Preset::Strict => Self::strict(),
            Preset::Development => Self::development(),
        }
    }

    /// The preset these values match exactly, if any.
    pub fn matching_preset(&self) -> Option<Preset> {
        [Preset::Strict, Preset::Development]
            .into_iter()
            .find(|preset| *self == Self::preset(*preset))
    }

    /// Settings looser than the strict defaults, for the UI warning.
    pub fn loosened(&self) -> Vec<Loosened> {
        let strict = Self::strict();
        [
            (self.mcp_enabled, Loosened::McpEnabled),
            (self.max_profile > strict.max_profile, Loosened::MaxProfile),
            (self.temporary_objects, Loosened::TemporaryObjects),
            (self.deploy != strict.deploy, Loosened::Deploy),
            (self.exec > strict.exec, Loosened::Exec),
            (
                self.session_ttl_minutes > strict.session_ttl_minutes,
                Loosened::SessionTtl,
            ),
            (self.namespaces > strict.namespaces, Loosened::Namespaces),
            (
                !self.hide_secret_values && strict.hide_secret_values,
                Loosened::SecretValues,
            ),
            (
                self.effective_agent_secrets() > strict.agent_secrets,
                Loosened::AgentSecretValues,
            ),
            (self.agent_other_kinds, Loosened::AgentOtherKinds),
        ]
        .into_iter()
        .filter_map(|(loose, which)| loose.then_some(which))
        .collect()
    }

    pub fn validate(&self) -> Vec<PolicyIssue> {
        let mut issues = Vec::new();
        if !(MIN_SESSION_TTL_MINUTES..=MAX_SESSION_TTL_MINUTES).contains(&self.session_ttl_minutes)
        {
            issues.push(PolicyIssue::TtlOutOfRange);
        }
        if self.exec != ExecMode::Disabled && self.max_profile < Profile::Debug {
            issues.push(PolicyIssue::ExecAboveProfile);
        }
        if self.deploy != DeployMode::DryRun && self.max_profile < Profile::Deploy {
            issues.push(PolicyIssue::DeployAboveProfile);
        }
        if self.hide_secret_values && self.agent_secrets == AgentSecrets::RevealOnRequest {
            issues.push(PolicyIssue::RevealWithHiddenValues);
        }
        issues
    }

    /// Fixes [`validate`](Self::validate) issues toward the stricter value:
    /// clamps the TTL, drops exec / apply the profile does not cover and
    /// revealing Secret values to the agent while they are hidden.
    pub fn normalized(mut self) -> Self {
        self.agent_secrets = self.effective_agent_secrets();
        self.session_ttl_minutes = self
            .session_ttl_minutes
            .clamp(MIN_SESSION_TTL_MINUTES, MAX_SESSION_TTL_MINUTES);
        if self.max_profile < Profile::Debug {
            self.exec = ExecMode::Disabled;
        }
        if self.max_profile < Profile::Deploy {
            self.deploy = DeployMode::DryRun;
        }
        self
    }

    /// Whether a session with `requested` profile may start on this context.
    pub fn allows(&self, requested: Profile) -> bool {
        self.mcp_enabled && requested <= self.max_profile
    }

    /// Whether the user may do `action` with a Secret of this context in
    /// the UI. With [`hide_secret_values`](Self::hide_secret_values) only
    /// actions that do not expose an existing value remain. Unrelated to
    /// the agent, which has its own setting ([`agent_secrets`](Self::agent_secrets)).
    pub fn allows_secret(&self, action: SecretAction) -> bool {
        !(self.hide_secret_values && action.exposes_value())
    }

    /// What the agent may learn about Secrets: [`agent_secrets`](Self::agent_secrets),
    /// at most metadata and checks while values are hidden from the user.
    pub fn effective_agent_secrets(&self) -> AgentSecrets {
        if self.hide_secret_values {
            self.agent_secrets.min(AgentSecrets::MetadataAndChecks)
        } else {
            self.agent_secrets
        }
    }

    /// Whether a session may request a TTL of `minutes`.
    pub fn allows_ttl(&self, minutes: u32) -> bool {
        minutes > 0 && minutes <= self.session_ttl_minutes.min(MAX_SESSION_TTL_MINUTES)
    }

    /// Raises the agent's ceiling at least to `profile`, with applying
    /// changes (after confirmation) and one-off exec commands when asked,
    /// because the user ticked "Always allow on this cluster" in an agent's
    /// request (#417). Never lowers anything and never touches other
    /// settings. Returns what changed, as `max profile: read-only → deploy`
    /// lines; nothing on a protected context.
    pub fn raise_ceiling(&mut self, profile: Profile, apply: bool, exec: bool) -> Vec<String> {
        if self.protected {
            return Vec::new();
        }
        let mut changes = Vec::new();
        let profile = if apply {
            profile.max(Profile::Deploy)
        } else if exec {
            profile.max(Profile::Debug)
        } else {
            profile
        };
        if profile > self.max_profile {
            changes.push(format!(
                "max profile: {} → {}",
                profile_name(self.max_profile),
                profile_name(profile)
            ));
            self.max_profile = profile;
        }
        if apply && self.deploy == DeployMode::DryRun {
            changes.push("changes: dry-run only → apply after confirmation".to_owned());
            self.deploy = DeployMode::ApplyWithConfirmation;
        }
        if exec && self.exec == ExecMode::Disabled {
            changes.push("commands in containers: disabled → confirm each".to_owned());
            self.exec = ExecMode::ConfirmEach;
        }
        changes
    }
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::ReadOnly => "read-only",
        Profile::Debug => "debug",
        Profile::Deploy => "deploy",
    }
}

/// A trace of the agent's ceiling raised for good from a session (#417):
/// what, when and from which session. Kept next to the policies so the
/// settings file itself tells why a context allows more than it used to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeilingRaise {
    pub context: String,
    /// Normalized API server URL.
    pub server: String,
    /// RFC 3339.
    pub time: String,
    /// The MCP session the request came from.
    pub session: String,
    /// The MCP client's self-reported name.
    #[serde(default)]
    pub client: String,
    /// `max profile: read-only → deploy` lines.
    pub changes: Vec<String>,
}

impl CeilingRaise {
    pub fn key(&self) -> ContextKey {
        ContextKey::new(self.context.clone(), &self.server)
    }
}

/// Traces kept per settings file; the oldest go first.
pub const MAX_CEILING_RAISES: usize = 50;

/// Identifies a cluster: context name **and** API server URL, so renaming a
/// context does not carry its permissions over to another cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContextKey {
    pub context: String,
    /// Normalized: see [`normalize_server`].
    pub server: String,
}

impl ContextKey {
    pub fn new(context: impl Into<String>, server: &str) -> Self {
        Self {
            context: context.into(),
            server: normalize_server(server),
        }
    }

    /// Local dev clusters: `minikube`, `kind-*`, `docker-desktop`, `k3d-*`,
    /// or an API server on the loopback interface.
    pub fn is_local(&self) -> bool {
        is_local_cluster(&self.context, &self.server)
    }

    /// Preset to *suggest* (never apply automatically) for this cluster.
    pub fn suggested_preset(&self) -> Option<Preset> {
        self.is_local().then_some(Preset::Development)
    }
}

/// Trims whitespace and trailing slashes and lowercases scheme and host;
/// the path is kept as is (proxies such as Rancher put cluster ids there).
pub fn normalize_server(server: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    let Some((scheme, rest)) = server.split_once("://") else {
        return server.to_ascii_lowercase();
    };
    let (authority, path) = match rest.find('/') {
        Some(ix) => rest.split_at(ix),
        None => (rest, ""),
    };
    format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase(),
        path
    )
}

/// See [`ContextKey::is_local`].
pub fn is_local_cluster(context: &str, server: &str) -> bool {
    let name = context.to_ascii_lowercase();
    let local_name = matches!(
        name.as_str(),
        "minikube" | "docker-desktop" | "docker-for-desktop"
    ) || name.starts_with("kind-")
        || name.starts_with("k3d-");
    local_name || is_loopback_server(server)
}

fn is_loopback_server(server: &str) -> bool {
    let server = normalize_server(server);
    let rest = server
        .split_once("://")
        .map_or(server.as_str(), |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    // Drop userinfo, then the port (IPv6 hosts are bracketed).
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Stored policy of one context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSecurity {
    pub context: String,
    /// Normalized API server URL.
    pub server: String,
    #[serde(flatten)]
    pub policy: ContextPolicy,
}

impl ContextSecurity {
    pub fn key(&self) -> ContextKey {
        ContextKey::new(self.context.clone(), &self.server)
    }
}

/// Security settings of all contexts. Only non-default policies are stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecuritySettings {
    #[serde(rename = "context")]
    pub contexts: Vec<ContextSecurity>,
    /// Ceilings raised for good from agent sessions (#417), oldest first.
    #[serde(rename = "ceiling-raise", skip_serializing_if = "Vec::is_empty")]
    pub ceiling_raises: Vec<CeilingRaise>,
}

impl SecuritySettings {
    /// Stored policy for `key`, or the strict default. A context whose name
    /// matches but whose server differs gets the default.
    pub fn policy(&self, key: &ContextKey) -> ContextPolicy {
        self.find(key)
            .map(|entry| entry.policy.clone())
            .unwrap_or_default()
    }

    pub fn find(&self, key: &ContextKey) -> Option<&ContextSecurity> {
        self.contexts.iter().find(|entry| entry.key() == *key)
    }

    /// Stores `policy` (normalized) for `key`; a strict policy removes the
    /// entry so the file only lists exceptions.
    pub fn set_policy(&mut self, key: &ContextKey, policy: ContextPolicy) {
        let policy = policy.normalized();
        self.contexts.retain(|entry| entry.key() != *key);
        if policy != ContextPolicy::strict() {
            self.contexts.push(ContextSecurity {
                context: key.context.clone(),
                server: key.server.clone(),
                policy,
            });
        }
    }

    /// Raises the ceiling of `key` as [`ContextPolicy::raise_ceiling`] does
    /// and records the trace; returns what changed (nothing on a protected
    /// context or when the ceiling already covers the request, and then no
    /// trace is written either).
    pub fn raise_ceiling(
        &mut self,
        key: &ContextKey,
        (profile, apply, exec): (Profile, bool, bool),
        session: &str,
        client: &str,
        time: &str,
    ) -> Vec<String> {
        let mut policy = self.policy(key);
        let changes = policy.raise_ceiling(profile, apply, exec);
        if changes.is_empty() {
            return changes;
        }
        self.set_policy(key, policy);
        self.ceiling_raises.push(CeilingRaise {
            context: key.context.clone(),
            server: key.server.clone(),
            time: time.to_owned(),
            session: session.to_owned(),
            client: client.to_owned(),
            changes: changes.clone(),
        });
        let extra = self.ceiling_raises.len().saturating_sub(MAX_CEILING_RAISES);
        self.ceiling_raises.drain(..extra);
        changes
    }

    /// Traces of `key`, newest first.
    pub fn raises_of(&self, key: &ContextKey) -> Vec<&CeilingRaise> {
        self.ceiling_raises
            .iter()
            .rev()
            .filter(|raise| raise.key() == *key)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_strict() {
        let policy = ContextPolicy::default();
        assert!(!policy.mcp_enabled);
        assert_eq!(policy.max_profile, Profile::ReadOnly);
        assert!(!policy.temporary_objects);
        assert_eq!(policy.deploy, DeployMode::DryRun);
        assert_eq!(policy.exec, ExecMode::Disabled);
        assert_eq!(policy.session_ttl_minutes, 30);
        assert_eq!(policy.namespaces, NamespaceLimit::Single);
        assert!(policy.hide_secret_values);
        assert!(policy.loosened().is_empty());
        assert!(policy.validate().is_empty());
        assert_eq!(policy.matching_preset(), Some(Preset::Strict));
        assert!(!policy.allows(Profile::ReadOnly));
        const { assert!(SESSION_ENDS_ON_WINDOW_CLOSE) };
        assert_eq!(policy.agent_secrets, AgentSecrets::MetadataAndChecks);
        assert_eq!(MAX_SESSION_TTL_MINUTES, 480);
    }

    #[test]
    fn development_preset_is_valid_and_loose() {
        let policy = ContextPolicy::preset(Preset::Development);
        assert!(policy.validate().is_empty());
        assert_eq!(policy.clone().normalized(), policy);
        assert_eq!(policy.matching_preset(), Some(Preset::Development));
        assert_eq!(policy.loosened().len(), 9);
        assert!(!policy.hide_secret_values);
        assert_eq!(
            policy.effective_agent_secrets(),
            AgentSecrets::RevealOnRequest
        );
        assert!(policy.allows(Profile::Deploy));
        assert_eq!(policy.exec, ExecMode::ConfirmEach);
        assert_eq!(policy.deploy, DeployMode::ApplyWithConfirmation);
    }

    #[test]
    fn custom_values_match_no_preset() {
        let policy = ContextPolicy {
            mcp_enabled: true,
            ..ContextPolicy::strict()
        };
        assert_eq!(policy.matching_preset(), None);
        assert_eq!(policy.loosened(), [Loosened::McpEnabled]);
        assert!(policy.allows(Profile::ReadOnly));
        assert!(!policy.allows(Profile::Debug));
    }

    #[test]
    fn validation_and_normalization() {
        let policy = ContextPolicy {
            max_profile: Profile::ReadOnly,
            exec: ExecMode::InteractiveShell,
            deploy: DeployMode::ApplyWithConfirmation,
            session_ttl_minutes: 9 * 60,
            ..ContextPolicy::strict()
        };
        assert_eq!(
            policy.validate(),
            [
                PolicyIssue::TtlOutOfRange,
                PolicyIssue::ExecAboveProfile,
                PolicyIssue::DeployAboveProfile
            ]
        );
        let fixed = policy.normalized();
        assert!(fixed.validate().is_empty());
        assert_eq!(fixed.session_ttl_minutes, MAX_SESSION_TTL_MINUTES);
        assert_eq!(fixed.exec, ExecMode::Disabled);
        assert_eq!(fixed.deploy, DeployMode::DryRun);

        let zero = ContextPolicy {
            session_ttl_minutes: 0,
            ..ContextPolicy::strict()
        };
        assert_eq!(
            zero.normalized().session_ttl_minutes,
            MIN_SESSION_TTL_MINUTES
        );

        // Debug covers exec but not apply.
        let debug = ContextPolicy {
            max_profile: Profile::Debug,
            exec: ExecMode::ConfirmEach,
            deploy: DeployMode::ApplyWithConfirmation,
            ..ContextPolicy::strict()
        };
        assert_eq!(debug.validate(), [PolicyIssue::DeployAboveProfile]);
        let debug = debug.normalized();
        assert_eq!(debug.exec, ExecMode::ConfirmEach);
        assert_eq!(debug.deploy, DeployMode::DryRun);
    }

    const ALL_SECRET_ACTIONS: [SecretAction; 9] = [
        SecretAction::Show,
        SecretAction::Copy,
        SecretAction::LoadValue,
        SecretAction::Reveal,
        SecretAction::Summary,
        SecretAction::ReplaceBlind,
        SecretAction::AddKey,
        SecretAction::RemoveKey,
        SecretAction::Create,
    ];

    #[test]
    fn hidden_secret_values_block_only_exposing_actions() {
        let strict = ContextPolicy::strict();
        for action in ALL_SECRET_ACTIONS {
            assert_eq!(
                strict.allows_secret(action),
                !action.exposes_value(),
                "{action:?}"
            );
        }
        for action in [
            SecretAction::Show,
            SecretAction::Copy,
            SecretAction::LoadValue,
            SecretAction::Reveal,
        ] {
            assert!(!strict.allows_secret(action), "{action:?}");
        }
        // Blind edits stay possible.
        for action in [
            SecretAction::ReplaceBlind,
            SecretAction::AddKey,
            SecretAction::RemoveKey,
            SecretAction::Create,
            SecretAction::Summary,
        ] {
            assert!(strict.allows_secret(action), "{action:?}");
        }
    }

    #[test]
    fn shown_secret_values_allow_everything_and_count_as_loosened() {
        let policy = ContextPolicy {
            hide_secret_values: false,
            ..ContextPolicy::strict()
        };
        assert!(
            ALL_SECRET_ACTIONS
                .into_iter()
                .all(|action| policy.allows_secret(action))
        );
        assert_eq!(policy.loosened(), [Loosened::SecretValues]);
        assert_eq!(policy.matching_preset(), None);
        // Independent of the agent settings: MCP stays off.
        assert!(!policy.allows(Profile::ReadOnly));
        assert!(ContextPolicy::development().allows_secret(SecretAction::Show));
    }

    #[test]
    fn secret_values_setting_is_stored_per_context() {
        let mut settings = SecuritySettings::default();
        let prod = ContextKey::new("prod", "https://prod.example.com");
        let dev = ContextKey::new("dev", "https://dev:6443");
        settings.set_policy(
            &dev,
            ContextPolicy {
                hide_secret_values: false,
                ..ContextPolicy::strict()
            },
        );
        assert!(settings.policy(&dev).allows_secret(SecretAction::Show));
        assert!(!settings.policy(&prod).allows_secret(SecretAction::Show));
        // Same name on another server: hidden again.
        let moved = ContextKey::new("dev", "https://other:6443");
        assert!(!settings.policy(&moved).allows_secret(SecretAction::Copy));
    }

    #[test]
    fn hidden_values_cap_what_the_agent_may_reveal() {
        let policy = ContextPolicy {
            agent_secrets: AgentSecrets::RevealOnRequest,
            ..ContextPolicy::strict()
        };
        assert!(policy.hide_secret_values);
        assert_eq!(
            policy.effective_agent_secrets(),
            AgentSecrets::MetadataAndChecks
        );
        assert_eq!(policy.validate(), [PolicyIssue::RevealWithHiddenValues]);
        assert!(policy.loosened().is_empty());
        let fixed = policy.normalized();
        assert_eq!(fixed.agent_secrets, AgentSecrets::MetadataAndChecks);
        assert_eq!(fixed, ContextPolicy::strict());

        let shown = ContextPolicy {
            hide_secret_values: false,
            agent_secrets: AgentSecrets::RevealOnRequest,
            ..ContextPolicy::strict()
        };
        assert!(shown.validate().is_empty());
        assert_eq!(
            shown.loosened(),
            [Loosened::SecretValues, Loosened::AgentSecretValues]
        );
        // Less than the default is stricter, not looser.
        let none = ContextPolicy {
            agent_secrets: AgentSecrets::None,
            ..ContextPolicy::strict()
        };
        assert!(none.loosened().is_empty());
        assert_eq!(none.effective_agent_secrets(), AgentSecrets::None);
    }

    #[test]
    fn other_kinds_are_off_by_default_and_looser() {
        assert!(!ContextPolicy::strict().agent_other_kinds);
        assert!(!ContextPolicy::development().agent_other_kinds);
        let policy = ContextPolicy {
            agent_other_kinds: true,
            ..ContextPolicy::strict()
        };
        assert!(policy.validate().is_empty());
        assert_eq!(policy.loosened(), [Loosened::AgentOtherKinds]);
        assert_eq!(policy.matching_preset(), None);
        // Settings stored before #192 read as off.
        let old: ContextPolicy = serde_json::from_str(r#"{"mcp_enabled": true}"#).unwrap();
        assert!(!old.agent_other_kinds);
    }

    #[test]
    fn requiring_server_limits_is_stricter_and_stored() {
        let policy = ContextPolicy {
            require_server_enforced: true,
            ..ContextPolicy::development()
        };
        assert!(!ContextPolicy::strict().require_server_enforced);
        assert!(policy.validate().is_empty());
        assert_eq!(policy.loosened(), ContextPolicy::development().loosened());
        assert_eq!(policy.matching_preset(), None);
        let mut settings = SecuritySettings::default();
        let key = ContextKey::new("prod", "https://prod.example.com");
        settings.set_policy(
            &key,
            ContextPolicy {
                require_server_enforced: true,
                ..ContextPolicy::strict()
            },
        );
        assert!(settings.policy(&key).require_server_enforced);
    }

    #[test]
    fn raising_the_ceiling_only_widens_and_leaves_a_trace() {
        let key = ContextKey::new("minikube", "https://192.168.49.2:8443");
        let mut settings = SecuritySettings::default();
        settings.set_policy(
            &key,
            ContextPolicy {
                mcp_enabled: true,
                ..ContextPolicy::strict()
            },
        );
        let changes = settings.raise_ceiling(
            &key,
            (Profile::Deploy, true, false),
            "s1",
            "claude-code",
            "2026-10-25T10:00:00Z",
        );
        assert_eq!(
            changes,
            [
                "max profile: read-only → deploy",
                "changes: dry-run only → apply after confirmation"
            ]
        );
        let policy = settings.policy(&key);
        assert_eq!(policy.max_profile, Profile::Deploy);
        assert_eq!(policy.deploy, DeployMode::ApplyWithConfirmation);
        assert_eq!(policy.exec, ExecMode::Disabled, "exec was not asked for");
        assert!(policy.hide_secret_values, "other settings stay");
        let raises = settings.raises_of(&key);
        assert_eq!(raises.len(), 1);
        assert_eq!(raises[0].session, "s1");
        // Already covered: nothing changes, no second trace.
        assert!(
            settings
                .raise_ceiling(&key, (Profile::Debug, false, false), "s2", "", "t")
                .is_empty()
        );
        assert_eq!(settings.raises_of(&key).len(), 1);
        // Exec implies debug at least.
        let mut lone = ContextPolicy::strict();
        assert_eq!(lone.raise_ceiling(Profile::ReadOnly, false, true).len(), 2);
        assert_eq!(lone.max_profile, Profile::Debug);
        // Round trip through the file format.
        let toml = toml::to_string(&settings).unwrap();
        let back: SecuritySettings = toml::from_str(&toml).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn a_protected_cluster_is_never_raised() {
        let key = ContextKey::new("prod", "https://prod.example.com");
        let mut settings = SecuritySettings::default();
        settings.set_policy(
            &key,
            ContextPolicy {
                mcp_enabled: true,
                protected: true,
                ..ContextPolicy::strict()
            },
        );
        let policy = settings.policy(&key);
        assert_eq!(
            policy.loosened(),
            [Loosened::McpEnabled],
            "protection is stricter"
        );
        assert!(
            settings
                .raise_ceiling(&key, (Profile::Deploy, true, true), "s", "", "t")
                .is_empty()
        );
        assert_eq!(settings.policy(&key), policy);
        assert!(settings.ceiling_raises.is_empty());
    }

    #[test]
    fn ttl_requests_are_bounded_by_the_context() {
        let policy = ContextPolicy::strict();
        assert!(policy.allows_ttl(30));
        assert!(!policy.allows_ttl(31));
        assert!(!policy.allows_ttl(0));
    }

    #[test]
    fn keyed_by_context_and_server() {
        let mut settings = SecuritySettings::default();
        let minikube = ContextKey::new("minikube", "https://192.168.49.2:8443/");
        settings.set_policy(&minikube, ContextPolicy::development());

        // Same server written differently: same key.
        let same = ContextKey::new("minikube", " HTTPS://192.168.49.2:8443 ");
        assert_eq!(settings.policy(&same), ContextPolicy::development());

        // Same name, other cluster (a renamed context): strict.
        let other = ContextKey::new("minikube", "https://prod.example.com");
        assert_eq!(settings.policy(&other), ContextPolicy::strict());
        // Same server, other name: strict as well.
        let renamed = ContextKey::new("dev", "https://192.168.49.2:8443");
        assert_eq!(settings.policy(&renamed), ContextPolicy::strict());

        // Setting back to strict removes the entry.
        settings.set_policy(&same, ContextPolicy::strict());
        assert!(settings.contexts.is_empty());
    }

    #[test]
    fn set_policy_replaces_and_normalizes() {
        let mut settings = SecuritySettings::default();
        let key = ContextKey::new("dev", "https://dev:6443");
        settings.set_policy(&key, ContextPolicy::development());
        settings.set_policy(
            &key,
            ContextPolicy {
                mcp_enabled: true,
                exec: ExecMode::InteractiveShell,
                ..ContextPolicy::strict()
            },
        );
        assert_eq!(settings.contexts.len(), 1);
        let stored = settings.policy(&key);
        assert!(stored.mcp_enabled);
        assert_eq!(stored.exec, ExecMode::Disabled);
    }

    #[test]
    fn server_normalization_keeps_path_case() {
        assert_eq!(
            normalize_server("https://Rancher.Example.com/k8s/clusters/C-AbC/"),
            "https://rancher.example.com/k8s/clusters/C-AbC"
        );
    }

    #[test]
    fn detects_local_clusters() {
        let local = [
            ("minikube", "https://192.168.49.2:8443"),
            ("kind-dev", "https://10.0.0.1:6443"),
            ("docker-desktop", "https://kubernetes.docker.internal:6443"),
            ("k3d-test", "https://0.0.0.0:6550"),
            ("my-ctx", "https://127.0.0.1:6443"),
            ("my-ctx", "https://localhost:6443"),
            ("my-ctx", "https://[::1]:6443"),
            ("my-ctx", "https://127.0.1.1"),
        ];
        for (context, server) in local {
            let key = ContextKey::new(context, server);
            assert!(key.is_local(), "{context} {server}");
            assert_eq!(key.suggested_preset(), Some(Preset::Development));
        }
        let remote = [
            ("prod", "https://prod.example.com"),
            ("kindly", "https://10.0.0.1"),
            ("eks", "https://127.example.com"),
            ("gke", "https://localhost.example.com"),
        ];
        for (context, server) in remote {
            let key = ContextKey::new(context, server);
            assert!(!key.is_local(), "{context} {server}");
            assert_eq!(key.suggested_preset(), None);
        }
    }
}
