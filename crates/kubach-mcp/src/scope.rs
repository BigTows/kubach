//! What a session is allowed to touch: profile, settings, TTL and, if the
//! launch pinned them, namespaces. Validated once when the session starts;
//! immutable afterwards.
//!
//! Since #401 a session starts without namespaces: the agent names one,
//! and the user is asked for reading it the first time (ADR 0005, "Права
//! по запросу"). Namespaces pinned at launch (`kubach mcp --namespace`)
//! restrict the session to them, as before.

use std::time::{Duration, Instant};

use k8s_openapi::jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::names;
use crate::settings::{Profile, SecurityPolicy};

/// What a session is started with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    /// kubeconfig context name.
    pub context: String,
    /// API server URL of the context; settings are keyed by both.
    pub server_url: String,
    /// Namespaces pinned at launch: the session may ask only for these.
    /// Empty: any namespace the agent names, each asked for (#401).
    #[serde(default)]
    pub namespaces: Vec<String>,
    pub profile: Profile,
    /// `None` takes the context's default TTL.
    pub ttl: Option<Duration>,
    /// Namespaces Kubach knows of for this context without listing them
    /// (kubeconfig, its settings, recently opened), for `list_namespaces`
    /// when the user may not list namespaces. Never a right.
    #[serde(default)]
    pub hints: Vec<NamespaceHint>,
}

/// Where Kubach knows a namespace from, for `list_namespaces` (#401).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NamespaceSource {
    /// Listed from the cluster with the user's credentials.
    Cluster,
    /// Pinned by the launch arguments (`kubach mcp --namespace`).
    LaunchArguments,
    /// The namespace of the kubeconfig context.
    Kubeconfig,
    /// The context's default namespace in Kubach's settings.
    KubachSettings,
    /// Recently opened in Kubach on this context.
    RecentlyOpened,
    /// Already used in this session.
    Session,
}

/// A namespace Kubach knows of, and from where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceHint {
    pub name: String,
    pub source: NamespaceSource,
}

impl NamespaceHint {
    pub fn new(name: impl Into<String>, source: NamespaceSource) -> Self {
        Self {
            name: name.into(),
            source,
        }
    }
}

/// Under which identity the session's API calls run (ADR 0005, "Решение").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMode {
    /// The user's own credentials; only the client-side policy holds the
    /// boundary. The UI must mark such sessions.
    ClientOnly,
    /// `Impersonate-User`/`-Group` of an administrator-prepared identity.
    Impersonation,
    /// A per-session ServiceAccount with a Role of the profile.
    TemporaryServiceAccount,
}

