//! Local port-forwards to pods, tracked by a [`PortForwardManager`].
//!
//! Every forward listens on `127.0.0.1` only. Each accepted local connection
//! opens its own port-forward WebSocket to the pod, so connections are
//! independent and a failing one does not take the others down.
//!
//! A forward to a Service or workload goes to one of its ready pods. When
//! that pod goes away, the next connection moves to another ready pod.
//!
//! A paused forward keeps its local port (the listener stays bound) but
//! forwards nothing: its connections are closed and new ones are accepted
//! and closed at once (#476). Resuming forwards again on the same port.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures::channel::mpsc;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, Portforwarder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use super::StreamError;
use super::forward_target::{ForwardSource, ResolvedTarget, resolve_in};
use crate::runtime::{self, BackgroundTask};
use crate::sync::lock;

pub type ForwardId = u64;

/// How many ports after the preferred one [`LocalPort::Near`] tries.
const LOCAL_PORT_TRIES: u16 = 100;

/// The local port a forward listens on (always on `127.0.0.1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPort {
    /// This port; 0 lets the OS choose.
    Exact(u16),
    /// This port when it can be bound, otherwise the nearest free one (see
    /// [`local_port_candidates`]), otherwise any free port.
    Near(u16),
}

impl From<u16> for LocalPort {
    fn from(port: u16) -> Self {
        Self::Exact(port)
    }
}

/// Ports [`LocalPort::Near`] tries in order: `preferred` and the ones above
/// it. A privileged `preferred` (below 1024, often not bindable without
/// root) continues at `preferred + 8000` and up (`80` → `8080`).
pub fn local_port_candidates(preferred: u16) -> impl Iterator<Item = u16> {
    let run =
        |start: u16| (0..=LOCAL_PORT_TRIES).filter_map(move |offset| start.checked_add(offset));
    let fallback = (preferred > 0 && preferred < 1024).then(|| preferred + 8000);
    run(preferred)
        .filter(|port| *port != 0)
        .chain(fallback.into_iter().flat_map(run))
}

/// Binds `127.0.0.1` on `local`, trying candidates in order for
/// [`LocalPort::Near`]. Binding checks and takes the port at once, so
/// forwards started together never pick the same one.
fn bind_local(local: LocalPort) -> std::io::Result<std::net::TcpListener> {
    let bind = |port: u16| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port));
    match local {
        LocalPort::Exact(port) => bind(port),
        LocalPort::Near(preferred) => local_port_candidates(preferred)
            .find_map(|port| bind(port).ok())
            .map_or_else(|| bind(0), Ok),
    }
}

/// A taken or forbidden exact port gets its own error, for a clear message.
fn bind_error(local: LocalPort, error: std::io::Error) -> StreamError {
    match local {
        LocalPort::Exact(port) if port != 0 => match error.kind() {
            std::io::ErrorKind::AddrInUse => StreamError::PortInUse(port),
            std::io::ErrorKind::PermissionDenied => StreamError::PortDenied(port),
            _ => StreamError::Io(error),
        },
        _ => StreamError::Io(error),
    }
}

/// A forward as shown in the manager list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardInfo {
    pub id: ForwardId,
    pub namespace: String,
    /// What was forwarded: `svc/web`, `deploy/api`, `pod/web-1`.
    pub source: String,
    /// The port asked for on the source (a Service port for Services).
    pub source_port: u16,
    /// Pod currently forwarded to.
    pub pod: String,
    /// Port on [`pod`](Self::pod).
    pub remote_port: u16,
    /// Always on `127.0.0.1`; the port is OS-assigned when 0 was requested.
    pub local_addr: SocketAddr,
    pub state: ForwardState,
    pub active_connections: usize,
    pub total_connections: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
    /// Holds its local port but forwards nothing (see
    /// [`PortForwardManager::set_paused`]).
    pub paused: bool,
    /// The AI agent's session that opened it (#17); `None` for the user's
    /// own forwards.
    pub agent: Option<ForwardAgent>,
}

