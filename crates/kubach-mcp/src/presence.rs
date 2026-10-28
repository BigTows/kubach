//! Whether an HTTP agent is still connected to the app's MCP endpoint
//! (#34), so its port-forwards do not outlive it.
//!
//! The app's HTTP session is shared by the agents holding its token and
//! lives until its TTL, so that a client that restarts or reconnects goes
//! on without asking the user again. Its port-forwards do not: they stop
//! - at once when a client ends the last MCP session it opened (`DELETE`
//!   with `Mcp-Session-Id`), and
//! - after [`DISCONNECT_GRACE`] without any open request or stream (the
//!   client's `GET` SSE stream counts) and without a new request: the
//!   client crashed, closed without saying goodbye or lost the connection.
//!
//! An agent over stdio (`kubach mcp`) is not tracked here: the end of stdin
//! ends its session, forwards included.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use http::{Method, StatusCode};
use kubach_core::sync;
use tokio::sync::Notify;
use tokio::time::Instant;

/// How long the endpoint may go without an open request or stream, and
/// without a new request, before the agent counts as gone.
pub const DISCONNECT_GRACE: Duration = Duration::from_secs(120);

/// The header carrying a legacy MCP session's id.
pub(crate) const SESSION_HEADER: &str = "mcp-session-id";

#[derive(Default)]
struct State {
    /// Requests and streams being answered now.
    open: usize,
    /// When a request last started or ended.
    last_seen: Option<Instant>,
    /// Legacy MCP sessions the endpoint handed out and nobody closed.
    sessions: HashSet<String>,
    /// The last session was closed with `DELETE`, and nothing came since.
    left: bool,
}

/// What the endpoint knows of its clients' connections.
#[derive(Default)]
pub(crate) struct Presence {
    state: Mutex<State>,
    /// Woken when the last open request ends.
    idle: Notify,
}

impl Presence {
    /// A request starts; the returned guard ends it when dropped (with the
    /// response body, so a stream stays open as long as it is read).
    pub(crate) fn enter(self: &std::sync::Arc<Self>) -> Visit {
        let mut state = sync::lock(&self.state);
        state.open += 1;
        state.last_seen = Some(Instant::now());
        state.left = false;
        Visit {
            presence: self.clone(),
        }
    }

    fn leave(&self) {
        let idle = {
            let mut state = sync::lock(&self.state);
            state.open = state.open.saturating_sub(1);
            state.last_seen = Some(Instant::now());
            state.open == 0
        };
        if idle {
            self.idle.notify_one();
        }
    }

    /// Notes what a request did to the MCP sessions: a session handed out,
    /// one closed with `DELETE`, or one the server no longer knows.
    pub(crate) fn answered(
        &self,
        method: &Method,
        asked: Option<&str>,
        status: StatusCode,
        handed_out: Option<&str>,
    ) {
        let mut state = sync::lock(&self.state);
        if status.is_success()
            && let Some(id) = handed_out
        {
            state.sessions.insert(id.to_owned());
        }
        let Some(asked) = asked else {
            return;
        };
        if *method == Method::DELETE && status.is_success() {
            state.sessions.remove(asked);
            if state.sessions.is_empty() {
                state.left = true;
            }
        } else if status == StatusCode::NOT_FOUND {
            state.sessions.remove(asked);
        }
    }

    /// Whether the agent is gone at `now`: nothing open, and either it
    /// closed its last session or nothing came for `grace`.
    pub(crate) fn gone(&self, grace: Duration, now: Instant) -> bool {
        let state = sync::lock(&self.state);
        state.open == 0
            && (state.left
                || state
                    .last_seen
                    .is_some_and(|seen| now.saturating_duration_since(seen) >= grace))
    }

    /// Waits until the last open request ends, or `limit` passes.
    pub(crate) async fn idle_or(&self, limit: Duration) {
        let _ = tokio::time::timeout(limit, self.idle.notified()).await;
    }
}

/// One open request or stream; see [`Presence::enter`].
pub(crate) struct Visit {
    presence: std::sync::Arc<Presence>,
}

impl Drop for Visit {
    fn drop(&mut self) {
        self.presence.leave();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use http::{Method, StatusCode};
    use tokio::time::Instant;

    use super::Presence;

    const GRACE: Duration = Duration::from_secs(120);

    fn later(by: u64) -> Instant {
        Instant::now() + Duration::from_secs(by)
    }

    #[tokio::test]
    async fn nobody_ever_came_is_not_gone() {
        let presence = Presence::default();
        assert!(!presence.gone(GRACE, later(3600)));
    }

    #[tokio::test]
    async fn an_open_stream_keeps_the_agent_present() {
        let presence = Arc::new(Presence::default());
        let stream = presence.enter();
        assert!(!presence.gone(GRACE, later(3600)));
        drop(stream);
        // Gone only once the grace period passed without a request.
        assert!(!presence.gone(GRACE, later(60)));
        assert!(presence.gone(GRACE, later(121)));
        // A new request brings it back.
        drop(presence.enter());
        assert!(!presence.gone(GRACE, later(60)));
    }

    #[tokio::test]
    async fn closing_the_last_session_is_leaving_at_once() {
        let presence = Arc::new(Presence::default());
        let (a, b) = (Some("a"), Some("b"));
        presence.answered(&Method::POST, None, StatusCode::OK, a);
        presence.answered(&Method::POST, None, StatusCode::OK, b);
        // One of two clients leaves: the other one may still be working.
        presence.answered(&Method::DELETE, a, StatusCode::ACCEPTED, None);
        assert!(!presence.gone(GRACE, Instant::now()));
        // The server forgot `b` (timed out): it is not waited for.
        presence.answered(&Method::POST, b, StatusCode::NOT_FOUND, None);
        presence.answered(&Method::POST, None, StatusCode::OK, Some("c"));
        presence.answered(&Method::DELETE, Some("c"), StatusCode::ACCEPTED, None);
        assert!(presence.gone(GRACE, Instant::now()));
        // An agent connecting again is present again.
        let visit = presence.enter();
        drop(visit);
        assert!(!presence.gone(GRACE, Instant::now()));
    }

    #[tokio::test]
    async fn a_refused_delete_changes_nothing() {
        let presence = Arc::new(Presence::default());
        drop(presence.enter());
        presence.answered(&Method::POST, None, StatusCode::OK, Some("a"));
        presence.answered(&Method::DELETE, Some("x"), StatusCode::NOT_FOUND, None);
        presence.answered(&Method::DELETE, Some("a"), StatusCode::BAD_REQUEST, None);
        assert!(!presence.gone(GRACE, Instant::now()));
    }
}
