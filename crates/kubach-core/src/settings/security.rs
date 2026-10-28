//! Per-context security settings (ADR 0005, #39): what the MCP agent gets
//! without asking, and whether the user sees Secret values in the UI (#96).
//!
//! Since #15 there is no ceiling to configure. A context has a **minimal
//! access** for the agent: a level (read, debug, changes) plus Secrets and
//! custom resources, granted without a question. Everything above it the
//! agent asks for, and the user answers for the session or "by default",
//! which raises the minimum here. Only the user's own RBAC bounds the rest.
//! Settings written before #15 may still hold the old keys (`max_profile`,
//! `protected`, …); they are ignored, and a stored ceiling never becomes a
//! minimum.

use serde::{Deserialize, Serialize};

/// What the agent may do without asking, ordered by power (#15).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum AgentLevel {
    /// Built-in kinds in every namespace, events included; no Secrets.
    #[default]
    Read,
    /// Read plus container logs and exec (each command confirmed).
    Debug,
    /// Debug plus apply, delete, scale, restarts and Helm (each change
    /// confirmed).
    Changes,
}

impl AgentLevel {
    pub const ALL: [AgentLevel; 3] = [AgentLevel::Read, AgentLevel::Debug, AgentLevel::Changes];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Debug => "debug",
            Self::Changes => "changes",
        }
    }
}

/// The agent's minimal access on a context: a level plus the rights that
/// are asked for on their own (#15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AgentAccess {
    pub level: AgentLevel,
    /// Secret metadata and checks inside Kubach (never values; revealing
    /// one Secret is confirmed each time).
    pub secrets: bool,
    /// Kinds beyond the built-in allowlist: custom resources (#192).
    pub crds: bool,
}

/// Security settings of one context. `Default` is what a new context gets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextPolicy {
    /// Whether an MCP agent may open sessions on this context at all.
    pub mcp_enabled: bool,
    /// Secret values stay hidden from the user in the UI as well: no
    /// show, copy or load of a value, no reveal in Helm and Kustomize.
    /// Keys can still be replaced blindly. See [`ContextPolicy::allows_secret`].
    /// The agent never gets a value the user may not see either.
    pub hide_secret_values: bool,
    /// The level the agent gets without asking (#15).
    pub minimum_access: AgentLevel,
    /// Secret metadata and checks without asking.
    pub minimum_secrets: bool,
    /// Custom resources without asking.
    pub minimum_crds: bool,
}

impl Default for ContextPolicy {
    /// AI agent access on (#9), reading built-in kinds without asking,
    /// Secret values hidden.
    fn default() -> Self {
        Self {
            mcp_enabled: true,
            hide_secret_values: true,
            minimum_access: AgentLevel::Read,
            minimum_secrets: false,
            minimum_crds: false,
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

impl ContextPolicy {
    /// The agent's minimal access.
    pub fn minimum(&self) -> AgentAccess {
        AgentAccess {
            level: self.minimum_access,
            secrets: self.minimum_secrets,
            crds: self.minimum_crds,
        }
    }

    pub fn set_minimum(&mut self, access: AgentAccess) {
        self.minimum_access = access.level;
        self.minimum_secrets = access.secrets;
        self.minimum_crds = access.crds;
    }

    /// Whether the minimum is above reading built-in kinds.
    pub fn minimum_raised(&self) -> bool {
        self.minimum() != AgentAccess::default()
    }

    /// Whether the user may do `action` with a Secret of this context in
    /// the UI. With [`hide_secret_values`](Self::hide_secret_values) only
    /// actions that do not expose an existing value remain.
    pub fn allows_secret(&self, action: SecretAction) -> bool {
        !(self.hide_secret_values && action.exposes_value())
    }

    /// Raises the minimal access at least to `access`, because the user
    /// ticked "Allow by default" in an agent's request (#15). Never lowers
    /// anything and never touches other settings. Returns what changed, as
    /// `minimum access: read → changes` lines.
    pub fn raise_minimum(&mut self, access: AgentAccess) -> Vec<String> {
        let mut changes = Vec::new();
        if access.level > self.minimum_access {
            changes.push(format!(
                "minimum access: {} → {}",
                self.minimum_access.as_str(),
                access.level.as_str()
            ));
            self.minimum_access = access.level;
        }
        if access.secrets && !self.minimum_secrets {
            changes.push("Secret metadata and checks: asked → by default".to_owned());
            self.minimum_secrets = true;
        }
        if access.crds && !self.minimum_crds {
            changes.push("custom resources: asked → by default".to_owned());
            self.minimum_crds = true;
        }
        changes
    }
}

/// A trace of the agent's minimal access raised from a session (#417,
/// #15): what, when and from which session. Kept next to the policies so
/// the settings file itself tells why a context allows more than it used
/// to. Stored as `[[security.ceiling-raise]]`, the name it had before #15.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRaise {
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
    /// `minimum access: read → changes` lines (`max profile: …` before #15).
    pub changes: Vec<String>,
}

impl AccessRaise {
    pub fn key(&self) -> ContextKey {
        ContextKey::new(self.context.clone(), &self.server)
    }
}

/// Traces kept per settings file; the oldest go first.
pub const MAX_ACCESS_RAISES: usize = 50;

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
    /// Minimal access raised from agent sessions (#417, #15), oldest
    /// first. Ceilings raised before #15 stay listed as they were.
    #[serde(rename = "ceiling-raise", skip_serializing_if = "Vec::is_empty")]
    pub raises: Vec<AccessRaise>,
}

impl SecuritySettings {
    /// Stored policy for `key`, or the default. A context whose name
    /// matches but whose server differs gets the default.
    pub fn policy(&self, key: &ContextKey) -> ContextPolicy {
        self.find(key)
            .map(|entry| entry.policy.clone())
            .unwrap_or_default()
    }

