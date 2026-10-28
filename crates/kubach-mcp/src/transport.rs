//! Transports.
//!
//! - **stdio**: for a `kubach mcp` launcher that an agent spawns as a child
//!   process; the process boundary is the access control.
//! - **Streamable HTTP** on `127.0.0.1` with a random bearer token: for
//!   agents connecting to the running app. Loopback only, a token per
//!   server, `Host` validation against DNS rebinding and `Origin` rejection
//!   so that web pages in a local browser cannot call it.
//! - The **bridge** for `kubach mcp` processes on the same listener, under
//!   its own token (#191): the MCP token, which HTTP agents hold, opens
//!   only [`MCP_PATH`], the bridge token only [`BRIDGE_PATH`].
//! - The app's endpoint watches whether its agents are still connected
//!   ([`crate::presence`], #34) and stops the session's port-forwards once
//!   they are gone.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use std::sync::Arc;

use rmcp::ServerHandler;

use crate::bridge::{self, BRIDGE_PATH, BridgeHandler, random_token};
use crate::presence::{DISCONNECT_GRACE, Presence, SESSION_HEADER};
use crate::server::McpServer;
use crate::shared::SharedServer;

/// Path of the MCP endpoint.
pub const MCP_PATH: &str = "/mcp";
/// Largest accepted request body (manifests included).
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP initialization failed: {0}")]
    Init(String),
}

/// Serves `server` over stdin/stdout until the client disconnects.
pub async fn serve_stdio<S: ServerHandler>(server: S) -> Result<(), TransportError> {
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| TransportError::Init(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| TransportError::Io(std::io::Error::other(e)))?;
    Ok(())
}

