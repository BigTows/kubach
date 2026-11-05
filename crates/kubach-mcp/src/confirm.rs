//! Human-in-the-loop: mutating calls and rights on request wait for the
//! user's decision.
//!
//! The app implements [`Confirmer`] with a UI prompt. Confirmation never
//! widens rights by itself: only calls that already passed the policy reach
//! it, and more access is asked for as such (#15). The same request
//! travels over the bridge for `kubach mcp` processes.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kubach_core::plugin::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::access::Access;
use crate::grants::Grant;
use crate::helm_tools::HelmChange;
use crate::policy::Finding;
use crate::scope::SessionMode;
use crate::secret_write::{SecretInput, SecretRequest};
use crate::tools::Progress;
use kubach_core::sync;

/// How long a call waits for the user before it counts as denied.
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(300);

/// How long an answer may take before the call counts as waiting for the
/// user (#116): the app's settings can answer at once, without asking.
pub const WAIT_NOTICE_AFTER: Duration = Duration::from_millis(200);

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
    /// One port-forward of the agent (#17), confirmed every time:
    /// `127.0.0.1:<local_port>` (any free port when `None`) to `port`
    /// (number or name) of `target` (`pod/web-0`, `service/web`). TCP only.
    PortForward {
        target: String,
        port: String,
        local_port: Option<u16>,
    },
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
    /// An `allNamespaces` call found namespaces the session has not opened
    /// (#107): a yes opens them for this and later such calls, until the
    /// session ends. One question for all of them, never one per namespace.
    Namespaces { names: Vec<String> },
    /// A batch of scales or restarts (#108): one question for every object
    /// it will change, never remembered. `namespace` of the request is
    /// empty when the batch spans several.
    Batch { batch: Box<BatchChange> },
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

/// What a [`BatchChange`] does to its objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchAction {
    Scale,
    Restart,
}

/// One object a batch changes, as the user approves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchObject {
    pub kind: String,
    pub namespace: String,
    pub name: String,
    /// Replicas now and after a scale; `None` for a restart.
    pub replicas: Option<(i64, i64)>,
}

impl BatchObject {
    /// `Deployment shop/web`.
    pub fn label(&self) -> String {
        format!("{} {}/{}", self.kind, self.namespace, self.name)
    }

    /// `3 → 0`, for a scale.
    pub fn change(&self) -> Option<String> {
        self.replicas.map(|(from, to)| format!("{from} → {to}"))
    }
}

/// The content of a [`RequestKind::Batch`]: `objects` is exactly what will
/// change, in full; the UI must show every one of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchChange {
    pub action: BatchAction,
    pub objects: Vec<BatchObject>,
    /// What the agent asked for and the batch does not include, with why.
    pub left_out: Vec<String>,
}

impl BatchChange {
    /// The namespaces of the objects, sorted.
    pub fn namespaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self.objects.iter().map(|o| o.namespace.clone()).collect();
        names.sort();
        names.dedup();
        names
    }
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
    /// "Don't ask again this session" may be offered for this kind of
    /// change in this namespace (#30): set by Kubach from
    /// [`rememberable`](crate::remember::rememberable), never in client-only
    /// mode (ADR 0005). The UI checks that function too.
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
    /// Approved, and the user ticked "don't ask again this session" for
    /// this kind of change in this namespace (#30). Kubach remembers it
    /// only for a request it marked [`ConfirmationRequest::allow_remember`];
    /// any other request answered this way counts as a plain approval.
    ApprovedForSession,
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
        matches!(
            self,
            Self::Approved | Self::ApprovedAlways | Self::ApprovedForSession
        )
    }

    /// The user answered no themselves.
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Approved
            | Self::ApprovedAlways
            | Self::ApprovedForSession
            | Self::Provided { .. } => None,
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

/// What the agent is told while a call waits for the user (#116): `what`
/// is the question, in the words of its summary.
pub(crate) fn waiting_text(what: &str) -> String {
    format!(
        "Waiting for the user's answer in the Kubach window (at most {} s): {what}",
        CONFIRMATION_TIMEOUT.as_secs()
    )
}