/// Who opened a forward through MCP (#17), for the "Port forwards" screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardAgent {
    /// The kubeconfig context of the session.
    pub context: String,
    /// The MCP client's self-reported name.
    pub client: String,
    pub session: String,
}

/// Health of a forward, derived from its most recent connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardState {
    /// Listening; the last connection (if any) succeeded.
    Ready,
    /// The last connection to the pod failed; new connections are still tried.
    Failing,
    /// After a failure, a new connection is reaching for the pod (another
    /// ready pod of a Service or workload, or the same pod again).
    Reconnecting,
}

/// Tracks active forwards. Cheap to clone; dropping the last clone stops all
/// forwards.
#[derive(Clone, Default)]
pub struct PortForwardManager {
    shared: Arc<Mutex<Registry>>,
}

impl PortForwardManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forwards `127.0.0.1:local_port` (a `u16` is exact, 0 = any free port;
    /// [`LocalPort::Near`] takes the nearest free one) to `port` of
    /// `source`: a running pod, or a ready pod of a Service or workload (for
    /// a Service `port` is a Service port, mapped to its `targetPort`).
    /// Must run on the [`runtime`](crate::runtime).
    pub async fn start(
        &self,
        client: &Client,
        source: &ForwardSource,
        port: u16,
        local_port: impl Into<LocalPort>,
    ) -> Result<ForwardInfo, StreamError> {
        self.start_source(client, source, port, local_port.into(), false, None)
            .await
    }

    /// Like [`start`](Self::start), for an AI agent's session (#17): the
    /// forward is marked as the agent's. Same listener, same `127.0.0.1`.
    pub async fn start_for_agent(
        &self,
        client: &Client,
        source: &ForwardSource,
        port: u16,
        local_port: impl Into<LocalPort>,
        agent: ForwardAgent,
    ) -> Result<ForwardInfo, StreamError> {
        self.start_source(client, source, port, local_port.into(), false, Some(agent))
            .await
    }

    /// Like [`start`](Self::start), but paused from the first moment: the
    /// local port is taken, nothing is forwarded until
    /// [`set_paused`](Self::set_paused) resumes it.
    pub async fn start_paused(
        &self,
        client: &Client,
        source: &ForwardSource,
        port: u16,
        local_port: impl Into<LocalPort>,
    ) -> Result<ForwardInfo, StreamError> {
        self.start_source(client, source, port, local_port.into(), true, None)
            .await
    }

    async fn start_source(
        &self,
        client: &Client,
        source: &ForwardSource,
        port: u16,
        local_port: LocalPort,
        paused: bool,
        agent: Option<ForwardAgent>,
    ) -> Result<ForwardInfo, StreamError> {
        let api: Api<Pod> = Api::namespaced(client.clone(), &source.namespace);
        let target = resolve_in(&api, source, port).await?;
        let route = Route {
            namespace: source.namespace.clone(),
            source: source.label(),
            source_port: port,
            pod: target.pod.clone(),
            remote_port: target.port,
            agent,
        };
        let connector = source_connector(api, source.clone(), port, target);
        self.start_with(route, local_port, connector, paused)
    }

    fn start_with(
        &self,
        route: Route,
        local_port: impl Into<LocalPort>,
        connector: Connector,
        paused: bool,
    ) -> Result<ForwardInfo, StreamError> {
        let local_port = local_port.into();
        let listener = bind_local(local_port).map_err(|error| bind_error(local_port, error))?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let (pause, paused_rx) = watch::channel(paused);
        let mut registry = self.lock();
        let id = registry.insert(route, local_addr, paused);
        let task = runtime::spawn(serve(
            listener,
            connector,
            paused_rx,
            Arc::downgrade(&self.shared),
            id,
        ));
        if let Some(entry) = registry.forwards.get_mut(&id) {
            entry.task = Some(task);
            entry.pause = Some(pause);
        }
        Ok(registry.info(id).expect("just inserted"))
    }

    /// Pauses a forward (its connections are closed, new ones are closed
    /// at once, the local port stays taken) or resumes it. False if it did
    /// not exist.
    pub fn set_paused(&self, id: ForwardId, paused: bool) -> bool {
        let mut registry = self.lock();
        let Some(entry) = registry.forwards.get_mut(&id) else {
            return false;
        };
        if entry.info.paused != paused {
            if let Some(pause) = &entry.pause {
                pause.send_replace(paused);
            }
            registry.update(id, |info| {
                info.paused = paused;
                if paused {
                    // The serving task drops them without reporting.
                    info.active_connections = 0;
                } else {
                    // Health is measured again from the next connection.
                    info.state = ForwardState::Ready;
                }
            });
        }
        true
    }

    /// All forwards, ordered by id.
    pub fn list(&self) -> Vec<ForwardInfo> {
        self.lock()
            .forwards
            .values()
            .map(|e| e.info.clone())
            .collect()
    }

    pub fn get(&self, id: ForwardId) -> Option<ForwardInfo> {
        self.lock().info(id)
    }

    /// Stops a forward and closes its connections. False if it did not exist.
    pub fn stop(&self, id: ForwardId) -> bool {
        // Drop the task outside the lock.
        let entry = self.lock().remove(id);
        entry.is_some()
    }

    pub fn stop_all(&self) {
        let entries = std::mem::take(&mut self.lock().forwards);
        drop(entries);
        self.lock().notify();
    }

    /// Fires whenever a forward is added, removed or its counters change.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<()> {
        let (tx, rx) = mpsc::unbounded();
        self.lock().subscribers.push(tx);
        rx
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        lock(&self.shared)
    }
}

