//! Bridge between `kubach mcp` (stdio) and the running app.
//!
//! An agent spawns `kubach mcp` as its own child process, so that process
//! has no window to ask the user in. When the app runs with its HTTP server
//! on, the launcher reaches it on `127.0.0.1` with the bridge's own bearer
//! token (kept in [`BRIDGE_TOKEN_FILE`] in the config directory, readable
//! only by the user). The MCP endpoint's token, which HTTP agents get, is
//! not accepted here (#191). Over the bridge the launcher:
//!
//! - asks the app to confirm mutating calls ([`BridgeConfirmer`]);
//! - reports its session, so the app lists it and can revoke it
//!   ([`BridgeClient::heartbeat`]);
//! - for a context imported into the encrypted vault, asks the app to run
//!   the whole session and relays the agent's calls to it
//!   ([`BridgeClient::host`], [`crate::hosted`], [`crate::proxy`]): the
//!   vault's credentials never leave the app. The user approves every such
//!   session in the app's window first.
//!
//! Each session has a key only its process knows: chosen by the process
//! for a session it runs itself (sent with every heartbeat), issued by the
//! app for a hosted one ([`HostReply::key`]). Heartbeats, ending a session
//! and every hosted call need it, so one process cannot end, revive or use
//! another's session by its id.
//!
//! Without the app, mutating calls are refused with an explanation, and
//! imported contexts are not available at all.

use std::io;
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use kubach_core::plugin::BoxFuture;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use rmcp::model::Tool;

use kubach_core::secrets::value::wipe;

use crate::confirm::{ConfirmationDecision, ConfirmationRequest, Confirmer};
use crate::grants::{Grant, GrantEntry};
use crate::hosted::{Gone, HostedSessions};
use crate::scope::SessionMode;
use crate::server::CallOutcome;
use crate::session::BoundaryInfo;
use crate::session::Session;
use crate::settings::Profile;

/// Bearer token of the app's MCP HTTP endpoint, in the config directory.
/// Given to HTTP agents; the bridge does not accept it (#191).
pub const TOKEN_FILE: &str = "mcp-token";

/// Bearer token of the bridge, in the config directory, readable only by
/// the user. Only `kubach mcp` reads it; it is never shown in the app nor
/// given to HTTP agents, and the app replaces it every time its HTTP
/// server starts.
pub const BRIDGE_TOKEN_FILE: &str = "mcp-bridge-token";

/// Path prefix of the bridge routes.
pub const BRIDGE_PATH: &str = "/kubach/v1";

/// A 256-bit random token, hex.
pub fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn read_token_file(dir: &Path, name: &str) -> Option<String> {
    let token = std::fs::read_to_string(dir.join(name)).ok()?;
    let token = token.trim();
    (token.len() >= 32 && token.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| token.into())
}

/// Writes a fresh token to `dir/name` atomically: `0600` on Unix, a DACL
/// with only the current user on Windows, in a `0700` folder.
fn write_token_file(dir: &Path, name: &str) -> io::Result<String> {
    kubach_core::settings::create_private_dir(dir)?;
    let token = random_token();
    let temp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        kubach_core::kubeconfig::restrict_permissions(&temp)?;
        std::io::Write::write_all(&mut file, token.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, dir.join(name))
    })();
    if result.is_err() {
        std::fs::remove_file(&temp).ok();
    }
    result.map(|()| token)
}

/// The stored MCP HTTP token, if there is a valid one.
pub fn read_token(dir: &Path) -> Option<String> {
    read_token_file(dir, TOKEN_FILE)
}

/// The stored MCP HTTP token, creating one on first use.
pub fn load_or_create_token(dir: &Path) -> io::Result<String> {
    match read_token(dir) {
        Some(token) => Ok(token),
        None => regenerate_token(dir),
    }
}

/// Replaces the stored MCP HTTP token with a new one; agents configured
/// with the old one stop working.
pub fn regenerate_token(dir: &Path) -> io::Result<String> {
    write_token_file(dir, TOKEN_FILE)
}

/// The bridge token the running app wrote, if there is a valid one.
pub fn read_bridge_token(dir: &Path) -> Option<String> {
    read_token_file(dir, BRIDGE_TOKEN_FILE)
}

/// A new bridge token, replacing the stored one. `kubach mcp` processes
/// read it again on every request, so they follow.
pub fn regenerate_bridge_token(dir: &Path) -> io::Result<String> {
    write_token_file(dir, BRIDGE_TOKEN_FILE)
}

