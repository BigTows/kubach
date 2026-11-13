//! `kubach mcp` for a context imported into the encrypted vault (#121, ADR
//! 0008): the process serves MCP over stdio but holds no session of its
//! own. The app runs it ([`crate::hosted`]), and the process relays the
//! agent's calls over the bridge, so the vault's credentials never reach
//! the process.
//!
//! While the app is not running or the vault is locked, the agent sees one
//! `session_info` tool that explains what to do; once a session starts, the
//! process tells the agent the tool list changed. When the vault locks, the
//! next call explains it, and the one after unlocking starts a new session
//! (the app checks the vault and the settings again). A revoked or expired
//! session stays ended.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::bridge::{BridgeClient, BridgeError, HostReply, HostRequest, Locate, RefusalKind};
use crate::confirm::{notice_if_slow, waiting_text};
use crate::hosted::Gone;
use crate::server::{CallOutcome, client_name, progress_of, stops_on_cancel};
use crate::tools::Progress;
use kubach_core::sync;

/// How often a relayed call asks the app whether it waits for the user,
/// when the agent wants progress notifications (#116).
const WAITING_POLL: Duration = Duration::from_millis(500);

/// Why no session could be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotConnected {
    pub message: String,
    /// Whether it may work later without changing anything but the app's
    /// state: the app starts, the vault is unlocked.
    pub may_retry: bool,
}

#[derive(Default)]
struct State {
    /// The hosted session.
    id: Option<String>,
    /// Its key, issued by the app.
    key: String,
    /// The user declined the session in the app; not asked again.
    refused: Option<String>,
    /// How the last session ended.
    ended: Option<Gone>,
    /// The agent was offered only the placeholder tool.
    placeholder: bool,
}

struct Inner {
    locate: Locate,
    request: HostRequest,
    /// Held while a session is being started, so only one starts.
    state: tokio::sync::Mutex<State>,
    instructions: Mutex<String>,
}

/// MCP handler relaying to a session hosted by the app. Cheap to clone.
#[derive(Clone)]
pub struct ProxyServer {
    inner: Arc<Inner>,
}

impl ProxyServer {
    /// `locate` finds the app (read on every use); `request` is what to ask
    /// it to host.
    pub fn new(locate: Locate, request: HostRequest) -> Self {
        let instructions = format!(
            "Kubach gives access to one Kubernetes namespace of context `{}`, which is \
             imported into the Kubach vault: it works while the Kubach app runs and the vault \
             is unlocked. Call `session_info` for details.",
            request.context
        );
        Self {
            inner: Arc::new(Inner {
                locate,
                request,
                state: tokio::sync::Mutex::new(State::default()),
                instructions: Mutex::new(instructions),
            }),
        }
    }

    /// The vault context the session is asked for.
    pub fn context(&self) -> &str {
        &self.inner.request.context
    }

    /// What the agent is told about the session: the app's text once one
    /// runs.
    pub fn instructions(&self) -> String {
        sync::lock(&self.inner.instructions).clone()
    }

    fn no_app(&self) -> String {
        format!(
            "Context `{}` is imported into the Kubach vault, so its credentials stay in the \
             Kubach app. Ask the user to start Kubach, turn on Settings → AI agent → HTTP \
             server and unlock the vault, then retry.",
            self.context()
        )
    }

    fn unreachable(&self, error: BridgeError) -> NotConnected {
        match error {
            BridgeError::Unreachable(_) => NotConnected {
                message: self.no_app(),
                may_retry: true,
            },
            error => NotConnected {
                message: format!("cannot use the Kubach app: {error}"),
                may_retry: true,
            },
        }
    }

    /// Why no session may be asked for now, if so.
    fn blocked(state: &State) -> Option<NotConnected> {
        if let Some(message) = &state.refused {
            return Some(NotConnected {
                message: message.clone(),
                may_retry: false,
            });
        }
        state
            .ended
            .as_ref()
            .filter(|g| !g.reason.may_restart())
            .map(|gone| NotConnected {
                message: gone.message.clone(),
                may_retry: false,
            })
    }

