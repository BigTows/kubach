//! Active port-forwards, shared by the forward dialog and the sessions screen.
//!
//! Forwards started in a context are remembered in the application state
//! (#474) and restarted on the next run, with the same local port, once
//! their context is connected. A forward leaves the state only when the user
//! stops or removes it.
//!
//! A paused forward (#476) keeps its local port and forwards nothing; it
//! stays remembered as paused and comes back paused on the next run.

use std::collections::HashSet;

use futures::StreamExt as _;
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::runtime;
use kubach_core::state::SavedForward;
use kubach_core::streams::{
    ForwardId, ForwardInfo, ForwardSource, LocalPort, PortForwardManager, StreamError, fetch_source,
};
use kubach_i18n::t;
use kube::Client;

use crate::cluster::Cluster;
use crate::state::StateStore;

/// Forward `127.0.0.1:local_port` to `port` of `source` (a Service port for
/// Services, a pod port otherwise). `LocalPort::Exact(0)` takes a random
/// free port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRequest {
    /// Context the forward runs in; only then it is restored on the next run.
    pub context: Option<String>,
    pub source: ForwardSource,
    pub port: u16,
    pub local_port: LocalPort,
    pub port_name: Option<String>,
}

/// What a failed start says: a taken or forbidden local port in plain
/// words (#464), anything else as it is.
pub fn start_error(error: &StreamError) -> String {
    match error {
        StreamError::PortInUse(port) => t!("dock-forward-port-busy", port = port.to_string()),
        StreamError::PortDenied(port) => t!("dock-forward-port-denied", port = port.to_string()),
        error => error.to_string(),
    }
}

/// What a failed restart of a remembered forward says (#474).
pub fn restore_error(error: &StreamError) -> String {
    match error {
        StreamError::PortInUse(port) => {
            t!("dock-forwards-restore-port-busy", port = port.to_string())
        }
        StreamError::SourceGone(source) => t!("dock-forwards-restore-gone", source = source),
        error => start_error(error),
    }
}

/// Where a forward from the last run is on its way back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreStatus {
    /// Its context is not connected yet; `known` is false while the
    /// context is not among the loaded ones (a locked vault, a missing
    /// kubeconfig).
    Waiting {
        known: bool,
    },
    Starting,
    /// Could not start; Retry or Remove.
    Failed(String),
}

/// A forward from the last run that is not running yet.
pub struct Restore {
    /// Identifies the row on the screen.
    pub id: u64,
    pub saved: SavedForward,
    pub status: RestoreStatus,
    _task: Option<Task<()>>,
}

/// What a waiting restore does next, given its context.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Wait {
        known: bool,
    },
    /// Connect the context in the background.
    Connect,
    Start,
    CannotConnect(String),
    /// The context was connected for the restore and disconnected again
    /// (pushed out of the background connections, or by the user).
    Dropped,
}

/// The next step for restores in a context: start once it is connected,
/// connect it in the background once it is known (only once), wait for
/// one that is not loaded (locked vault) or still connecting.
fn restore_step(known: bool, state: &ConnectionState, connected: bool, requested: bool) -> Step {
    if connected {
        return Step::Start;
    }
    if !known {
        return Step::Wait { known };
    }
    match state {
        ConnectionState::Error(error) => Step::CannotConnect(error.clone()),
        ConnectionState::Disconnected if requested => Step::Dropped,
        ConnectionState::Disconnected => Step::Connect,
        ConnectionState::Connecting | ConnectionState::Connected { .. } => Step::Wait { known },
    }
}

/// Owns the [`PortForwardManager`]; forwards live until stopped or the app
/// quits, across context switches.
pub struct Forwards {
    manager: PortForwardManager,
    list: Vec<ForwardInfo>,
    /// Port names by forward, for "Open in browser".
    names: Vec<(ForwardId, Option<String>)>,
    /// What is remembered of the forwards started in a context, so that
    /// stopping one forgets it.
    saved: Vec<(ForwardId, SavedForward)>,
    /// Forwards of the last run not running yet, in saved order.
    restoring: Vec<Restore>,
    next_restore: u64,
    cluster: Option<WeakEntity<Cluster>>,
    /// Contexts connected in the background for restores.
    requested: HashSet<String>,
    _cluster: Option<Subscription>,
    _watch: Task<()>,
}