/// A session of a `kubach mcp` process, as the app lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSession {
    pub id: String,
    pub pid: u32,
    pub context: String,
    /// Pinned at launch, else the namespaces the user allowed reading.
    pub namespaces: Vec<String>,
    pub profile: Profile,
    /// The weakest mode among the namespaces; see `boundaries`.
    pub mode: SessionMode,
    /// Who holds the boundary in each namespace set up so far (#401).
    #[serde(default)]
    pub boundaries: Vec<BoundaryInfo>,
    /// RFC 3339.
    pub started_at: String,
    /// RFC 3339.
    pub expires_at: String,
    pub clients: Vec<String>,
    pub calls: u64,
    /// The user's answers for rights so far (#227).
    #[serde(default)]
    pub grants: Vec<GrantEntry>,
    /// The ceiling the user raised for this session, if any (#417).
    #[serde(default)]
    pub raised: Option<crate::ceiling::Ceiling>,
}

impl ExternalSession {
    /// `session` of this process.
    pub fn of(session: &Session) -> Self {
        Self::with_pid(session, std::process::id())
    }

    /// `session` of the process `pid`.
    pub fn with_pid(session: &Session, pid: u32) -> Self {
        let scope = session.scope();
        let activity = session.activity();
        Self {
            id: session.id().to_owned(),
            pid,
            context: scope.context.clone(),
            namespaces: session.namespaces(),
            profile: scope.profile,
            mode: session.mode(),
            boundaries: session.boundaries(),
            started_at: scope.started_at.to_string(),
            expires_at: scope.expires_at.to_string(),
            clients: activity.clients,
            calls: activity.calls,
            grants: session.grants().entries(),
            raised: session.raised(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatReply {
    /// The user revoked the session in the app.
    pub revoked: bool,
    /// Rights the user took back in the app since the last heartbeat.
    #[serde(default)]
    pub revoke_grants: Vec<Grant>,
}

/// A heartbeat: the session and its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Heartbeat {
    #[serde(flatten)]
    session: ExternalSession,
    /// Chosen by the process; the app ties the session to it on the first
    /// heartbeat and refuses heartbeats and ends with another key.
    key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Ended {
    id: String,
    key: String,
}

/// What `kubach mcp` asks the app to host: the command-line options, which
/// the app resolves against its own settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRequest {
    /// Of the `kubach mcp` process, for the sessions screen.
    pub pid: u32,
    pub context: String,
    /// Empty: the context's default namespace.
    #[serde(default)]
    pub namespaces: Vec<String>,
    /// `None`: the context's maximum profile (a ceiling; each right in it
    /// is asked for, #227).
    #[serde(default)]
    pub profile: Option<Profile>,
    /// `None`: the context's TTL.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// `--grant-read`: reading the session's namespaces is granted from the
    /// start. The app applies it under the same rules as `kubach mcp`.
    #[serde(default)]
    pub grant_read: bool,
}

/// A session the app started for a [`HostRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReply {
    pub session: ExternalSession,
    /// For the agent, as [`McpServer::instructions`](crate::McpServer::instructions).
    pub instructions: String,
    /// Why a stronger identity was not used.
    #[serde(default)]
    pub notes: Vec<String>,
    /// The session's key, issued by the app: every later request about the
    /// session must carry it.
    pub key: String,
}

/// Why the app did not host a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefusalKind {
    /// The vault is locked; retry after the user unlocks it.
    Locked,
    /// The context, the settings or the cluster do not allow it, or the
    /// user did not answer in time.
    Denied,
    /// The user said no in the app's window; final for this process.
    Rejected,
    /// The app cannot host sessions (an older Kubach).
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRefusal {
    pub kind: RefusalKind,
    /// For the user and the agent.
    pub message: String,
}

impl HostRefusal {
    pub fn locked(context: &str) -> Self {
        Self {
            kind: RefusalKind::Locked,
            message: format!(
                "Context `{context}` is imported into the Kubach vault, which is locked. Ask \
                 the user to unlock it in the Kubach window, then retry."
            ),
        }
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self {
            kind: RefusalKind::Denied,
            message: message.into(),
        }
    }

    pub fn rejected(context: &str) -> Self {
        Self {
            kind: RefusalKind::Rejected,
            message: format!(
                "The user declined the session on context `{context}` in the Kubach window. \
                 Restart the MCP server (`kubach mcp`) to ask again."
            ),
        }
    }
}

/// The app's side of the bridge.
pub trait BridgeHandler: Send + Sync + 'static {
    /// Shows `request` to the user; resolves with their decision.
    fn confirm(&self, request: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision>;
    /// A session is alive (sent periodically); returns whether the user
    /// revoked it and the rights taken back since the last heartbeat.
    /// `None` when the session is known under another `key`: the heartbeat
    /// is not from its process.
    fn heartbeat(&self, session: ExternalSession, key: &str) -> Option<HeartbeatReply>;
    /// The session ended. Returns `false`, changing nothing, when the
    /// session is known under another `key`.
    fn ended(&self, id: &str, key: &str) -> bool;
    /// Hosted sessions changed (one ended); refresh what shows them.
    fn changed(&self) {}

    /// Starts a session in the app for a `kubach mcp` process, if the vault
    /// and the settings allow it and the user approves it in the window,
    /// and adds it to [`hosted`](Self::hosted) under a fresh key.
    fn host(&self, request: HostRequest) -> BoxFuture<'static, Result<HostReply, HostRefusal>> {
        let _ = request;
        Box::pin(async {
            Err(HostRefusal {
                kind: RefusalKind::Unsupported,
                message: "this Kubach app cannot host sessions".into(),
            })
        })
    }

    /// Sessions started by [`host`](Self::host).
    fn hosted(&self) -> Option<HostedSessions> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedId {
    id: String,
    key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct HostedCall {
    id: String,
    key: String,
    client: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Map<String, serde_json::Value>,
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Box<Response>> {
    serde_json::from_slice(body)
        .map_err(|error| Box::new((StatusCode::BAD_REQUEST, error.to_string()).into_response()))
}

fn json(value: &impl Serialize) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => ([("content-type", "application/json")], body).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

type Handler = Arc<dyn BridgeHandler>;

/// A request about a session under the wrong key.
fn not_yours() -> Response {
    (StatusCode::FORBIDDEN, "not this process's session").into_response()
}

async fn confirm_route(State(handler): State<Handler>, body: Bytes) -> Response {
    let request: ConfirmationRequest = match parse(&body) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let decision = crate::confirm::ask(&HandlerConfirmer(handler), &request).await;
    json(&decision)
}

async fn heartbeat_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<Heartbeat>(&body) {
        Ok(Heartbeat { session, key }) => match handler.heartbeat(session, &key) {
            Some(reply) => json(&reply),
            None => not_yours(),
        },
        Err(response) => *response,
    }
}

async fn ended_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<Ended>(&body) {
        Ok(Ended { id, key }) => {
            if handler.ended(&id, &key) {
                StatusCode::NO_CONTENT.into_response()
            } else {
                not_yours()
            }
        }
        Err(response) => *response,
    }
}

async fn host_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<HostRequest>(&body) {
        Ok(request) => json(&handler.host(request).await),
        Err(response) => *response,
    }
}

/// An app without hosted sessions answers like for an unknown session.
fn hosted_or_unknown(handler: &Handler) -> Result<HostedSessions, Gone> {
    handler.hosted().ok_or_else(Gone::unknown)
}

async fn hosted_tools_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<HostedId>(&body) {
        Ok(HostedId { id, key }) => {
            json(&hosted_or_unknown(&handler).and_then(|h| h.tools(&id, &key)))
        }
        Err(response) => *response,
    }
}