    pub fn find(&self, key: &ContextKey) -> Option<&ContextSecurity> {
        self.contexts.iter().find(|entry| entry.key() == *key)
    }

    /// Stores `policy` for `key`; the default policy removes the entry so
    /// the file only lists exceptions.
    pub fn set_policy(&mut self, key: &ContextKey, policy: ContextPolicy) {
        self.contexts.retain(|entry| entry.key() != *key);
        if policy != ContextPolicy::default() {
            self.contexts.push(ContextSecurity {
                context: key.context.clone(),
                server: key.server.clone(),
                policy,
            });
        }
    }

    /// Raises the minimal access of `key` as [`ContextPolicy::raise_minimum`]
    /// does and records the trace; returns what changed (nothing when the
    /// minimum already covers the request, and then no trace either).
    pub fn raise_minimum(
        &mut self,
        key: &ContextKey,
        access: AgentAccess,
        session: &str,
        client: &str,
        time: &str,
    ) -> Vec<String> {
        let mut policy = self.policy(key);
        let changes = policy.raise_minimum(access);
        if changes.is_empty() {
            return changes;
        }
        self.set_policy(key, policy);
        self.raises.push(AccessRaise {
            context: key.context.clone(),
            server: key.server.clone(),
            time: time.to_owned(),
            session: session.to_owned(),
            client: client.to_owned(),
            changes: changes.clone(),
        });
        let extra = self.raises.len().saturating_sub(MAX_ACCESS_RAISES);
        self.raises.drain(..extra);
        changes
    }

