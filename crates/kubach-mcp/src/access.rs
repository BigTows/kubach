//! The agent's access and raising it from a session (#15, ADR 0005
//! "Минимальный доступ вместо потолка").
//!
//! A context's settings give the agent a **minimal access**: a level
//! (read-only, debug, deploy) plus Secret metadata and custom resources.
//! Everything within it the agent gets without a question. A call that
//! needs more is not refused: the policy names what is missing
//! ([`DenialCode::NeedsAccess`](crate::policy::DenialCode::NeedsAccess)),
//! Kubach asks the user in its window, and a yes raises the session's
//! access until it ends, or the context's minimum for good when the user
//! ticks "Allow by default". There is no ceiling: only the user's RBAC
//! bounds the agent, and `kubach mcp --profile` may narrow one process.
//!
//! Access is not a confirmation: every change, deletion, Helm action and
//! exec command is still confirmed one by one, and revealing a Secret's
//! values is asked for that very Secret.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::settings::Profile;

/// What the agent may call without asking for more access, in the
/// dimensions that are raised on request. It approves no change (see the
/// module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    /// read-only, debug (logs, exec) or deploy (changes, Helm).
    pub profile: Profile,
    /// Secret metadata and checks inside Kubach (never values).
    pub secrets: bool,
    /// Kinds beyond the built-in allowlist: custom resources (#192).
    pub crds: bool,
    /// Nodes, their events and usage (#61). Cluster-scoped, so read with
    /// the user's credentials, by Kubach's fixed read-only calls.
    #[serde(default)]
    pub nodes: bool,
}

impl Default for Access {
    fn default() -> Self {
        Self::NONE
    }
}

impl Access {
    /// Reading built-in kinds only: the default minimum.
    pub const NONE: Self = Self {
        profile: Profile::ReadOnly,
        secrets: false,
        crds: false,
        nodes: false,
    };

    /// Only the profile.
    pub const fn profile(profile: Profile) -> Self {
        Self {
            profile,
            secrets: false,
            crds: false,
            nodes: false,
        }
    }

    /// Everything: for tests and the fixed policies of the CLI.
    pub const ALL: Self = Self {
        profile: Profile::Deploy,
        secrets: true,
        crds: true,
        nodes: true,
    };

    /// Both together.
    pub fn join(self, other: Self) -> Self {
        Self {
            profile: self.profile.max(other.profile),
            secrets: self.secrets || other.secrets,
            crds: self.crds || other.crds,
            nodes: self.nodes || other.nodes,
        }
    }

    /// The profile at most `cap` (`kubach mcp --profile`).
    pub fn capped(self, cap: Profile) -> Self {
        Self {
            profile: self.profile.min(cap),
            ..self
        }
    }

    /// Whether `self` allows everything `other` asks for.
    pub fn covers(self, other: Self) -> bool {
        self.profile >= other.profile
            && (self.secrets || !other.secrets)
            && (self.crds || !other.crds)
            && (self.nodes || !other.nodes)
    }

    /// What `needed` asks for beyond `self`; `None` when covered. The
    /// profile is kept only when it is higher.
    pub fn missing(self, needed: Self) -> Option<Self> {
        if self.covers(needed) {
            return None;
        }
        Some(Self {
            profile: if needed.profile > self.profile {
                needed.profile
            } else {
                Profile::ReadOnly
            },
            secrets: needed.secrets && !self.secrets,
            crds: needed.crds && !self.crds,
            nodes: needed.nodes && !self.nodes,
        })
    }

    /// The parts of this request, for the dialog and the agent.
    pub fn parts(self) -> Vec<String> {
        let mut parts = Vec::new();
        match self.profile {
            Profile::ReadOnly => {}
            Profile::Debug => parts.push("debugging: logs and commands in containers".into()),
            Profile::Deploy => parts.push(
                "changes: apply, delete, scale, restarts, Helm and commands in containers".into(),
            ),
        }
        if self.secrets {
            parts.push("Secret metadata and checks (no values)".into());
        }
        if self.crds {
            parts.push("custom resources (CRDs)".into());
        }
        if self.nodes {
            parts.push(
                "nodes: conditions, events and usage (read with the user's credentials)".into(),
            );
        }
        parts
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = self.parts();
        if parts.is_empty() {
            f.write_str("reading built-in resources")
        } else if self.profile == Profile::ReadOnly {
            write!(f, "reading built-in resources, {}", parts.join(", "))
        } else {
            f.write_str(&parts.join(", "))
        }
    }
}

/// The user's answers about access in one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raises {
    /// Everything the user allowed so far, joined.
    pub granted: Access,
    /// Requests the user refused; the same request is not asked again.
    pub refused: Vec<Access>,
    /// Allowed "by default": the settings were raised too.
    pub saved: bool,
}

impl Raises {
    pub fn is_refused(&self, request: Access) -> bool {
        self.refused.iter().any(|refused| refused.covers(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_parts_only() {
        let base = Access::NONE;
        let deploy = Access::profile(Profile::Deploy);
        assert_eq!(base.missing(deploy), Some(deploy));
        assert_eq!(deploy.missing(deploy), None);
        assert!(Access::ALL.covers(deploy));
        // Deploy already, only Secrets are missing.
        let writes = Access {
            secrets: true,
            ..deploy
        };
        assert_eq!(
            deploy.missing(writes),
            Some(Access {
                profile: Profile::ReadOnly,
                secrets: true,
                crds: false,
                nodes: false,
            })
        );
        assert_eq!(
            Access::profile(Profile::Debug).to_string(),
            "debugging: logs and commands in containers"
        );
        assert_eq!(
            Access {
                secrets: true,
                ..Access::NONE
            }
            .to_string(),
            "reading built-in resources, Secret metadata and checks (no values)"
        );
    }

    #[test]
    fn capping_narrows_the_profile_only() {
        assert_eq!(
            Access::ALL.capped(Profile::Debug),
            Access {
                profile: Profile::Debug,
                secrets: true,
                crds: true,
                nodes: true,
            }
        );
        assert_eq!(Access::NONE.capped(Profile::Deploy), Access::NONE);
    }

    #[test]
    fn a_refusal_covers_smaller_requests() {
        let raises = Raises {
            refused: vec![Access {
                profile: Profile::Deploy,
                secrets: true,
                crds: false,
                nodes: false,
            }],
            ..Raises::default()
        };
        assert!(raises.is_refused(Access::profile(Profile::Debug)));
        assert!(!raises.is_refused(Access {
            crds: true,
            ..Access::NONE
        }));
    }
}
