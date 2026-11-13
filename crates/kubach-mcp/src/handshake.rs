//! How `kubach mcp` and the app verify each other on the bridge.
//!
//! Both sides hold the bridge key: the app makes a new one every time its
//! HTTP server starts and keeps it in a file only the user reads
//! ([`crate::bridge::BRIDGE_KEY_FILE`]). The key itself never travels.
//! Every bridge request is two exchanges:
//!
//! 1. `POST /kubach/v1/hello` with the protocol version and a fresh nonce
//!    of the process. The app answers with its own fresh nonce and
//!    [`app_proof`]: HMAC-SHA256 under the key over both nonces. The
//!    process checks it before it sends anything else, so only the holder
//!    of the key is given the request.
//! 2. The request itself, with the app's nonce, the SHA-256 of the body and
//!    [`request_proof`] over both nonces, the route and that digest. The
//!    app accepts each of its nonces once. Its answer carries
//!    [`answer_proof`] over the same nonces, the route, the status and the
//!    digest of the answer, and the process acts only on an answer whose
//!    proof holds: an answer belongs to exactly one request.
//!
//! A peer that speaks the bridge without these steps (another version of
//! Kubach) is refused with a message asking to update and restart; there
//! is no fallback to an exchange without proofs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::bridge::{BRIDGE_PATH, MAX_CONFIRM_BODY_BYTES, random_token};
use crate::transport::refuse;

/// Version of the bridge protocol; both sides must name the same one.
pub const PROTOCOL: u32 = 2;

/// Route of the first exchange, under [`BRIDGE_PATH`].
pub(crate) const HELLO_PATH: &str = "/hello";
/// The app's nonce from the hello, on the request.
pub(crate) const NONCE_HEADER: &str = "x-kubach-nonce";
/// SHA-256 of the request body, hex.
pub(crate) const DIGEST_HEADER: &str = "x-kubach-digest";
/// [`request_proof`] on a request, [`answer_proof`] on its answer.
pub(crate) const PROOF_HEADER: &str = "x-kubach-proof";

/// What the app answers a request made without the handshake.
pub(crate) const OTHER_VERSION: &str = "This Kubach app and the `kubach mcp` process are different \
     versions and cannot verify each other. Update Kubach, then restart the app and the agent's \
     MCP server (`kubach mcp`).";

/// How long the app keeps a nonce it gave out and that was not used.
const NONCE_TTL: Duration = Duration::from_secs(30);
/// Most nonces waiting to be used; a hello beyond it is refused.
const MAX_PENDING: usize = 4096;
/// Largest body of a bridge request other than a confirmation.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Hello {
    pub version: u32,
    /// Fresh for every request of the process.
    pub nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HelloReply {
    pub version: u32,
    /// Fresh for every hello; opens one request.
    pub nonce: String,
    /// [`app_proof`].
    pub proof: String,
}