    /// Asks the app for a session; it asks the user in its window first,
    /// so this may wait for their answer.
    async fn start(
        &self,
        client: &BridgeClient,
        state: &mut State,
    ) -> Result<HostReply, NotConnected> {
        match client.host(&self.inner.request).await {
            Ok(Ok(reply)) => {
                state.id = Some(reply.session.id.clone());
                state.key = reply.key.clone();
                state.ended = None;
                *sync::lock(&self.inner.instructions) = reply.instructions.clone();
                Ok(reply)
            }
            Ok(Err(refusal)) => {
                if refusal.kind == RefusalKind::Rejected {
                    state.refused = Some(refusal.message.clone());
                }
                Err(NotConnected {
                    message: refusal.message,
                    may_retry: refusal.kind == RefusalKind::Locked,
                })
            }
            Err(error) => Err(self.unreachable(error)),
        }
    }

    /// Starts a session in the app unless one runs. Returns the app, the
    /// session id and key.
    async fn ensure(&self) -> Result<(BridgeClient, String, String), NotConnected> {
        let client = (self.inner.locate)().ok_or_else(|| NotConnected {
            message: self.no_app(),
            may_retry: true,
        })?;
        let mut state = self.inner.state.lock().await;
        if let Some(id) = &state.id {
            return Ok((client, id.clone(), state.key.clone()));
        }
        if let Some(not) = Self::blocked(&state) {
            return Err(not);
        }
        let reply = self.start(&client, &mut state).await?;
        Ok((client, reply.session.id, reply.key))
    }

    /// Starts the session now, to report it on start-up. `None` when one
    /// already runs.
    pub async fn connect(&self) -> Result<Option<HostReply>, NotConnected> {
        let client = (self.inner.locate)().ok_or_else(|| NotConnected {
            message: self.no_app(),
            may_retry: true,
        })?;
        let mut state = self.inner.state.lock().await;
        if state.id.is_some() {
            return Ok(None);
        }
        if let Some(not) = Self::blocked(&state) {
            return Err(not);
        }
        self.start(&client, &mut state).await.map(Some)
    }

    /// The running session, without waiting for one being started.
    fn current(&self) -> Option<(String, String)> {
        let state = self.inner.state.try_lock().ok()?;
        state.id.clone().map(|id| (id, state.key.clone()))
    }

    /// Forgets session `id`, which ended for `gone`.
    async fn ended(&self, id: &str, gone: &Gone) {
        let mut state = self.inner.state.lock().await;
        if state.id.as_deref() == Some(id) {
            state.id = None;
        }
        state.ended = Some(gone.clone());
    }

    /// The tools of the session; just the placeholder without one. Never
    /// starts a session: that needs the user's answer in the app, and the
    /// agent's first call waits for it, then learns the tool list changed.
    pub async fn tools(&self) -> Vec<Tool> {
        if let (Some((id, key)), Some(client)) = (self.current(), (self.inner.locate)()) {
            match client.hosted_tools(&id, &key).await {
                Ok(Ok(tools)) => return tools,
                Ok(Err(gone)) => self.ended(&id, &gone).await,
                Err(_) => {}
            }
        }
        self.inner.state.lock().await.placeholder = true;
        vec![placeholder_tool()]
    }

    /// Runs a tool call in the session, starting one if needed. The `bool`
    /// tells whether the agent should be told the tool list changed.
    pub async fn call(
        &self,
        client_name: &str,
        tool: &str,
        args: Map<String, Value>,
    ) -> (CallOutcome, bool) {
        self.call_with_progress(client_name, tool, args, None).await
    }