impl Forwards {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let manager = PortForwardManager::new();
        let mut changes = manager.subscribe();
        let watch = cx.spawn(async move |this, cx| {
            while changes.next().await.is_some() {
                while changes.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            manager,
            list: Vec::new(),
            names: Vec::new(),
            saved: Vec::new(),
            restoring: Vec::new(),
            next_restore: 0,
            cluster: None,
            requested: HashSet::new(),
            _cluster: None,
            _watch: watch,
        }
    }

    pub fn list(&self) -> &[ForwardInfo] {
        &self.list
    }

    /// The manager the AI agent's sessions run their forwards in (#17), so
    /// they show here and the user can stop them. They are never
    /// remembered for the next run.
    pub fn manager(&self) -> PortForwardManager {
        self.manager.clone()
    }

    /// Forwards of the last run not running yet.
    pub fn restoring(&self) -> &[Restore] {
        &self.restoring
    }

    /// Name of the forwarded container port, when it had one.
    pub fn port_name(&self, id: ForwardId) -> Option<&str> {
        self.names
            .iter()
            .find(|(forward, _)| *forward == id)
            .and_then(|(_, name)| name.as_deref())
    }

    /// Context a forward runs in, when it was started in one (an agent's
    /// in its session's context).
    pub fn context(&self, id: ForwardId) -> Option<&str> {
        self.saved
            .iter()
            .find(|(forward, _)| *forward == id)
            .map(|(_, saved)| saved.context.as_str())
            .or_else(|| {
                self.list
                    .iter()
                    .find(|info| info.id == id)
                    .and_then(|info| info.agent.as_ref())
                    .map(|agent| agent.context.as_str())
            })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.list = self.manager.list();
        let ids: Vec<ForwardId> = self.list.iter().map(|info| info.id).collect();
        self.names.retain(|(id, _)| ids.contains(id));
        self.saved.retain(|(id, _)| ids.contains(id));
        cx.notify();
    }

    /// Starts a forward; resolves once it listens.
    pub fn start(
        &mut self,
        client: Client,
        request: ForwardRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<ForwardInfo, String>> {
        let manager = self.manager.clone();
        let ForwardRequest {
            context,
            source,
            port,
            local_port,
            port_name,
        } = request;
        let (kind, name) = (source.kind.clone(), source.name.clone());
        let task =
            runtime::spawn(async move { manager.start(&client, &source, port, local_port).await });
        cx.spawn(async move |this, cx| {
            let info = match task.await {
                Ok(Ok(info)) => info,
                Ok(Err(error)) => return Err(start_error(&error)),
                Err(error) => return Err(error.to_string()),
            };
            this.update(cx, |this, cx| {
                if let Some(context) = context {
                    let saved = SavedForward {
                        context,
                        namespace: info.namespace.clone(),
                        kind,
                        name,
                        port,
                        local_port: info.local_addr.port(),
                        port_name: port_name.clone(),
                        paused: false,
                    };
                    this.started(&info, saved, port_name, cx);
                } else {
                    this.names.push((info.id, port_name));
                }
                this.refresh(cx);
            })
            .ok();
            Ok(info)
        })
    }

    /// A forward started in a context: remembered for the next run,
    /// replacing a restore of the same target.
    fn started(
        &mut self,
        info: &ForwardInfo,
        saved: SavedForward,
        port_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.names.push((info.id, port_name));
        self.restoring
            .retain(|restore| !restore.saved.same_target(&saved));
        if let Some(store) = StateStore::try_global(cx) {
            let remembered = saved.clone();
            store.update(cx, |store, cx| {
                store.edit(cx, |state| state.remember_forward(remembered))
            });
        }
        self.saved.push((info.id, saved));
    }

    /// Stops a forward; it is not restored on the next run.
    pub fn stop(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        self.manager.stop(id);
        if let Some((_, saved)) = self.saved.iter().find(|(forward, _)| *forward == id) {
            forget(saved, cx);
        }
        self.refresh(cx);
    }

    /// Pauses a forward or resumes it: the local port stays taken either
    /// way, the state remembers it for the next run.
    pub fn set_paused(&mut self, id: ForwardId, paused: bool, cx: &mut Context<Self>) {
        if !self.manager.set_paused(id, paused) {
            return;
        }
        if let Some((_, saved)) = self.saved.iter_mut().find(|(forward, _)| *forward == id) {
            saved.paused = paused;
            if let Some(store) = StateStore::try_global(cx) {
                let remembered = saved.clone();
                store.update(cx, |store, cx| {
                    store.edit(cx, |state| state.remember_forward(remembered))
                });
            }
        }
        self.refresh(cx);
    }

    /// Restarts the forwards remembered from the last run (#474) as their
    /// contexts connect: the startup context as usual, any other one in the
    /// background once the kubeconfig (or the unlocked vault) lists it.
    pub fn restore(&mut self, cluster: &Entity<Cluster>, cx: &mut Context<Self>) {
        let saved: Vec<SavedForward> = StateStore::try_global(cx)
            .map(|store| store.read(cx).get().saved_forwards().to_vec())
            .unwrap_or_default();
        for saved in saved {
            self.next_restore += 1;
            self.restoring.push(Restore {
                id: self.next_restore,
                saved,
                status: RestoreStatus::Waiting { known: true },
                _task: None,
            });
        }
        if self.restoring.is_empty() {
            return;
        }
        self.cluster = Some(cluster.downgrade());
        self._cluster = Some(cx.observe(cluster, |this, cluster, cx| this.drive(&cluster, cx)));
        self.drive(cluster, cx);
    }

    /// Moves waiting restores on as their contexts load and connect.
    fn drive(&mut self, cluster: &Entity<Cluster>, cx: &mut Context<Self>) {
        let waiting: Vec<String> = self
            .restoring
            .iter()
            .filter(|restore| matches!(restore.status, RestoreStatus::Waiting { .. }))
            .map(|restore| restore.saved.context.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if waiting.is_empty() {
            return;
        }
        // Nothing connects before the kubeconfig is read.
        if cluster.read(cx).is_loading() {
            return;
        }
        let mut changed = false;
        for context in waiting {
            let (step, client) = {
                let cluster = cluster.read(cx);
                let known = cluster
                    .contexts()
                    .is_ok_and(|contexts| contexts.get(&context).is_some());
                let client = cluster
                    .connection_of(&context)
                    .map(|connection| connection.client.clone());
                let step = restore_step(
                    known,
                    cluster.state_of(&context),
                    client.is_some(),
                    self.requested.contains(&context),
                );
                (step, client)
            };
            match step {
                Step::Wait { known } => {
                    for restore in self.waiting_in(&context) {
                        if restore.status != (RestoreStatus::Waiting { known }) {
                            restore.status = RestoreStatus::Waiting { known };
                            changed = true;
                        }
                    }
                }
                Step::Connect => {
                    self.requested.insert(context.clone());
                    // Deferred: the cluster is being read by its observers.
                    let cluster = cluster.downgrade();
                    cx.defer(move |cx| {
                        cluster
                            .update(cx, |cluster, cx| cluster.connect_in_background(context, cx))
                            .ok();
                    });
                }
                Step::Start => {
                    let Some(client) = client else { continue };
                    let ids: Vec<u64> = self.waiting_in(&context).map(|r| r.id).collect();
                    for id in ids {
                        self.start_restore(id, client.clone(), cx);
                    }
                    changed = true;
                }
                Step::CannotConnect(error) => {
                    let message = t!(
                        "dock-forwards-restore-cannot-connect",
                        context = &context,
                        error = error
                    );
                    self.fail_waiting(&context, message);
                    changed = true;
                }
                Step::Dropped => {
                    let message = t!("dock-forwards-restore-disconnected", context = &context);
                    self.fail_waiting(&context, message);
                    changed = true;
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn waiting_in<'a>(&'a mut self, context: &'a str) -> impl Iterator<Item = &'a mut Restore> {
        self.restoring.iter_mut().filter(move |restore| {
            restore.saved.context == context
                && matches!(restore.status, RestoreStatus::Waiting { .. })
        })
    }

    fn fail_waiting(&mut self, context: &str, message: String) {
        for restore in self.waiting_in(context) {
            restore.status = RestoreStatus::Failed(message.clone());
        }
    }

    /// Reads the source again and starts the forward on its saved local
    /// port.
    fn start_restore(&mut self, id: u64, client: Client, cx: &mut Context<Self>) {
        let Some(restore) = self.restoring.iter_mut().find(|r| r.id == id) else {
            return;
        };
        let manager = self.manager.clone();
        let saved = restore.saved.clone();
        let task = runtime::spawn(async move {
            let source = fetch_source(&client, &saved.kind, &saved.namespace, &saved.name).await?;
            let local_port = LocalPort::Exact(saved.local_port);
            // A paused one takes its port back, still paused.
            if saved.paused {
                manager
                    .start_paused(&client, &source, saved.port, local_port)
                    .await
            } else {
                manager
                    .start(&client, &source, saved.port, local_port)
                    .await
            }
        });
        restore.status = RestoreStatus::Starting;
        restore._task = Some(cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(Ok(info)) => Ok(info),
                Ok(Err(error)) => Err(restore_error(&error)),
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| this.restored(id, result, cx))
                .ok();
        }));
    }

    fn restored(&mut self, id: u64, result: Result<ForwardInfo, String>, cx: &mut Context<Self>) {
        let Some(ix) = self.restoring.iter().position(|r| r.id == id) else {
            // Removed while starting.
            if let Ok(info) = result {
                self.manager.stop(info.id);
            }
            return;
        };
        match result {
            Ok(info) => {
                let restore = self.restoring.remove(ix);
                let saved = restore.saved;
                self.names.push((info.id, saved.port_name.clone()));
                self.saved.push((info.id, saved));
                self.refresh(cx);
            }
            Err(error) => {
                self.restoring[ix].status = RestoreStatus::Failed(error);
                self.restoring[ix]._task = None;
                cx.notify();
            }
        }
    }

    /// Tries a failed restore again, connecting its context again if needed.
    pub fn retry(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(restore) = self.restoring.iter_mut().find(|r| r.id == id) else {
            return;
        };
        restore.status = RestoreStatus::Waiting { known: true };
        restore._task = None;
        let context = restore.saved.context.clone();
        self.requested.remove(&context);
        if let Some(cluster) = self.cluster.as_ref().and_then(WeakEntity::upgrade) {
            self.drive(&cluster, cx);
        }
        cx.notify();
    }

    /// Drops a forward of the last run; it is not restored again.
    pub fn remove(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.restoring.iter().position(|r| r.id == id) else {
            return;
        };
        let restore = self.restoring.remove(ix);
        forget(&restore.saved, cx);
        cx.notify();
    }
}

/// Forgets a forward in the application state.
fn forget(saved: &SavedForward, cx: &mut App) {
    if let Some(store) = StateStore::try_global(cx) {
        store.update(cx, |store, cx| {
            store.edit(cx, |state| state.forget_forward(saved))
        });
    }
}

/// The forward in `list` of `port` of `source` (same namespace, source and
/// port).
pub fn find_forward<'a>(
    list: &'a [ForwardInfo],
    source: &ForwardSource,
    port: u16,
) -> Option<&'a ForwardInfo> {
    let label = source.label();
    list.iter().find(|info| {
        info.namespace == source.namespace && info.source == label && info.source_port == port
    })
}

#[cfg(test)]
mod tests {
    use kubach_core::ConnectionState;
    use kubach_core::streams::StreamError;

    use super::{Step, restore_error, restore_step};

    #[test]
    fn restores_wait_for_their_context_and_connect_it_once() {
        let connecting = ConnectionState::Connecting;
        let disconnected = ConnectionState::Disconnected;
        // Not loaded (a locked vault): wait, never connect.
        assert_eq!(
            restore_step(false, &disconnected, false, false),
            Step::Wait { known: false }
        );
        // Loaded and not connected: connect in the background, once.
        assert_eq!(
            restore_step(true, &disconnected, false, false),
            Step::Connect
        );
        assert_eq!(
            restore_step(true, &connecting, false, true),
            Step::Wait { known: true }
        );
        assert_eq!(
            restore_step(true, &disconnected, false, true),
            Step::Dropped
        );
        let error = ConnectionState::Error("token expired".into());
        assert_eq!(
            restore_step(true, &error, false, true),
            Step::CannotConnect("token expired".into())
        );
    }

    #[test]
    fn a_taken_port_or_a_gone_source_says_so() {
        assert!(restore_error(&StreamError::PortInUse(18080)).contains("18080"));
        let gone = restore_error(&StreamError::SourceGone("kube-system/svc/dns".into()));
        assert!(gone.contains("kube-system/svc/dns"));
        assert_ne!(gone, StreamError::SourceGone(String::new()).to_string());
    }
}