/// Runs `work`, calling `notice` once if it is not done after
/// [`WAIT_NOTICE_AFTER`].
pub(crate) async fn notice_if_slow<T>(work: impl Future<Output = T>, notice: impl FnOnce()) -> T {
    let mut work = std::pin::pin!(work);
    match tokio::time::timeout(WAIT_NOTICE_AFTER, &mut work).await {
        Ok(done) => done,
        Err(_) => {
            notice();
            work.await
        }
    }
}

/// A call of the session waiting for the user's answer, as `session_info`
/// and the bridge show it (#116). The summary says what is asked, never a
/// value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingCall {
    /// Unique within the session.
    pub id: u64,
    pub tool: String,
    pub summary: String,
    /// Until the call counts as denied.
    pub seconds_left: u64,
}

struct WaitEntry {
    id: u64,
    tool: String,
    summary: String,
    since: Instant,
}

#[derive(Default)]
struct WaitState {
    next: u64,
    entries: Vec<WaitEntry>,
}

/// The calls of one session waiting for the user right now. It is how an
/// agent without progress notifications tells a call waiting for the user
/// from a stuck one, and how a hosted session's process learns of a wait.
#[derive(Clone, Default)]
pub struct WaitingCalls {
    state: Arc<Mutex<WaitState>>,
}

impl WaitingCalls {
    fn add(&self, request: &ConfirmationRequest) -> WaitTicket {
        let mut state = sync::lock(&self.state);
        state.next += 1;
        let id = state.next;
        state.entries.push(WaitEntry {
            id,
            tool: request.tool.clone(),
            summary: request.summary.clone(),
            since: Instant::now(),
        });
        WaitTicket {
            calls: self.clone(),
            id,
        }
    }

    pub fn list(&self) -> Vec<WaitingCall> {
        sync::lock(&self.state)
            .entries
            .iter()
            .map(|entry| WaitingCall {
                id: entry.id,
                tool: entry.tool.clone(),
                summary: entry.summary.clone(),
                seconds_left: CONFIRMATION_TIMEOUT
                    .saturating_sub(entry.since.elapsed())
                    .as_secs(),
            })
            .collect()
    }
}

/// Keeps a call in [`WaitingCalls`] until the answer, a timeout or the
/// client giving up on the call.
struct WaitTicket {
    calls: WaitingCalls,
    id: u64,
}

impl Drop for WaitTicket {
    fn drop(&mut self) {
        sync::lock(&self.calls.state)
            .entries
            .retain(|entry| entry.id != self.id);
    }
}

/// A [`Confirmer`] that lets the agent know when a question really waits
/// for the user (#116): it lists the call in `waiting` and sends one
/// progress notification, if the client asked for them. Every question of a
/// tool call goes through it, whichever tool asks.
pub(crate) struct AnnouncingConfirmer<'a> {
    pub confirmer: &'a dyn Confirmer,
    pub waiting: &'a WaitingCalls,
    pub progress: Option<&'a Progress>,
}

impl Confirmer for AnnouncingConfirmer<'_> {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        // Not a question: the user is only told.
        if matches!(request.kind, RequestKind::RevealBlocked { .. }) {
            return self.confirmer.confirm(request);
        }
        Box::pin(async move {
            let mut ticket = None;
            notice_if_slow(self.confirmer.confirm(request), || {
                ticket = Some(self.waiting.add(request));
                if let Some(progress) = self.progress {
                    progress(waiting_text(&request.summary));
                }
            })
            .await
        })
    }
}

/// Answers every request the same way and records them. For tests.
#[derive(Debug)]
pub struct ScriptedConfirmer {
    approve: bool,
    /// Ticks "Allow by default" for access requests.
    always: bool,
    /// Ticks "Don't ask again this session" where offered (#30).
    remember: bool,
    /// What an approving confirmer enters into Secret forms.
    input: Option<SecretInput>,
    requests: Mutex<Vec<ConfirmationRequest>>,
}