    /// [`Self::call`], telling `progress` when the call waits for the user.
    pub(crate) async fn call_with_progress(
        &self,
        client_name: &str,
        tool: &str,
        args: Map<String, Value>,
        progress: Option<Progress>,
    ) -> (CallOutcome, bool) {
        let error = |text: String| CallOutcome {
            text,
            is_error: true,
        };
        let ensured = notice_if_slow(self.ensure(), || {
            if let Some(progress) = &progress {
                progress(waiting_text(&format!(
                    "a session on context {}",
                    self.context()
                )));
            }
        })
        .await;
        let (client, id, key) = match ensured {
            Ok(found) => found,
            Err(not) => {
                let text = format!(
                    "No Kubach session on context `{}`: {}",
                    self.context(),
                    not.message
                );
                return (error(text), false);
            }
        };
        // The agent knew only the placeholder: it learns of the real tools.
        let list_changed = std::mem::take(&mut self.inner.state.lock().await.placeholder);
        let call = client.hosted_call(&id, &key, client_name, tool, args);
        let result = match &progress {
            Some(progress) => {
                tokio::select! {
                    result = call => result,
                    result = relay_waits(&client, &id, &key, tool, progress) => result,
                }
            }
            None => call.await,
        };
        let outcome = match result {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(gone)) => {
                self.ended(&id, &gone).await;
                error(gone.message)
            }
            Err(error_) => error(self.unreachable(error_).message),
        };
        (outcome, list_changed)
    }

    /// [`Self::call_with_progress`], dropping the relayed request when `cancel` fires on a
    /// tool that stops then: the app ends the call on that (#109).
    pub(crate) async fn call_cancellable(
        &self,
        client_name: &str,
        tool: &str,
        args: Map<String, Value>,
        progress: Option<Progress>,
        cancel: &CancellationToken,
    ) -> (CallOutcome, bool) {
        if !stops_on_cancel(tool) {
            return self
                .call_with_progress(client_name, tool, args, progress)
                .await;
        }
        tokio::select! {
            result = self.call_with_progress(client_name, tool, args, progress) => result,
            () = cancel.cancelled() => {
                let text = "the call was cancelled".to_owned();
                (CallOutcome { text, is_error: true }, false)
            }
        }
    }

    /// Tells the app the process is alive. Returns how the session ended
    /// if the app ended it since the last ping.
    pub async fn ping(&self) -> Option<Gone> {
        let (id, key) = {
            let state = self.inner.state.lock().await;
            (state.id.clone()?, state.key.clone())
        };
        let client = (self.inner.locate)()?;
        match client.hosted_ping(&id, &key).await {
            Ok(Err(gone)) => {
                self.ended(&id, &gone).await;
                Some(gone)
            }
            _ => None,
        }
    }

    /// Ends the session in the app (the agent disconnected).
    pub async fn end(&self) {
        let (id, key) = {
            let mut state = self.inner.state.lock().await;
            let Some(id) = state.id.take() else {
                return;
            };
            (id, std::mem::take(&mut state.key))
        };
        if let Some(client) = (self.inner.locate)() {
            let _ = client.hosted_end(&id, &key).await;
        }
    }

    /// Id of the running session.
    pub async fn session_id(&self) -> Option<String> {
        self.inner.state.lock().await.id.clone()
    }
}

/// Tells `progress` of each wait for the user of a `tool` call in the
/// hosted session, while the call runs in the app (#116). Never ends: it is
/// dropped with the call, and just stops asking an app that cannot tell.
async fn relay_waits<T>(
    client: &BridgeClient,
    id: &str,
    key: &str,
    tool: &str,
    progress: &Progress,
) -> T {
    let mut told = HashSet::new();
    loop {
        tokio::time::sleep(WAITING_POLL).await;
        let Ok(Ok(calls)) = client.hosted_waiting(id, key).await else {
            return std::future::pending().await;
        };
        for call in calls {
            if call.tool == tool && told.insert(call.id) {
                progress(waiting_text(&call.summary));
            }
        }
    }
}

/// Offered while there is no session; calling it explains why.
fn placeholder_tool() -> Tool {
    let schema = serde_json::json!({"type": "object", "properties": {}});
    let Value::Object(schema) = schema else {
        unreachable!()
    };
    Tool::new(
        "session_info",
        "The Kubach session: its namespace, profile and limits, or why there is none yet.",
        Arc::new(schema),
    )
    .annotate(ToolAnnotations::new().read_only(true).destructive(false))
}

