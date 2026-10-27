//! The agent's ceiling and raising it from a session (#417, ADR 0005
//! "Повышение потолка по запросу").
//!
//! The context's settings set a ceiling: the broadest profile, whether
//! changes are applied (after confirmation) or only previewed, whether
//! one-off exec commands exist. A call that needs more is not refused
//! silently any more: the policy names the missing part
//! ([`DenialCode::NeedsCeiling`](crate::policy::DenialCode::NeedsCeiling)),
//! Kubach asks the user in its window, and a yes raises the ceiling until
//! the session ends, or for good in the settings when the user ticks
//! "Always allow on this cluster". A "Protected cluster" never asks.
//!
//! Raising the ceiling is not a right in a namespace: reading and changing
//! a namespace are still granted one by one (#227, #401), each change is
//! still confirmed with its dry run, and the user's RBAC still bounds all.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::rules::deploy_persists;
use crate::settings::{DeployMode, ExecMode, Profile, SecurityPolicy};

/// What the agent may do at most in a session, in the three dimensions
/// that can be raised on request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ceiling {
    pub profile: Profile,
    /// Changes are applied after confirmation (otherwise dry run only).
    pub apply: bool,
    /// One-off exec commands, each confirmed.
    pub exec: bool,
}

impl Default for Ceiling {
    fn default() -> Self {
        Self::NONE
    }
}

impl Ceiling {
    /// Read-only, dry run, no exec.
    pub const NONE: Self = Self {
        profile: Profile::ReadOnly,
        apply: false,
        exec: false,
    };

    /// The ceiling a session starts with: its profile and the context's
    /// settings.
    pub fn of(profile: Profile, policy: &SecurityPolicy) -> Self {
        Self {
            profile,
            apply: profile >= Profile::Deploy && deploy_persists(policy),
            exec: profile >= Profile::Debug && policy.exec.allows_one_shot(),
        }
    }

    /// Both together.
    pub fn join(self, other: Self) -> Self {
        Self {
            profile: self.profile.max(other.profile),
            apply: self.apply || other.apply,
            exec: self.exec || other.exec,
        }
    }

    /// Whether `self` allows everything `other` asks for.
    pub fn covers(self, other: Self) -> bool {
        self.profile >= other.profile && (self.apply || !other.apply) && (self.exec || !other.exec)
    }

    /// What `needed` asks for beyond `self`; [`Ceiling::NONE`] when
    /// covered. The profile is kept only when it is higher.
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
            apply: needed.apply && !self.apply,
            exec: needed.exec && !self.exec,
        })
    }

    /// `policy` with this ceiling applied on top: the profile, applying
    /// changes and exec raised where this asks for them, nothing lowered.
    pub fn widen(self, profile: Profile, policy: &SecurityPolicy) -> (Profile, SecurityPolicy) {
        let mut policy = policy.clone();
        let profile = profile.max(self.profile);
        policy.max_profile = policy.max_profile.max(profile);
        if self.apply {
            policy.deploy = DeployMode::ApplyAfterConfirmation;
        }
        if self.exec && !policy.exec.allows_one_shot() {
            policy.exec = ExecMode::OneShot;
        }
        (profile, policy)
    }

    /// The parts of this request, for the dialog and the agent: `the deploy
    /// profile`, `applying changes after your confirmation`, `exec`.
    pub fn parts(self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.profile > Profile::ReadOnly {
            parts.push(format!("the {} profile", self.profile));
        }
        if self.apply {
            parts.push("applying changes (each confirmed with its dry run)".into());
        }
        if self.exec {
            parts.push("one-off commands in containers (each confirmed)".into());
        }
        parts
    }
}

impl fmt::Display for Ceiling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = self.parts();
        if parts.is_empty() {
            f.write_str("read-only")
        } else {
            f.write_str(&parts.join(", "))
        }
    }
}

/// The user's answers about the ceiling in one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raises {
    /// Everything the user allowed so far, joined.
    pub granted: Ceiling,
    /// Requests the user refused; the same request is not asked again.
    pub refused: Vec<Ceiling>,
    /// Allowed "always": the settings were raised too.
    pub saved: bool,
}

impl Raises {
    pub fn is_refused(&self, request: Ceiling) -> bool {
        self.refused.iter().any(|refused| refused.covers(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_parts_only() {
        let base = Ceiling::of(Profile::ReadOnly, &SecurityPolicy::default());
        assert_eq!(base, Ceiling::NONE);
        let deploy = Ceiling {
            profile: Profile::Deploy,
            apply: true,
            exec: false,
        };
        assert_eq!(base.missing(deploy), Some(deploy));
        assert_eq!(deploy.missing(deploy), None);
        let dev = Ceiling::of(Profile::Deploy, &SecurityPolicy::development());
        assert!(dev.covers(deploy));
        // Deploy profile already, only applying is missing.
        let dry = Ceiling {
            profile: Profile::Deploy,
            apply: false,
            exec: false,
        };
        assert_eq!(
            dry.missing(deploy),
            Some(Ceiling {
                profile: Profile::ReadOnly,
                apply: true,
                exec: false
            })
        );
        assert_eq!(
            deploy.to_string(),
            "the deploy profile, applying changes (each confirmed with its dry run)"
        );
    }

    #[test]
    fn widening_never_lowers() {
        let (profile, policy) = Ceiling {
            profile: Profile::Debug,
            apply: false,
            exec: true,
        }
        .widen(Profile::ReadOnly, &SecurityPolicy::default());
        assert_eq!(profile, Profile::Debug);
        assert_eq!(policy.exec, ExecMode::OneShot);
        assert_eq!(policy.deploy, DeployMode::DryRunOnly);
        let (profile, policy) =
            Ceiling::NONE.widen(Profile::Deploy, &SecurityPolicy::development());
        assert_eq!(profile, Profile::Deploy);
        assert_eq!(policy, SecurityPolicy::development());
    }

    #[test]
    fn a_refusal_covers_smaller_requests() {
        let raises = Raises {
            refused: vec![Ceiling {
                profile: Profile::Deploy,
                apply: true,
                exec: false,
            }],
            ..Raises::default()
        };
        assert!(raises.is_refused(Ceiling {
            profile: Profile::Deploy,
            apply: false,
            exec: false
        }));
        assert!(!raises.is_refused(Ceiling {
            profile: Profile::ReadOnly,
            apply: false,
            exec: true
        }));
    }
}