/// Where a forward goes when it starts.
struct Route {
    namespace: String,
    source: String,
    source_port: u16,
    pod: String,
    remote_port: u16,
    agent: Option<ForwardAgent>,
}

#[derive(Default)]
struct Registry {
    next_id: ForwardId,
    forwards: BTreeMap<ForwardId, Entry>,
    subscribers: Vec<mpsc::UnboundedSender<()>>,
}

struct Entry {
    info: ForwardInfo,
    task: Option<BackgroundTask>,
    /// Tells the serving task whether the forward is paused.
    pause: Option<watch::Sender<bool>>,
}

impl Registry {
    fn insert(&mut self, route: Route, local_addr: SocketAddr, paused: bool) -> ForwardId {
        self.next_id += 1;
        let id = self.next_id;
        let info = ForwardInfo {
            id,
            namespace: route.namespace,
            source: route.source,
            source_port: route.source_port,
            pod: route.pod,
            remote_port: route.remote_port,
            local_addr,
            state: ForwardState::Ready,
            active_connections: 0,
            total_connections: 0,
            error_count: 0,
            last_error: None,
            paused,
            agent: route.agent,
        };
        self.forwards.insert(
            id,
            Entry {
                info,
                task: None,
                pause: None,
            },
        );
        self.notify();
        id
    }

    fn info(&self, id: ForwardId) -> Option<ForwardInfo> {
        self.forwards.get(&id).map(|entry| entry.info.clone())
    }

    fn remove(&mut self, id: ForwardId) -> Option<Entry> {
        let entry = self.forwards.remove(&id);
        self.notify();
        entry
    }

    fn update(&mut self, id: ForwardId, change: impl FnOnce(&mut ForwardInfo)) {
        if let Some(entry) = self.forwards.get_mut(&id) {
            change(&mut entry.info);
            self.notify();
        }
    }

    /// A connection reached `target`; records a move to another pod and
    /// the end of a reconnect.
    fn connected(&mut self, id: ForwardId, target: &ResolvedTarget) {
        let changed = self.forwards.get(&id).is_some_and(|entry| {
            entry.info.pod != target.pod
                || entry.info.remote_port != target.port
                || entry.info.state == ForwardState::Reconnecting
        });
        if changed {
            self.update(id, |info| {
                info.pod = target.pod.clone();
                info.remote_port = target.port;
                if info.state == ForwardState::Reconnecting {
                    info.state = ForwardState::Ready;
                }
            });
        }
    }

