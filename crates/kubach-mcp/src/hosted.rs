//! Sessions the app runs on behalf of `kubach mcp` processes (#121, ADR
//! 0005 and 0008).
//!
//! A context imported into the encrypted vault has its credentials only in
//! the app's memory, and only while the vault is unlocked. A `kubach mcp`
//! process never gets them: it asks the app over the bridge to start the
//! session, then relays the agent's tool calls ([`crate::proxy`]). The app
//! keeps the session here, and ends it the moment the vault locks, the user
//! revokes it, its TTL runs out or the process stops pinging.
//!
//! An ended session leaves a [`Gone`] behind, so the process can tell the
//! agent why its next call failed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bridge::ExternalSession;
use crate::server::{CallOutcome, McpServer};
use crate::session::Session;
use kubach_core::sync;

/// Why a hosted session is no longer there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndReason {
    /// The vault locked (by hand, idle timer, screen lock) or the context
    /// left it. A new session can start once it is unlocked again.
    Locked,
    /// The user revoked it in the app; final.
    Revoked,
    /// Its TTL ran out; final.
    Expired,
    /// The app does not know it: restarted, or the process was silent too
    /// long. A new session may start.
    Unknown,
}

impl EndReason {
    /// Whether the process may ask for a new session (re-checked by the app
    /// against the vault and the settings).
    pub fn may_restart(self) -> bool {
        matches!(self, Self::Locked | Self::Unknown)
    }
}

/// What the process is told about an ended session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gone {
    pub reason: EndReason,
    /// For the agent.
    pub message: String,
}

impl Gone {
    pub fn locked(context: &str) -> Self {
        Self {
            reason: EndReason::Locked,
            message: format!(
                "The Kubach vault was locked, so the session on imported context `{context}` \
                 ended. Ask the user to unlock the vault in the Kubach window, then retry."
            ),
        }
    }

    pub fn revoked() -> Self {
        Self {
            reason: EndReason::Revoked,
            message: "The user revoked this session in Kubach. Restart the MCP server \
                      (`kubach mcp`) for a new one."
                .into(),
        }
    }

    pub fn expired() -> Self {
        Self {
            reason: EndReason::Expired,
            message: "The session's time ran out. Restart the MCP server (`kubach mcp`) for a \
                      new one."
                .into(),
        }
    }

    pub fn unknown() -> Self {
        Self {
            reason: EndReason::Unknown,
            message: "The Kubach app does not know this session (it was restarted or lost \
                      contact with `kubach mcp`)."
                .into(),
        }
    }
}

/// Ended sessions remembered for their processes; older ones are dropped.
const GONE_KEPT: usize = 64;

struct Entry {
    server: McpServer,
    pid: u32,
    /// Issued to the process; requests without it are not its own.
    key: String,
    seen: Instant,
}

/// Constant-time comparison of session keys.
fn key_matches(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[derive(Default)]
struct State {
    live: HashMap<String, Entry>,
    gone: HashMap<String, (Gone, Instant)>,
}

impl State {
    fn bury(&mut self, id: String, gone: Gone) {
        if self.gone.len() >= GONE_KEPT
            && let Some(oldest) = self
                .gone
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(id, _)| id.clone())
        {
            self.gone.remove(&oldest);
        }
        self.gone.insert(id, (gone, Instant::now()));
    }

    fn take(&mut self, id: &str, gone: Gone) -> Option<Arc<Session>> {
        let entry = self.live.remove(id)?;
        self.bury(id.to_owned(), gone);
        Some(entry.server.session().clone())
    }

    /// The live server of `id`, noting the contact; buries it when its
    /// session ended by itself. A wrong `key` is answered as an unknown
    /// session and changes nothing.
    fn server(&mut self, id: &str, key: &str) -> Result<McpServer, Gone> {
        let Some(entry) = self.live.get_mut(id) else {
            return Err(self
                .gone
                .get(id)
                .map(|(gone, _)| gone.clone())
                .unwrap_or_else(Gone::unknown));
        };
        if !key_matches(key, &entry.key) {
            return Err(Gone::unknown());
        }
        entry.seen = Instant::now();
        let session = entry.server.session();
        if session.is_active() {
            return Ok(entry.server.clone());
        }
        let gone = if session.scope().expired() {
            Gone::expired()
        } else {
            Gone::revoked()
        };
        self.take(id, gone.clone());
        Err(gone)
    }
}

/// Hosted sessions by id. Cheap to clone; clones share them.
///
/// Methods that end sessions return them; the caller closes them on the
/// I/O runtime ([`Session::close`] deletes a temporary ServiceAccount).
#[derive(Clone, Default)]
pub struct HostedSessions {
    state: Arc<Mutex<State>>,
}