impl SessionMode {
    /// Whether the API server enforces the boundary as well.
    pub fn server_enforced(self) -> bool {
        !matches!(self, Self::ClientOnly)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScopeError {
    #[error("MCP access is disabled for context `{0}`")]
    Disabled(String),
    #[error("`{0}` is not a valid namespace name")]
    InvalidNamespace(String),
    #[error("namespace `{0}` is listed twice")]
    DuplicateNamespace(String),
    #[error("this context allows a single namespace per session")]
    SingleNamespaceOnly,
    #[error("profile {requested} exceeds the context's maximum ({max})")]
    ProfileTooBroad { requested: Profile, max: Profile },
    #[error("TTL must be between 1 second and {max:?}")]
    InvalidTtl { max: Duration },
}

/// Validated limits of a session.
#[derive(Clone, Debug)]
pub struct SessionScope {
    pub id: String,
    pub context: String,
    pub server_url: String,
    /// Pinned at launch; empty: any namespace, each asked for.
    pub namespaces: Vec<String>,
    /// Known namespaces for `list_namespaces`; valid names only.
    pub hints: Vec<NamespaceHint>,
    pub profile: Profile,
    pub policy: SecurityPolicy,
    pub ttl: Duration,
    pub started_at: Timestamp,
    pub expires_at: Timestamp,
    deadline: Instant,
}

impl SessionScope {
    /// Validates `spec` against the context's settings: settings restrict and
    /// never widen.
    pub fn new(spec: &SessionSpec, policy: &SecurityPolicy) -> Result<Self, ScopeError> {
        if !policy.mcp_enabled {
            return Err(ScopeError::Disabled(spec.context.clone()));
        }
        for (i, namespace) in spec.namespaces.iter().enumerate() {
            if !names::is_dns_label(namespace) {
                return Err(ScopeError::InvalidNamespace(namespace.clone()));
            }
            if spec.namespaces[..i].contains(namespace) {
                return Err(ScopeError::DuplicateNamespace(namespace.clone()));
            }
        }
        if spec.namespaces.len() > 1 && !policy.multiple_namespaces {
            return Err(ScopeError::SingleNamespaceOnly);
        }
        if spec.profile > policy.max_profile {
            return Err(ScopeError::ProfileTooBroad {
                requested: spec.profile,
                max: policy.max_profile,
            });
        }
        let cap = policy.ttl_cap();
        let ttl = spec.ttl.unwrap_or(policy.default_ttl.min(cap));
        if ttl.is_zero() || ttl > cap {
            return Err(ScopeError::InvalidTtl { max: cap });
        }
        let started_at = Timestamp::now();
        let expires_at = started_at.checked_add(ttl).unwrap_or(Timestamp::MAX);
        Ok(Self {
            id: new_session_id(),
            context: spec.context.clone(),
            server_url: spec.server_url.clone(),
            namespaces: spec.namespaces.clone(),
            hints: clean_hints(&spec.hints),
            profile: spec.profile,
            policy: policy.clone(),
            ttl,
            started_at,
            expires_at,
            deadline: Instant::now() + ttl,
        })
    }

    /// Monotonic deadline of the session.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// The only pinned namespace, for sessions pinned to one.
    pub fn single_namespace(&self) -> Option<&str> {
        match self.namespaces.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Whether the launch pinned the session's namespaces.
    pub fn pinned(&self) -> bool {
        !self.namespaces.is_empty()
    }

    /// Where the tool catalog is probed before any namespace is granted:
    /// the first pinned one, else the first hint, else `default`.
    pub fn home_namespace(&self) -> &str {
        self.namespaces
            .first()
            .or_else(|| self.hints.first().map(|hint| &hint.name))
            .map_or("default", String::as_str)
    }

    #[cfg(test)]
    pub(crate) fn expire_now(&mut self) {
        self.deadline = Instant::now();
    }
}

/// Hints with valid names, each name once (the first source wins).
fn clean_hints(hints: &[NamespaceHint]) -> Vec<NamespaceHint> {
    let mut out: Vec<NamespaceHint> = Vec::new();
    for hint in hints {
        if names::is_dns_label(&hint.name) && !out.iter().any(|known| known.name == hint.name) {
            out.push(hint.clone());
        }
    }
    out
}

/// Short random id; also part of the temporary ServiceAccount name.
fn new_session_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ExecMode;

    fn spec(namespaces: &[&str], profile: Profile) -> SessionSpec {
        SessionSpec {
            context: "minikube".into(),
            server_url: "https://127.0.0.1:8443".into(),
            namespaces: namespaces.iter().map(|n| n.to_string()).collect(),
            profile,
            ttl: None,
            hints: Vec::new(),
        }
    }

    #[test]
    fn disabled_when_turned_off() {
        // On by default (#9); the user may turn it off per context.
        let policy = SecurityPolicy {
            mcp_enabled: false,
            ..SecurityPolicy::default()
        };
        let err = SessionScope::new(&spec(&["web"], Profile::ReadOnly), &policy).unwrap_err();
        assert_eq!(err, ScopeError::Disabled("minikube".into()));
    }

    #[test]
    fn profile_cannot_exceed_settings() {
        let policy = SecurityPolicy::default();
        assert!(policy.mcp_enabled);
        assert!(matches!(
            SessionScope::new(&spec(&["web"], Profile::Debug), &policy),
            Err(ScopeError::ProfileTooBroad { .. })
        ));
        let scope = SessionScope::new(&spec(&["web"], Profile::ReadOnly), &policy).unwrap();
        assert_eq!(scope.ttl, policy.default_ttl);
        assert_eq!(scope.single_namespace(), Some("web"));
        assert_eq!(scope.id.len(), 12);
    }

    #[test]
    fn namespaces_are_validated() {
        let mut policy = SecurityPolicy::development();
        for bad in [
            "",
            "*",
            "all",
            "-A",
            "Web",
            "a/b",
            "../kube-system",
            "a b",
            "a.b",
        ] {
            let result = SessionScope::new(&spec(&[bad], Profile::ReadOnly), &policy);
            if bad == "all" {
                // A namespace literally named "all" is legal and stays exact.
                assert!(result.is_ok());
            } else {
                assert!(result.is_err(), "{bad:?} accepted");
            }
        }
        assert_eq!(
            SessionScope::new(&spec(&["a", "b"], Profile::ReadOnly), &policy).unwrap_err(),
            ScopeError::SingleNamespaceOnly
        );
        policy.multiple_namespaces = true;
        assert!(SessionScope::new(&spec(&["a", "b"], Profile::ReadOnly), &policy).is_ok());
        assert_eq!(
            SessionScope::new(&spec(&["a", "a"], Profile::ReadOnly), &policy).unwrap_err(),
            ScopeError::DuplicateNamespace("a".into())
        );
        // No namespace: the agent asks for them one by one (#401).
        let open = SessionScope::new(&spec(&[], Profile::ReadOnly), &policy).unwrap();
        assert!(!open.pinned());
        assert_eq!(open.home_namespace(), "default");
    }

    #[test]
    fn hints_are_cleaned() {
        let mut s = spec(&[], Profile::ReadOnly);
        s.hints = vec![
            NamespaceHint::new("team", NamespaceSource::Kubeconfig),
            NamespaceHint::new("Bad Name", NamespaceSource::RecentlyOpened),
            NamespaceHint::new("team", NamespaceSource::RecentlyOpened),
            NamespaceHint::new("web", NamespaceSource::RecentlyOpened),
        ];
        let scope = SessionScope::new(&s, &SecurityPolicy::development()).unwrap();
        assert_eq!(
            scope.hints,
            [
                NamespaceHint::new("team", NamespaceSource::Kubeconfig),
                NamespaceHint::new("web", NamespaceSource::RecentlyOpened),
            ]
        );
        assert_eq!(scope.home_namespace(), "team");
    }

    #[test]
    fn ttl_is_bounded() {
        let policy = SecurityPolicy {
            exec: ExecMode::OneShot,
            ..SecurityPolicy::development()
        };
        let mut s = spec(&["web"], Profile::Deploy);
        s.ttl = Some(Duration::from_secs(9 * 3600));
        assert!(matches!(
            SessionScope::new(&s, &policy),
            Err(ScopeError::InvalidTtl { .. })
        ));
        s.ttl = Some(Duration::ZERO);
        assert!(SessionScope::new(&s, &policy).is_err());
        s.ttl = Some(Duration::from_secs(60));
        let scope = SessionScope::new(&s, &policy).unwrap();
        assert!(!scope.expired());
        assert!(scope.expires_at > scope.started_at);
    }
}
