//! Texts of the status bar at the bottom of the main window, without UI:
//! the namespace label, the watch state, port forwards, MCP and the version
//! summary copied to the clipboard. Labels are [`Message`]s the UI
//! translates (ADR 0018); the version summary stays English, for bug
//! reports.

use kubach_i18n::{Message, msg};

use crate::ConnectionState;

/// State of the watches of the active connection, from
/// [`ResourceCache::sync_summary`](crate::cache::ResourceCache::sync_summary).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncSummary {
    /// Watches whose initial list has not completed yet.
    pub loading: usize,
    /// Watches that lost the connection and retry (not "forbidden").
    pub reconnecting: usize,
}

impl SyncSummary {
    pub fn is_syncing(&self) -> bool {
        self.loading > 0 || self.reconnecting > 0
    }

    /// Whether a list is loading: the loading bar runs only then, not while
    /// watches merely retry (#316).
    pub fn is_loading(&self) -> bool {
        self.loading > 0
    }

    /// "Syncing…" during the initial sync, "Reconnecting…" while watches
    /// only retry; nothing when the cache mirrors the cluster.
    pub fn label(&self) -> Option<Message> {
        if self.is_loading() {
            Some(msg!("status-syncing"))
        } else if self.reconnecting > 0 {
            Some(msg!("status-reconnecting"))
        } else {
            None
        }
    }

    /// Details for the tooltip.
    pub fn details(&self) -> Option<Message> {
        match (self.loading, self.reconnecting) {
            (0, 0) => None,
            (loading, 0) => Some(msg!("status-sync-loading", count = loading)),
            (0, reconnecting) => Some(msg!("status-sync-reconnecting", count = reconnecting)),
            (loading, reconnecting) => Some(msg!(
                "status-sync-both",
                loading = loading,
                reconnecting = reconnecting
            )),
        }
    }
}

/// Namespaces shown: `None` for all of them, else the selected ones.
/// `web` for one, `3 namespaces` for several, `All namespaces` for all.
pub fn namespace_label<S: AsRef<str>>(selected: Option<&[S]>) -> Message {
    match selected {
        None => msg!("status-all-namespaces"),
        Some([]) => msg!("status-no-namespace"),
        Some([one]) => msg!("status-namespace", namespace = one.as_ref()),
        Some(many) => msg!("status-namespaces", count = many.len()),
    }
}

/// Text next to the connection dot: the context and, once connected, the
/// Kubernetes version (`minikube · v1.31.0`).
pub fn cluster_label(context: Option<&str>, state: &ConnectionState) -> Message {
    let Some(context) = context else {
        return msg!("status-no-cluster");
    };
    match state {
        ConnectionState::Connected { server_version } if !server_version.is_empty() => msg!(
            "status-cluster-version",
            context = context,
            version = server_version
        ),
        ConnectionState::Connecting => msg!("status-cluster-connecting", context = context),
        ConnectionState::Error(_) => msg!("status-cluster-error", context = context),
        _ => msg!("status-cluster", context = context),
    }
}

/// `1 port forward`, `2 port forwards`; `None` (item hidden) with none.
pub fn forwards_label(active: usize) -> Option<Message> {
    (active > 0).then(|| msg!("status-forwards", count = active))
}

/// What the MCP item shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpStatus {
    /// No HTTP server and no `kubach mcp` sessions.
    Off,
    /// The HTTP server failed to start and nothing else is running.
    Failed,
    /// The HTTP server runs or `kubach mcp` processes are connected.
    On { sessions: usize, pending: usize },
}

impl McpStatus {
    /// From the server state (`running`, `failed`) and the counts.
    pub fn new(running: bool, failed: bool, sessions: usize, pending: usize) -> Self {
        if running || sessions > 0 || pending > 0 {
            Self::On { sessions, pending }
        } else if failed {
            Self::Failed
        } else {
            Self::Off
        }
    }

    pub fn label(&self) -> Message {
        match self {
            Self::Off => msg!("status-mcp-off"),
            Self::Failed => msg!("status-mcp-failed"),
            Self::On { pending, .. } if *pending > 0 => {
                msg!("status-mcp-pending", count = *pending)
            }
            Self::On { sessions: 0, .. } => msg!("status-mcp-no-sessions"),
            Self::On { sessions, .. } => msg!("status-mcp-sessions", count = *sessions),
        }
    }

    /// A mutating call waits for the user's confirmation.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Self::On { pending, .. } if *pending > 0)
    }
}

/// `Kubach 2026.10.1`.
pub fn version_label(version: &str) -> String {
    format!("Kubach {version}")
}

/// Text copied by a click on the version: Kubach version, OS and the
/// version of the connected cluster, for bug reports.
pub fn version_summary(
    version: &str,
    os: &str,
    arch: &str,
    context: Option<&str>,
    state: &ConnectionState,
) -> String {
    let kubernetes = match (context, state) {
        (Some(context), ConnectionState::Connected { server_version }) => {
            format!("{server_version} (context {context})")
        }
        (Some(context), _) => format!("not connected (context {context})"),
        (None, _) => "no cluster".to_owned(),
    };
    format!(
        "{}\nOS: {os} {arch}\nKubernetes: {kubernetes}",
        version_label(version)
    )
}

/// Longest error kept in a status bar notice, in characters.
pub const NOTICE_ERROR_MAX: usize = 120;

