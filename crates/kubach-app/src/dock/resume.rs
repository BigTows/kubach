//! A dock tab the vault lock closed comes back when the vault is unlocked
//! (#213, ADR 0008).
//!
//! The tab itself remembers that the lock ended it, by keeping a
//! [`Resume`]; its target (pod, container, node, context) it knows anyway.
//! Nothing else is kept, and nothing of it is a secret or leaves the
//! process. The memory goes with the tab when the user closes it, when the
//! tab reconnects by hand, and when the vault is reset.
//!
//! After the unlock the tab waits for its context to be listed again,
//! connects it in the background if nothing else does (the active context
//! is connected by [`Cluster`] itself) and then starts a new session, like
//! its Reconnect button does. A context that can't be connected leaves the
//! tab with the reason.

use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_i18n::t;

use super::forwards::{Step, restore_step};
use crate::cluster::Cluster;
use crate::vault::{self, AfterLock};

/// What a tab closed by the lock does now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// The vault is still locked, or its contexts are being read.
    Wait,
    /// Unlocked; the context of the tab is connecting.
    Connecting,
    /// The context is connected: start a new session.
    Start,
    /// The context could not be connected; the tab stays with the reason.
    Fail(String),
    /// Nothing to come back to (the vault was reset).
    Forget,
}

/// [`Next`], with the context still to be connected in the background.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Decision {
    Connect,
    Next(Next),
}

/// Where a context is, as a tab waiting for it sees it.
struct ContextSeen<'a> {
    name: &'a str,
    /// The kubeconfig files have not been read yet.
    loading: bool,
    /// Listed, and its vault unlocked.
    known: bool,
    state: &'a ConnectionState,
    connected: bool,
}

/// What a tab does given the vault and its context; `requested`: the tab
/// already asked for the context to be connected.
fn decide(after: AfterLock, context: &ContextSeen, requested: bool) -> Decision {
    match after {
        AfterLock::Forget => return Decision::Next(Next::Forget),
        AfterLock::Wait => return Decision::Next(Next::Wait),
        AfterLock::Resume => {}
    }
    if context.loading {
        return Decision::Next(Next::Wait);
    }
    let name = context.name;
    Decision::Next(
        match restore_step(context.known, context.state, context.connected, requested) {
            Step::Wait { known: false } => Next::Wait,
            Step::Wait { known: true } => Next::Connecting,
            Step::Connect => return Decision::Connect,
            Step::Start => Next::Start,
            Step::CannotConnect(error) => Next::Fail(t!(
                "dock-vault-reopen-cannot-connect",
                context = name,
                error = error
            )),
            Step::Dropped => Next::Fail(t!("dock-vault-reopen-disconnected", context = name)),
        },
    )
}

/// Kept by a tab from the lock that closed it until it is back.
pub struct Resume {
    /// The tab asked for its context to be connected.
    requested: bool,
    _watch: Vec<Subscription>,
}

impl Resume {
    /// Calls `poll` on the tab whenever the vault or the contexts change;
    /// the tab then asks [`next`](Self::next).
    pub fn new<T: 'static>(
        cluster: &Entity<Cluster>,
        cx: &mut Context<T>,
        poll: fn(&mut T, &mut Context<T>),
    ) -> Self {
        let mut watch = vec![cx.observe(cluster, move |tab, _, cx| poll(tab, cx))];
        if let Some(vault) = vault::global(cx) {
            watch.push(cx.observe(&vault, move |tab, _, cx| poll(tab, cx)));
        }
        Self {
            requested: false,
            _watch: watch,
        }
    }

    /// What the tab of `context` does now. Connects the context in the
    /// background, once, when it is listed and nothing connects it.
    pub fn next(&mut self, cluster: &Entity<Cluster>, context: Option<&str>, cx: &mut App) -> Next {
        let Some(context) = context else {
            return Next::Forget;
        };
        let decision = {
            let cluster = cluster.read(cx);
            let known = cluster.is_available(context, cx);
            let seen = ContextSeen {
                name: context,
                loading: cluster.is_loading(),
                known,
                state: cluster.state_of(context),
                connected: known && cluster.connection_of(context).is_some(),
            };
            decide(vault::after_lock_now(cx), &seen, self.requested)
        };
        match decision {
            Decision::Next(next) => next,
            Decision::Connect => {
                self.requested = true;
                let name = context.to_owned();
                cluster.update(cx, |cluster, cx| cluster.connect_in_background(name, cx));
                Next::Connecting
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::ConnectionState;

    use super::{ContextSeen, Decision, Next, decide};
    use crate::vault::AfterLock;

    fn seen(known: bool, state: &ConnectionState, connected: bool) -> ContextSeen<'_> {
        ContextSeen {
            name: "prod",
            loading: false,
            known,
            state,
            connected,
        }
    }

    #[test]
    fn a_tab_waits_for_the_unlock_then_for_its_context() {
        let disconnected = ConnectionState::Disconnected;
        let connecting = ConnectionState::Connecting;
        let wait = Decision::Next(Next::Wait);
        // Locked: nothing happens, whatever the context looks like.
        let listed = seen(true, &disconnected, false);
        assert_eq!(decide(AfterLock::Wait, &listed, false), wait);
        // Unlocked, the contexts are not read again yet.
        let unlisted = seen(false, &disconnected, false);
        assert_eq!(decide(AfterLock::Resume, &unlisted, false), wait);
        let loading = ContextSeen {
            loading: true,
            ..seen(true, &disconnected, false)
        };
        assert_eq!(decide(AfterLock::Resume, &loading, false), wait);
        // Listed and nobody connects it: the tab does, once.
        assert_eq!(decide(AfterLock::Resume, &listed, false), Decision::Connect);
        // Connecting (by the tab, or as the active context).
        for requested in [false, true] {
            assert_eq!(
                decide(
                    AfterLock::Resume,
                    &seen(true, &connecting, false),
                    requested
                ),
                Decision::Next(Next::Connecting)
            );
        }
        // Connected: a new session.
        let connected = ConnectionState::Connected {
            server_version: String::new(),
        };
        assert_eq!(
            decide(AfterLock::Resume, &seen(true, &connected, true), true),
            Decision::Next(Next::Start)
        );
    }

    #[test]
    fn a_context_that_does_not_connect_leaves_the_tab_with_the_reason() {
        let error = ConnectionState::Error("token expired".into());
        let Decision::Next(Next::Fail(message)) =
            decide(AfterLock::Resume, &seen(true, &error, false), true)
        else {
            panic!("the tab fails");
        };
        assert!(message.contains("prod") && message.contains("token expired"));
        // Connected for the tab and disconnected again (pushed out of the
        // background connections, or by the user).
        let disconnected = ConnectionState::Disconnected;
        let Decision::Next(Next::Fail(message)) =
            decide(AfterLock::Resume, &seen(true, &disconnected, false), true)
        else {
            panic!("the tab fails");
        };
        assert!(message.contains("prod"));
    }

    #[test]
    fn a_reset_vault_has_nothing_to_come_back_to() {
        let connected = ConnectionState::Connected {
            server_version: String::new(),
        };
        assert_eq!(
            decide(AfterLock::Forget, &seen(true, &connected, true), false),
            Decision::Next(Next::Forget)
        );
    }
}
