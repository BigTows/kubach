//! AI agent access in the app (#86, ADR 0005): the MCP HTTP server on
//! `127.0.0.1`, the session HTTP agents use, sessions of `kubach mcp`
//! processes reported over the bridge, confirmations of mutating calls and
//! requests for rights (#227) in the main window and the local audit log.
//!
//! Contexts imported into the vault (#121, ADR 0008) are used only here:
//! the app runs their sessions for `kubach mcp` processes too
//! ([`HostedSessions`]), and ends every session on a vault context as soon
//! as the vault locks.
//!
//! The MCP layer runs on the I/O runtime; it reaches the UI through a
//! channel ([`UiEvent`]) and waits for the user's decision on a oneshot.

pub(crate) mod cli;
mod confirm_dialog;
mod hosted;
pub mod install;
mod secret_dialog;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui_kit::*;
use kubach_core::Contexts;
use kubach_core::plugin::BoxFuture;
use kubach_core::runtime;
use kubach_core::settings::{self, ContextKey};
use kubach_i18n::{Message, msg, t};
use kubach_mcp::bridge::{
    self, BridgeHandler, ExternalSession, HeartbeatReply, HostRefusal, HostReply, HostRequest,
};
use kubach_mcp::confirm::CONFIRMATION_TIMEOUT;
use kubach_mcp::hosted::HostedSessions;
use kubach_mcp::{
    AuditLog, BoundaryInfo, BridgeOptions, ConfirmationDecision, ConfirmationRequest, Confirmer,
    ContextChooser, ContextEntry, Grant, GrantEntry, HttpEndpoint, HttpOptions, McpServer, Profile,
    RequestKind, SecurityPolicy, Session, SessionMode, SessionSpec, SharedServer,
};
use serde_json::Value;

pub use cli::run as run_cli;
pub use confirm_dialog::watch_confirmations;

use crate::settings::AppSettings;
use kubach_core::sync;

/// The user's `helm` for the agent's Helm tools (#418): the binary of
/// Settings → Tools (or `PATH`, or the downloaded one), the kubeconfig
/// files when the context lives in one (`in_file`; not for vault
/// contexts), and the repositories of the user's helm.
pub(crate) fn helm_env(settings: &settings::Settings, in_file: bool) -> kubach_mcp::HelmEnv {
    kubach_mcp::HelmEnv {
        binary: settings.tools.helm(),
        kubeconfig: in_file.then(|| {
            kubach_core::helm::cli::kubeconfig_files(&settings.clusters.resolved_kubeconfig_paths())
        }),
        paths: kubach_core::helm::repo::HelmPaths::from_env(),
    }
}

/// Audit log of every MCP call, in the config directory (JSON lines).
pub const AUDIT_FILE: &str = "mcp-audit.jsonl";

/// How often sessions, confirmations and the audit view are refreshed.
const TICK: Duration = Duration::from_secs(1);
/// A `kubach mcp` process not heard from for this long is gone.
const STALE: Duration = Duration::from_secs(15);
/// Why the app's session ended or did not start.
const LOCKED_SESSION: &str =
    "The session ended: its context is in the vault, which was locked. Unlock it to start again.";
/// Audit entries kept for the settings page, newest last.
const AUDIT_ROWS: usize = 200;
/// Bytes read from the end of the audit log for the view.
const AUDIT_TAIL_BYTES: u64 = 256 * 1024;

/// From the MCP layer to the UI.
enum UiEvent {
    Confirm(ConfirmationRequest, oneshot::Sender<ConfirmationDecision>),
    /// A `kubach mcp` process asks for a session on a vault context.
    Host(HostRequest, oneshot::Sender<Result<HostReply, HostRefusal>>),
    /// External sessions changed.
    Changed,
    /// An HTTP agent lists the contexts it may choose (#421).
    ListContexts(oneshot::Sender<Vec<ContextEntry>>),
    /// An HTTP agent chose a context with `use_context` (#421).
    UseContext {
        context: String,
        client: String,
        reply: oneshot::Sender<Result<McpServer, String>>,
    },
}

/// How long the user's no to a context holds before `use_context` may ask
/// again.
const CONTEXT_REFUSAL: Duration = Duration::from_secs(10 * 60);

/// Lets HTTP agents pick their context (#421): both calls go to the UI
/// thread, where the settings, the vault and the window are.
struct AppChooser {
    tx: mpsc::UnboundedSender<UiEvent>,
}

impl ContextChooser for AppChooser {
    fn list(&self) -> BoxFuture<'_, Vec<ContextEntry>> {
        let (reply, answer) = oneshot::channel();
        let sent = self.tx.unbounded_send(UiEvent::ListContexts(reply)).is_ok();
        Box::pin(async move {
            if !sent {
                return Vec::new();
            }
            answer.await.unwrap_or_default()
        })
    }

    fn choose<'a>(
        &'a self,
        context: &'a str,
        client: &'a str,
    ) -> BoxFuture<'a, Result<McpServer, String>> {
        let (reply, answer) = oneshot::channel();
        let sent = self
            .tx
            .unbounded_send(UiEvent::UseContext {
                context: context.to_owned(),
                client: client.to_owned(),
                reply,
            })
            .is_ok();
        Box::pin(async move {
            let closed = || "the Kubach window is closed".to_owned();
            if !sent {
                return Err(closed());
            }
            answer.await.unwrap_or_else(|_| Err(closed()))
        })
    }
}