async fn hosted_call_route(State(handler): State<Handler>, body: Bytes) -> Response {
    let call = match parse::<HostedCall>(&body) {
        Ok(call) => call,
        Err(response) => return *response,
    };
    let result = match hosted_or_unknown(&handler) {
        Ok(hosted) => {
            hosted
                .call(&call.id, &call.key, &call.client, &call.tool, call.args)
                .await
        }
        Err(gone) => Err(gone),
    };
    json(&result)
}

async fn hosted_ping_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<HostedId>(&body) {
        Ok(HostedId { id, key }) => {
            json(&hosted_or_unknown(&handler).and_then(|h| h.ping(&id, &key)))
        }
        Err(response) => *response,
    }
}

async fn hosted_end_route(State(handler): State<Handler>, body: Bytes) -> Response {
    match parse::<HostedId>(&body) {
        Ok(HostedId { id, key }) => {
            let Some(session) = handler.hosted().and_then(|hosted| hosted.end(&id, &key)) else {
                return not_yours();
            };
            tokio::spawn(async move { session.close().await });
            handler.changed();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(response) => *response,
    }
}

/// [`BridgeHandler::confirm`] as a [`Confirmer`], to reuse the timeout.
struct HandlerConfirmer(Handler);

impl Confirmer for HandlerConfirmer {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        self.0.confirm(request.clone())
    }
}