    /// A local connection came in; after a failure it is a reconnect.
    fn connection_opened(&mut self, id: ForwardId) {
        self.update(id, |info| {
            info.active_connections += 1;
            info.total_connections += 1;
            if info.state == ForwardState::Failing {
                info.state = ForwardState::Reconnecting;
            }
        });
    }

    fn connection_closed(&mut self, id: ForwardId, error: Option<String>) {
        self.update(id, |info| {
            info.active_connections = info.active_connections.saturating_sub(1);
            match error {
                Some(error) => {
                    info.error_count += 1;
                    info.last_error = Some(error);
                    info.state = ForwardState::Failing;
                }
                None => info.state = ForwardState::Ready,
            }
        });
    }

    fn listener_error(&mut self, id: ForwardId, error: String) {
        self.update(id, |info| {
            info.error_count += 1;
            info.last_error = Some(error);
        });
    }

    fn notify(&mut self) {
        self.subscribers.retain(|tx| tx.unbounded_send(()).is_ok());
    }
}

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A connection to the remote port.
struct Upstream {
    io: Box<dyn Io>,
    /// Resolves with an error reported by the remote side, or `None` when
    /// the connection closes.
    error: BoxFuture<Option<String>>,
    _guard: Option<Box<dyn Send>>,
    /// The pod and port reached, when the connector picks them.
    target: Option<ResolvedTarget>,
}

type Connector = Arc<dyn Fn() -> BoxFuture<Result<Upstream, String>> + Send + Sync>;

/// Aborts the port-forward WebSocket when the local connection is done.
struct AbortOnDrop(Portforwarder);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn connect_pod(api: &Api<Pod>, target: ResolvedTarget) -> Result<Upstream, String> {
    let port = target.port;
    let mut forwarder = api
        .portforward(&target.pod, &[port])
        .await
        .map_err(|error| error.to_string())?;
    let missing = || format!("port {port} missing in port-forward response");
    let io = forwarder.take_stream(port).ok_or_else(missing)?;
    let error = forwarder.take_error(port).ok_or_else(missing)?;
    Ok(Upstream {
        io: Box::new(io),
        error: Box::pin(error),
        _guard: Some(Box::new(AbortOnDrop(forwarder))),
        target: Some(target),
    })
}

/// Connects to the current pod of `source`. For a Service or workload, a
/// failed connection looks for another ready pod and retries there once.
fn source_connector(
    api: Api<Pod>,
    source: ForwardSource,
    port: u16,
    initial: ResolvedTarget,
) -> Connector {
    let source = Arc::new(source);
    let current = Arc::new(Mutex::new(initial));
    Arc::new(move || {
        let api = api.clone();
        let source = source.clone();
        let current = current.clone();
        Box::pin(async move {
            let target = lock(&current).clone();
            let error = match connect_pod(&api, target.clone()).await {
                Ok(upstream) => return Ok(upstream),
                Err(error) => error,
            };
            if source.selector.is_none() {
                return Err(error);
            }
            match resolve_in(&api, &source, port).await {
                Ok(next) if next != target => {
                    *lock(&current) = next.clone();
                    connect_pod(&api, next).await
                }
                Ok(_) => Err(error),
                Err(resolve_error) => Err(format!("{error}; {resolve_error}")),
            }
        })
    })
}