/// A running HTTP endpoint. Dropping it does not stop it; call
/// [`shutdown`](Self::shutdown).
pub struct HttpEndpoint {
    pub addr: SocketAddr,
    /// Bearer token the agent must send; give it to the agent's config only.
    pub token: String,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl HttpEndpoint {
    pub fn url(&self) -> String {
        format!("http://{}{MCP_PATH}", self.addr)
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

/// Constant-time comparison, so the token cannot be guessed byte by byte.
fn token_matches(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Largest request body read before a refusal is sent.
const REFUSED_BODY_LIMIT: usize = 64 * 1024;

/// Answers `status` after reading the request body (up to a limit): closing
/// with the body unread resets the connection, and the client sees a reset
/// instead of the refusal.
async fn refuse(request: Request, status: StatusCode, message: &'static str) -> Response {
    let _ = axum::body::to_bytes(request.into_body(), REFUSED_BODY_LIMIT).await;
    (status, message).into_response()
}

async fn require_token(State(expected): State<String>, request: Request, next: Next) -> Response {
    let given = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if token_matches(given.as_bytes(), expected.as_bytes()) {
        next.run(request).await
    } else {
        refuse(
            request,
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token",
        )
        .await
    }
}

/// Bridge routes are not behind rmcp's checks: refuse browsers (they send
/// `Origin`) and hosts other than the loopback address (DNS rebinding).
async fn local_only(State(port): State<u16>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let host_ok = host == "127.0.0.1" || host == format!("127.0.0.1:{port}");
    if request.headers().contains_key(header::ORIGIN) || !host_ok {
        return refuse(request, StatusCode::FORBIDDEN, "forbidden").await;
    }
    next.run(request).await
}

/// Serves `server` on `127.0.0.1:<port>` (`0` picks a free port) with a
/// fresh random token.
pub async fn serve_http(server: McpServer, port: u16) -> Result<HttpEndpoint, TransportError> {
    serve(server, port, random_token(), None, None).await
}

/// What the app's HTTP server needs.
pub struct HttpOptions {
    /// `0` picks a free port.
    pub port: u16,
    /// Bearer token for MCP, for HTTP agents. Not accepted by the bridge.
    pub token: String,
    /// Serves the bridge for `kubach mcp` processes when set.
    pub bridge: Option<BridgeOptions>,
}

/// The bridge for `kubach mcp` processes.
pub struct BridgeOptions {
    pub handler: Arc<dyn BridgeHandler>,
    /// Bearer token of the bridge; must differ from the MCP token.
    pub token: String,
}

/// Serves whatever session `server` currently holds, plus the bridge.
pub async fn serve_shared_http(
    server: SharedServer,
    options: HttpOptions,
) -> Result<HttpEndpoint, TransportError> {
    serve_shared_with_grace(server, options, DISCONNECT_GRACE).await
}

/// [`serve_shared_http`] with the agents counted gone after `grace`.
pub(crate) async fn serve_shared_with_grace(
    server: SharedServer,
    options: HttpOptions,
    grace: Duration,
) -> Result<HttpEndpoint, TransportError> {
    let watch = Watch {
        server: server.clone(),
        presence: Arc::new(Presence::default()),
        grace,
    };
    serve(
        server,
        options.port,
        options.token,
        options.bridge,
        Some(watch),
    )
    .await
}

/// Watches the agents of a shared endpoint (#34).
struct Watch {
    server: SharedServer,
    presence: Arc<Presence>,
    grace: Duration,
}

impl Watch {
    /// Stops the current session's port-forwards whenever the agents are
    /// gone, until `cancel`.
    async fn run(self, cancel: CancellationToken) {
        // Often enough that a lost agent's forwards stop soon after the
        // grace period.
        let tick = (self.grace / 4).clamp(Duration::from_millis(20), Duration::from_secs(5));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = self.presence.idle_or(tick) => {}
            }
            if !self.presence.gone(self.grace, tokio::time::Instant::now()) {
                continue;
            }
            if let Some(server) = self.server.get() {
                crate::forwards::stop_disconnected(server.session()).await;
            }
        }
    }
}

/// Counts the request as open until its response body is done (an SSE
/// stream: until the client goes), and notes the MCP sessions it opens
/// and closes.
async fn track_presence(
    State(presence): State<Arc<Presence>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let asked = session_id(request.headers());
    let visit = presence.enter();
    let response = next.run(request).await;
    presence.answered(
        &method,
        asked.as_deref(),
        response.status(),
        session_id(response.headers()).as_deref(),
    );
    let (parts, body) = response.into_parts();
    let body = axum::body::Body::from_stream(body.into_data_stream().map(move |chunk| {
        let _open = &visit;
        chunk
    }));
    Response::from_parts(parts, body)
}

fn session_id(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn serve<S>(
    server: S,
    port: u16,
    token: String,
    bridge: Option<BridgeOptions>,
    watch: Option<Watch>,
) -> Result<HttpEndpoint, TransportError>
where
    S: ServerHandler + Clone + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let cancel = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(["127.0.0.1".to_owned(), format!("127.0.0.1:{}", addr.port())])
        .enforce_origin_validation()
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .with_cancellation_token(cancel.child_token());
    let service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
    if bridge.as_ref().is_some_and(|bridge| bridge.token == token) {
        return Err(TransportError::Init(
            "the bridge needs a token of its own".into(),
        ));
    }
    // Each token opens only its own routes.
    let mut router = axum::Router::new().nest_service(MCP_PATH, service);
    if let Some(watch) = &watch {
        router = router.layer(middleware::from_fn_with_state(
            watch.presence.clone(),
            track_presence,
        ));
    }
    let mut router = router.layer(middleware::from_fn_with_state(token.clone(), require_token));
    if let Some(BridgeOptions {
        handler,
        token: bridge_token,
    }) = bridge
    {
        router = router.nest(
            BRIDGE_PATH,
            bridge::router(handler)
                .layer(middleware::from_fn_with_state(addr.port(), local_only))
                .layer(middleware::from_fn_with_state(bridge_token, require_token)),
        );
    }

    if let Some(watch) = watch {
        tokio::spawn(watch.run(cancel.child_token()));
    }
    let shutdown = cancel.clone();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
            .await;
    });
    Ok(HttpEndpoint {
        addr,
        token,
        cancel,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_distinct() {
        let a = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token());
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches(b"abc", b"abc"));
        assert!(!token_matches(b"abd", b"abc"));
        assert!(!token_matches(b"ab", b"abc"));
        assert!(!token_matches(b"", b"abc"));
    }
}

