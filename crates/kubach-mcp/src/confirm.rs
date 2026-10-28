//! Human-in-the-loop: mutating calls and rights on request wait for the
//! user's decision.
//!
//! The app implements [`Confirmer`] with a UI prompt. Confirmation never
//! widens rights by itself: only calls that already passed the policy reach
//! it, and more access is asked for as such (#15). The same request
//! travels over the bridge for `kubach mcp` processes.

use std::sync::Mutex;
use std::time::Duration;

use kubach_core::plugin::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::access::Access;
use crate::grants::Grant;
use crate::helm_tools::HelmChange;
use crate::policy::Finding;
use crate::scope::SessionMode;
use crate::secret_write::{SecretInput, SecretRequest};
use kubach_core::sync;

/// How long a call waits for the user before it counts as denied.
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(300);

/// What a [`ConfirmationRequest`] is about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RequestKind {
    /// One mutating call.
    #[default]
    Call,
    /// One exec command, confirmed every time. `command` is the exact argv;
    /// the UI shows each argument on its own, with invisible characters
    /// spelled out, and not as a diff (#194). `details` holds the same
    /// command as one shell line.
    Exec { command: Vec<String> },
    /// A right for the rest of the session (#227): since #15 only revealing
    /// one Secret, and the UI must show the warning that the value leaves
    /// for the model's provider.
    Grant { grant: Grant },
    /// Not a question: the agent asked to reveal a Secret on a context
    /// whose Secret values are hidden. The UI tells the user (with a way to
    /// the context's security settings) and answers
    /// [`ConfirmationDecision::Denied`] at once; the setting is changed
    /// only in Settings, never from here.
    RevealBlocked { name: String },
    /// Writing a Secret through the user (#230). For
    /// [`SecretOp::takes_values`](crate::secret_write::SecretOp::takes_values)
    /// the UI shows its own input form and answers
    /// [`ConfirmationDecision::Provided`] with the values; copying and
    /// deleting are approved or rejected like a call. The name, keys and
    /// explanation come from the agent: the UI shows them as untrusted
    /// text, and never offers "remember".
    Secret { secret: Box<SecretRequest> },
    /// Access beyond what the agent has on this context (#15): `raise` is
    /// what the call needs beyond `current`. Answered with
    /// [`ConfirmationDecision::Approved`] (until the session ends),
    /// [`ConfirmationDecision::ApprovedAlways`] (the app raised the
    /// context's minimal access too: "Allow by default", unticked by
    /// default) or a rejection. The app approves at once, without asking,
    /// when the context's minimum in its settings already covers `raise`
    /// (allowed by default from another session).
    Access {
        raise: Access,
        current: Access,
        /// API server of the context: settings are keyed by both.
        server_url: String,
    },
    /// A Helm change (#418): chart, version, values (masked), the objects
    /// of the server-side dry run and, for an upgrade or rollback, the diff
    /// in `details`. Cluster-scoped objects come as `dangerous` findings and
    /// need their own explicit tick.
    Helm { helm: Box<HelmChange> },
    /// Using charts from a source not added in Kubach (an `oci://`
    /// repository) for the rest of the session (#418).
    ChartSource { source: String },
    /// The agent chose this context with `use_context` (#421): a yes
    /// starts a session there, without namespaces or rights. `context` of
    /// the request names it; `session_id` is empty, no session runs yet.
    /// The app refuses at once when its settings have AI agent access off.
    UseContext {
        server_url: String,
        /// `kubeconfig` or `vault`.
        source: String,
    },
}

/// What the user is asked to approve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequest {
    #[serde(default)]
    pub kind: RequestKind,
    pub session_id: String,
    /// kubeconfig context of the session.
    #[serde(default)]
    pub context: String,
    /// Name of the MCP client, as it introduced itself. Shown only: rights
    /// belong to the session, not to this name.
    pub client: String,
    pub tool: String,
    pub namespace: String,
    /// One line: `Scale Deployment web to 3 replicas`.
    pub summary: String,
    /// The dry-run diff of an apply, or the exact exec command.
    pub details: String,
    /// Fields that need their own explicit confirmation (hostPath,
    /// privileged, ServiceAccount change…). The UI must not hide them.
    pub dangerous: Vec<Finding>,
    pub mode: SessionMode,
    /// "Remember this decision" may be offered. Never in client-only mode
    /// (ADR 0005).
    pub allow_remember: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum ConfirmationDecision {
    Approved,
    /// Approved, and the app raised the context's minimal access in the
    /// settings for good ([`RequestKind::Access`] only; any other request
    /// answered this way counts as a plain approval).
    ApprovedAlways,
    /// The values the user entered or generated for a
    /// [`RequestKind::Secret`] form (#230). They are written by Kubach and
    /// never reach the agent, the audit log or `Debug` output; the answer
    /// counts as the user's approval of that one write. Any other request
    /// answered this way counts as not approved.
    Provided {
        input: SecretInput,
    },
    /// Not approved, without the user saying so: no window, a timeout, a
    /// dismissed dialog. A right is asked for again next time.
    Denied {
        reason: Option<String>,
    },
    /// The user said no. A refused right is remembered for the session.
    Rejected {
        reason: Option<String>,
    },
}

impl ConfirmationDecision {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved | Self::ApprovedAlways)
    }

    /// The user answered no themselves.
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Approved | Self::ApprovedAlways | Self::Provided { .. } => None,
            Self::Denied { reason } | Self::Rejected { reason } => reason.as_deref(),
        }
    }
}