async fn serve(
    listener: std::net::TcpListener,
    connector: Connector,
    mut paused: watch::Receiver<bool>,
    registry: Weak<Mutex<Registry>>,
    id: ForwardId,
) {
    let with_registry = |f: &dyn Fn(&mut Registry)| match registry.upgrade() {
        Some(shared) => {
            f(&mut lock(&shared));
            true
        }
        None => false,
    };
    let listener = match TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            with_registry(&|r| r.listener_error(id, error.to_string()));
            return;
        }
    };
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                // Paused: the port stays taken, the connection is closed.
                Ok((socket, _)) if *paused.borrow() => drop(socket),
                Ok((socket, _)) => {
                    if !with_registry(&|r| r.connection_opened(id)) {
                        return;
                    }
                    let connector = connector.clone();
                    let registry = registry.clone();
                    connections.spawn(async move {
                        let error = forward_connection(socket, &connector, |target| {
                            if let Some(shared) = registry.upgrade() {
                                lock(&shared).connected(id, target);
                            }
                        })
                        .await;
                        if let Some(shared) = registry.upgrade() {
                            lock(&shared).connection_closed(id, error);
                        }
                    });
                }
                Err(error) => {
                    if !with_registry(&|r| r.listener_error(id, error.to_string())) {
                        return;
                    }
                    // E.g. out of file descriptors: back off instead of spinning.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            changed = paused.changed() => {
                if changed.is_err() {
                    // The forward was stopped.
                    return;
                }
                if *paused.borrow_and_update() {
                    connections.abort_all();
                }
            }
        }
    }
}