/// Contexts an agent may name, from kubeconfig and the vault (#421).
/// `unlocked`: vault contexts whose credentials are in memory.
pub(crate) fn context_entries(
    settings: &settings::Settings,
    contexts: Option<&Contexts>,
    unlocked: &HashSet<String>,
    vault_names: &[String],
) -> Vec<ContextEntry> {
    let mut entries: Vec<ContextEntry> = Vec::new();
    let mut add = |name: &str, server: &str, source: &str, unavailable: Option<String>| {
        if entries.iter().any(|entry| entry.name == name) {
            return;
        }
        let stored = settings
            .security
            .policy(&ContextKey::new(name.to_owned(), server));
        entries.push(ContextEntry {
            name: name.to_owned(),
            server: server.to_owned(),
            source: source.to_owned(),
            agent_access: stored.mcp_enabled,
            unavailable,
            current: false,
        });
    };
    if let Some(contexts) = contexts {
        for context in contexts.list() {
            let server = contexts.server(&context.name).unwrap_or_default();
            let imported = contexts
                .source(&context.name)
                .is_some_and(|source| source.is_imported());
            add(
                &context.name,
                server,
                if imported { "vault" } else { "kubeconfig" },
                None,
            );
        }
    }
    // Vault contexts while it is locked: names only, from its header.
    for name in vault_names {
        if !unlocked.contains(name) {
            add(
                name,
                "",
                "vault",
                Some("the Kubach vault is locked; the user unlocks it in Kubach".into()),
            );
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Whether an agent may get `name` at all (#421): known, in an unlocked
/// vault if it is imported, AI agent access on.
/// Returns its API server and whether it is in the vault.
pub(crate) fn check_context_choice(
    settings: &settings::Settings,
    contexts: Option<&Contexts>,
    vault_names: &[String],
    unlocked: &HashSet<String>,
    name: &str,
) -> Result<(String, bool), String> {
    let known = contexts.and_then(|contexts| contexts.get(name).map(|_| contexts));
    let Some(contexts) = known else {
        if vault_names.iter().any(|known| known == name) && !unlocked.contains(name) {
            return Err(format!(
                "context `{name}` is in the Kubach vault, which is locked. Ask the user to \
                 unlock it in Kubach, then call use_context again"
            ));
        }
        return Err(format!(
            "context `{name}` is neither in kubeconfig nor in the Kubach vault; list_contexts \
             shows the ones Kubach knows"
        ));
    };
    let server = contexts.server(name).unwrap_or_default().to_owned();
    let in_vault = contexts
        .source(name)
        .is_some_and(|source| source.is_imported());
    let stored = settings
        .security
        .policy(&ContextKey::new(name.to_owned(), &server));
    if !stored.mcp_enabled {
        return Err(format!(
            "AI agent access is off for context `{name}`. Only the user turns it on, in Kubach: \
             Settings → Security → `{name}` → AI agent access"
        ));
    }
    Ok((server, in_vault))
}

/// Confirmations one session may have waiting for the user at once (#195).
/// Each waits up to [`CONFIRMATION_TIMEOUT`]; an agent asking for more is
/// refused right away instead of piling them up.
const MAX_PENDING_PER_SESSION: usize = 5;

/// Confirmations waiting per session, counted from the ask to the answer.
#[derive(Clone, Default)]
struct WaitingCount(Arc<Mutex<HashMap<String, usize>>>);

impl WaitingCount {
    /// A slot for one more confirmation of `session`, or `None` when it
    /// already has [`MAX_PENDING_PER_SESSION`] waiting.
    fn take(&self, session: &str) -> Option<WaitingSlot> {
        let mut waiting = sync::lock(&self.0);
        let count = waiting.entry(session.to_owned()).or_default();
        if *count >= MAX_PENDING_PER_SESSION {
            return None;
        }
        *count += 1;
        Some(WaitingSlot {
            count: self.clone(),
            session: session.to_owned(),
        })
    }
}

/// Frees its place in [`WaitingCount`] when dropped.
struct WaitingSlot {
    count: WaitingCount,
    session: String,
}

impl Drop for WaitingSlot {
    fn drop(&mut self) {
        let mut waiting = sync::lock(&self.count.0);
        if let Some(count) = waiting.get_mut(&self.session) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                waiting.remove(&self.session);
            }
        }
    }
}

/// Asks the user in the main window. Used by the app's own session and by
/// the bridge for `kubach mcp` processes.
#[derive(Clone)]
struct UiConfirmer {
    tx: mpsc::UnboundedSender<UiEvent>,
    /// Bounds what reaches the channel and the pending list per session.
    waiting: WaitingCount,
}

impl UiConfirmer {
    fn ask(&self, request: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision> {
        let Some(slot) = self.waiting.take(&request.session_id) else {
            return Box::pin(std::future::ready(ConfirmationDecision::Denied {
                reason: Some(format!(
                    "too many confirmations are waiting for the user \
                     ({MAX_PENDING_PER_SESSION} per session); wait for them to be answered"
                )),
            }));
        };
        let (reply, answer) = oneshot::channel();
        let sent = self
            .tx
            .unbounded_send(UiEvent::Confirm(request, reply))
            .is_ok();
        Box::pin(async move {
            let _slot = slot;
            if !sent {
                return ConfirmationDecision::Denied {
                    reason: Some("the Kubach window is closed".into()),
                };
            }
            answer.await.unwrap_or(ConfirmationDecision::Denied {
                reason: Some("the confirmation was dismissed".into()),
            })
        })
    }
}

impl Confirmer for UiConfirmer {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        self.ask(request.clone())
    }
}

/// Sessions of `kubach mcp` processes, shared with the bridge handler.
#[derive(Default)]
struct External {
    /// By id: the session, its key (from the first heartbeat) and when it
    /// was last heard of.
    live: HashMap<String, (ExternalSession, String, Instant)>,
    /// Revoked by the user; told to the process on its next heartbeat.
    revoked: HashSet<String>,
    /// Rights the user took back, by session; sent with the next heartbeat.
    revoked_grants: HashMap<String, Vec<Grant>>,
    /// Sessions whose "don't ask again" the user cleared (#30); told to the
    /// process on its next heartbeat.
    forget_remembered: HashSet<String>,
}

/// The app's side of the bridge.
struct AppBridge {
    confirmer: UiConfirmer,
    external: Arc<Mutex<External>>,
    hosted: HostedSessions,
}

impl AppBridge {
    fn external(&self) -> std::sync::MutexGuard<'_, External> {
        sync::lock(&self.external)
    }
}

impl BridgeHandler for AppBridge {
    fn confirm(&self, request: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision> {
        self.confirmer.ask(request)
    }

    fn heartbeat(&self, mut session: ExternalSession, key: &str) -> Option<HeartbeatReply> {
        let mut external = self.external();
        if external
            .live
            .get(&session.id)
            .is_some_and(|(_, known, _)| known != key)
        {
            // Not from the session's process (#191).
            return None;
        }
        let revoked = external.revoked.contains(&session.id);
        let revoke_grants = external
            .revoked_grants
            .remove(&session.id)
            .unwrap_or_default();
        let forget_remembered = external.forget_remembered.remove(&session.id);
        // Shown as taken back right away; the process confirms it with its
        // next heartbeat.
        for entry in &mut session.grants {
            if revoke_grants.contains(&entry.grant) {
                entry.state = kubach_mcp::GrantState::Denied;
            }
        }
        if forget_remembered {
            session.remembered.clear();
        }
        external.live.insert(
            session.id.clone(),
            (session, key.to_owned(), Instant::now()),
        );
        drop(external);
        self.confirmer.tx.unbounded_send(UiEvent::Changed).ok();
        Some(HeartbeatReply {
            revoked,
            revoke_grants,
            forget_remembered,
        })
    }

    fn ended(&self, id: &str, key: &str) -> bool {
        let mut external = self.external();
        match external.live.get(id) {
            // Unknown: nothing to end (a revocation is kept only for live
            // sessions).
            None => return true,
            // Only the session's own process ends it, and so clears a
            // revocation (#191).
            Some((_, known, _)) if known != key => return false,
            Some(_) => {}
        }
        external.live.remove(id);
        external.revoked.remove(id);
        external.revoked_grants.remove(id);
        external.forget_remembered.remove(id);
        drop(external);
        self.confirmer.tx.unbounded_send(UiEvent::Changed).ok();
        true
    }

    fn changed(&self) {
        self.confirmer.tx.unbounded_send(UiEvent::Changed).ok();
    }

    fn host(&self, request: HostRequest) -> BoxFuture<'static, Result<HostReply, HostRefusal>> {
        let (reply, answer) = oneshot::channel();
        let sent = self
            .confirmer
            .tx
            .unbounded_send(UiEvent::Host(request, reply))
            .is_ok();
        Box::pin(async move {
            let closing = || HostRefusal::denied("the Kubach window is closed");
            if !sent {
                return Err(closing());
            }
            answer.await.unwrap_or_else(|_| Err(closing()))
        })
    }

    fn hosted(&self) -> Option<HostedSessions> {
        Some(self.hosted.clone())
    }
}

/// Closes ended sessions on the I/O runtime (a temporary ServiceAccount is
/// deleted on the way).
fn close_all(sessions: Vec<Arc<Session>>) {
    for session in sessions {
        // Detached: the session's owner has already let it go, and the
        // temporary ServiceAccount must be deleted all the same.
        runtime::spawn_detached(async move { session.close().await });
    }
}

/// Contexts of the vault with credentials in memory; empty while locked.
fn unlocked_vault_contexts(cx: &App) -> HashSet<String> {
    crate::vault::global(cx)
        .filter(|vault| vault.read(cx).is_unlocked())
        .map(|vault| hosted::unlocked_contexts(&vault.read(cx).imported()))
        .unwrap_or_default()
}

/// State of the HTTP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Off,
    Starting,
    Running { url: String },
    Failed(SharedString),
}