impl ServerHandler for ProxyServer {
    fn get_info(&self) -> ServerConfig {
        let instructions = self.instructions();
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("kubach", kubach_core::VERSION))
        .with_instructions(instructions)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools().await))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = client_name(&context);
        let progress = progress_of(&context);
        let (outcome, list_changed) = self
            .call_cancellable(
                &client,
                &request.name,
                request.arguments.unwrap_or_default(),
                progress,
                &context.ct,
            )
            .await;
        if list_changed {
            let _ = context.peer.notify_tool_list_changed().await;
        }
        let content = vec![ContentBlock::text(outcome.text)];
        Ok(if outcome.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use kubach_core::plugin::BoxFuture;
    use kubach_core::sync;
    use serde_json::Map;

    use super::{Progress, ProxyServer, WAITING_POLL};
    use crate::bridge::{
        BridgeClient, BridgeHandler, ExternalSession, HostRefusal, HostReply, HostRequest,
    };
    use crate::confirm::{ConfirmationDecision, ConfirmationRequest, Confirmer, DenyAll};
    use crate::hosted::{EndReason, Gone, HostedSessions};
    use crate::identity::Identity;
    use crate::server::McpServer;
    use crate::session::Session;
    use crate::settings::Profile;
    use crate::shared::{ChosenSession, ContextChooser, ContextEntry, SharedServer};
    use crate::testutil::{Gate, catalog, permissive, scope_with};
    use crate::transport::{BridgeOptions, HttpEndpoint, HttpOptions, serve_shared_http};

    const CONTEXT: &str = "prod-imported";

    /// The app's side: hosts sessions only while "unlocked" and, as the
    /// user would answer, `approve`d.
    struct FakeApp {
        unlocked: Arc<AtomicBool>,
        approve: Arc<AtomicBool>,
        asked: Arc<AtomicUsize>,
        hosted: HostedSessions,
        audit: Option<Arc<crate::audit::AuditLog>>,
        /// Answers the questions of hosted sessions.
        confirmer: Arc<dyn Confirmer>,
    }

    impl FakeApp {
        fn new(unlocked: bool) -> Self {
            Self {
                unlocked: Arc::new(AtomicBool::new(unlocked)),
                approve: Arc::new(AtomicBool::new(true)),
                asked: Arc::default(),
                hosted: HostedSessions::new(),
                audit: None,
                confirmer: Arc::new(DenyAll),
            }
        }
    }

    impl BridgeHandler for FakeApp {
        fn confirm(&self, _: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision> {
            Box::pin(async { ConfirmationDecision::Denied { reason: None } })
        }

        fn heartbeat(&self, _: ExternalSession, _: &str) -> Option<crate::bridge::HeartbeatReply> {
            Some(crate::bridge::HeartbeatReply::default())
        }

        fn ended(&self, _: &str, _: &str) -> bool {
            true
        }

        fn host(&self, request: HostRequest) -> BoxFuture<'static, Result<HostReply, HostRefusal>> {
            let unlocked = self.unlocked.load(Ordering::SeqCst);
            let approve = self.approve.load(Ordering::SeqCst);
            let asked = self.asked.clone();
            let hosted = self.hosted.clone();
            let audit = self.audit.clone();
            let confirmer = self.confirmer.clone();
            Box::pin(async move {
                if !unlocked {
                    return Err(HostRefusal::locked(&request.context));
                }
                if request.context != CONTEXT {
                    return Err(HostRefusal::denied("unknown context"));
                }
                asked.fetch_add(1, Ordering::SeqCst);
                if !approve {
                    return Err(HostRefusal::rejected(&request.context));
                }
                let mut scope = scope_with(Profile::ReadOnly, &permissive(), &["web"]);
                scope.context = request.context.clone();
                let config = kube::Config::new("http://127.0.0.1:9".parse().unwrap());
                let client = kube::Client::try_from(config).unwrap();
                let session = Session::from_parts(
                    scope,
                    Identity::ClientOnly { client },
                    catalog(),
                    HashSet::new(),
                );
                let server = McpServer::new(session.clone(), confirmer, audit);
                let session_info = ExternalSession::with_pid(&session, request.pid);
                let instructions = server.instructions();
                let key = hosted.insert(server, request.pid);
                Ok(HostReply {
                    session: session_info,
                    instructions,
                    notes: Vec::new(),
                    key,
                })
            })
        }

        fn hosted(&self) -> Option<HostedSessions> {
            Some(self.hosted.clone())
        }
    }

    fn request(context: &str) -> HostRequest {
        HostRequest {
            pid: 7,
            context: context.into(),
            namespaces: Vec::new(),
            profile: None,
            ttl_secs: None,
            grant_read: false,
        }
    }

    async fn serve(app: Arc<FakeApp>) -> (HttpEndpoint, u16) {
        let endpoint = serve_shared_http(
            SharedServer::new(),
            HttpOptions {
                port: 0,
                token: "m".repeat(64),
                bridge: Some(BridgeOptions {
                    handler: app,
                    key: "b".repeat(64),
                }),
            },
        )
        .await
        .unwrap();
        let port = endpoint.addr.port();
        (endpoint, port)
    }

    fn proxy(port: u16, context: &str) -> ProxyServer {
        ProxyServer::new(
            Box::new(move || Some(BridgeClient::new(port, "b".repeat(64)))),
            request(context),
        )
    }

    /// The app runs the session, yet the agent of the process hears of a
    /// call waiting for the user, once; without a progress token it sees
    /// the wait in `session_info` (#116).
    #[tokio::test]
    async fn a_hosted_call_waiting_for_the_user_is_relayed() {
        let gate = Arc::new(Gate::new());
        let mut app = FakeApp::new(true);
        app.confirmer = gate.clone();
        let (endpoint, port) = serve(Arc::new(app)).await;
        let proxy = proxy(port, CONTEXT);
        proxy.connect().await.unwrap();

        let told: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = told.clone();
        let progress: Progress = Arc::new(move |text| sync::lock(&sink).push(text));
        let call = {
            let proxy = proxy.clone();
            tokio::spawn(async move {
                // Beyond the session's access, so the user is asked.
                let args = serde_json::json!({"pod": "web-0"});
                let args = args.as_object().unwrap().clone();
                proxy
                    .call_with_progress("agent", "get_logs", args, Some(progress))
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while sync::lock(&told).is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the agent is told of the wait");
        assert!(sync::lock(&told)[0].contains("Kubach window"));

        let (info, _) = proxy.call("agent", "session_info", Map::new()).await;
        let info: serde_json::Value = serde_json::from_str(&info.text).unwrap();
        assert_eq!(info["waitingForUser"][0]["tool"], "get_logs");

        // Polled again meanwhile, it is still one notice.
        tokio::time::sleep(WAITING_POLL * 2).await;
        assert_eq!(sync::lock(&told).len(), 1);

        gate.release();
        let (outcome, _) = call.await.unwrap();
        assert!(outcome.is_error, "{}", outcome.text);
        let (info, _) = proxy.call("agent", "session_info", Map::new()).await;
        let info: serde_json::Value = serde_json::from_str(&info.text).unwrap();
        assert_eq!(info["waitingForUser"], serde_json::json!([]));
        proxy.end().await;
        endpoint.shutdown().await;
    }

    /// A cancelled `wait_for_condition` stops in the app too, through the
    /// real bridge: nothing but the dropped request tells it (#109).
    #[tokio::test]
    async fn cancelling_a_relayed_wait_stops_it_in_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut app = FakeApp::new(true);
        app.audit = Some(Arc::new(crate::audit::AuditLog::open(&path).unwrap()));
        let (endpoint, port) = serve(Arc::new(app)).await;
        let proxy = proxy(port, CONTEXT);
        proxy.connect().await.unwrap();

        let cancel = tokio_util::sync::CancellationToken::new();
        let call = {
            let (proxy, cancel) = (proxy.clone(), cancel.clone());
            tokio::spawn(async move {
                let args = serde_json::json!({"kind": "Pod", "name": "web-0", "namespace": "web",
                                              "condition": "exists", "timeoutSeconds": 60});
                let args = args.as_object().unwrap().clone();
                proxy
                    .call_cancellable("agent", "wait_for_condition", args, None, &cancel)
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel.cancel();
        let (outcome, _) = call.await.unwrap();
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(outcome.text.contains("cancelled"), "{}", outcome.text);

        let audited = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let lines = std::fs::read_to_string(&path).unwrap();
                if let Some(line) = lines.lines().find(|l| l.contains("was cancelled")) {
                    return line.to_owned();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the app ended the call");
        assert!(audited.contains("wait_for_condition"), "{audited}");
        proxy.end().await;
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn relays_to_the_app_and_follows_the_vault() {
        let app = Arc::new(FakeApp::new(false));
        let (unlocked, hosted) = (app.unlocked.clone(), app.hosted.clone());
        let (endpoint, port) = serve(app).await;
        let proxy = proxy(port, CONTEXT);

        // Locked: nothing hosted, one explaining tool, retry allowed.
        let not = proxy.connect().await.unwrap_err();
        assert!(not.may_retry);
        assert!(not.message.contains("locked"), "{}", not.message);
        let tools = proxy.tools().await;
        assert_eq!(tools.len(), 1);
        let (outcome, changed) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(
            outcome.is_error && outcome.text.contains("unlock"),
            "{}",
            outcome.text
        );
        assert!(!changed);
        assert!(hosted.is_empty());

        // Unlocked: the next call starts a session in the app.
        unlocked.store(true, Ordering::SeqCst);
        let (outcome, changed) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(changed, "the agent only knew the placeholder");
        let id = proxy.session_id().await.unwrap();
        assert!(hosted.contains(&id));
        assert_eq!(hosted.list()[0].pid, 7);
        assert!(proxy.tools().await.len() > 1);
        assert_eq!(proxy.ping().await, None);

        // Locking ends it in the app; the process learns on its next ping.
        unlocked.store(false, Ordering::SeqCst);
        let ended = hosted.end_where(|s| Some(Gone::locked(&s.scope().context)));
        assert_eq!(ended.len(), 1);
        assert_eq!(proxy.ping().await.unwrap().reason, EndReason::Locked);
        assert_eq!(proxy.session_id().await, None);
        let (outcome, _) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(
            outcome.is_error && outcome.text.contains("locked"),
            "{}",
            outcome.text
        );

        // Unlocked again: a fresh session, checked by the app again.
        unlocked.store(true, Ordering::SeqCst);
        let (outcome, _) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(!outcome.is_error, "{}", outcome.text);
        let second = proxy.session_id().await.unwrap();
        assert_ne!(second, id);

        // Revoked: final, even though the vault is unlocked.
        hosted.revoke(&second).unwrap();
        let (outcome, _) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(
            outcome.is_error && outcome.text.contains("revoked"),
            "{}",
            outcome.text
        );
        let (outcome, _) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(
            outcome.is_error && outcome.text.contains("revoked"),
            "{}",
            outcome.text
        );
        assert!(hosted.is_empty());

        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn without_the_app_nothing_is_served() {
        let proxy = ProxyServer::new(Box::new(|| None), request(CONTEXT));
        let not = proxy.connect().await.unwrap_err();
        assert!(not.may_retry);
        assert!(not.message.contains("start Kubach"), "{}", not.message);
        assert_eq!(proxy.tools().await.len(), 1);
        let (outcome, _) = proxy.call("agent", "list_resources", Map::new()).await;
        assert!(outcome.is_error);
        assert_eq!(proxy.ping().await, None);
        proxy.end().await;
    }

    #[tokio::test]
    async fn the_app_refuses_unknown_contexts() {
        let (endpoint, port) = serve(Arc::new(FakeApp::new(true))).await;
        let not = proxy(port, "other").connect().await.unwrap_err();
        assert!(!not.may_retry);
        endpoint.shutdown().await;
    }

    /// The user said no: the process does not ask again on every call.
    #[tokio::test]
    async fn a_declined_session_is_not_asked_for_again() {
        let app = Arc::new(FakeApp::new(true));
        app.approve.store(false, Ordering::SeqCst);
        let (asked, hosted) = (app.asked.clone(), app.hosted.clone());
        let (endpoint, port) = serve(app).await;
        let proxy = proxy(port, CONTEXT);
        let not = proxy.connect().await.unwrap_err();
        assert!(!not.may_retry);
        assert!(not.message.contains("declined"), "{}", not.message);
        let (outcome, _) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(
            outcome.is_error && outcome.text.contains("declined"),
            "{}",
            outcome.text
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(hosted.is_empty());
        endpoint.shutdown().await;
    }

    /// Picks [`CONTEXT`] as `kubach mcp` does for a vault context (#88):
    /// asks the app to host the session and relays to it.
    struct VaultChooser {
        port: u16,
    }

    impl ContextChooser for VaultChooser {
        fn list(&self) -> BoxFuture<'_, Vec<ContextEntry>> {
            Box::pin(async { Vec::new() })
        }

        fn choose<'a>(
            &'a self,
            context: &'a str,
            _client: &'a str,
        ) -> BoxFuture<'a, Result<ChosenSession, String>> {
            Box::pin(async move {
                let proxy = proxy(self.port, context);
                proxy.connect().await.map_err(|not| not.message)?;
                Ok(ChosenSession::Hosted(proxy))
            })
        }
    }

    /// `use_context` on a vault context: the app runs the session, the
    /// shared server relays to it and keeps the context tools (#88).
    #[tokio::test]
    async fn use_context_on_a_vault_context_relays_to_the_app() {
        let app = Arc::new(FakeApp::new(true));
        let (asked, hosted) = (app.asked.clone(), app.hosted.clone());
        let (endpoint, port) = serve(app).await;
        let shared = SharedServer::new();
        shared.set_chooser(Arc::new(VaultChooser { port }));
        let mut args = Map::new();
        args.insert("context".into(), CONTEXT.into());

        let (text, error, changed) = shared
            .call_context_tool("agent", "use_context", &args)
            .await
            .unwrap();
        assert!(!error && changed, "{text}");
        assert!(
            text.contains("The user allowed context prod-imported"),
            "{text}"
        );
        assert!(shared.get().is_none(), "nothing runs in this process");
        let proxy = shared.hosted().unwrap();
        assert_eq!(hosted.list().len(), 1);

        let names: Vec<String> = shared
            .all_tools()
            .await
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(names.contains(&"use_context".to_owned()), "{names:?}");
        assert!(names.contains(&"list_resources".to_owned()), "{names:?}");
        let infos = names.iter().filter(|name| *name == "session_info").count();
        assert_eq!(infos, 1, "{names:?}");
        // session_info goes to the hosted session, not the chooser.
        assert!(
            shared
                .call_context_tool("agent", "session_info", &Map::new())
                .await
                .is_none()
        );

        let (text, _, _) = shared
            .call_context_tool("agent", "list_contexts", &Map::new())
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(listed["current"], CONTEXT);

        // The same context again is not asked again.
        let (text, error, _) = shared
            .call_context_tool("agent", "use_context", &args)
            .await
            .unwrap();
        assert!(!error && text.contains("Already working"), "{text}");
        assert_eq!(asked.load(Ordering::SeqCst), 1);

        // A refused switch keeps the running session.
        args.insert("context".into(), "other".into());
        let (_, error, _) = shared
            .call_context_tool("agent", "use_context", &args)
            .await
            .unwrap();
        assert!(error);
        assert_eq!(shared.hosted().unwrap().context(), CONTEXT);

        proxy.end().await;
        assert!(hosted.is_empty());
        endpoint.shutdown().await;
    }

    /// Listing tools never starts a session (that needs the user's answer).
    #[tokio::test]
    async fn listing_tools_does_not_start_a_session() {
        let app = Arc::new(FakeApp::new(true));
        let (asked, hosted) = (app.asked.clone(), app.hosted.clone());
        let (endpoint, port) = serve(app).await;
        let proxy = proxy(port, CONTEXT);
        assert_eq!(proxy.tools().await.len(), 1);
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        let (outcome, changed) = proxy.call("agent", "session_info", Map::new()).await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(changed);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // Already running: `connect` does not start a second one.
        assert_eq!(proxy.connect().await.unwrap(), None);
        assert_eq!(hosted.list().len(), 1);
        proxy.end().await;
        endpoint.shutdown().await;
    }
}