/// An error cut down for a status bar notice (#459): its first non-empty
/// line, clipped to [`NOTICE_ERROR_MAX`] characters with an ellipsis. The
/// full text stays in the operation's output.
pub fn short_error(error: &str) -> String {
    let line = error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= NOTICE_ERROR_MAX {
        return line.to_owned();
    }
    let mut short: String = line.chars().take(NOTICE_ERROR_MAX - 1).collect();
    short.truncate(short.trim_end().len());
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_error_keeps_the_first_line_clipped() {
        assert_eq!(short_error(""), "");
        assert_eq!(
            short_error("\n  Error: INSTALLATION FAILED: timed out\nmore\n"),
            "Error: INSTALLATION FAILED: timed out"
        );
        let long = "ошибка ".repeat(40);
        let short = short_error(&long);
        assert_eq!(short.chars().count(), NOTICE_ERROR_MAX - 1);
        assert!(short.ends_with("ка…"));
    }

    #[test]
    fn sync_label_only_while_syncing() {
        let idle = SyncSummary::default();
        assert_eq!(idle.label(), None);
        assert_eq!(idle.details(), None);

        let loading = SyncSummary {
            loading: 2,
            reconnecting: 0,
        };
        assert_eq!(
            loading.label().map(|m| m.english()).as_deref(),
            Some("Syncing…")
        );
        assert_eq!(
            loading.details().map(|m| m.english()).as_deref(),
            Some("2 watches loading")
        );

        let both = SyncSummary {
            loading: 1,
            reconnecting: 1,
        };
        assert_eq!(
            both.details().map(|m| m.english()).as_deref(),
            Some("1 watch loading, 1 watch reconnecting")
        );
        assert_eq!(
            both.label().map(|m| m.english()).as_deref(),
            Some("Syncing…")
        );
        assert!(both.is_loading());

        // Retrying is not progress: another label, no loading bar.
        let reconnecting = SyncSummary {
            loading: 0,
            reconnecting: 1,
        };
        assert_eq!(
            reconnecting.label().map(|m| m.english()).as_deref(),
            Some("Reconnecting…")
        );
        assert!(reconnecting.is_syncing());
        assert!(!reconnecting.is_loading());
    }

    #[test]
    fn namespace_labels() {
        assert_eq!(namespace_label::<&str>(None).english(), "All namespaces");
        assert_eq!(namespace_label(Some(&["web"][..])).english(), "web");
        assert_eq!(
            namespace_label(Some(&["web", "api", "db"][..])).english(),
            "3 namespaces"
        );
        assert_eq!(namespace_label::<&str>(Some(&[])).english(), "No namespace");
    }

    #[test]
    fn cluster_labels() {
        let connected = ConnectionState::Connected {
            server_version: "v1.31.0".into(),
        };
        assert_eq!(cluster_label(None, &connected).english(), "No cluster");
        assert_eq!(
            cluster_label(Some("minikube"), &connected).english(),
            "minikube · v1.31.0"
        );
        assert_eq!(
            cluster_label(Some("minikube"), &ConnectionState::Connecting).english(),
            "minikube · connecting…"
        );
        assert_eq!(
            cluster_label(Some("minikube"), &ConnectionState::Disconnected).english(),
            "minikube"
        );
        assert_eq!(
            cluster_label(Some("prod"), &ConnectionState::Error("boom".into())).english(),
            "prod · error"
        );
    }

    #[test]
    fn forwards_hidden_without_any() {
        assert_eq!(forwards_label(0), None);
        assert_eq!(
            forwards_label(1).map(|m| m.english()).as_deref(),
            Some("1 port forward")
        );
        assert_eq!(
            forwards_label(4).map(|m| m.english()).as_deref(),
            Some("4 port forwards")
        );
        // The UI translates the same message.
        assert_eq!(
            forwards_label(5)
                .map(|m| m.translate(kubach_i18n::Locale::Ru))
                .as_deref(),
            Some("5 переадресаций портов")
        );
    }

    #[test]
    fn mcp_states() {
        assert_eq!(McpStatus::new(false, false, 0, 0), McpStatus::Off);
        assert_eq!(
            McpStatus::new(false, false, 0, 0).label().english(),
            "MCP off"
        );
        assert_eq!(
            McpStatus::new(false, true, 0, 0).label().english(),
            "MCP failed"
        );
        assert_eq!(
            McpStatus::new(true, false, 0, 0).label().english(),
            "MCP · no sessions"
        );
        assert_eq!(
            McpStatus::new(true, false, 1, 0).label().english(),
            "MCP · 1 session"
        );
        // `kubach mcp` sessions count even with the HTTP server off.
        let external = McpStatus::new(false, false, 3, 0);
        assert_eq!(external.label().english(), "MCP · 3 sessions");
        assert!(!external.needs_attention());

        let waiting = McpStatus::new(true, false, 1, 2);
        assert!(waiting.needs_attention());
        assert_eq!(waiting.label().english(), "MCP · 2 to confirm");
    }

    #[test]
    fn version_summary_for_bug_reports() {
        assert_eq!(version_label("2026.10.1"), "Kubach 2026.10.1");
        let connected = ConnectionState::Connected {
            server_version: "v1.31.0".into(),
        };
        assert_eq!(
            version_summary(
                "2026.10.1",
                "macos",
                "aarch64",
                Some("minikube"),
                &connected
            ),
            "Kubach 2026.10.1\nOS: macos aarch64\nKubernetes: v1.31.0 (context minikube)"
        );
        assert_eq!(
            version_summary(
                "2026.10.1",
                "linux",
                "x86_64",
                Some("prod"),
                &ConnectionState::Connecting
            ),
            "Kubach 2026.10.1\nOS: linux x86_64\nKubernetes: not connected (context prod)"
        );
        assert_eq!(
            version_summary("2026.10.1", "windows", "x86_64", None, &connected),
            "Kubach 2026.10.1\nOS: windows x86_64\nKubernetes: no cluster"
        );
    }
}