/// Something the user should know about an agent's request that is not a
/// question: revealing a Secret where values are hidden, access that could
/// not be saved as the default (#15). Shown as a notification with a way to
/// the context's security settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub title: Message,
    pub context: String,
    pub message: Message,
}

/// A mutating call or a request for a right, waiting for the user.
pub struct Pending {
    pub id: u64,
    pub request: ConfirmationRequest,
    pub deadline: Instant,
    reply: Option<oneshot::Sender<ConfirmationDecision>>,
}

/// One row of the sessions list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    /// `None` for the app's own (HTTP) session, else the `kubach mcp` pid.
    pub pid: Option<u32>,
    /// On a vault context: ends when the vault locks.
    pub vault: bool,
    pub clients: Vec<String>,
    pub context: String,
    /// Pinned at launch, else the namespaces the user allowed reading.
    pub namespaces: Vec<String>,
    /// The session's access level now (#15).
    pub profile: Profile,
    pub mode: SessionMode,
    /// Who holds the boundary in each namespace (#401).
    pub boundaries: Vec<BoundaryInfo>,
    pub started_at: String,
    pub expires_at: String,
    pub calls: u64,
    pub revoking: bool,
    /// The user's answers for rights (#227).
    pub grants: Vec<GrantEntry>,
    /// Access the user allowed beyond the minimum for this session (#15).
    pub raised: Option<kubach_mcp::Access>,
    /// Kinds of change per namespace the user said not to ask about again
    /// in this session (#30).
    pub remembered: Vec<kubach_mcp::Remembered>,
}

/// One audit log line, as shown in the settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    pub time: String,
    pub session: String,
    pub client: String,
    pub tool: String,
    pub result: String,
    pub decided_by: String,
    pub allowed: bool,
    pub reason: Option<String>,
    pub args: String,
}

impl AuditRow {
    fn parse(line: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(line).ok()?;
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        Some(Self {
            time: text("time"),
            session: text("session"),
            client: text("client"),
            tool: text("tool"),
            result: text("result"),
            decided_by: text("decided_by"),
            allowed: value["allowed"].as_bool().unwrap_or(false),
            reason: value["reason"].as_str().map(str::to_owned),
            args: serde_json::to_string(&value["args"]).unwrap_or_default(),
        })
    }
}

/// Reads the last [`AUDIT_ROWS`] entries of the log at `path`.
fn read_audit_tail(path: &Path) -> std::io::Result<Vec<AuditRow>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(AUDIT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut text = String::new();
    file.take(AUDIT_TAIL_BYTES).read_to_string(&mut text)?;
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is cut in the middle.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(AUDIT_ROWS);
    Ok(lines[skip..]
        .iter()
        .filter_map(|line| AuditRow::parse(line))
        .collect())
}

/// AI agent state everyone reads; observe it for changes.
pub struct AgentState {
    dir: Option<PathBuf>,
    token: Option<String>,
    shared: SharedServer,
    endpoint: Option<HttpEndpoint>,
    status: ServerStatus,
    /// Port the server should listen on; `None` when off.
    wanted: Option<u16>,
    /// Bumped on every start or stop; late results of older starts are
    /// dropped.
    generation: u64,
    session: Option<Arc<Session>>,
    /// The app's session is on a vault context.
    session_in_vault: bool,
    session_error: Option<SharedString>,
    /// Contexts the user refused to an HTTP agent, and when (#421).
    refused_contexts: HashMap<String, Instant>,
    external: Arc<Mutex<External>>,
    /// Sessions the app runs for `kubach mcp` on vault contexts.
    hosted: HostedSessions,
    confirmer: UiConfirmer,
    pending: VecDeque<Pending>,
    next_pending: u64,
    /// The pending confirmation the main window shows.
    shown: Option<u64>,
    /// Notices for the main window, oldest first.
    notices: VecDeque<Notice>,
    /// Errors of taking back a right, for the sessions screen.
    grant_error: Option<SharedString>,
    audit: Option<Arc<AuditLog>>,
    audit_rows: Vec<AuditRow>,
    audit_error: Option<SharedString>,
    audit_len: Option<u64>,
    _tasks: Vec<Task<()>>,
    _start: Option<Task<()>>,
}

struct GlobalAgent(Entity<AgentState>);

impl Global for GlobalAgent {}

/// The agent state, if [`init`] ran.
pub fn global(cx: &App) -> Option<Entity<AgentState>> {
    cx.try_global::<GlobalAgent>()
        .map(|global| global.0.clone())
}

/// Creates the agent state and starts the HTTP server if it is on. Call
/// after `settings::init`.
pub fn init(cx: &mut App) {
    let entity = cx.new(|cx| AgentState::new(settings::config_dir(), cx));
    cx.set_global(GlobalAgent(entity));
}