/// Bridge routes, nested under [`BRIDGE_PATH`] by the transport.
pub(crate) fn router(handler: Handler) -> axum::Router {
    axum::Router::new()
        .route("/confirm", post(confirm_route))
        .route("/heartbeat", post(heartbeat_route))
        .route("/ended", post(ended_route))
        .route("/hosted/start", post(host_route))
        .route("/hosted/tools", post(hosted_tools_route))
        .route("/hosted/call", post(hosted_call_route))
        .route("/hosted/ping", post(hosted_ping_route))
        .route("/hosted/end", post(hosted_end_route))
        .with_state(handler)
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("Kubach is not running or its MCP HTTP server is off ({0})")]
    Unreachable(io::Error),
    #[error("the Kubach app answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected answer from the Kubach app: {0}")]
    Protocol(String),
}

/// Talks to the app's bridge over loopback HTTP/1.1.
#[derive(Clone, Debug)]
pub struct BridgeClient {
    port: u16,
    token: String,
}

impl BridgeClient {
    pub fn new(port: u16, token: String) -> Self {
        Self { port, token }
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<Option<T>, BridgeError> {
        let body = serde_json::to_vec(body).map_err(|e| BridgeError::Protocol(e.to_string()))?;
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .map_err(BridgeError::Unreachable)?;
        let head = format!(
            "POST {BRIDGE_PATH}{path} HTTP/1.1\r\nhost: 127.0.0.1:{}\r\n\
             authorization: Bearer {}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n",
            self.port,
            self.token,
            body.len()
        );
        let mut response = Vec::new();
        async {
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(&body).await?;
            stream.read_to_end(&mut response).await
        }
        .await
        .map_err(BridgeError::Unreachable)?;
        let parsed = split_response(&response).and_then(|(status, mut body)| {
            let result = if !(200..300).contains(&status) {
                Err(BridgeError::Status {
                    status,
                    body: String::from_utf8_lossy(&body).trim().to_owned(),
                })
            } else if body.is_empty() {
                Ok(None)
            } else {
                serde_json::from_slice(&body)
                    .map(Some)
                    .map_err(|e| BridgeError::Protocol(e.to_string()))
            };
            wipe(&mut body);
            result
        });
        // An answer may carry Secret values the user entered (#230).
        wipe(&mut response);
        parsed
    }

    pub async fn confirm(
        &self,
        request: &ConfirmationRequest,
    ) -> Result<ConfirmationDecision, BridgeError> {
        self.post("/confirm", request)
            .await?
            .ok_or_else(|| BridgeError::Protocol("empty answer".into()))
    }

    /// Reports `session` with its `key`; returns whether the user revoked
    /// it and which rights they took back.
    pub async fn heartbeat(
        &self,
        session: &ExternalSession,
        key: &str,
    ) -> Result<HeartbeatReply, BridgeError> {
        let body = Heartbeat {
            session: session.clone(),
            key: key.to_owned(),
        };
        let reply: Option<HeartbeatReply> = self.post("/heartbeat", &body).await?;
        Ok(reply.unwrap_or_default())
    }

    pub async fn ended(&self, id: &str, key: &str) -> Result<(), BridgeError> {
        let body = Ended {
            id: id.to_owned(),
            key: key.to_owned(),
        };
        self.post::<serde_json::Value>("/ended", &body)
            .await
            .map(drop)
    }

    async fn answer<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, BridgeError> {
        self.post(path, body)
            .await?
            .ok_or_else(|| BridgeError::Protocol("empty answer".into()))
    }

    /// Asks the app to run a session for an imported context.
    pub async fn host(
        &self,
        request: &HostRequest,
    ) -> Result<Result<HostReply, HostRefusal>, BridgeError> {
        self.answer("/hosted/start", request).await
    }

    /// Tools of hosted session `id`.
    pub async fn hosted_tools(
        &self,
        id: &str,
        key: &str,
    ) -> Result<Result<Vec<Tool>, Gone>, BridgeError> {
        self.answer("/hosted/tools", &hosted_id(id, key)).await
    }

    /// Runs a tool call in hosted session `id`; waits for the user's
    /// confirmation if the call needs one.
    pub async fn hosted_call(
        &self,
        id: &str,
        key: &str,
        client: &str,
        tool: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<CallOutcome, Gone>, BridgeError> {
        let call = HostedCall {
            id: id.to_owned(),
            key: key.to_owned(),
            client: client.to_owned(),
            tool: tool.to_owned(),
            args,
        };
        self.answer("/hosted/call", &call).await
    }

    /// Tells the app the process of hosted session `id` is alive.
    pub async fn hosted_ping(&self, id: &str, key: &str) -> Result<Result<(), Gone>, BridgeError> {
        self.answer("/hosted/ping", &hosted_id(id, key)).await
    }

    /// Ends hosted session `id`.
    pub async fn hosted_end(&self, id: &str, key: &str) -> Result<(), BridgeError> {
        self.post::<serde_json::Value>("/hosted/end", &hosted_id(id, key))
            .await
            .map(drop)
    }
}

fn hosted_id(id: &str, key: &str) -> HostedId {
    HostedId {
        id: id.to_owned(),
        key: key.to_owned(),
    }
}

/// Status code and (de-chunked) body of an HTTP/1.1 response.
fn split_response(response: &[u8]) -> Result<(u16, Vec<u8>), BridgeError> {
    let bad = || BridgeError::Protocol("malformed HTTP response".into());
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(bad)?;
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let body = &response[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(bad)?;
    if !head.contains("transfer-encoding: chunked") {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n").ok_or_else(bad)?;
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| bad())?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        if rest.len() < size + 2 {
            return Err(bad());
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Where to find the app; `None` when it is not configured. Looked up on
/// every call, so starting the app later works.
pub type Locate = Box<dyn Fn() -> Option<BridgeClient> + Send + Sync>;

/// Confirms through the app's window; refuses when the app is unreachable.
pub struct BridgeConfirmer {
    locate: Locate,
}

/// Why a call that needs the user's answer was refused without the app.
pub const NO_APP: &str = "This call needs the user's answer in the Kubach window (a change, or a \
     right such as reading the namespace), but the Kubach app is not reachable. Ask the user to \
     start Kubach and turn on Settings → AI agent → HTTP server, then retry.";

impl BridgeConfirmer {
    pub fn new(locate: Locate) -> Self {
        Self { locate }
    }
}

impl Confirmer for BridgeConfirmer {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        Box::pin(async move {
            let Some(client) = (self.locate)() else {
                return ConfirmationDecision::Denied {
                    reason: Some(NO_APP.into()),
                };
            };
            match client.confirm(request).await {
                Ok(decision) => decision,
                Err(BridgeError::Unreachable(_)) => ConfirmationDecision::Denied {
                    reason: Some(NO_APP.into()),
                },
                Err(error) => ConfirmationDecision::Denied {
                    reason: Some(format!("no approval: {error}")),
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_files_are_private_and_separate() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_token(dir.path()), None);
        let token = load_or_create_token(dir.path()).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(load_or_create_token(dir.path()).unwrap(), token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let fresh = regenerate_token(dir.path()).unwrap();
        assert_ne!(fresh, token);
        assert_eq!(read_token(dir.path()), Some(fresh));
        std::fs::write(dir.path().join(TOKEN_FILE), "short").unwrap();
        assert_eq!(read_token(dir.path()), None);

        // The bridge has its own file, replaced on every start.
        assert_eq!(read_bridge_token(dir.path()), None);
        let bridge = regenerate_bridge_token(dir.path()).unwrap();
        assert_eq!(read_bridge_token(dir.path()), Some(bridge.clone()));
        assert_ne!(regenerate_bridge_token(dir.path()).unwrap(), bridge);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(BRIDGE_TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o077, 0);
        }
    }

    #[test]
    fn parses_plain_and_chunked_responses() {
        let plain = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(split_response(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked =
            b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(split_response(chunked).unwrap(), (401, b"abcde".to_vec()));
        assert!(split_response(b"garbage").is_err());
    }

    #[tokio::test]
    async fn confirmer_refuses_without_the_app() {
        let request = ConfirmationRequest {
            kind: crate::confirm::RequestKind::Call,
            context: "minikube".into(),
            session_id: "s".into(),
            client: "c".into(),
            tool: "scale".into(),
            namespace: "web".into(),
            summary: "Scale".into(),
            details: String::new(),
            dangerous: Vec::new(),
            mode: SessionMode::ClientOnly,
            allow_remember: false,
        };
        let none = BridgeConfirmer::new(Box::new(|| None));
        let decision = none.confirm(&request).await;
        assert_eq!(
            decision,
            ConfirmationDecision::Denied {
                reason: Some(NO_APP.into())
            }
        );
        // A port nothing listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let closed =
            BridgeConfirmer::new(Box::new(move || Some(BridgeClient::new(port, "t".into()))));
        assert!(!closed.confirm(&request).await.is_approved());
    }
}
