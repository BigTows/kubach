//! Rights granted on request within a session (#227, ADR 0005 "Права по
//! запросу").
//!
//! A session starts with none. Since #15 reading and changing a namespace
//! are set up without a question when the session's access covers them
//! (they carry the namespace's identity and Role); revealing a Secret is
//! asked for that Secret. Answers are kept for the rest of the session and
//! die with it (TTL, revocation, vault lock). The ladder:
//!
//! ```text
//!  none ──► read(ns) ──► secret(ns, name) ──► modify(ns)
//! ```
//!
//! Every step stays within the session's access, which the policy checks
//! first; a grant never widens it.
//! Grants belong to the session id, never to the client's self-reported
//! name.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One right the user may grant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "right", rename_all = "kebab-case")]
pub enum Grant {
    /// Read the namespace's resources within the profile (Secret metadata
    /// and checks included when the context allows them).
    Read { namespace: String },
    /// Reveal the values of one Secret.
    Secret { namespace: String, name: String },
    /// Change the namespace: covers `scale` and `rollout_restart` without
    /// asking again; `apply_manifest`, `delete_resource` and `exec` are
    /// still confirmed one by one.
    Modify { namespace: String },
}

impl Grant {
    pub fn read(namespace: impl Into<String>) -> Self {
        Self::Read {
            namespace: namespace.into(),
        }
    }

    pub fn secret(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self::Secret {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    pub fn modify(namespace: impl Into<String>) -> Self {
        Self::Modify {
            namespace: namespace.into(),
        }
    }

    pub fn namespace(&self) -> &str {
        match self {
            Self::Read { namespace }
            | Self::Secret { namespace, .. }
            | Self::Modify { namespace } => namespace,
        }
    }

    /// The step below this one, which must be granted first.
    pub fn requires(&self) -> Option<Grant> {
        match self {
            Self::Read { .. } => None,
            Self::Secret { namespace, .. } | Self::Modify { namespace } => {
                Some(Self::read(namespace.clone()))
            }
        }
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { namespace } => write!(f, "read namespace {namespace}"),
            Self::Secret { namespace, name } => write!(f, "reveal Secret {namespace}/{name}"),
            Self::Modify { namespace } => write!(f, "change namespace {namespace}"),
        }
    }
}

/// The user's answer, kept for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantState {
    Granted,
    /// Refused, or revoked later; not asked again in this session.
    Denied,
}

/// A grant and its state, for the sessions screen and the bridge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantEntry {
    pub grant: Grant,
    pub state: GrantState,
    /// Granted by the launch arguments (`kubach mcp --grant-read`), not by
    /// a question in the window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub at_launch: bool,
}

/// The grant table of one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    entries: BTreeMap<Grant, GrantState>,
    /// Granted by the launch arguments.
    at_launch: std::collections::BTreeSet<Grant>,
    /// Secrets (namespace, name) the user approved a write or a deletion
    /// of (#230). Not a right of its own: every write is still confirmed
    /// in the window. A temporary ServiceAccount gets `patch`/`delete` on
    /// these names only, while `modify(ns)` stands.
    secret_writes: std::collections::BTreeSet<(String, String)>,
}