impl ScriptedConfirmer {
    pub fn approving() -> Self {
        Self {
            approve: true,
            always: false,
            remember: false,
            input: None,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn denying() -> Self {
        Self {
            approve: false,
            always: false,
            remember: false,
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

    /// Approves everything, ticking "Don't ask again this session" where
    /// the request offers it (#30).
    pub fn approving_for_session() -> Self {
        Self {
            remember: true,
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
        } else if self.approve && self.remember && request.allow_remember {
            ConfirmationDecision::ApprovedForSession
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
        let remembered: ConfirmationDecision =
            serde_json::from_str(r#"{"decision":"approved-for-session"}"#).unwrap();
        assert_eq!(remembered, ConfirmationDecision::ApprovedForSession);
        assert!(remembered.is_approved());
        let rejected: ConfirmationDecision =
            serde_json::from_str(r#"{"decision":"rejected","reason":null}"#).unwrap();
        assert!(rejected.is_rejected() && !rejected.is_approved());
    }

    /// Answers after `delay`.
    struct Slow {
        delay: Duration,
    }

    impl Confirmer for Slow {
        fn confirm<'a>(
            &'a self,
            _: &'a ConfirmationRequest,
        ) -> BoxFuture<'a, ConfirmationDecision> {
            Box::pin(async {
                tokio::time::sleep(self.delay).await;
                ConfirmationDecision::Approved
            })
        }
    }

    fn collector() -> (Progress, Arc<Mutex<Vec<String>>>) {
        let told: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = told.clone();
        let progress: Progress = Arc::new(move |text| sync::lock(&sink).push(text));
        (progress, told)
    }

    /// A question the user has to answer is announced once, right after
    /// the short grace, and listed until the answer (#116).
    #[tokio::test(start_paused = true)]
    async fn a_slow_answer_is_announced_and_listed() {
        let (progress, told) = collector();
        let waiting = WaitingCalls::default();
        let slow = Slow {
            delay: Duration::from_secs(30),
        };
        let announcing = AnnouncingConfirmer {
            confirmer: &slow,
            waiting: &waiting,
            progress: Some(&progress),
        };
        let request = request();
        let check = async {
            tokio::time::sleep(WAIT_NOTICE_AFTER * 2).await;
            let listed = waiting.list();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].tool, "scale");
            assert_eq!(listed[0].summary, "Scale");
            assert!(listed[0].seconds_left <= CONFIRMATION_TIMEOUT.as_secs());
            assert_eq!(sync::lock(&told).len(), 1);
            assert!(sync::lock(&told)[0].contains("Kubach window"));
        };
        let (decision, ()) = tokio::join!(ask(&announcing, &request), check);
        assert!(decision.is_approved());
        assert!(waiting.list().is_empty());
        assert_eq!(sync::lock(&told).len(), 1);
    }

    /// Without a progress token the call is still listed, and a timeout
    /// takes it off the list.
    #[tokio::test(start_paused = true)]
    async fn without_progress_a_wait_is_only_listed() {
        let waiting = WaitingCalls::default();
        let never = Slow {
            delay: CONFIRMATION_TIMEOUT * 2,
        };
        let announcing = AnnouncingConfirmer {
            confirmer: &never,
            waiting: &waiting,
            progress: None,
        };
        let request = request();
        let check = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(waiting.list().len(), 1);
        };
        let (decision, ()) = tokio::join!(ask(&announcing, &request), check);
        assert!(!decision.is_approved());
        assert!(waiting.list().is_empty());
    }

    /// An answer given at once (the settings already allow it) and a mere
    /// notice are no wait.
    #[tokio::test(start_paused = true)]
    async fn nothing_is_announced_without_a_wait() {
        let (progress, told) = collector();
        let waiting = WaitingCalls::default();
        let scripted = ScriptedConfirmer::approving();
        let announcing = AnnouncingConfirmer {
            confirmer: &scripted,
            waiting: &waiting,
            progress: Some(&progress),
        };
        assert!(ask(&announcing, &request()).await.is_approved());
        let slow = Slow {
            delay: Duration::from_secs(1),
        };
        let announcing = AnnouncingConfirmer {
            confirmer: &slow,
            waiting: &waiting,
            progress: Some(&progress),
        };
        let notice = ConfirmationRequest {
            kind: RequestKind::RevealBlocked { name: "db".into() },
            ..request()
        };
        ask(&announcing, &notice).await;
        assert!(sync::lock(&told).is_empty());
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