/// Pipes one local connection to the pod. Returns the remote error, if any;
/// local I/O errors (client resets) are not reported.
async fn forward_connection(
    mut socket: TcpStream,
    connector: &Connector,
    on_connected: impl FnOnce(&ResolvedTarget),
) -> Option<String> {
    let mut upstream = match connector().await {
        Ok(upstream) => upstream,
        Err(error) => return Some(error),
    };
    if let Some(target) = &upstream.target {
        on_connected(target);
    }
    let copy = tokio::io::copy_bidirectional(&mut socket, &mut upstream.io);
    tokio::pin!(copy);
    tokio::select! {
        _ = &mut copy => None,
        error = &mut upstream.error => match error {
            Some(error) => Some(error),
            None => {
                let _ = copy.await;
                None
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn addr() -> SocketAddr {
        (Ipv4Addr::LOCALHOST, 8080).into()
    }

    fn route(pod: &str, port: u16) -> Route {
        Route {
            namespace: "ns".into(),
            source: format!("pod/{pod}"),
            source_port: port,
            pod: pod.into(),
            remote_port: port,
            agent: None,
        }
    }

    #[test]
    fn local_port_candidates_start_at_the_same_port() {
        let first = |preferred, count| -> Vec<u16> {
            local_port_candidates(preferred).take(count).collect()
        };
        assert_eq!(first(8080, 3), [8080, 8081, 8082]);
        assert_eq!(local_port_candidates(8080).count(), 101);
        // Privileged ports continue in the dev range: 80 → 8080.
        let low: Vec<u16> = local_port_candidates(80).collect();
        assert_eq!(&low[..2], [80, 81]);
        assert_eq!(low[101..103], [8080, 8081]);
        // No overflow past 65535.
        assert_eq!(first(65534, 5), [65534, 65535]);
    }

    #[test]
    fn near_local_port_skips_bound_ports() {
        let taken = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let near = bind_local(LocalPort::Near(port)).unwrap();
        let picked = near.local_addr().unwrap().port();
        assert_ne!(picked, port);
        assert!(bind_local(LocalPort::Exact(port)).is_err());
        let error = bind_local(LocalPort::Exact(port)).unwrap_err();
        assert!(matches!(
            bind_error(LocalPort::Exact(port), error),
            StreamError::PortInUse(taken_port) if taken_port == port
        ));
        drop(taken);
        let same = bind_local(LocalPort::Near(port)).unwrap();
        assert_eq!(same.local_addr().unwrap().port(), port);
    }

    #[test]
    fn a_connection_after_a_failure_reconnects() {
        let mut registry = Registry::default();
        let id = registry.insert(route("web", 80), addr(), false);
        registry.connection_opened(id);
        registry.connection_closed(id, Some("pod is gone".into()));
        assert_eq!(registry.info(id).unwrap().state, ForwardState::Failing);
        registry.connection_opened(id);
        assert_eq!(registry.info(id).unwrap().state, ForwardState::Reconnecting);
        let target = ResolvedTarget {
            pod: "web-2".into(),
            port: 80,
        };
        registry.connected(id, &target);
        let info = registry.info(id).unwrap();
        assert_eq!(info.state, ForwardState::Ready);
        assert_eq!(info.pod, "web-2");
        // A reconnect that fails again goes back to Failing.
        registry.connection_closed(id, None);
        registry.connection_opened(id);
        registry.connection_closed(id, Some("refused".into()));
        registry.connection_opened(id);
        registry.connection_closed(id, Some("refused".into()));
        assert_eq!(registry.info(id).unwrap().state, ForwardState::Failing);
    }

    #[test]
    fn registry_tracks_connections_and_errors() {
        let mut registry = Registry::default();
        let (tx, mut rx) = mpsc::unbounded();
        registry.subscribers.push(tx);

        let a = registry.insert(route("web", 80), addr(), false);
        let b = registry.insert(route("db", 5432), addr(), false);
        assert_ne!(a, b);
        registry.connection_opened(a);
        registry.connection_opened(a);
        registry.connection_closed(a, None);
        registry.connection_closed(a, Some("connection refused".into()));

        let info = registry.info(a).unwrap();
        assert_eq!(info.active_connections, 0);
        assert_eq!(info.total_connections, 2);
        assert_eq!(info.error_count, 1);
        assert_eq!(info.last_error.as_deref(), Some("connection refused"));
        assert_eq!(info.state, ForwardState::Failing);
        assert_eq!(registry.info(b).unwrap().state, ForwardState::Ready);

        assert!(registry.remove(a).is_some());
        assert!(registry.remove(a).is_none());
        registry.connection_opened(a); // unknown ids are ignored
        assert_eq!(registry.forwards.len(), 1);

        let mut notifications = 0;
        while rx.try_recv().is_ok() {
            notifications += 1;
        }
        assert_eq!(notifications, 8);

        let target = ResolvedTarget {
            pod: "db-2".into(),
            port: 5433,
        };
        registry.connected(b, &target);
        registry.connected(b, &target);
        let info = registry.info(b).unwrap();
        assert_eq!((info.pod.as_str(), info.remote_port), ("db-2", 5433));
        assert_eq!(info.source, "pod/db");
        let mut notifications = 0;
        while rx.try_recv().is_ok() {
            notifications += 1;
        }
        assert_eq!(notifications, 1);
    }

    /// Upstream that echoes everything back.
    fn echo_connector() -> Connector {
        Arc::new(|| {
            Box::pin(async {
                let (local, remote) = tokio::io::duplex(1024);
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(remote);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
                Ok(Upstream {
                    io: Box::new(local),
                    error: Box::pin(futures::future::pending()),
                    _guard: None,
                    target: None,
                })
            })
        })
    }

    async fn roundtrip(addr: SocketAddr, message: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(message).await.unwrap();
        let mut buf = vec![0; message.len()];
        stream.read_exact(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn forwards_concurrent_connections() {
        let manager = PortForwardManager::new();
        let info = manager
            .start_with(route("web", 80), 0, echo_connector(), false)
            .unwrap();
        assert!(info.local_addr.ip().is_loopback());
        assert_ne!(info.local_addr.port(), 0);

        let replies = futures::future::join_all(
            (0..3).map(|i| roundtrip(info.local_addr, format!("hello {i}").leak().as_bytes())),
        )
        .await;
        for (i, reply) in replies.iter().enumerate() {
            assert_eq!(reply, format!("hello {i}").as_bytes());
        }
        assert_eq!(manager.get(info.id).unwrap().total_connections, 3);
        assert_eq!(manager.list().len(), 1);

        assert!(manager.stop(info.id));
        assert!(manager.list().is_empty());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(TcpStream::connect(info.local_addr).await.is_err());
    }

    #[tokio::test]
    async fn reports_connect_errors_per_forward() {
        let manager = PortForwardManager::new();
        let failing: Connector = Arc::new(|| Box::pin(async { Err("pod is gone".to_owned()) }));
        let bad = manager
            .start_with(route("bad", 80), 0, failing, false)
            .unwrap();
        let good = manager
            .start_with(route("good", 80), 0, echo_connector(), false)
            .unwrap();

        let mut stream = TcpStream::connect(bad.local_addr).await.unwrap();
        let mut buf = [0u8; 1];
        // The connection is closed without data.
        assert_eq!(stream.read(&mut buf).await.unwrap(), 0);
        assert_eq!(roundtrip(good.local_addr, b"ok").await, b"ok");

        // The error is recorded right after the socket is closed.
        let mut bad = manager.get(bad.id).unwrap();
        for _ in 0..100 {
            if bad.error_count > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            bad = manager.get(bad.id).unwrap();
        }
        assert_eq!(bad.last_error.as_deref(), Some("pod is gone"));
        assert_eq!(bad.state, ForwardState::Failing);
        assert_eq!(manager.get(good.id).unwrap().error_count, 0);
        manager.stop_all();
        assert!(manager.list().is_empty());
    }

    /// Reads until the peer closes; true when it closed without data.
    async fn closed(stream: &mut TcpStream) -> bool {
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
        matches!(read, Ok(Ok(0)) | Ok(Err(_)))
    }

    #[tokio::test]
    async fn a_paused_forward_keeps_its_port_and_closes_connections() {
        let manager = PortForwardManager::new();
        let info = manager
            .start_with(route("web", 80), 0, echo_connector(), false)
            .unwrap();
        let port = info.local_addr.port();
        // A connection open before the pause.
        let mut open = TcpStream::connect(info.local_addr).await.unwrap();
        open.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        open.read_exact(&mut buf).await.unwrap();

        assert!(manager.set_paused(info.id, true));
        let paused = manager.get(info.id).unwrap();
        assert!(paused.paused);
        assert_eq!(paused.active_connections, 0);
        // The open connection is closed, the port stays taken, a new
        // connection is accepted and closed at once.
        assert!(closed(&mut open).await);
        assert!(bind_local(LocalPort::Exact(port)).is_err());
        let mut late = TcpStream::connect(info.local_addr).await.unwrap();
        let _ = late.write_all(b"lost").await;
        assert!(closed(&mut late).await);

        assert!(manager.set_paused(info.id, false));
        assert!(!manager.get(info.id).unwrap().paused);
        assert_eq!(roundtrip(info.local_addr, b"again").await, b"again");
        assert_eq!(manager.get(info.id).unwrap().local_addr.port(), port);
        assert!(!manager.set_paused(info.id + 1, true));
    }

    #[tokio::test]
    async fn an_agent_forward_is_marked_and_listens_on_loopback_only() {
        let manager = PortForwardManager::new();
        let agent = ForwardAgent {
            context: "minikube".into(),
            client: "agent".into(),
            session: "s1".into(),
        };
        let route = Route {
            agent: Some(agent.clone()),
            ..route("web", 80)
        };
        let info = manager
            .start_with(route, 0, echo_connector(), false)
            .unwrap();
        assert_eq!(
            info.local_addr.ip(),
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(manager.get(info.id).unwrap().agent, Some(agent));
        assert_eq!(roundtrip(info.local_addr, b"ok").await, b"ok");
    }

    #[tokio::test]
    async fn a_forward_can_start_paused() {
        let manager = PortForwardManager::new();
        let info = manager
            .start_with(route("web", 80), 0, echo_connector(), true)
            .unwrap();
        assert!(info.paused);
        let mut stream = TcpStream::connect(info.local_addr).await.unwrap();
        assert!(closed(&mut stream).await);
        assert_eq!(manager.get(info.id).unwrap().total_connections, 0);
        manager.set_paused(info.id, false);
        assert_eq!(roundtrip(info.local_addr, b"ok").await, b"ok");
    }
}