/// A nonce as either side makes it: nothing that could break a header.
pub(crate) fn valid_nonce(nonce: &str) -> bool {
    (32..=128).contains(&nonce.len()) && nonce.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// SHA-256 of `body`, hex.
pub(crate) fn digest(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}

/// HMAC-SHA256 under `key` over `label` and `parts`, each with its length,
/// so that no two different inputs give the same bytes.
fn mac(key: &str, label: &str, parts: &[&str]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes a key of any length");
    for part in std::iter::once(&label).chain(parts) {
        mac.update(&(part.len() as u64).to_le_bytes());
        mac.update(part.as_bytes());
    }
    hex(&mac.finalize().into_bytes())
}

/// The app's proof that it holds the key, for one hello.
pub(crate) fn app_proof(key: &str, client_nonce: &str, app_nonce: &str) -> String {
    mac(key, "kubach-bridge/app", &[client_nonce, app_nonce])
}

/// The process's proof that it holds the key, for one request.
pub(crate) fn request_proof(
    key: &str,
    client_nonce: &str,
    app_nonce: &str,
    path: &str,
    body_digest: &str,
) -> String {
    mac(
        key,
        "kubach-bridge/request",
        &[client_nonce, app_nonce, path, body_digest],
    )
}

/// The app's proof that an answer is its own and answers this request.
pub(crate) fn answer_proof(
    key: &str,
    client_nonce: &str,
    app_nonce: &str,
    path: &str,
    status: u16,
    body_digest: &str,
) -> String {
    mac(
        key,
        "kubach-bridge/answer",
        &[
            client_nonce,
            app_nonce,
            path,
            &status.to_string(),
            body_digest,
        ],
    )
}

/// Constant-time comparison, so a value cannot be guessed byte by byte.
pub(crate) fn same(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// The app's side: the key and the nonces it gave out.
pub(crate) struct Gate {
    key: String,
    /// The app's nonce → the process's nonce and when the pair expires.
    pending: Mutex<HashMap<String, (String, Instant)>>,
}

impl Gate {
    pub(crate) fn new(key: String) -> Self {
        Self {
            key,
            pending: Mutex::default(),
        }
    }

    /// Answers a hello; `None` when too many nonces wait to be used.
    fn greet(&self, client_nonce: &str) -> Option<HelloReply> {
        let nonce = random_token();
        let now = Instant::now();
        let mut pending = kubach_core::sync::lock(&self.pending);
        pending.retain(|_, (_, expires)| *expires > now);
        if pending.len() >= MAX_PENDING {
            return None;
        }
        pending.insert(nonce.clone(), (client_nonce.to_owned(), now + NONCE_TTL));
        Some(HelloReply {
            version: PROTOCOL,
            proof: app_proof(&self.key, client_nonce, &nonce),
            nonce,
        })
    }

    /// The process's nonce of the hello that gave out `app_nonce`, once.
    fn take(&self, app_nonce: &str) -> Option<String> {
        let (client_nonce, expires) = kubach_core::sync::lock(&self.pending).remove(app_nonce)?;
        (expires > Instant::now()).then_some(client_nonce)
    }
}

/// The first exchange. Open to any local process: the answer proves the
/// app to the holder of the key and tells nothing to anyone else.
pub(crate) async fn hello_route(State(gate): State<Arc<Gate>>, body: Bytes) -> Response {
    let hello = match serde_json::from_slice::<Hello>(&body) {
        Ok(hello) if hello.version == PROTOCOL && valid_nonce(&hello.nonce) => hello,
        Ok(hello) if hello.version != PROTOCOL => {
            return (StatusCode::UPGRADE_REQUIRED, OTHER_VERSION).into_response();
        }
        _ => return (StatusCode::BAD_REQUEST, "malformed hello").into_response(),
    };
    match gate.greet(&hello.nonce) {
        Some(reply) => match serde_json::to_vec(&reply) {
            Ok(body) => ([("content-type", "application/json")], body).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        None => (StatusCode::SERVICE_UNAVAILABLE, "too many handshakes").into_response(),
    }
}

/// Lets a bridge request through only with a nonce of this app, not used
/// before, and the proof of the key over this very request; signs the
/// answer for it.
pub(crate) async fn verified(
    State(gate): State<Arc<Gate>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let path = path.strip_prefix(BRIDGE_PATH).unwrap_or(path).to_owned();
    let headers = {
        let header = |name: &str| {
            let value = request.headers().get(name)?.to_str().ok()?;
            Some(value.to_owned())
        };
        (|| {
            Some((
                header(NONCE_HEADER)?,
                header(DIGEST_HEADER)?,
                header(PROOF_HEADER)?,
            ))
        })()
    };
    let Some((app_nonce, claimed, proof)) = headers else {
        return refuse(request, StatusCode::UPGRADE_REQUIRED, OTHER_VERSION).await;
    };
    let Some(client_nonce) = gate.take(&app_nonce) else {
        return refuse(request, StatusCode::UNAUTHORIZED, "unknown or used nonce").await;
    };
    let expected = request_proof(&gate.key, &client_nonce, &app_nonce, &path, &claimed);
    if !same(proof.as_bytes(), expected.as_bytes()) {
        return refuse(request, StatusCode::UNAUTHORIZED, "invalid proof").await;
    }
    // The body is read only now, from a process that holds the key.
    let limit = if path == "/confirm" {
        MAX_CONFIRM_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    };
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, limit).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response();
    };
    if !same(digest(&body).as_bytes(), claimed.as_bytes()) {
        return (
            StatusCode::BAD_REQUEST,
            "the body does not match its digest",
        )
            .into_response();
    }
    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    let (mut parts, body) = response.into_parts();
    let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let proof = answer_proof(
        &gate.key,
        &client_nonce,
        &app_nonce,
        &path,
        parts.status.as_u16(),
        &digest(&body),
    );
    match HeaderValue::from_str(&proof) {
        Ok(proof) => parts.headers.insert(PROOF_HEADER, proof),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    Response::from_parts(parts, Body::from(body))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use kubach_core::plugin::BoxFuture;
    use kubach_core::sync;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;
    use crate::bridge::{
        self, BridgeClient, BridgeConfirmer, BridgeError, BridgeHandler, ExternalSession,
        HeartbeatReply, NO_APP, exchange, header, split_response,
    };
    use crate::confirm::{ConfirmationDecision, ConfirmationRequest, Confirmer as _};
    use crate::scope::SessionMode;
    use crate::shared::SharedServer;
    use crate::transport::{BridgeOptions, HttpEndpoint, HttpOptions, serve_shared_http};

    /// The app's side: answers as the user would and counts the questions.
    struct App {
        approve: AtomicBool,
        asked: AtomicUsize,
    }

    impl BridgeHandler for App {
        fn confirm(&self, _: ConfirmationRequest) -> BoxFuture<'static, ConfirmationDecision> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let approve = self.approve.load(Ordering::SeqCst);
            Box::pin(async move {
                if approve {
                    ConfirmationDecision::Approved
                } else {
                    ConfirmationDecision::Denied { reason: None }
                }
            })
        }

        fn heartbeat(&self, _: ExternalSession, _: &str) -> Option<HeartbeatReply> {
            Some(HeartbeatReply::default())
        }

        fn ended(&self, _: &str, _: &str) -> bool {
            true
        }
    }

    async fn serve(key: &str, approve: bool) -> (HttpEndpoint, u16, Arc<App>) {
        let app = Arc::new(App {
            approve: AtomicBool::new(approve),
            asked: AtomicUsize::new(0),
        });
        let endpoint = serve_shared_http(
            SharedServer::new(),
            HttpOptions {
                port: 0,
                token: "m".repeat(64),
                bridge: Some(BridgeOptions {
                    handler: app.clone(),
                    key: key.to_owned(),
                }),
            },
        )
        .await
        .unwrap();
        let port = endpoint.addr.port();
        (endpoint, port, app)
    }

    const SUMMARY: &str = "Scale deployment web to 3 replicas";

    fn question() -> ConfirmationRequest {
        ConfirmationRequest {
            kind: Default::default(),
            context: "minikube".into(),
            session_id: "s".into(),
            client: "agent".into(),
            tool: "scale".into(),
            namespace: "web".into(),
            summary: SUMMARY.into(),
            details: String::new(),
            dangerous: Vec::new(),
            mode: SessionMode::ClientOnly,
            allow_remember: false,
            diff_cut: None,
        }
    }

    fn confirmer(port: u16, key: &str) -> BridgeConfirmer {
        let client = BridgeClient::new(port, key.to_owned());
        BridgeConfirmer::new(Box::new(move || Some(client.clone())))
    }

    fn reason(decision: &ConfirmationDecision) -> String {
        match decision {
            ConfirmationDecision::Denied { reason } => reason.clone().unwrap_or_default(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A bare bridge request: status, lower-cased head, body.
    async fn raw(
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, String, Vec<u8>) {
        let mut head = format!(
            "POST {BRIDGE_PATH}{path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let response = exchange(port, &head, body).await.unwrap();
        split_response(&response).unwrap()
    }

    /// A hello of a process that made `nonce`.
    async fn hello(port: u16, nonce: &str) -> HelloReply {
        let body = serde_json::to_vec(&Hello {
            version: PROTOCOL,
            nonce: nonce.to_owned(),
        })
        .unwrap();
        let (status, _, body) = raw(port, HELLO_PATH, &[], &body).await;
        assert_eq!(status, 200);
        serde_json::from_slice(&body).unwrap()
    }

    /// A whole HTTP request from `stream`: head and `content-length` body.
    async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            if let Some(split) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..split]).to_ascii_lowercase();
                let length: usize = header(&head, "content-length")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                if request.len() >= split + 4 + length {
                    return request;
                }
            }
            match stream.read(&mut buffer).await {
                Ok(n) if n > 0 => request.extend_from_slice(&buffer[..n]),
                _ => return request,
            }
        }
    }

    fn http(status: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// A listener that is not the app. It answers connection `n` with
    /// `answer(n, request)` and keeps everything it was sent.
    async fn listener(
        answer: impl Fn(usize, &[u8]) -> BoxFuture<'static, Vec<u8>> + Send + 'static,
    ) -> (u16, Arc<Mutex<Vec<u8>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<u8>>> = Arc::default();
        let kept = seen.clone();
        tokio::spawn(async move {
            let mut n = 0;
            while let Ok((mut stream, _)) = listener.accept().await {
                let request = read_request(&mut stream).await;
                sync::lock(&kept).extend_from_slice(&request);
                let response = answer(n, &request).await;
                n += 1;
                let _ = stream.write_all(&response).await;
                let _ = stream.shutdown().await;
            }
        });
        (port, seen)
    }

    /// Passes `request` on to the app on `port`, under the app's own
    /// `Host`; returns its answer.
    async fn pass(port: u16, request: Vec<u8>) -> Vec<u8> {
        let request = String::from_utf8(request).unwrap();
        let start = request.find("host: ").unwrap();
        let end = start + request[start..].find("\r\n").unwrap();
        let request = format!(
            "{}host: 127.0.0.1:{port}{}",
            &request[..start],
            &request[end..]
        )
        .into_bytes();
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream.write_all(&request).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        response
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    #[test]
    fn a_proof_covers_every_part_of_the_exchange() {
        // Fresh values on every run: nothing here is a fixed key or nonce.
        let (key, other_key) = (random_token(), random_token());
        let (c, a, x) = (random_token(), random_token(), random_token());
        let (key, c, a, x) = (key.as_str(), c.as_str(), a.as_str(), x.as_str());
        let base = request_proof(key, c, a, "/confirm", "d");
        assert_eq!(base.len(), 64);
        assert_eq!(base, request_proof(key, c, a, "/confirm", "d"));
        for other in [
            request_proof(&other_key, c, a, "/confirm", "d"),
            request_proof(key, x, a, "/confirm", "d"),
            request_proof(key, c, x, "/confirm", "d"),
            request_proof(key, c, a, "/heartbeat", "d"),
            request_proof(key, c, a, "/confirm", "x"),
            // The same bytes split differently between the parts.
            request_proof(key, &format!("{c}{a}"), "", "/confirm", "d"),
            // An answer or a hello proof is no request proof.
            answer_proof(key, c, a, "/confirm", 200, "d"),
            app_proof(key, c, a),
        ] {
            assert_ne!(base, other);
        }
        assert_ne!(
            answer_proof(key, c, a, "/confirm", 200, "d"),
            answer_proof(key, c, a, "/confirm", 403, "d")
        );
        let nonce = random_token();
        assert!(valid_nonce(&nonce));
        // Too short, and with characters a header line must not carry.
        assert!(!valid_nonce(&nonce[..5]));
        assert!(!valid_nonce(&format!("{nonce}\r\n")));
    }

    #[tokio::test]
    async fn both_sides_hold_the_key_and_the_answer_is_the_apps() {
        let key = random_token();
        let (endpoint, port, app) = serve(&key, true).await;
        let decision = confirmer(port, &key).confirm(&question()).await;
        assert_eq!(decision, ConfirmationDecision::Approved);
        assert_eq!(app.asked.load(Ordering::SeqCst), 1);
        // A refusal of the user arrives the same way.
        app.approve.store(false, Ordering::SeqCst);
        let decision = confirmer(port, &key).confirm(&question()).await;
        assert_eq!(decision, ConfirmationDecision::Denied { reason: None });
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn another_key_on_either_side_is_refused() {
        let key = random_token();
        let (endpoint, port, app) = serve(&key, true).await;

        // The process holds another key: the app's proof does not hold for
        // it, and it asks nothing.
        let other = random_token();
        let result = BridgeClient::new(port, other.clone())
            .confirm(&question())
            .await;
        assert!(
            matches!(result, Err(BridgeError::Unverified { .. })),
            "{result:?}"
        );
        let decision = confirmer(port, &other).confirm(&question()).await;
        assert!(reason(&decision).contains("did not prove"), "{decision:?}");

        // A process that asks anyway, with a proof under another key, is
        // not served, and its nonce is spent.
        let body = serde_json::to_vec(&question()).unwrap();
        let sum = digest(&body);
        let nonce = random_token();
        let reply = hello(port, &nonce).await;
        let wrong = request_proof(&other, &nonce, &reply.nonce, "/confirm", &sum);
        let right = request_proof(&key, &nonce, &reply.nonce, "/confirm", &sum);
        for proof in [&wrong, &right] {
            let headers = [
                (NONCE_HEADER, reply.nonce.as_str()),
                (DIGEST_HEADER, sum.as_str()),
                (PROOF_HEADER, proof.as_str()),
            ];
            let (status, head, _) = raw(port, "/confirm", &headers, &body).await;
            assert_eq!(status, 401);
            assert_eq!(header(&head, PROOF_HEADER), None);
        }
        assert_eq!(app.asked.load(Ordering::SeqCst), 0);
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn a_request_is_taken_once_and_as_it_was_proven() {
        let key = random_token();
        let (endpoint, port, app) = serve(&key, true).await;
        let body = serde_json::to_vec(&question()).unwrap();
        let sum = digest(&body);
        let send = async |path: &str, proven_path: &str, body: &[u8], twice: bool| {
            let nonce = random_token();
            let reply = hello(port, &nonce).await;
            let proof = request_proof(&key, &nonce, &reply.nonce, proven_path, &sum);
            let headers = [
                (NONCE_HEADER, reply.nonce.as_str()),
                (DIGEST_HEADER, sum.as_str()),
                (PROOF_HEADER, proof.as_str()),
            ];
            let mut last = raw(port, path, &headers, body).await;
            if twice {
                last = raw(port, path, &headers, body).await;
            }
            (last, nonce, reply.nonce)
        };

        // As proven: served, and the answer is proven for this request.
        let ((status, head, answer), nonce, app_nonce) =
            send("/confirm", "/confirm", &body, false).await;
        assert_eq!(status, 200);
        let expected = answer_proof(&key, &nonce, &app_nonce, "/confirm", 200, &digest(&answer));
        assert_eq!(header(&head, PROOF_HEADER), Some(expected.as_str()));
        assert_eq!(app.asked.load(Ordering::SeqCst), 1);

        // The same request sent again: refused, the app is not asked twice.
        let ((status, ..), ..) = send("/confirm", "/confirm", &body, true).await;
        assert_eq!(status, 401);
        assert_eq!(app.asked.load(Ordering::SeqCst), 2);

        // Proven for another route, or with another body: refused.
        let ((status, ..), ..) = send("/confirm", "/heartbeat", &body, false).await;
        assert_eq!(status, 401);
        let mut altered = body.clone();
        let at = altered.windows(5).position(|w| w == b"Scale").unwrap();
        altered[at..at + 5].copy_from_slice(b"Purge");
        let ((status, ..), ..) = send("/confirm", "/confirm", &altered, false).await;
        assert_eq!(status, 400);
        assert_eq!(app.asked.load(Ordering::SeqCst), 2);
        endpoint.shutdown().await;
    }

    /// A listener that does not hold the key gets a nonce and nothing
    /// else, and no answer of its is acted on.
    #[tokio::test]
    async fn a_listener_without_the_key_is_told_nothing_and_decides_nothing() {
        let key = random_token();
        let approved = r#"{"decision":"approved"}"#;

        // It answers "approved" to whatever comes.
        let (port, seen) =
            listener(move |_, _| Box::pin(async move { http("200 OK", approved) })).await;
        let decision = confirmer(port, &key).confirm(&question()).await;
        assert!(reason(&decision).contains("did not prove"), "{decision:?}");

        // It answers the hello in the right shape, with a proof it made up
        // or copied from the process, then "approved".
        let (shaped, seen_shaped) = listener(move |n, request| {
            let split = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let sent: Option<Hello> = serde_json::from_slice(&request[split + 4..]).ok();
            Box::pin(async move {
                let reply = HelloReply {
                    version: PROTOCOL,
                    nonce: random_token(),
                    proof: match (n, sent) {
                        (0, Some(hello)) => hello.nonce,
                        _ => random_token(),
                    },
                };
                match n % 2 {
                    0 => http("200 OK", &serde_json::to_string(&reply).unwrap()),
                    _ => http("200 OK", approved),
                }
            })
        })
        .await;
        for _ in 0..2 {
            let decision = confirmer(shaped, &key).confirm(&question()).await;
            assert!(!decision.is_approved());
            assert!(reason(&decision).contains("did not prove"), "{decision:?}");
        }

        for seen in [seen, seen_shaped] {
            let seen = sync::lock(&seen).clone();
            assert!(contains(&seen, HELLO_PATH), "only hellos arrived");
            assert!(!contains(&seen, &key), "the key stays with the process");
            assert!(!contains(&seen, SUMMARY), "the request was not sent");
            assert!(!contains(&seen, "/confirm"));
        }
    }

    /// Answers recorded from the app do not answer a later request, and an
    /// answer changed on the way is not acted on.
    #[tokio::test]
    async fn a_recorded_or_altered_answer_is_not_acted_on() {
        let key = random_token();
        let (endpoint, app_port, app) = serve(&key, true).await;

        // Connections 0 and 1 (a hello and a confirmation) pass and are
        // recorded. 2: the recorded hello answer again. 3: a hello passes;
        // 4: the recorded confirmation answer again.
        let recorded: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let tape = recorded.clone();
        let (port, _) = listener(move |n, request| {
            let (tape, request) = (tape.clone(), request.to_vec());
            Box::pin(async move {
                match n {
                    0 | 1 => {
                        let answer = pass(app_port, request).await;
                        sync::lock(&tape).push(answer.clone());
                        answer
                    }
                    2 => sync::lock(&tape)[0].clone(),
                    3 => pass(app_port, request).await,
                    _ => sync::lock(&tape)[1].clone(),
                }
            })
        })
        .await;
        let client = BridgeClient::new(port, key.clone());
        assert_eq!(
            client.confirm(&question()).await.ok(),
            Some(ConfirmationDecision::Approved)
        );
        assert_eq!(app.asked.load(Ordering::SeqCst), 1);
        assert!(contains(&sync::lock(&recorded)[1], "approved"));
        let again = client.confirm(&question()).await;
        assert!(
            matches!(again, Err(BridgeError::Unverified { .. })),
            "{again:?}"
        );
        let again = client.confirm(&question()).await;
        assert!(
            matches!(again, Err(BridgeError::Unproven { status: 200, .. })),
            "{again:?}"
        );
        assert_eq!(
            app.asked.load(Ordering::SeqCst),
            1,
            "the app was asked once"
        );

        // The user says no; on the way the answer becomes "approved".
        app.approve.store(false, Ordering::SeqCst);
        let (altering, _) = listener(move |_, request| {
            let request = request.to_vec();
            Box::pin(async move {
                let answer = pass(app_port, request).await;
                String::from_utf8(answer)
                    .unwrap()
                    .replace(
                        r#"{"decision":"denied","reason":null}"#,
                        r#"{"decision":"approved"}"#,
                    )
                    .into_bytes()
            })
        })
        .await;
        let result = BridgeClient::new(altering, key.clone())
            .confirm(&question())
            .await;
        assert!(
            matches!(result, Err(BridgeError::Unproven { status: 200, .. })),
            "{result:?}"
        );
        let decision = confirmer(altering, &key).confirm(&question()).await;
        assert!(reason(&decision).contains("no valid proof"), "{decision:?}");
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn a_key_file_without_its_app_opens_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let port = Arc::new(AtomicUsize::new(0));
        let confirmer = {
            let (dir, port) = (dir.path().to_owned(), port.clone());
            BridgeConfirmer::new(Box::new(move || {
                let key = bridge::read_bridge_key(&dir)?;
                Some(BridgeClient::new(port.load(Ordering::SeqCst) as u16, key))
            }))
        };

        // No file: there is no app to ask.
        assert_eq!(reason(&confirmer.confirm(&question()).await), NO_APP);

        // A file an app left behind, nothing on its port.
        let stale = random_token();
        bridge::store_bridge_key(dir.path(), &stale).unwrap();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        port.store(free.local_addr().unwrap().port().into(), Ordering::SeqCst);
        drop(free);
        assert_eq!(reason(&confirmer.confirm(&question()).await), NO_APP);

        // The same file, and an app that started since with its own key
        // but has not written it yet: no proof for the old key.
        let key = random_token();
        let (endpoint, live, app) = serve(&key, true).await;
        port.store(live.into(), Ordering::SeqCst);
        let decision = confirmer.confirm(&question()).await;
        assert!(reason(&decision).contains("did not prove"), "{decision:?}");
        assert_eq!(app.asked.load(Ordering::SeqCst), 0);

        // Once the app wrote its key, the process follows.
        bridge::store_bridge_key(dir.path(), &key).unwrap();
        assert!(confirmer.confirm(&question()).await.is_approved());
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn another_version_is_refused_with_a_message() {
        let key = random_token();
        let (endpoint, port, app) = serve(&key, true).await;
        let body = serde_json::to_vec(&question()).unwrap();

        // A process of the earlier protocol: a bearer token, no handshake.
        let bearer = format!("Bearer {key}");
        let (status, _, answer) = raw(
            port,
            "/confirm",
            &[("authorization", bearer.as_str())],
            &body,
        )
        .await;
        assert_eq!(status, 426);
        let answer = String::from_utf8(answer).unwrap();
        assert_eq!(answer, OTHER_VERSION);
        assert!(answer.contains("Update Kubach") && answer.contains("restart"));
        // A hello of another protocol version.
        let (status, _, answer) = raw(
            port,
            HELLO_PATH,
            &[],
            format!(r#"{{"version":1,"nonce":"{}"}}"#, random_token()).as_bytes(),
        )
        .await;
        assert_eq!((status, answer), (426, OTHER_VERSION.as_bytes().to_vec()));
        assert_eq!(app.asked.load(Ordering::SeqCst), 0);
        endpoint.shutdown().await;

        // An app of the earlier protocol wants its bearer token for every
        // route; an app of a later one names its version.
        let (old, seen) = listener(|_, _| {
            Box::pin(async { http("401 Unauthorized", "missing or invalid bearer token") })
        })
        .await;
        let (newer, _) = listener(|_, _| {
            Box::pin(async {
                let reply = HelloReply {
                    version: PROTOCOL + 1,
                    nonce: random_token(),
                    proof: random_token(),
                };
                http("200 OK", &serde_json::to_string(&reply).unwrap())
            })
        })
        .await;
        for port in [old, newer] {
            let result = BridgeClient::new(port, key.clone())
                .confirm(&question())
                .await;
            assert!(
                matches!(result, Err(BridgeError::Incompatible { .. })),
                "{result:?}"
            );
            let decision = confirmer(port, &key).confirm(&question()).await;
            let reason = reason(&decision);
            assert!(reason.contains("different versions"), "{reason}");
            assert!(
                reason.contains("Update Kubach") && reason.contains("restart"),
                "{reason}"
            );
        }
        let seen = sync::lock(&seen).clone();
        assert!(!contains(&seen, &key) && !contains(&seen, SUMMARY));
        assert!(
            !contains(&seen, "authorization"),
            "no fallback to a bearer token"
        );
    }

    #[test]
    fn unused_nonces_expire_and_are_bounded() {
        let gate = Gate::new("key".into());
        let reply = gate.greet(&random_token()).unwrap();
        assert!(gate.take(&reply.nonce).is_some());
        assert!(gate.take(&reply.nonce).is_none(), "one use");
        assert!(gate.take("never-given").is_none());

        let reply = gate.greet(&random_token()).unwrap();
        let past = Instant::now() - Duration::from_secs(1);
        sync::lock(&gate.pending).get_mut(&reply.nonce).unwrap().1 = past;
        assert!(gate.take(&reply.nonce).is_none(), "expired");

        for _ in 0..MAX_PENDING {
            assert!(gate.greet(&random_token()).is_some());
        }
        assert!(gate.greet(&random_token()).is_none(), "full");
        for (_, expires) in sync::lock(&gate.pending).values_mut() {
            *expires = past;
        }
        assert!(
            gate.greet(&random_token()).is_some(),
            "expired ones make room"
        );
    }
}