    /// Traces of `key`, newest first.
    pub fn raises_of(&self, key: &ContextKey) -> Vec<&AccessRaise> {
        self.raises
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
    fn defaults_read_built_in_kinds_only() {
        let policy = ContextPolicy::default();
        // AI agent access is on by default (#9).
        assert!(policy.mcp_enabled);
        assert!(policy.hide_secret_values);
        assert_eq!(policy.minimum(), AgentAccess::default());
        assert_eq!(policy.minimum().level, AgentLevel::Read);
        assert!(!policy.minimum().secrets && !policy.minimum().crds);
        assert!(!policy.minimum_raised());
    }

    #[test]
    fn agent_access_defaults_on_but_stored_off_stays_off() {
        // A stored entry without the field reads as the default: on.
        let missing: ContextPolicy =
            serde_json::from_str(r#"{"session_ttl_minutes": 20}"#).unwrap();
        assert!(missing.mcp_enabled);
        // An explicit choice made before #9 is kept.
        let off: ContextPolicy = serde_json::from_str(r#"{"mcp_enabled": false}"#).unwrap();
        assert!(!off.mcp_enabled);
        // And it is stored, since it differs from the default.
        let mut settings = SecuritySettings::default();
        let key = ContextKey::new("prod", "https://prod.example.com");
        settings.set_policy(&key, off);
        assert_eq!(settings.contexts.len(), 1);
        assert!(!settings.policy(&key).mcp_enabled);
    }

    /// Settings written before #15: the ceiling and the other agent
    /// settings are ignored, never carried over into the minimum.
    #[test]
    fn a_stored_ceiling_is_not_a_minimum() {
        let old = r#"
            [[context]]
            context = "minikube"
            server = "https://192.168.49.2:8443"
            mcp_enabled = true
            max_profile = "deploy"
            temporary_objects = true
            deploy = "apply-with-confirmation"
            exec = "confirm-each"
            session_ttl_minutes = 120
            namespaces = "multiple"
            hide_secret_values = false
            agent_secrets = "reveal-on-request"
            agent_other_kinds = true
            require_server_enforced = true
            protected = true

            [[context]]
            context = "prod"
            server = "https://prod.example.com"
            mcp_enabled = false
            max_profile = "debug"

            [[ceiling-raise]]
            context = "minikube"
            server = "https://192.168.49.2:8443"
            time = "2026-10-25T10:00:00Z"
            session = "s1"
            client = "claude-code"
            changes = ["max profile: read-only → deploy"]
        "#;
        let settings: SecuritySettings = toml::from_str(old).unwrap();
        let minikube = settings.policy(&ContextKey::new("minikube", "https://192.168.49.2:8443"));
        assert_eq!(minikube.minimum(), AgentAccess::default());
        assert!(minikube.mcp_enabled);
        // The user's own choice about Secret values in the UI stays.
        assert!(!minikube.hide_secret_values);
        let prod = settings.policy(&ContextKey::new("prod", "https://prod.example.com"));
        assert!(!prod.mcp_enabled, "an explicit off is kept");
        assert_eq!(prod.minimum(), AgentAccess::default());
        // The history of raises stays.
        let key = ContextKey::new("minikube", "https://192.168.49.2:8443");
        assert_eq!(settings.raises_of(&key).len(), 1);
        // Saved again, the old keys are gone and nothing else changed.
        let text = toml::to_string(&settings).unwrap();
        for gone in [
            "max_profile",
            "temporary_objects",
            "protected",
            "agent_secrets",
            "session_ttl_minutes",
        ] {
            assert!(!text.contains(gone), "{gone}: {text}");
        }
        assert!(text.contains("ceiling-raise"), "{text}");
        assert_eq!(toml::from_str::<SecuritySettings>(&text).unwrap(), settings);
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
        let policy = ContextPolicy::default();
        for action in ALL_SECRET_ACTIONS {
            assert_eq!(
                policy.allows_secret(action),
                !action.exposes_value(),
                "{action:?}"
            );
        }
        let shown = ContextPolicy {
            hide_secret_values: false,
            ..ContextPolicy::default()
        };
        assert!(
            ALL_SECRET_ACTIONS
                .into_iter()
                .all(|action| shown.allows_secret(action))
        );
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
                ..ContextPolicy::default()
            },
        );
        assert!(settings.policy(&dev).allows_secret(SecretAction::Show));
        assert!(!settings.policy(&prod).allows_secret(SecretAction::Show));
        // Same name on another server: hidden again.
        let moved = ContextKey::new("dev", "https://other:6443");
        assert!(!settings.policy(&moved).allows_secret(SecretAction::Copy));
    }

    #[test]
    fn raising_the_minimum_only_widens_and_leaves_a_trace() {
        let key = ContextKey::new("minikube", "https://192.168.49.2:8443");
        let mut settings = SecuritySettings::default();
        let changes = settings.raise_minimum(
            &key,
            AgentAccess {
                level: AgentLevel::Changes,
                ..AgentAccess::default()
            },
            "s1",
            "claude-code",
            "2026-10-25T10:00:00Z",
        );
        assert_eq!(changes, ["minimum access: read → changes"]);
        let policy = settings.policy(&key);
        assert_eq!(policy.minimum_access, AgentLevel::Changes);
        assert!(!policy.minimum_secrets, "Secrets were not asked for");
        assert!(policy.hide_secret_values, "other settings stay");
        let raises = settings.raises_of(&key);
        assert_eq!(raises.len(), 1);
        assert_eq!(raises[0].session, "s1");
        // Already covered: nothing changes, no second trace.
        let debug = AgentAccess {
            level: AgentLevel::Debug,
            ..AgentAccess::default()
        };
        assert!(
            settings
                .raise_minimum(&key, debug, "s2", "", "t")
                .is_empty()
        );
        assert_eq!(settings.raises_of(&key).len(), 1);
        // Secrets and custom resources are their own lines.
        let both = AgentAccess {
            level: AgentLevel::Read,
            secrets: true,
            crds: true,
        };
        assert_eq!(settings.raise_minimum(&key, both, "s3", "", "t").len(), 2);
        assert_eq!(
            settings.policy(&key).minimum(),
            AgentAccess {
                level: AgentLevel::Changes,
                secrets: true,
                crds: true
            }
        );
        // Round trip through the file format.
        let toml = toml::to_string(&settings).unwrap();
        let back: SecuritySettings = toml::from_str(&toml).unwrap();
        assert_eq!(back, settings);
        // Reset to reading: the entry goes, the history stays.
        let mut policy = settings.policy(&key);
        policy.set_minimum(AgentAccess::default());
        settings.set_policy(&key, policy);
        assert!(settings.contexts.is_empty());
        assert_eq!(settings.raises_of(&key).len(), 2);
    }

    #[test]
    fn keyed_by_context_and_server() {
        let mut settings = SecuritySettings::default();
        let minikube = ContextKey::new("minikube", "https://192.168.49.2:8443/");
        let raised = ContextPolicy {
            minimum_access: AgentLevel::Changes,
            ..ContextPolicy::default()
        };
        settings.set_policy(&minikube, raised.clone());

        // Same server written differently: same key.
        let same = ContextKey::new("minikube", " HTTPS://192.168.49.2:8443 ");
        assert_eq!(settings.policy(&same), raised);

        // Same name, other cluster (a renamed context): the default.
        let other = ContextKey::new("minikube", "https://prod.example.com");
        assert_eq!(settings.policy(&other), ContextPolicy::default());
        // Same server, other name: the default as well.
        let renamed = ContextKey::new("dev", "https://192.168.49.2:8443");
        assert_eq!(settings.policy(&renamed), ContextPolicy::default());

        // Setting back to the default removes the entry.
        settings.set_policy(&same, ContextPolicy::default());
        assert!(settings.contexts.is_empty());
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
        }
    }
}
