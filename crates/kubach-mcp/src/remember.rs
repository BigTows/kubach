//! "Don't ask again this session" for one kind of change in one namespace
//! (#30).
//!
//! The user may tick it in the window of a change; later changes of the
//! same kind in the same namespace then run without a window until the
//! session ends. Only in memory: nothing reaches the settings, and a new
//! session asks again. Revoking `read(ns)` or `modify(ns)` forgets what was
//! remembered in that namespace; the user can forget everything of a
//! session in Settings → Agent.
//!
//! Some questions are always asked, without the tick and past anything
//! remembered: [`rememberable`] lists what may be remembered, everything
//! else is not.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::confirm::{ConfirmationRequest, RequestKind};
use crate::helm_tools::{HelmAction, HelmCredentials};

/// A kind of change that may run without a window once the user said so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberedAction {
    Scale,
    RolloutRestart,
    /// `apply_manifest`, never a dry run (it is not asked about).
    ApplyManifest,
    Exec,
    PortForwardStart,
    HelmInstall,
    HelmUpgrade,
    HelmRollback,
}

impl RememberedAction {
    pub const ALL: [Self; 8] = [
        Self::Scale,
        Self::RolloutRestart,
        Self::ApplyManifest,
        Self::Exec,
        Self::PortForwardStart,
        Self::HelmInstall,
        Self::HelmUpgrade,
        Self::HelmRollback,
    ];

    /// The tool that makes this kind of change.
    pub fn tool(self) -> &'static str {
        match self {
            Self::Scale => "scale",
            Self::RolloutRestart => "rollout_restart",
            Self::ApplyManifest => "apply_manifest",
            Self::Exec => "exec",
            Self::PortForwardStart => "port_forward_start",
            Self::HelmInstall => "helm_install",
            Self::HelmUpgrade => "helm_upgrade",
            Self::HelmRollback => "helm_rollback",
        }
    }
}

impl fmt::Display for RememberedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tool())
    }
}

/// One remembered "don't ask again": a kind of change in a namespace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Remembered {
    pub action: RememberedAction,
    pub namespace: String,
}

impl fmt::Display for Remembered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} in {}", self.action, self.namespace)
    }
}

/// What `request` would be remembered as, if the user may tick "don't ask
/// again" for it. `None` for everything that is always asked:
/// - deletions (`delete_resource`, `helm_uninstall`) and anything not
///   listed in [`RememberedAction`]: Secrets (writing, copying, deleting,
///   revealing), rights, access, chart sources, contexts, logs of the
///   agent's own pods;
/// - a change with dangerous findings (they need "I understand" every
///   time, #194);
/// - Helm under the user's own credentials (#18), with cluster-scoped
///   objects, in other namespaces (#21) or creating its namespace;
/// - a namespace where only Kubach holds the boundary (ADR 0005: client
///   only confirms every change without "remember").
pub fn rememberable(request: &ConfirmationRequest) -> Option<Remembered> {
    if request.namespace.is_empty()
        || !request.dangerous.is_empty()
        || !request.mode.server_enforced()
    {
        return None;
    }
    let action = match (&request.kind, request.tool.as_str()) {
        (RequestKind::Call, "scale") => RememberedAction::Scale,
        (RequestKind::Call, "rollout_restart") => RememberedAction::RolloutRestart,
        (RequestKind::Call, "apply_manifest") => RememberedAction::ApplyManifest,
        (RequestKind::Exec { .. }, "exec") => RememberedAction::Exec,
        (RequestKind::PortForward { .. }, "port_forward_start") => {
            RememberedAction::PortForwardStart
        }
        (RequestKind::Helm { helm }, tool) => {
            if helm.credentials != HelmCredentials::Session
                || !helm.user_objects.is_empty()
                || !helm.cluster_objects.is_empty()
                || !helm.namespaces.is_empty()
                || helm.create_namespace
            {
                return None;
            }
            match (helm.action, tool) {
                (HelmAction::Install, "helm_install") => RememberedAction::HelmInstall,
                (HelmAction::Upgrade, "helm_upgrade") => RememberedAction::HelmUpgrade,
                (HelmAction::Rollback, "helm_rollback") => RememberedAction::HelmRollback,
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(Remembered {
        action,
        namespace: request.namespace.clone(),
    })
}

/// What a session remembers. `epoch` changes whenever something is
/// forgotten, so a "don't ask again" the user ticked while a revocation
/// happened is not kept.
#[derive(Debug, Default)]
pub(crate) struct RememberedSet {
    items: BTreeSet<Remembered>,
    epoch: u64,
}

impl RememberedSet {
    pub(crate) fn contains(&self, remembered: &Remembered) -> bool {
        self.items.contains(remembered)
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Keeps `remembered` unless something was forgotten since `epoch`.
    pub(crate) fn insert(&mut self, remembered: Remembered, epoch: u64) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.items.insert(remembered);
        true
    }

    /// Forgets everything remembered in `namespace`.
    pub(crate) fn forget_in(&mut self, namespace: &str) {
        self.epoch += 1;
        self.items.retain(|r| r.namespace != namespace);
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.epoch += 1;
        self.items.clear();
    }

    pub(crate) fn list(&self) -> Vec<Remembered> {
        self.items.iter().cloned().collect()
    }
}

/// How remembering took part in a call, for the audit log and the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RememberNote {
    /// Approved without a window: the user said not to ask again.
    Used(Remembered),
    /// The user approved and ticked "don't ask again" just now.
    Saved(Remembered),
}

impl RememberNote {
    /// `used` or `saved`, for the audit log.
    pub(crate) fn audit(&self) -> &'static str {
        match self {
            Self::Used(_) => "used",
            Self::Saved(_) => "saved",
        }
    }

    /// What the agent is told.
    pub(crate) fn agent_text(&self) -> String {
        match self {
            Self::Used(r) => format!(
                "Approved by the user's earlier choice: no confirmation for {} in namespace {} \
                 until this session ends.",
                r.action, r.namespace
            ),
            Self::Saved(r) => format!(
                "The user approved this and chose not to be asked again about {} in namespace {} \
                 until this session ends.",
                r.action, r.namespace
            ),
        }
    }
}

#[cfg(test)]
mod tests;