impl AgentState {
    fn new(dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded();
        let audit = dir.as_ref().and_then(|dir| {
            std::fs::create_dir_all(dir).ok()?;
            AuditLog::open(dir.join(AUDIT_FILE)).ok().map(Arc::new)
        });
        let listener = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    match event {
                        UiEvent::Confirm(request, reply) => {
                            let settings =
                                AppSettings::try_global(cx).map(|s| s.read(cx).get().clone());
                            this.push_pending(request, reply, settings.as_ref())
                        }
                        UiEvent::Host(request, reply) => this.host(request, reply, cx),
                        UiEvent::Changed => {}
                        UiEvent::ListContexts(reply) => {
                            reply.send(this.list_contexts(cx)).ok();
                        }
                        UiEvent::UseContext {
                            context,
                            client,
                            reply,
                        } => this.use_context(context, client, reply, cx),
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        });
        let tasks = vec![listener, ticker];
        if let Some(settings) = AppSettings::try_global(cx) {
            cx.observe(&settings, |this, _, cx| this.sync_server(cx))
                .detach();
        }
        // Locking the vault (by hand, idle timer, screen lock) ends every
        // session on its contexts (ADR 0008).
        if let Some(vault) = crate::vault::global(cx) {
            cx.observe(&vault, |this, _, cx| this.vault_changed(cx))
                .detach();
        }
        // Sessions run by the app end with the app (ADR 0005); a temporary
        // ServiceAccount is deleted on the way out.
        cx.on_app_quit(|this, _| {
            let mut sessions = this.hosted.drain();
            sessions.extend(this.session.take());
            this.shared.set(None);
            async move {
                for session in sessions {
                    // Detached: if the quit stops waiting, the close keeps
                    // going for as long as the process lives.
                    let _ = runtime::spawn_detached(async move { session.close().await }).await;
                }
            }
        })
        .detach();
        let shared = SharedServer::new();
        shared.set_chooser(Arc::new(AppChooser { tx: tx.clone() }));
        let mut this = Self {
            dir,
            token: None,
            shared,
            endpoint: None,
            status: ServerStatus::Off,
            wanted: None,
            generation: 0,
            session: None,
            session_in_vault: false,
            session_error: None,
            refused_contexts: HashMap::new(),
            external: Arc::default(),
            hosted: HostedSessions::new(),
            confirmer: UiConfirmer {
                tx,
                waiting: WaitingCount::default(),
            },
            pending: VecDeque::new(),
            next_pending: 0,
            shown: None,
            notices: VecDeque::new(),
            grant_error: None,
            audit,
            audit_rows: Vec::new(),
            audit_error: None,
            audit_len: None,
            _tasks: tasks,
            _start: None,
        };
        if let Some(dir) = &this.dir {
            this.token = bridge::read_token(dir);
        }
        this.refresh_audit();
        this.sync_server(cx);
        this
    }

    pub fn status(&self) -> &ServerStatus {
        &self.status
    }

    /// The bearer token for the HTTP server, created when it first starts.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Where the HTTP server's token is kept, for commands that read it
    /// from the file instead of carrying it.
    pub fn token_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(bridge::TOKEN_FILE))
    }

    pub fn audit_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(AUDIT_FILE))
    }

    pub fn audit_rows(&self) -> &[AuditRow] {
        &self.audit_rows
    }

    pub fn audit_error(&self) -> Option<&SharedString> {
        self.audit_error.as_ref()
    }

    fn pending_by_id(&self, id: u64) -> Option<&Pending> {
        self.pending.iter().find(|pending| pending.id == id)
    }

    /// Mutating calls waiting for the user's confirmation.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Notices not shown yet; the main window takes them.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        self.notices.drain(..).collect()
    }

    /// Why the last revocation of a right failed on the server, if it did.
    pub fn grant_error(&self) -> Option<&SharedString> {
        self.grant_error.as_ref()
    }

    /// The HTTP server runs and no HTTP agent chose a context yet (#421).
    pub fn http_waiting_for_context(&self) -> bool {
        matches!(self.status, ServerStatus::Running { .. })
            && !self.session.as_ref().is_some_and(|s| s.is_active())
    }

    pub fn session_error(&self) -> Option<&SharedString> {
        self.session_error.as_ref()
    }

    /// `server-side limits are off: …` for `namespace` of session `id` (the
    /// app's own or one it hosts) that fell back to client-only limits
    /// (#197).
    pub fn server_limits_warning(&self, id: &str, namespace: &str) -> Option<String> {
        self.session
            .as_ref()
            .filter(|session| session.id() == id)
            .cloned()
            .or_else(|| self.hosted.session(id))
            .and_then(|session| session.server_limits_warning(namespace))
    }

    /// Active sessions: the app's own first, then `kubach mcp` processes.
    pub fn sessions(&self) -> Vec<SessionRow> {
        let mut rows = Vec::new();
        if let Some(session) = self.session.as_ref().filter(|s| s.is_active()) {
            let scope = session.scope();
            let activity = session.activity();
            rows.push(SessionRow {
                id: session.id().to_owned(),
                pid: None,
                vault: self.session_in_vault,
                clients: activity.clients,
                context: scope.context.clone(),
                namespaces: session.namespaces(),
                profile: session.access().profile,
                mode: session.mode(),
                boundaries: session.boundaries(),
                started_at: scope.started_at.to_string(),
                expires_at: scope.expires_at.to_string(),
                calls: activity.calls,
                revoking: false,
                grants: session.grants().entries(),
                raised: session.raised(),
                remembered: session.remembered(),
            });
        }
        let external = sync::lock(&self.external);
        let mut others: Vec<SessionRow> = external
            .live
            .values()
            .map(|(session, _, _)| SessionRow {
                id: session.id.clone(),
                pid: Some(session.pid),
                vault: false,
                clients: session.clients.clone(),
                context: session.context.clone(),
                namespaces: session.namespaces.clone(),
                profile: session.profile,
                mode: session.mode,
                boundaries: session.boundaries.clone(),
                started_at: session.started_at.clone(),
                expires_at: session.expires_at.clone(),
                calls: session.calls,
                revoking: external.revoked.contains(&session.id),
                grants: session.grants.clone(),
                raised: session.raised,
                remembered: session.remembered.clone(),
            })
            .collect();
        drop(external);
        others.extend(self.hosted.list().into_iter().map(|session| SessionRow {
            id: session.id,
            pid: Some(session.pid),
            vault: true,
            clients: session.clients,
            context: session.context,
            namespaces: session.namespaces,
            profile: session.profile,
            mode: session.mode,
            boundaries: session.boundaries,
            started_at: session.started_at,
            expires_at: session.expires_at,
            calls: session.calls,
            revoking: false,
            grants: session.grants,
            raised: session.raised,
            remembered: session.remembered,
        }));
        others.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        rows.extend(others);
        rows
    }

    // ---------------------------------------------------------------- server

    /// Starts, stops or restarts the server to match the settings.
    fn sync_server(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = AppSettings::try_global(cx) else {
            return;
        };
        let agent = settings.read(cx).get().agent.clone();
        let wanted = agent.http_enabled.then_some(agent.http_port);
        if wanted == self.wanted {
            return;
        }
        self.restart(wanted, cx);
    }

    fn restart(&mut self, wanted: Option<u16>, cx: &mut Context<Self>) {
        self.wanted = wanted;
        self.generation += 1;
        self._start = None;
        if let Some(endpoint) = self.endpoint.take() {
            // Detached: the old server must finish shutting down (and free
            // its port) although nothing waits for it.
            runtime::spawn_detached(endpoint.shutdown());
        }
        let Some(port) = wanted else {
            self.status = ServerStatus::Off;
            cx.notify();
            return;
        };
        let Some(dir) = self.dir.clone() else {
            self.status =
                ServerStatus::Failed("the OS has no settings folder for the token".into());
            cx.notify();
            return;
        };
        let token = match bridge::load_or_create_token(&dir) {
            Ok(token) => token,
            Err(error) => {
                self.status =
                    ServerStatus::Failed(format!("cannot store the token: {error}").into());
                cx.notify();
                return;
            }
        };
        // The bridge's own token, new on every start: never shown, never
        // given to HTTP agents (#191).
        let bridge_token = match bridge::regenerate_bridge_token(&dir) {
            Ok(token) => token,
            Err(error) => {
                self.status =
                    ServerStatus::Failed(format!("cannot store the bridge token: {error}").into());
                cx.notify();
                return;
            }
        };
        self.token = Some(token.clone());
        self.status = ServerStatus::Starting;
        let handler: Arc<dyn BridgeHandler> = Arc::new(AppBridge {
            confirmer: self.confirmer.clone(),
            external: self.external.clone(),
            hosted: self.hosted.clone(),
        });
        let shared = self.shared.clone();
        let task = runtime::spawn(async move {
            kubach_mcp::serve_shared_http(
                shared,
                HttpOptions {
                    port,
                    token,
                    bridge: Some(BridgeOptions {
                        handler,
                        token: bridge_token,
                    }),
                },
            )
            .await
        });
        let generation = self.generation;
        self._start = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let result = match result {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                match result {
                    Ok(endpoint) if this.generation == generation => {
                        this.status = ServerStatus::Running {
                            url: endpoint.url(),
                        };
                        this.endpoint = Some(endpoint);
                    }
                    Ok(endpoint) => {
                        // Detached: a server started for an older request
                        // shuts down with nobody waiting for it.
                        runtime::spawn_detached(endpoint.shutdown());
                    }
                    Err(error) if this.generation == generation => {
                        this.status = ServerStatus::Failed(
                            format!("cannot listen on 127.0.0.1:{port}: {error}").into(),
                        );
                    }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Replaces the token; agents configured with the old one stop working.
    pub fn regenerate_token(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let dir = self.dir.clone().ok_or("the OS has no settings folder")?;
        let token = bridge::regenerate_token(&dir).map_err(|error| error.to_string())?;
        self.token = Some(token);
        if self.wanted.is_some() {
            self.restart(self.wanted, cx);
        }
        cx.notify();
        Ok(())
    }

    // -------------------------------------------------------------- sessions

    /// Contexts and what Kubach knows of them, for `list_contexts` (#421).
    fn list_contexts(&self, cx: &App) -> Vec<ContextEntry> {
        let Some(settings) = AppSettings::try_global(cx) else {
            return Vec::new();
        };
        let settings = settings.read(cx).get();
        let imported = crate::vault::imported(cx);
        let vault_names: Vec<String> = imported
            .iter()
            .flat_map(|entry| entry.contexts.iter().cloned())
            .collect();
        let contexts = Contexts::load_all(
            kubach_core::kubeconfig::sources_with(&settings.clusters.resolved_kubeconfig_paths()),
            imported,
        )
        .ok();
        let mut entries = context_entries(
            settings,
            contexts.as_ref(),
            &unlocked_vault_contexts(cx),
            &vault_names,
        );
        if let Some(session) = self.session.as_ref().filter(|s| s.is_active()) {
            for entry in &mut entries {
                entry.current = entry.name == session.scope().context;
            }
        }
        entries
    }

    /// An HTTP agent chose `context` (#421). Settings and the vault are
    /// checked first; then the user is asked in the window, and only a yes
    /// starts the session (with the context's minimal access, until Kubach
    /// quits, at most 8 h). It replaces the previous HTTP session.
    fn use_context(
        &mut self,
        context: String,
        client: String,
        reply: oneshot::Sender<Result<McpServer, String>>,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = AppSettings::try_global(cx) else {
            reply.send(Err("Kubach has no settings".into())).ok();
            return;
        };
        let app_settings = settings.read(cx).get().clone();
        let imported = crate::vault::imported(cx);
        let vault_names: Vec<String> = imported
            .iter()
            .flat_map(|entry| entry.contexts.iter().cloned())
            .collect();
        let unlocked = unlocked_vault_contexts(cx);
        let contexts = Contexts::load_all(
            kubach_core::kubeconfig::sources_with(
                &app_settings.clusters.resolved_kubeconfig_paths(),
            ),
            imported,
        )
        .ok();
        let (server_url, in_vault) = match check_context_choice(
            &app_settings,
            contexts.as_ref(),
            &vault_names,
            &unlocked,
            &context,
        ) {
            Ok(found) => found,
            Err(refusal) => {
                reply.send(Err(refusal)).ok();
                return;
            }
        };
        if let Some(at) = self.refused_contexts.get(&context)
            && at.elapsed() < CONTEXT_REFUSAL
        {
            reply
                .send(Err(format!(
                    "the user did not allow working in context `{context}` a few minutes ago; \
                     Kubach does not ask again yet"
                )))
                .ok();
            return;
        }
        let Some(contexts) = contexts else {
            reply.send(Err("cannot read kubeconfig".into())).ok();
            return;
        };
        let stored = app_settings
            .security
            .policy(&ContextKey::new(context.clone(), &server_url));
        let policy = SecurityPolicy::from(&stored);
        let state =
            crate::state::StateStore::try_global(cx).map(|state| state.read(cx).get().clone());
        let kubeconfig = contexts
            .get(&context)
            .and_then(|info| info.namespace.clone());
        let spec = SessionSpec {
            hints: cli::namespace_hints(
                &app_settings,
                &context,
                kubeconfig.as_deref(),
                state.as_ref(),
            ),
            context: context.clone(),
            server_url: server_url.clone(),
            namespaces: Vec::new(),
            // Not narrowed: more than the minimum is asked for (#15).
            profile: Profile::Deploy,
            ttl: None,
        };
        let helm = helm_env(&app_settings, !in_vault);
        let question = ConfirmationRequest {
            kind: RequestKind::UseContext {
                server_url,
                source: if in_vault { "vault" } else { "kubeconfig" }.into(),
            },
            session_id: String::new(),
            context: context.clone(),
            client: client.clone(),
            tool: "use_context".into(),
            namespace: String::new(),
            summary: t!(
                "agent-use-context-summary",
                client = client.as_str(),
                context = context.as_str()
            ),
            details: t!(
                "agent-use-context-details",
                context = context.as_str(),
                access = access_label(policy.minimum)
            ),
            dangerous: Vec::new(),
            mode: SessionMode::ClientOnly,
            allow_remember: false,
        };
        let confirmer = self.confirmer.clone();
        let task = runtime::spawn(async move {
            let decision = kubach_mcp::confirm::ask(&confirmer, &question).await;
            match decision {
                decision if decision.is_approved() => {}
                ConfirmationDecision::Rejected { .. } => return Err(None),
                other => {
                    return Err(Some(format!(
                        "no answer from the user to working in context `{}`{}",
                        spec.context,
                        other
                            .reason()
                            .map(|reason| format!(" ({reason})"))
                            .unwrap_or_default()
                    )));
                }
            }
            let config = contexts
                .config(&spec.context)
                .await
                .map_err(|error| Some(error.to_string()))?;
            Session::start(&config, &spec, &policy)
                .await
                .map_err(|error| Some(format!("cannot start the session: {error}")))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let answer = match result {
                    Ok(Ok(session))
                        if in_vault
                            && !unlocked_vault_contexts(cx).contains(&session.scope().context) =>
                    {
                        close_all(vec![session]);
                        Err(LOCKED_SESSION.to_owned())
                    }
                    Ok(Ok(session)) => {
                        let server = McpServer::new(
                            session.clone(),
                            Arc::new(this.confirmer.clone()),
                            this.audit.clone(),
                        );
                        server.set_helm(helm);
                        // The agent's forwards show on the "Port
                        // forwards" screen (#17).
                        if let Some(forwards) = crate::dock::forwards(cx) {
                            server.set_forward_manager(forwards.read(cx).manager());
                        }
                        // The previous HTTP session ends; the shared
                        // handler serves the new one from now on.
                        close_all(this.session.take().into_iter().collect());
                        this.session = Some(session);
                        this.session_in_vault = in_vault;
                        this.session_error = None;
                        Ok(server)
                    }
                    Ok(Err(None)) => {
                        this.refused_contexts
                            .insert(context.clone(), Instant::now());
                        Err(format!(
                            "the user did not allow working in context `{context}`"
                        ))
                    }
                    Ok(Err(Some(error))) => Err(error),
                    Err(error) => Err(error.to_string()),
                };
                reply.send(answer).ok();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ends the app's session; the agent's next call is told `why` (#56).
    fn end_app_session(&mut self, why: &str) {
        self.shared.end(why);
        self.session_in_vault = false;
        close_all(self.session.take().into_iter().collect());
    }

    /// Starts a session on a vault context for a `kubach mcp` process, if
    /// the vault is unlocked, the context's settings allow it and the user
    /// approves it in the window (#191). The credentials stay in this
    /// process; the agent's calls come over the bridge.
    fn host(
        &mut self,
        request: HostRequest,
        reply: oneshot::Sender<Result<HostReply, HostRefusal>>,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = AppSettings::try_global(cx) else {
            reply
                .send(Err(HostRefusal::denied("Kubach has no settings")))
                .ok();
            return;
        };
        let settings = settings.read(cx).get();
        let imported = crate::vault::imported(cx);
        let vault_names: Vec<String> = imported
            .iter()
            .flat_map(|entry| entry.contexts.iter().cloned())
            .collect();
        let unlocked = unlocked_vault_contexts(cx);
        let contexts = Contexts::load_all(
            kubach_core::kubeconfig::sources_with(&settings.clusters.resolved_kubeconfig_paths()),
            imported,
        )
        .ok();
        let state =
            crate::state::StateStore::try_global(cx).map(|state| state.read(cx).get().clone());
        let plan = match hosted::plan(
            &request,
            settings,
            contexts.as_ref(),
            &vault_names,
            &unlocked,
            state.as_ref(),
        ) {
            Ok(plan) => plan,
            Err(refusal) => {
                reply.send(Err(refusal)).ok();
                return;
            }
        };
        let Some(contexts) = contexts else {
            return;
        };
        let confirmer: Arc<dyn Confirmer> = Arc::new(self.confirmer.clone());
        let audit = self.audit.clone();
        // A vault context is in no kubeconfig file: Helm works there only
        // under a server-side identity.
        let helm =
            AppSettings::try_global(cx).map(|settings| helm_env(settings.read(cx).get(), false));
        let pid = request.pid;
        // The agent's forwards run here and show on the "Port forwards"
        // screen (#17).
        let forward_manager = crate::dock::forwards(cx).map(|forwards| forwards.read(cx).manager());
        let task = runtime::spawn(async move {
            // Who asks, as far as the OS tells: the pid comes from the
            // process itself.
            let process = hosted::describe_process(request.pid);
            let consent = hosted::consent_request(&request, &plan, process.as_deref());
            hosted::after_consent(confirmer.as_ref(), &consent, &plan.spec.context, async {
                let config = contexts
                    .config(&plan.spec.context)
                    .await
                    .map_err(|error| error.to_string())?;
                let session = Session::start(&config, &plan.spec, &plan.policy)
                    .await
                    .map_err(|error| error.to_string())?;
                let server = McpServer::new(session.clone(), confirmer.clone(), audit);
                if let Some(helm) = helm {
                    server.set_helm(helm);
                }
                if let Some(manager) = forward_manager {
                    server.set_forward_manager(manager);
                }
                // `--grant-read` of the process, under the same rules as there.
                if plan.grant_read
                    && let Err(error) = server.grant_read_at_launch().await
                {
                    let _ = session.close().await;
                    return Err(format!("--grant-read: {error}"));
                }
                Ok(server)
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let answer = this.update(cx, |this, cx| {
                let server = match result {
                    Ok(Ok(server)) => server,
                    Ok(Err(refusal)) => return Err(refusal),
                    Err(error) => return Err(HostRefusal::denied(error.to_string())),
                };
                let session = server.session().clone();
                let context = session.scope().context.clone();
                if let Some(gone) = hosted::lock_ends(&context, &unlocked_vault_contexts(cx)) {
                    // The vault locked while the session started.
                    close_all(vec![session]);
                    return Err(HostRefusal {
                        kind: kubach_mcp::bridge::RefusalKind::Locked,
                        message: gone.message,
                    });
                }
                let reply_session = ExternalSession::with_pid(&session, pid);
                let instructions = server.instructions();
                let key = this.hosted.insert(server, pid);
                cx.notify();
                Ok(HostReply {
                    session: reply_session,
                    instructions,
                    notes: session
                        .boundaries()
                        .iter()
                        .map(BoundaryInfo::summary)
                        .collect(),
                    key,
                })
            });
            reply
                .send(answer.unwrap_or_else(|_| Err(HostRefusal::denied("Kubach is closing"))))
                .ok();
        })
        .detach();
    }

    /// Ends sessions on vault contexts that are no longer unlocked.
    fn vault_changed(&mut self, cx: &mut Context<Self>) {
        let unlocked = unlocked_vault_contexts(cx);
        let app_ends = self.session_in_vault
            && self.session.as_ref().is_some_and(|session| {
                hosted::lock_ends(&session.scope().context, &unlocked).is_some()
            });
        if app_ends {
            self.end_app_session(
                "its context is in the vault, which was locked; the user unlocks it",
            );
            self.session_error = Some(LOCKED_SESSION.into());
        }
        let ended = self
            .hosted
            .end_where(|session| hosted::lock_ends(&session.scope().context, &unlocked));
        if app_ends || !ended.is_empty() {
            close_all(ended);
            cx.notify();
        }
    }

    /// Ends session `id` at once (the app's or one it runs for `kubach
    /// mcp`) or on the next heartbeat of its `kubach mcp` process (within a
    /// few seconds).
    pub fn revoke(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.session.as_ref().is_some_and(|s| s.id() == id) {
            self.end_app_session("the user ended it in Kubach");
        } else if let Some(session) = self.hosted.revoke(id) {
            close_all(vec![session]);
        } else {
            let mut external = sync::lock(&self.external);
            if external.live.contains_key(id) {
                external.revoked.insert(id.to_owned());
            }
        }
        cx.notify();
    }

    /// "Ask again" (#30): session `id` forgets every "don't ask again" at
    /// once if the app runs it, else on the next heartbeat of its `kubach
    /// mcp` process (shown as forgotten right away).
    pub fn forget_remembered(&mut self, id: &str, cx: &mut Context<Self>) {
        let session = self
            .session
            .as_ref()
            .filter(|session| session.id() == id)
            .cloned()
            .or_else(|| self.hosted.session(id));
        if let Some(session) = session {
            session.forget_remembered();
        } else {
            let mut external = sync::lock(&self.external);
            if let Some((session, _, _)) = external.live.get_mut(id) {
                session.remembered.clear();
                external.forget_remembered.insert(id.to_owned());
            }
        }
        cx.notify();
    }

    /// Takes back one right of session `id` (#227): at once for sessions the
    /// app runs, with the next heartbeat for a `kubach mcp` process.
    pub fn revoke_grant(&mut self, id: &str, grant: Grant, cx: &mut Context<Self>) {
        let session = self
            .session
            .as_ref()
            .filter(|session| session.id() == id)
            .cloned()
            .or_else(|| self.hosted.session(id));
        if let Some(session) = session {
            let task = runtime::spawn(async move { session.revoke(&grant).await });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    this.grant_error = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(
                            format!(
                                "Kubach no longer allows it, but the session's Role was not \
                                 updated: {error}"
                            )
                            .into(),
                        ),
                        Err(error) => Some(error.to_string().into()),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else {
            let mut external = sync::lock(&self.external);
            if external.live.contains_key(id) {
                let queued = external.revoked_grants.entry(id.to_owned()).or_default();
                if !queued.contains(&grant) {
                    queued.push(grant);
                }
            }
        }
        cx.notify();
    }

    // --------------------------------------------------------- confirmations

    fn push_pending(
        &mut self,
        request: ConfirmationRequest,
        reply: oneshot::Sender<ConfirmationDecision>,
        settings: Option<&settings::Settings>,
    ) {
        if let RequestKind::RevealBlocked { .. } = &request.kind {
            // Not a question: tell the user and refuse at once. Turning
            // revealing on happens in Settings only.
            self.notices.push_back(Notice {
                title: msg!("agent-notice-reveal-off"),
                context: request.context.clone(),
                // The agent's wording, in English (kubach-mcp).
                message: msg!("agent-notice-text", text = request.summary.as_str()),
            });
            reply
                .send(ConfirmationDecision::Denied {
                    reason: Some("revealing Secret values is off for this context".into()),
                })
                .ok();
            return;
        }
        if let RequestKind::UseContext { server_url, .. } = &request.kind
            && let Some(settings) = settings
        {
            let stored = settings
                .security
                .policy(&ContextKey::new(request.context.clone(), server_url));
            let summary = request.summary.as_str();
            let refusal = (!stored.mcp_enabled).then(|| {
                (
                    "AI agent access is off for this context",
                    msg!("agent-notice-mcp-off", summary = summary),
                )
            });
            if let Some((refusal, message)) = refusal {
                self.notices.push_back(Notice {
                    title: msg!("agent-notice-refused"),
                    context: request.context.clone(),
                    message,
                });
                reply
                    .send(ConfirmationDecision::Rejected {
                        reason: Some(refusal.to_owned()),
                    })
                    .ok();
                return;
            }
        }
        if let RequestKind::Access {
            raise, server_url, ..
        } = &request.kind
            && let Some(settings) = settings
        {
            // Allowed by default meanwhile, from another session (#15): the
            // app's own settings decide, and the user is not asked twice.
            let stored = settings
                .security
                .policy(&ContextKey::new(request.context.clone(), server_url));
            if SecurityPolicy::from(&stored).minimum.covers(*raise) {
                reply.send(ConfirmationDecision::Approved).ok();
                return;
            }
        }
        self.next_pending += 1;
        self.pending.push_back(Pending {
            id: self.next_pending,
            request,
            deadline: Instant::now() + CONFIRMATION_TIMEOUT,
            reply: Some(reply),
        });
    }

    /// Answers confirmation `id`. "Allow by default" on an access request
    /// raises the context's minimal access here, with a trace of what, when
    /// and from which session (#15); if the settings cannot be saved the
    /// answer becomes "for this session" and the user is told.
    pub fn resolve(&mut self, id: u64, decision: ConfirmationDecision, cx: &mut Context<Self>) {
        if let Some(ix) = self.pending.iter().position(|p| p.id == id)
            && let Some(mut pending) = self.pending.remove(ix)
            && let Some(reply) = pending.reply.take()
        {
            let decision = match (&decision, &pending.request.kind) {
                (ConfirmationDecision::ApprovedAlways, RequestKind::Access { .. }) => {
                    self.save_raise(&pending.request, cx)
                }
                (ConfirmationDecision::ApprovedAlways, _) => ConfirmationDecision::Approved,
                _ => decision,
            };
            reply.send(decision).ok();
        }
        if self.shown == Some(id) {
            self.shown = None;
        }
        cx.notify();
    }

    /// Raises the minimal access of the request's context in the settings
    /// for good. Returns the answer for the agent's session.
    fn save_raise(
        &mut self,
        request: &ConfirmationRequest,
        cx: &mut Context<Self>,
    ) -> ConfirmationDecision {
        let RequestKind::Access {
            raise, server_url, ..
        } = &request.kind
        else {
            return ConfirmationDecision::Approved;
        };
        let key = ContextKey::new(request.context.clone(), server_url);
        let Some(store) = AppSettings::try_global(cx).filter(|s| s.read(cx).can_save()) else {
            self.notices.push_back(Notice {
                title: msg!("agent-notice-session-only"),
                context: request.context.clone(),
                message: msg!("agent-notice-session-only-text"),
            });
            return ConfirmationDecision::Approved;
        };
        let access = settings::AgentAccess {
            level: raise.profile.into(),
            secrets: raise.secrets,
            crds: raise.crds,
        };
        let time = k8s_openapi::jiff::Timestamp::now().to_string();
        store.update(cx, |store, cx| {
            store.edit(cx, |settings| {
                settings.security.raise_minimum(
                    &key,
                    access,
                    &request.session_id,
                    &request.client,
                    &time,
                );
            })
        });
        ConfirmationDecision::ApprovedAlways
    }

    // ------------------------------------------------------------------ tick

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if self.session.as_ref().is_some_and(|s| !s.is_active()) {
            // TTL: the session closed itself.
            self.end_app_session("its time limit ran out");
            changed = true;
        }
        let swept = self.hosted.sweep(STALE);
        changed |= !swept.is_empty();
        close_all(swept);
        {
            let mut external = sync::lock(&self.external);
            let before = external.live.len();
            external
                .live
                .retain(|_, (_, _, seen)| seen.elapsed() < STALE);
            let External {
                live,
                revoked,
                revoked_grants,
                forget_remembered,
            } = &mut *external;
            revoked.retain(|id| live.contains_key(id));
            revoked_grants.retain(|id, _| live.contains_key(id));
            forget_remembered.retain(|id| live.contains_key(id));
            changed |= external.live.len() != before;
        }
        let now = Instant::now();
        let before = self.pending.len();
        self.pending.retain_mut(|pending| {
            let Some(reply) = &pending.reply else {
                return false;
            };
            if reply.is_canceled() {
                // The call gave up (timeout on the agent side, session end).
                return false;
            }
            if now >= pending.deadline {
                if let Some(reply) = pending.reply.take() {
                    reply
                        .send(ConfirmationDecision::Denied {
                            reason: Some("timed out waiting for the user".into()),
                        })
                        .ok();
                }
                return false;
            }
            true
        });
        changed |= self.pending.len() != before;
        changed |= self.refresh_audit();
        // Countdowns and remaining session time move every second.
        if changed || !self.pending.is_empty() || !self.sessions().is_empty() {
            cx.notify();
        }
    }

    /// Re-reads the audit view when the file changed; returns whether it did.
    fn refresh_audit(&mut self) -> bool {
        let Some(path) = self.audit_path() else {
            return false;
        };
        let len = std::fs::metadata(&path).ok().map(|meta| meta.len());
        if len == self.audit_len {
            return false;
        }
        self.audit_len = len;
        match len {
            None => {
                self.audit_rows.clear();
                self.audit_error = None;
            }
            Some(_) => match read_audit_tail(&path) {
                Ok(rows) => {
                    self.audit_rows = rows;
                    self.audit_error = None;
                }
                Err(error) => self.audit_error = Some(error.to_string().into()),
            },
        }
        true
    }
}

/// Remaining time until `deadline` as `4:59`.
pub fn countdown(deadline: Instant) -> String {
    let left = deadline.saturating_duration_since(Instant::now()).as_secs();
    format!("{}:{:02}", left / 60, left % 60)
}

/// Remaining time until an RFC 3339 `expires_at`: `1 h 5 min`, `40 s`.
pub fn remaining(expires_at: &str) -> String {
    let Ok(at) = expires_at.parse::<k8s_openapi::jiff::Timestamp>() else {
        return String::new();
    };
    let left = at
        .duration_since(k8s_openapi::jiff::Timestamp::now())
        .as_secs()
        .max(0);
    match (left / 3600, (left % 3600) / 60) {
        (0, 0) => kubach_i18n::t!("settings-duration-seconds", seconds = left),
        (0, m) => kubach_i18n::t!("settings-duration-minutes", minutes = m),
        (h, m) => kubach_i18n::t!("settings-duration-hours-minutes", hours = h, minutes = m),
    }
}

/// `2026-10-22T10:00:00.123Z` → `10:00:00`, for compact lists.
pub fn clock(time: &str) -> String {
    time.split('T')
        .nth(1)
        .map(|rest| rest.chars().take(8).collect())
        .unwrap_or_else(|| time.to_owned())
}

/// An access level (#15) in the interface language, named like the choices
/// of Settings → Security.
pub fn level_label(profile: Profile) -> String {
    match profile {
        Profile::ReadOnly => t!("settings-minimum-read"),
        Profile::Debug => t!("settings-minimum-debug"),
        Profile::Deploy => t!("settings-minimum-changes"),
    }
}

/// What `access` covers, in the interface language: the level, plus
/// Secrets and custom resources when it has them.
pub fn access_label(access: kubach_mcp::Access) -> String {
    let mut parts = vec![level_label(access.profile)];
    if access.secrets {
        parts.push(t!("agent-access-secrets"));
    }
    if access.crds {
        parts.push(t!("agent-access-crds"));
    }
    parts.join(" + ")
}

/// A kind of change the user may say not to ask about again (#30).
pub fn remembered_action_label(action: kubach_mcp::RememberedAction) -> String {
    use kubach_mcp::RememberedAction;
    match action {
        RememberedAction::Scale => t!("agent-action-scale"),
        RememberedAction::RolloutRestart => t!("agent-action-rollout-restart"),
        RememberedAction::ApplyManifest => t!("agent-action-apply"),
        RememberedAction::Exec => t!("agent-action-exec"),
        RememberedAction::PortForwardStart => t!("agent-action-port-forward"),
        RememberedAction::HelmInstall => t!("agent-action-helm-install"),
        RememberedAction::HelmUpgrade => t!("agent-action-helm-upgrade"),
        RememberedAction::HelmRollback => t!("agent-action-helm-rollback"),
    }
}

/// What an access request asks for: only the parts beyond what the
/// session has, so a request for Secrets alone does not read as "Read".
pub fn raise_label(raise: kubach_mcp::Access) -> String {
    let mut parts = Vec::new();
    if raise.profile > Profile::ReadOnly {
        parts.push(level_label(raise.profile));
    }
    if raise.secrets {
        parts.push(t!("agent-access-secrets"));
    }
    if raise.crds {
        parts.push(t!("agent-access-crds"));
    }
    parts.join(" + ")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures::channel::mpsc;
    use kubach_mcp::bridge::{BridgeHandler, ExternalSession};
    use kubach_mcp::hosted::HostedSessions;
    use kubach_mcp::{Profile, SessionMode};

    use super::{
        AppBridge, AuditRow, External, MAX_PENDING_PER_SESSION, UiConfirmer, WaitingCount, clock,
        read_audit_tail,
    };

    fn external_session(id: &str) -> ExternalSession {
        ExternalSession {
            raised: None,
            id: id.into(),
            pid: 7,
            context: "minikube".into(),
            namespaces: vec!["web".into()],
            profile: Profile::ReadOnly,
            mode: SessionMode::ClientOnly,
            boundaries: Vec::new(),
            started_at: String::new(),
            expires_at: String::new(),
            clients: Vec::new(),
            calls: 0,
            grants: Vec::new(),
            remembered: Vec::new(),
        }
    }

    /// A `kubach mcp` process cannot end another's session nor lift its
    /// revocation by knowing the id (#191).
    #[test]
    fn only_the_sessions_process_reports_or_ends_it() {
        let (tx, _rx) = mpsc::unbounded();
        let external = Arc::new(Mutex::new(External::default()));
        let app = AppBridge {
            confirmer: UiConfirmer {
                tx,
                waiting: WaitingCount::default(),
            },
            external: external.clone(),
            hosted: HostedSessions::new(),
        };
        assert!(app.heartbeat(external_session("s"), "mine").is_some());
        external.lock().unwrap().revoked.insert("s".into());

        // Someone else with the id: refused, nothing changes.
        assert!(app.heartbeat(external_session("s"), "theirs").is_none());
        assert!(!app.ended("s", "theirs"));
        {
            let external = external.lock().unwrap();
            assert!(external.live.contains_key("s"));
            assert!(external.revoked.contains("s"));
        }

        // The process itself learns of the revocation and ends it.
        assert!(
            app.heartbeat(external_session("s"), "mine")
                .unwrap()
                .revoked
        );
        assert!(app.ended("s", "mine"));
        let external = external.lock().unwrap();
        assert!(external.live.is_empty() && external.revoked.is_empty());
    }

    /// "Ask again" reaches the process with its next heartbeat, once, and
    /// the list is shown empty meanwhile (#30).
    #[test]
    fn ask_again_reaches_the_process() {
        let (tx, _rx) = mpsc::unbounded();
        let external = Arc::new(Mutex::new(External::default()));
        let app = AppBridge {
            confirmer: UiConfirmer {
                tx,
                waiting: WaitingCount::default(),
            },
            external: external.clone(),
            hosted: HostedSessions::new(),
        };
        let mut session = external_session("s");
        session.remembered = vec![kubach_mcp::Remembered {
            action: kubach_mcp::RememberedAction::Scale,
            namespace: "web".into(),
        }];
        let reply = app.heartbeat(session.clone(), "mine").unwrap();
        assert!(!reply.forget_remembered);
        external
            .lock()
            .unwrap()
            .forget_remembered
            .insert("s".into());
        assert!(
            app.heartbeat(session.clone(), "mine")
                .unwrap()
                .forget_remembered
        );
        assert!(external.lock().unwrap().live["s"].0.remembered.is_empty());
        // Told once; the process reports what it remembers from then on.
        assert!(!app.heartbeat(session, "mine").unwrap().forget_remembered);
    }

    #[test]
    fn waiting_confirmations_are_bounded_per_session() {
        let waiting = WaitingCount::default();
        let mut slots: Vec<_> = (0..MAX_PENDING_PER_SESSION)
            .map(|_| waiting.take("a").expect("below the limit"))
            .collect();
        assert!(waiting.take("a").is_none());
        // Other sessions have their own limit.
        let other = waiting.take("b");
        assert!(other.is_some());
        // An answered confirmation frees its place.
        slots.pop();
        assert!(waiting.take("a").is_some());
        drop(slots);
        drop(other);
        assert!(waiting.0.lock().unwrap().is_empty());
    }

    #[test]
    fn audit_rows_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let line = r#"{"time":"2026-10-22T10:00:00Z","session":"s","client":"claude-code","tool":"list_resources","args":{"kind":"Pod"},"decided_by":"none","allowed":true,"identity":"user","result":"ok","duration_ms":3}"#;
        std::fs::write(&path, format!("{line}\nnot json\n{line}\n")).unwrap();
        let rows = read_audit_tail(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool, "list_resources");
        assert_eq!(rows[0].args, r#"{"kind":"Pod"}"#);
        assert!(rows[0].allowed);
        assert_eq!(AuditRow::parse("{}").unwrap().tool, "");
        assert_eq!(clock("2026-10-22T10:00:00.5Z"), "10:00:00");
    }

    const KUBECONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: dev
clusters:
- name: dev
  cluster: { server: "https://127.0.0.1:6443" }
- name: prod
  cluster: { server: "https://prod.example.com" }
contexts:
- name: dev
  context: { cluster: dev, user: dev }
- name: prod
  context: { cluster: prod, user: dev }
users:
- name: dev
  user: { token: secret-token }
"#;

    #[test]
    fn the_agent_may_choose_only_allowed_contexts() {
        use kubach_core::Contexts;
        use kubach_core::settings::{ContextKey, ContextPolicy, Settings};
        use std::collections::HashSet;

        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        let mut settings = Settings::default();
        let none = HashSet::new();
        let choose = |settings: &Settings, name: &str| {
            super::check_context_choice(
                settings,
                Some(&contexts),
                &["vaulted".to_owned()],
                &none,
                name,
            )
        };
        let dev = ContextKey::new("dev", "https://127.0.0.1:6443");
        // AI agent access is on by default (#9); turned off, the choice is refused.
        settings.security.set_policy(
            &dev,
            ContextPolicy {
                mcp_enabled: false,
                ..ContextPolicy::default()
            },
        );
        let error = choose(&settings, "dev").unwrap_err();
        assert!(error.contains("AI agent access is off"), "{error}");
        settings.security.set_policy(&dev, ContextPolicy::default());
        assert_eq!(
            choose(&settings, "dev").unwrap(),
            ("https://127.0.0.1:6443".to_owned(), false)
        );
        // Settings written before #15 with `protected = true`: ignored.
        let old: kubach_core::settings::SecuritySettings = serde_json::from_value(
            serde_json::json!({"context": [{"context": "prod", "server": "https://prod.example.com",
                "protected": true, "max_profile": "deploy"}]}),
        )
        .unwrap();

        settings.security = old;
        assert!(choose(&settings, "prod").is_ok());
        let error = choose(&settings, "vaulted").unwrap_err();
        assert!(error.contains("vault, which is locked"), "{error}");
        let error = choose(&settings, "nope").unwrap_err();
        assert!(error.contains("neither in kubeconfig"), "{error}");

        let entries =
            super::context_entries(&settings, Some(&contexts), &none, &["vaulted".to_owned()]);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["dev", "prod", "vaulted"]);
        assert!(entries[0].agent_access && entries[1].agent_access);
        assert!(entries[2].unavailable.is_some());

        // Names and servers only: no credentials.
        let json = serde_json::to_string(&entries).unwrap();
        assert!(!json.contains("secret-token"), "{json}");
    }
}