impl HostedSessions {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        sync::lock(&self.state)
    }

    /// Hosts `server` for the process `pid`; returns the key the process
    /// must send with every request about it.
    pub fn insert(&self, server: McpServer, pid: u32) -> String {
        let id = server.session().id().to_owned();
        let key = crate::bridge::random_token();
        self.state().live.insert(
            id,
            Entry {
                server,
                pid,
                key: key.clone(),
                seen: Instant::now(),
            },
        );
        key
    }

    pub fn contains(&self, id: &str) -> bool {
        self.state().live.contains_key(id)
    }

    pub fn is_empty(&self) -> bool {
        self.state().live.is_empty()
    }

    /// Tools of session `id`.
    pub fn tools(&self, id: &str, key: &str) -> Result<Vec<Tool>, Gone> {
        self.state().server(id, key).map(|server| server.tools())
    }

    /// Runs a tool call in session `id`. The lock is not held during the
    /// call, so a long confirmation does not block locking the vault.
    pub async fn call(
        &self,
        id: &str,
        key: &str,
        client: &str,
        tool: &str,
        args: Map<String, Value>,
    ) -> Result<CallOutcome, Gone> {
        let server = self.state().server(id, key)?;
        Ok(server.call(client, tool, args).await)
    }

    /// The process of `id` is alive.
    pub fn ping(&self, id: &str, key: &str) -> Result<(), Gone> {
        self.state().server(id, key).map(drop)
    }

    /// The process ended its session. `None`, changing nothing, unless `id`
    /// is live under `key`.
    pub fn end(&self, id: &str, key: &str) -> Option<Arc<Session>> {
        let mut state = self.state();
        if !state
            .live
            .get(id)
            .is_some_and(|entry| key_matches(key, &entry.key))
        {
            return None;
        }
        state.gone.remove(id);
        state
            .live
            .remove(id)
            .map(|entry| entry.server.session().clone())
    }

    /// The live session `id`, e.g. to take back one right.
    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.state()
            .live
            .get(id)
            .map(|entry| entry.server.session().clone())
    }

    /// The user revoked `id`.
    pub fn revoke(&self, id: &str) -> Option<Arc<Session>> {
        self.state().take(id, Gone::revoked())
    }

    /// Ends every session `ends` gives a reason for, e.g. on vault lock.
    pub fn end_where(&self, mut ends: impl FnMut(&Session) -> Option<Gone>) -> Vec<Arc<Session>> {
        let mut state = self.state();
        let ending: Vec<(String, Gone)> = state
            .live
            .iter()
            .filter_map(|(id, entry)| Some((id.clone(), ends(entry.server.session())?)))
            .collect();
        ending
            .into_iter()
            .filter_map(|(id, gone)| state.take(&id, gone))
            .collect()
    }

    /// Ends sessions whose process was silent for `stale`, and those that
    /// ended by themselves (TTL).
    pub fn sweep(&self, stale: Duration) -> Vec<Arc<Session>> {
        let mut state = self.state();
        let ending: Vec<(String, Gone)> = state
            .live
            .iter()
            .filter_map(|(id, entry)| {
                let session = entry.server.session();
                let gone = if entry.seen.elapsed() >= stale {
                    Gone::unknown()
                } else if session.scope().expired() {
                    Gone::expired()
                } else if session.is_closed() {
                    Gone::revoked()
                } else {
                    return None;
                };
                Some((id.clone(), gone))
            })
            .collect();
        ending
            .into_iter()
            .filter_map(|(id, gone)| state.take(&id, gone))
            .collect()
    }

    /// Ends all sessions (the app quits).
    pub fn drain(&self) -> Vec<Arc<Session>> {
        let mut state = self.state();
        state.gone.clear();
        state
            .live
            .drain()
            .map(|(_, entry)| entry.server.session().clone())
            .collect()
    }

    /// Live sessions, for the sessions screen.
    pub fn list(&self) -> Vec<ExternalSession> {
        self.state()
            .live
            .values()
            .map(|entry| ExternalSession::with_pid(entry.server.session(), entry.pid))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Map;

    use super::{EndReason, Gone, HostedSessions};
    use crate::confirm::DenyAll;
    use crate::identity::Identity;
    use crate::server::McpServer;
    use crate::session::Session;
    use crate::settings::Profile;
    use crate::testutil::{catalog, permissive, scope_with};

    fn server(context: &str) -> McpServer {
        let mut scope = scope_with(Profile::ReadOnly, &permissive(), &["web"]);
        scope.context = context.to_owned();
        let config = kube::Config::new("http://127.0.0.1:9".parse().unwrap());
        let client = kube::Client::try_from(config).unwrap();
        let session = Session::from_parts(
            scope,
            Identity::ClientOnly { client },
            catalog(),
            HashSet::new(),
        );
        McpServer::new(session, Arc::new(DenyAll), None)
    }

    #[tokio::test]
    async fn serves_live_sessions_and_explains_ended_ones() {
        let hosted = HostedSessions::new();
        let a = server("prod-imported");
        let id = a.session().id().to_owned();
        let key = hosted.insert(a, 42);
        assert!(hosted.ping(&id, &key).is_ok());
        assert!(!hosted.tools(&id, &key).unwrap().is_empty());
        let outcome = hosted
            .call(&id, &key, "agent", "session_info", Map::new())
            .await
            .unwrap();
        assert!(!outcome.is_error, "{}", outcome.text);
        assert_eq!(hosted.list()[0].pid, 42);
        assert_eq!(hosted.list()[0].context, "prod-imported");

        assert_eq!(
            hosted.ping("nope", &key).unwrap_err().reason,
            EndReason::Unknown
        );

        let revoked = hosted.revoke(&id).unwrap();
        assert_eq!(revoked.id(), id);
        let gone = hosted
            .call(&id, &key, "agent", "session_info", Map::new())
            .await
            .unwrap_err();
        assert_eq!(gone, Gone::revoked());
        assert!(!gone.reason.may_restart());
        assert!(hosted.list().is_empty());

        // The process says goodbye: nothing left to end, and the reason
        // stays (ended sessions are dropped only when the list is full).
        assert!(hosted.end(&id, &key).is_none());
        assert_eq!(
            hosted.ping(&id, &key).unwrap_err().reason,
            EndReason::Revoked
        );
    }

    /// A session is used and ended only with its own key (#191).
    #[tokio::test]
    async fn other_processes_cannot_use_or_end_a_session() {
        let hosted = HostedSessions::new();
        let mine = server("prod-imported");
        let theirs = server("prod-imported");
        let (id, other_id) = (
            mine.session().id().to_owned(),
            theirs.session().id().to_owned(),
        );
        let key = hosted.insert(mine, 1);
        let other_key = hosted.insert(theirs, 2);
        assert_ne!(key, other_key);

        // Process 2 knows the id of session 1, but not its key.
        assert_eq!(
            hosted.ping(&id, &other_key).unwrap_err().reason,
            EndReason::Unknown
        );
        assert!(hosted.tools(&id, "").is_err());
        let gone = hosted
            .call(&id, &other_key, "agent", "session_info", Map::new())
            .await
            .unwrap_err();
        assert_eq!(gone.reason, EndReason::Unknown);
        assert!(hosted.end(&id, &other_key).is_none());
        assert!(hosted.contains(&id));
        assert!(hosted.ping(&id, &key).is_ok());

        // Each ends its own.
        assert!(hosted.end(&other_id, &other_key).is_some());
        assert!(hosted.end(&id, &key).is_some());
        assert!(hosted.is_empty());
    }

    #[tokio::test]
    async fn locking_ends_only_sessions_of_unavailable_contexts() {
        let hosted = HostedSessions::new();
        let imported = server("prod-imported");
        let other = server("staging-imported");
        let (imported_id, other_id) = (
            imported.session().id().to_owned(),
            other.session().id().to_owned(),
        );
        let imported_key = hosted.insert(imported, 1);
        let other_key = hosted.insert(other, 2);

        // The vault still has `staging-imported`, not `prod-imported`.
        let ended = hosted.end_where(|session| {
            let context = &session.scope().context;
            (context != "staging-imported").then(|| Gone::locked(context))
        });
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].id(), imported_id);
        let gone = hosted.tools(&imported_id, &imported_key).unwrap_err();
        assert_eq!(gone.reason, EndReason::Locked);
        assert!(gone.reason.may_restart());
        assert!(
            gone.message.contains("unlock the vault"),
            "{}",
            gone.message
        );
        assert!(hosted.ping(&other_id, &other_key).is_ok());

        // Everything locked.
        let ended = hosted.end_where(|session| Some(Gone::locked(&session.scope().context)));
        assert_eq!(ended.len(), 1);
        assert!(hosted.is_empty());
    }

    #[tokio::test]
    async fn sweep_drops_silent_and_closed_sessions() {
        let hosted = HostedSessions::new();
        let a = server("a");
        let b = server("b");
        let (a_id, b_id) = (a.session().id().to_owned(), b.session().id().to_owned());
        let b_session = b.session().clone();
        let a_key = hosted.insert(a, 1);
        let b_key = hosted.insert(b, 2);
        assert!(hosted.sweep(Duration::from_secs(60)).is_empty());

        b_session.close().await.unwrap();
        let ended = hosted.sweep(Duration::from_secs(60));
        assert_eq!(ended.len(), 1);
        assert_eq!(
            hosted.ping(&b_id, &b_key).unwrap_err().reason,
            EndReason::Revoked
        );

        let ended = hosted.sweep(Duration::ZERO);
        assert_eq!(ended.len(), 1);
        assert_eq!(
            hosted.ping(&a_id, &a_key).unwrap_err().reason,
            EndReason::Unknown
        );

        assert!(hosted.drain().is_empty());
    }
}