/// Asks the user. Implemented by the app (UI prompt).
pub trait Confirmer: Send + Sync {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision>;
}

/// Denies everything: the default until the app provides a UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAll;

impl Confirmer for DenyAll {
    fn confirm<'a>(
        &'a self,
        _request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        Box::pin(async {
            ConfirmationDecision::Denied {
                reason: Some("no confirmation UI is connected".into()),
            }
        })
    }
}

/// Answers every request the same way and records them. For tests.
#[derive(Debug)]
pub struct ScriptedConfirmer {
    approve: bool,
    /// Ticks "Allow by default" for access requests.
    always: bool,
    /// What an approving confirmer enters into Secret forms.
    input: Option<SecretInput>,
    requests: Mutex<Vec<ConfirmationRequest>>,
}

impl ScriptedConfirmer {
    pub fn approving() -> Self {
        Self {
            approve: true,
            always: false,
            input: None,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn denying() -> Self {
        Self {
            approve: false,
            always: false,
            input: None,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Approves everything and enters `input` into every Secret form.
    pub fn providing(input: SecretInput) -> Self {
        Self {
            input: Some(input),
            ..Self::approving()
        }
    }

    /// Approves everything, access requests with "Allow by default".
    pub fn approving_always() -> Self {
        Self {
            always: true,
            ..Self::approving()
        }
    }

    /// Requests seen so far.
    pub fn requests(&self) -> Vec<ConfirmationRequest> {
        sync::lock(&self.requests).clone()
    }
}

impl Confirmer for ScriptedConfirmer {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        sync::lock(&self.requests).push(request.clone());
        let decision = if matches!(request.kind, RequestKind::RevealBlocked { .. }) {
            ConfirmationDecision::Denied { reason: None }
        } else if let RequestKind::Secret { secret } = &request.kind
            && secret.op.takes_values()
        {
            match (&self.input, self.approve) {
                (Some(input), true) => ConfirmationDecision::Provided {
                    input: input.clone(),
                },
                _ => ConfirmationDecision::Rejected {
                    reason: Some("cancelled by test".into()),
                },
            }
        } else if self.approve && self.always && matches!(request.kind, RequestKind::Access { .. })
        {
            ConfirmationDecision::ApprovedAlways
        } else if self.approve {
            ConfirmationDecision::Approved
        } else {
            ConfirmationDecision::Rejected {
                reason: Some("denied by test".into()),
            }
        };
        Box::pin(async move { decision })
    }
}

/// Asks `confirmer`, treating a timeout as a denial.
pub async fn ask(confirmer: &dyn Confirmer, request: &ConfirmationRequest) -> ConfirmationDecision {
    match tokio::time::timeout(CONFIRMATION_TIMEOUT, confirmer.confirm(request)).await {
        Ok(decision) => decision,
        Err(_) => ConfirmationDecision::Denied {
            reason: Some("timed out waiting for the user".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ConfirmationRequest {
        ConfirmationRequest {
            kind: RequestKind::Call,
            context: "minikube".into(),
            session_id: "s".into(),
            client: "test".into(),
            tool: "scale".into(),
            namespace: "web".into(),
            summary: "Scale".into(),
            details: String::new(),
            dangerous: Vec::new(),
            mode: SessionMode::ClientOnly,
            allow_remember: false,
        }
    }

    #[tokio::test]
    async fn default_denies() {
        assert!(!ask(&DenyAll, &request()).await.is_approved());
    }

    #[tokio::test]
    async fn scripted_records() {
        let confirmer = ScriptedConfirmer::approving();
        assert!(ask(&confirmer, &request()).await.is_approved());
        assert_eq!(confirmer.requests().len(), 1);
        assert!(
            !ask(&ScriptedConfirmer::denying(), &request())
                .await
                .is_approved()
        );
    }

    #[test]
    fn requests_of_older_peers_still_parse() {
        let mut json = serde_json::to_value(request()).unwrap();
        let object = json.as_object_mut().unwrap();
        object.remove("kind");
        object.remove("context");
        let parsed: ConfirmationRequest = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.kind, RequestKind::Call);
        let grant = ConfirmationRequest {
            kind: RequestKind::Grant {
                grant: Grant::secret("web", "db"),
            },
            ..request()
        };
        let back: ConfirmationRequest =
            serde_json::from_str(&serde_json::to_string(&grant).unwrap()).unwrap();
        assert_eq!(back, grant);
        let exec = ConfirmationRequest {
            kind: RequestKind::Exec {
                command: vec!["sh".into(), "-c".into(), "id ".into()],
            },
            ..request()
        };
        let back: ConfirmationRequest =
            serde_json::from_str(&serde_json::to_string(&exec).unwrap()).unwrap();
        assert_eq!(back, exec);
        let rejected: ConfirmationDecision =
            serde_json::from_str(r#"{"decision":"rejected","reason":null}"#).unwrap();
        assert!(rejected.is_rejected() && !rejected.is_approved());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_denies() {
        struct Never;
        impl Confirmer for Never {
            fn confirm<'a>(
                &'a self,
                _: &'a ConfirmationRequest,
            ) -> BoxFuture<'a, ConfirmationDecision> {
                Box::pin(std::future::pending())
            }
        }
        assert!(!ask(&Never, &request()).await.is_approved());
    }
}
