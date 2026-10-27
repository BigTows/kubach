//! Changes run in the context the user asked for them in (#344).
//!
//! The active context can change while a confirmation is open or a call is
//! in flight: the user switches (Cmd-1..9, the hotbar), or it switches by
//! itself (the vault locks, the kubeconfig is reloaded and the default
//! context connects). A change therefore captures the [`Connection`] when it
//! is requested and checks it is still the active one before it runs, like
//! the YAML editor does with its own connection.

use std::fmt;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use kubach_core::Connection;
use kubach_ui::ConfirmHandle;

use crate::cluster::Cluster;

/// Why a change captured in one context no longer runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextChanged {
    /// Context the change was asked in.
    pub requested: String,
    /// The active context now, if any.
    pub active: Option<String>,
}

impl fmt::Display for ContextChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.active.as_deref() {
            Some(active) if active != self.requested => {
                f.write_str(&kubach_i18n::t!("helm-context-changed", context = active))
            }
            // Same name: reconnected (perhaps to a reloaded kubeconfig).
            Some(active) => f.write_str(&kubach_i18n::t!(
                "helm-context-reconnected",
                context = active
            )),
            None => f.write_str(&kubach_i18n::t!(
                "helm-context-disconnected",
                context = self.requested.as_str()
            )),
        }
    }
}

/// Whether a change asked in `requested` may run, given the active context
/// and whether its connection is still the captured one.
pub fn verdict(
    requested: &str,
    active: Option<&str>,
    same_connection: bool,
) -> Result<(), ContextChanged> {
    if same_connection && active == Some(requested) {
        Ok(())
    } else {
        Err(ContextChanged {
            requested: requested.to_owned(),
            active: active.map(str::to_owned),
        })
    }
}

/// Whether `captured` is still the active connection.
pub fn check(cluster: &Cluster, captured: &Connection) -> Result<(), ContextChanged> {
    let same = cluster
        .connection()
        .is_some_and(|active| active.same_as(captured));
    verdict(&captured.context, cluster.active_name(), same)
}

/// Tells the user a change was not run, in the active window once the
/// current update is over.
pub fn refuse(changed: &ContextChanged, cx: &mut App) {
    let message = changed.to_string();
    cx.defer(move |cx| {
        let Some(handle) = cx
            .active_window()
            .or_else(|| cx.windows().into_iter().next())
        else {
            return;
        };
        handle
            .update(cx, |_, window, cx| {
                window.push_notification(Notification::warning(message), cx)
            })
            .ok();
    });
}

/// [`check`], telling the user when the change does not run.
pub fn still_active(cluster: &Entity<Cluster>, captured: &Connection, cx: &mut App) -> bool {
    match check(cluster.read(cx), captured) {
        Ok(()) => true,
        Err(changed) => {
            refuse(&changed, cx);
            false
        }
    }
}

/// `command` (a `kubectl …` line) with the context it runs in.
pub fn kubectl_in_context(command: &str, context: &str) -> String {
    match command.strip_prefix("kubectl ") {
        Some(rest) => format!("kubectl --context {} {rest}", shell_word(context)),
        None => command.to_owned(),
    }
}

/// `word` quoted for a POSIX shell when it needs it.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/@=+,".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// A tab title with the context it belongs to: `web-0 · staging`.
pub fn title_in_context(title: &str, context: Option<&str>) -> String {
    match context {
        Some(context) => format!("{title} · {context}"),
        None => title.to_owned(),
    }
}

/// The connection of `context` for a tab that belongs to it, never the
/// active context's: a tab opened in staging stays in staging.
pub fn tab_connection<'a>(
    cluster: &'a Cluster,
    context: Option<&str>,
) -> Result<&'a Connection, String> {
    let Some(context) = context else {
        return Err(kubach_i18n::t!("dock-not-connected"));
    };
    cluster
        .connection_of(context)
        .ok_or_else(|| not_connected(context))
}

/// Why a tab of `context` can't (re)connect.
fn not_connected(context: &str) -> String {
    kubach_i18n::t!("dock-context-not-connected", context = context)
}

/// Confirmations open on a screen with the connection each was asked in;
/// [`close_stale`](Self::close_stale) closes those whose context is gone.
#[derive(Default)]
pub struct OpenConfirms(Vec<(ConfirmHandle, Connection)>);

impl OpenConfirms {
    pub fn track(&mut self, handle: ConfirmHandle, connection: Connection) {
        self.0.retain(|(handle, _)| handle.is_open());
        self.0.push((handle, connection));
    }

    /// Closes the confirmations asked in another connection than the
    /// active one; call it whenever the cluster changes.
    pub fn close_stale(&mut self, cluster: &Entity<Cluster>, cx: &mut App) {
        if self.0.is_empty() {
            return;
        }
        let cluster = cluster.read(cx);
        let (keep, stale): (Vec<_>, Vec<_>) = self
            .0
            .drain(..)
            .filter(|(handle, _)| handle.is_open())
            .partition(|(_, connection)| check(cluster, connection).is_ok());
        self.0 = keep;
        for (handle, _) in stale {
            handle.dismiss(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextChanged, kubectl_in_context, not_connected, title_in_context, verdict};

    #[test]
    fn runs_only_in_the_captured_connection() {
        assert_eq!(verdict("staging", Some("staging"), true), Ok(()));
        let switched = verdict("staging", Some("prod"), false).unwrap_err();
        assert_eq!(
            switched.to_string(),
            "Context changed to prod; action cancelled"
        );
        // Same name, new connection (reconnect, reloaded kubeconfig).
        let reconnected = verdict("staging", Some("staging"), false).unwrap_err();
        assert_eq!(
            reconnected.to_string(),
            "Reconnected to staging; action cancelled"
        );
        let gone = verdict("staging", None, false).unwrap_err();
        assert_eq!(
            gone,
            ContextChanged {
                requested: "staging".into(),
                active: None
            }
        );
        assert_eq!(
            gone.to_string(),
            "Disconnected from staging; action cancelled"
        );
        // A connection that is not the active context's (kept connected
        // in the background) does not count.
        assert!(verdict("staging", Some("prod"), true).is_err());
    }

    #[test]
    fn commands_name_the_context() {
        assert_eq!(
            kubectl_in_context("kubectl delete pods a b -n ns", "minikube"),
            "kubectl --context minikube delete pods a b -n ns"
        );
        assert_eq!(
            kubectl_in_context(
                "kubectl delete pods a",
                "arn:aws:eks:eu-west-1:1:cluster/prod"
            ),
            "kubectl --context arn:aws:eks:eu-west-1:1:cluster/prod delete pods a"
        );
        assert_eq!(
            kubectl_in_context("kubectl delete pods a", "my ctx"),
            "kubectl --context 'my ctx' delete pods a"
        );
        assert_eq!(
            kubectl_in_context("kubectl delete pods a", "it's"),
            r"kubectl --context 'it'\''s' delete pods a"
        );
        assert_eq!(kubectl_in_context("helm list", "x"), "helm list");
    }

    #[test]
    fn tab_titles_name_the_context() {
        assert_eq!(
            title_in_context("web-0", Some("staging")),
            "web-0 · staging"
        );
        assert_eq!(title_in_context("web-0", None), "web-0");
        assert!(not_connected("staging").contains("staging"));
    }
}