/// The MCP token and the bridge token open only their own routes (#191).
#[cfg(test)]
mod token_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kubach_core::plugin::BoxFuture;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{BridgeOptions, HttpOptions, MCP_PATH, serve_shared_http};
    use crate::bridge::{
        BridgeClient, BridgeError, BridgeHandler, ExternalSession, HeartbeatReply, HostRefusal,
        HostReply, HostRequest,
    };
    use crate::confirm::{ConfirmationDecision, ConfirmationRequest};
    use crate::scope::SessionMode;
    use crate::settings::Profile;
    use crate::shared::SharedServer;

    /// Counts what reaches the app.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
    }

    impl BridgeHandler for Counting {
        fn confirm(&self, _: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { ConfirmationDecision::Denied { reason: None } })
        }

        fn heartbeat(&self, _: ExternalSession, _: &str) -> Option<HeartbeatReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(HeartbeatReply::default())
        }

        fn ended(&self, _: &str, _: &str) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            true
        }

        fn host(&self, _: HostRequest) -> BoxFuture<'static, Result<HostReply, HostRefusal>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(HostRefusal::denied("test")) })
        }
    }

    fn session() -> ExternalSession {
        ExternalSession {
            raised: None,
            id: "s".into(),
            pid: 1,
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
        }
    }

    fn host_request() -> HostRequest {
        HostRequest {
            pid: 1,
            context: "prod-imported".into(),
            namespaces: Vec::new(),
            profile: Some(Profile::Deploy),
            ttl_secs: None,
            grant_read: false,
        }
    }

    fn unauthorized<T: std::fmt::Debug>(result: Result<T, BridgeError>) -> bool {
        matches!(result, Err(BridgeError::Status { status: 401, .. }))
    }

    /// Status of a bare `POST /mcp` with `token`.
    async fn mcp_status(port: u16, token: &str) -> u16 {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let head = format!(
            "POST {MCP_PATH} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
             authorization: Bearer {token}\r\ncontent-type: application/json\r\n\
             accept: application/json, text/event-stream\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut response = vec![0u8; 64];
        let n = stream.read(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response[..n]).into_owned();
        text.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    #[tokio::test]
    async fn each_token_opens_only_its_routes() {
        let (mcp_token, bridge_token) = ("m".repeat(64), "b".repeat(64));
        let app = Arc::new(Counting::default());
        let endpoint = serve_shared_http(
            SharedServer::new(),
            HttpOptions {
                port: 0,
                token: mcp_token.clone(),
                bridge: Some(BridgeOptions {
                    handler: app.clone(),
                    token: bridge_token.clone(),
                }),
            },
        )
        .await
        .unwrap();
        let port = endpoint.addr.port();

        // An HTTP agent's token: no bridge route at all.
        let agent = BridgeClient::new(port, mcp_token.clone());
        assert!(unauthorized(agent.host(&host_request()).await));
        assert!(unauthorized(agent.heartbeat(&session(), "k").await));
        assert!(unauthorized(agent.ended("s", "k").await));
        assert!(unauthorized(agent.hosted_end("s", "k").await));
        assert!(unauthorized(agent.hosted_ping("s", "k").await));
        assert!(unauthorized(
            agent
                .hosted_call("s", "k", "agent", "session_info", Default::default())
                .await
        ));
        assert_eq!(app.calls.load(Ordering::SeqCst), 0);

        // The bridge token reaches the bridge, not MCP.
        let launcher = BridgeClient::new(port, bridge_token.clone());
        assert!(launcher.heartbeat(&session(), "k").await.is_ok());
        assert_eq!(app.calls.load(Ordering::SeqCst), 1);
        assert_eq!(mcp_status(port, &bridge_token).await, 401);
        assert_ne!(mcp_status(port, &mcp_token).await, 401);

        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn the_bridge_needs_its_own_token() {
        let token = "t".repeat(64);
        let result = serve_shared_http(
            SharedServer::new(),
            HttpOptions {
                port: 0,
                token: token.clone(),
                bridge: Some(BridgeOptions {
                    handler: Arc::new(Counting::default()),
                    token,
                }),
            },
        )
        .await;
        assert!(result.is_err());
    }
}