impl Grants {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self, grant: &Grant) -> Option<GrantState> {
        self.entries.get(grant).copied()
    }

    pub fn is_granted(&self, grant: &Grant) -> bool {
        self.state(grant) == Some(GrantState::Granted)
    }

    /// Records the user's answer.
    pub fn set(&mut self, grant: Grant, state: GrantState) {
        self.entries.insert(grant, state);
    }

    /// Takes `grant` back; with a namespace's read right go the rights that
    /// stand on it. Revoked rights count as refused.
    pub fn revoke(&mut self, grant: &Grant) {
        let dependent: Vec<Grant> = self
            .entries
            .keys()
            .filter(|other| other.requires().as_ref() == Some(grant))
            .cloned()
            .collect();
        for other in dependent {
            if self.is_granted(&other) {
                self.entries.insert(other, GrantState::Denied);
            }
        }
        self.entries.insert(grant.clone(), GrantState::Denied);
        self.at_launch.remove(grant);
        if matches!(grant, Grant::Read { .. } | Grant::Modify { .. }) {
            let namespace = grant.namespace();
            self.secret_writes.retain(|(ns, _)| ns != namespace);
        }
    }

    /// Records that the user approved writing Secret `namespace/name`.
    /// Returns `false` if it was recorded already.
    pub fn add_secret_write(&mut self, namespace: &str, name: &str) -> bool {
        self.secret_writes
            .insert((namespace.to_owned(), name.to_owned()))
    }

    /// Names of the Secrets in `namespace` the user approved a write of.
    pub fn secret_writes_in(&self, namespace: &str) -> Vec<String> {
        self.secret_writes
            .iter()
            .filter(|(ns, _)| ns == namespace)
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// Marks a granted right as given by the launch arguments.
    pub fn mark_at_launch(&mut self, grant: Grant) {
        if self.is_granted(&grant) {
            self.at_launch.insert(grant);
        }
    }

    /// Granted rights, in ladder order per namespace.
    pub fn granted(&self) -> impl Iterator<Item = &Grant> {
        self.entries
            .iter()
            .filter(|(_, state)| **state == GrantState::Granted)
            .map(|(grant, _)| grant)
    }

    pub fn entries(&self) -> Vec<GrantEntry> {
        self.entries
            .iter()
            .map(|(grant, state)| GrantEntry {
                grant: grant.clone(),
                state: *state,
                at_launch: *state == GrantState::Granted && self.at_launch.contains(grant),
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Everything granted in `namespaces`, for tests of the other layers.
    #[cfg(test)]
    pub(crate) fn all(namespaces: &[&str]) -> Self {
        let mut grants = Self::new();
        for namespace in namespaces {
            grants.set(Grant::read(*namespace), GrantState::Granted);
            grants.set(Grant::modify(*namespace), GrantState::Granted);
        }
        grants
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoking_read_takes_the_rights_on_top_of_it() {
        let mut grants = Grants::new();
        grants.set(Grant::read("web"), GrantState::Granted);
        grants.set(Grant::secret("web", "db"), GrantState::Granted);
        grants.set(Grant::modify("web"), GrantState::Granted);
        grants.set(Grant::read("api"), GrantState::Granted);
        grants.revoke(&Grant::read("web"));
        assert_eq!(grants.state(&Grant::read("web")), Some(GrantState::Denied));
        assert_eq!(
            grants.state(&Grant::secret("web", "db")),
            Some(GrantState::Denied)
        );
        assert_eq!(
            grants.state(&Grant::modify("web")),
            Some(GrantState::Denied)
        );
        assert!(grants.is_granted(&Grant::read("api")));
    }

    #[test]
    fn secret_writes_go_with_the_namespace_rights() {
        let mut grants = Grants::new();
        grants.set(Grant::read("web"), GrantState::Granted);
        grants.set(Grant::modify("web"), GrantState::Granted);
        assert!(grants.add_secret_write("web", "db"));
        assert!(!grants.add_secret_write("web", "db"));
        grants.add_secret_write("api", "token");
        assert_eq!(grants.secret_writes_in("web"), ["db"]);
        grants.revoke(&Grant::modify("web"));
        assert!(grants.secret_writes_in("web").is_empty());
        assert_eq!(grants.secret_writes_in("api"), ["token"]);
    }

    #[test]
    fn grants_serialize_with_their_right() {
        let json = serde_json::to_value(Grant::secret("web", "db")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"right": "secret", "namespace": "web", "name": "db"})
        );
        assert_eq!(Grant::modify("web").requires(), Some(Grant::read("web")));
        assert_eq!(Grant::read("web").to_string(), "read namespace web");
    }
}
