use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use kubach_core::cache::ResourceCache;
use kubach_core::kubeconfig::{FileProblem, FileState, FilesWatcher, Source};
use kubach_core::metrics::MetricsApi;
use kubach_core::metrics::shared::{MetricsHub, MetricsServer};
use kubach_core::runtime::{self, BackgroundTask};
use kubach_core::settings::{ContextKey, ContextPolicy, SecretAction, SecuritySettings};
use kubach_core::vault::Imported;
use kubach_core::{ApiCatalog, Connection, ConnectionState, ContextInfo, Contexts, discovery};

use crate::settings::AppSettings;
use crate::shell_env;

/// CRD changes often come in bursts (installing an operator creates many at
/// once); discovery reruns once the burst settles.
const CRD_DEBOUNCE: Duration = Duration::from_secs(1);

/// Contexts kept connected without settings; see
/// [`Clusters::keep_connected`](kubach_core::settings::Clusters::keep_connected).
const KEEP_CONNECTED: usize = kubach_core::settings::DEFAULT_KEEP_CONNECTED as usize;

static DISCONNECTED: ConnectionState = ConnectionState::Disconnected;
static NO_CATALOG: CatalogState = CatalogState::Pending;
static CONNECTING: ConnectionState = ConnectionState::Connecting;
/// What [`Cluster::contexts`] reports before the first load finishes; check
/// [`Cluster::is_loading`] to tell it from a real error.
static LOADING: SharedString = SharedString::new_static("Loading kubeconfig…");

/// API discovery state for a connection.
#[derive(Clone, Debug, Default)]
pub enum CatalogState {
    #[default]
    Pending,
    Ready(ApiCatalog),
    Error(String),
}

/// Most recently used contexts, most recent first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentContexts(Vec<String>);

impl RecentContexts {
    /// Makes `name` the most recent; returns the names beyond `max`, which
    /// are dropped from the list.
    pub fn touch(&mut self, name: &str, max: usize) -> Vec<String> {
        self.remove(name);
        self.0.insert(0, name.to_owned());
        if self.0.len() > max {
            self.0.split_off(max)
        } else {
            Vec::new()
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|recent| recent != name);
    }

    #[cfg(test)]
    pub fn names(&self) -> &[String] {
        &self.0
    }
}

/// A context whose kubeconfig file can't be read right now, as last seen.
#[derive(Clone, Debug)]
pub struct UnavailableContext {
    pub context: ContextInfo,
    pub file: FileState,
}

/// Connection to one context with its catalog. Dropping it stops all of its
/// work.
struct Session {
    state: ConnectionState,
    connection: Option<Connection>,
    /// Watches run only while something subscribes, i.e. while the context
    /// is shown; a background session keeps the cache but no watches.
    cache: Option<ResourceCache>,
    /// metrics-server polls of the connection, shared by the usage
    /// columns and the details (one per set of namespaces).
    metrics: MetricsHub,
    catalog: CatalogState,
    /// Where the catalog is cached on disk (#299).
    catalog_file: Option<PathBuf>,
    _connecting: Option<Task<()>>,
    _discovering: Option<Task<()>>,
    _crd_watch: Option<(BackgroundTask, Task<()>)>,
}

impl Session {
    fn new(catalog_file: Option<PathBuf>) -> Self {
        Self {
            state: ConnectionState::Connecting,
            connection: None,
            cache: None,
            metrics: MetricsHub::new(),
            catalog: CatalogState::Pending,
            catalog_file,
            _connecting: None,
            _discovering: None,
            _crd_watch: None,
        }
    }
}

/// Kubeconfig contexts, the active one and connections to the recently used.
///
/// Shared by every screen; observe it to react to context switches,
/// connection state and API catalog changes.
pub struct Cluster {
    /// `None` until the first load finishes. A reload keeps the previous
    /// contexts until the new ones arrive.
    contexts: Option<Result<Contexts, SharedString>>,
    /// Bumped whenever `contexts` is replaced, so views that derive state
    /// from the contexts (the settings screen) rebuild it only then.
    revision: u64,
    /// Reads the kubeconfig files and the vault off the UI thread; replacing
    /// it drops the result of a load that is no longer wanted.
    _loading: Option<Task<()>>,
    /// Context to switch to once the load in flight finishes: the startup
    /// context, or one picked before its file was read (e.g. right after
    /// unlocking the vault).
    pending: Option<String>,
    /// Extra kubeconfig files from settings that `contexts` was loaded with.
    kubeconfig_paths: Vec<PathBuf>,
    /// Contexts last read from each source, so the contexts of a file that
    /// went missing (or a vault entry now locked) are still listed, as
    /// unavailable.
    seen: HashMap<Source, Vec<ContextInfo>>,
    /// Reloads the contexts when a kubeconfig file changes or disappears.
    _files_watch: Option<(FilesWatcher, Task<()>)>,
    active: Option<String>,
    sessions: HashMap<String, Session>,
    /// Contexts with a session, most recent first.
    recent: RecentContexts,
    /// Contexts kept connected, the active one included, so switching back
    /// is instant; the least recently used beyond this disconnect.
    keep_connected: usize,
    /// Per-context security settings; observers are notified when they
    /// change, so screens re-check what they may show.
    security: SecuritySettings,
    _settings: Option<Subscription>,
    /// [`VaultState::revision`](crate::vault::VaultState::revision) the
    /// contexts were loaded with.
    vault_revision: u64,
    _vault: Option<Subscription>,
}

impl Cluster {
    /// Starts loading the kubeconfig (plus extra files from settings) in the
    /// background; once read, connects to the startup context from settings,
    /// else the kubeconfig default.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = AppSettings::try_global(cx);
        let (kubeconfig_paths, startup, keep_connected, security) = settings
            .as_ref()
            .map(|settings| {
                let settings = settings.read(cx).get();
                let clusters = &settings.clusters;
                (
                    clusters.resolved_kubeconfig_paths(),
                    clusters.startup_context.clone(),
                    clusters.keep_connected(),
                    settings.security.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    Vec::new(),
                    None,
                    KEEP_CONNECTED,
                    SecuritySettings::default(),
                )
            });
        let mut this = Self {
            contexts: None,
            revision: 0,
            _loading: None,
            pending: startup,
            kubeconfig_paths,
            seen: HashMap::new(),
            _files_watch: None,
            active: None,
            sessions: HashMap::new(),
            recent: RecentContexts::default(),
            keep_connected,
            security,
            _settings: settings.map(|settings| cx.observe(&settings, Self::settings_changed)),
            vault_revision: crate::vault::global(cx).map_or(0, |vault| vault.read(cx).revision()),
            // Lock, unlock and imports change the imported contexts.
            _vault: crate::vault::global(cx).map(|vault| {
                cx.observe(&vault, |this, vault, cx| {
                    let revision = vault.read(cx).revision();
                    if revision != this.vault_revision {
                        this.vault_revision = revision;
                        this.reload(cx);
                    }
                })
            }),
        };
        // `KUBECONFIG` and exec plugins come from the login shell, which
        // loads while the window opens (#299).
        let shell_env = shell_env::loaded(cx);
        this._loading = Some(cx.spawn(async move |this, cx| {
            shell_env.await;
            this.update(cx, |this, cx| {
                this.watch_files(cx);
                this.reload(cx);
            })
            .ok();
        }));
        this
    }

    /// Whether the kubeconfig is still being read for the first time; until
    /// then [`contexts`](Self::contexts) has nothing to show.
    pub fn is_loading(&self) -> bool {
        self.contexts.is_none()
    }

    /// Changes whenever the contexts are reloaded.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Records the contexts of every file that was read.
    fn remember_seen(&mut self) {
        let Some(Ok(contexts)) = &self.contexts else {
            return;
        };
        for file in contexts.files().iter().filter(|file| file.is_ok()) {
            let own = file
                .contexts
                .iter()
                .filter(|name| contexts.source(name) == Some(&file.source))
                .filter_map(|name| contexts.get(name).cloned())
                .collect();
            self.seen.insert(file.source.clone(), own);
        }
    }

    /// Watches the kubeconfig files `contexts` was loaded from.
    fn watch_files(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = kubach_core::kubeconfig::sources_with(&self.kubeconfig_paths)
            .iter()
            .map(|source| source.path().to_owned())
            .collect();
        self._files_watch = crate::kubeconfig::watch(&paths).map(|(watcher, mut changes)| {
            let listener = cx.spawn(async move |this, cx| {
                while changes.next().await.is_some() {
                    if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                        break;
                    }
                }
            });
            (watcher, listener)
        });
    }

    /// Contexts of unreadable files that were seen before, in file order,
    /// unless another file defines the same name now.
    pub fn unavailable(&self) -> Vec<UnavailableContext> {
        let Ok(contexts) = self.contexts() else {
            return Vec::new();
        };
        contexts
            .problems()
            .flat_map(|file| {
                let seen = self.seen.get(&file.source).cloned();
                // A locked vault entry knows its context names.
                let locked = (file.problem == Some(FileProblem::Locked)).then(|| {
                    file.contexts
                        .iter()
                        .map(|name| ContextInfo {
                            name: name.clone(),
                            cluster: String::new(),
                            user: None,
                            namespace: None,
                            server: None,
                        })
                        .collect()
                });
                seen.or(locked)
                    .into_iter()
                    .flatten()
                    .filter(|context| contexts.get(&context.name).is_none())
                    .map(|context| UnavailableContext {
                        context,
                        file: file.clone(),
                    })
            })
            .collect()
    }

    /// Unreadable linked files whose contexts were never seen (e.g. missing
    /// since startup).
    pub fn unavailable_files(&self) -> Vec<FileState> {
        let Ok(contexts) = self.contexts() else {
            return Vec::new();
        };
        contexts
            .problems()
            .filter(|file| file.source.is_linked())
            .filter(|file| self.seen.get(&file.source).is_none_or(Vec::is_empty))
            .cloned()
            .collect()
    }

    /// Reads the kubeconfig, linked `extra` files and the vault's imported
    /// configs. Blocks on file I/O: run it off the UI thread.
    fn load_contexts(extra: &[PathBuf], imported: Vec<Imported>) -> Result<Contexts, SharedString> {
        Contexts::load_all(kubach_core::kubeconfig::sources_with(extra), imported)
            .map_err(|error| error.user_message().into())
    }

    /// Applies the connection limit and reloads contexts when the extra
    /// kubeconfig files change. Connections stay unless their context
    /// disappeared.
    fn settings_changed(&mut self, settings: Entity<AppSettings>, cx: &mut Context<Self>) {
        let security = &settings.read(cx).get().security;
        if *security != self.security {
            self.security = security.clone();
            cx.notify();
        }
        let clusters = &settings.read(cx).get().clusters;
        let (paths, keep_connected) = (
            clusters.resolved_kubeconfig_paths(),
            clusters.keep_connected(),
        );
        if keep_connected != self.keep_connected {
            self.keep_connected = keep_connected;
            if let Some(active) = self.active.clone() {
                self.touch(&active);
            }
            cx.notify();
        }
        if paths == self.kubeconfig_paths {
            return;
        }
        self.kubeconfig_paths = paths;
        self.watch_files(cx);
        self.reload(cx);
    }

    /// Reads the kubeconfig files and the vault again in the background; a
    /// load still in flight is dropped. The current contexts stay until the
    /// new ones arrive.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let paths = self.kubeconfig_paths.clone();
        // Decrypted already; only the files are read.
        let imported = crate::vault::imported(cx);
        let load = cx.background_spawn(async move { Self::load_contexts(&paths, imported) });
        self._loading = Some(cx.spawn(async move |this, cx| {
            let contexts = load.await;
            this.update(cx, |this, cx| this.loaded(contexts, cx)).ok();
        }));
    }

    /// Applies freshly read contexts. Connections stay unless their context
    /// disappeared.
    fn loaded(&mut self, contexts: Result<Contexts, SharedString>, cx: &mut Context<Self>) {
        self._loading = None;
        self.contexts = Some(contexts);
        self.revision += 1;
        self.remember_seen();
        let contexts = self.contexts().ok();
        let exists = |name: &str| contexts.is_some_and(|contexts| contexts.get(name).is_some());
        let gone: Vec<String> = self
            .sessions
            .keys()
            .filter(|name| !exists(name))
            .cloned()
            .collect();
        let default = contexts
            .and_then(|contexts| contexts.default_context())
            .map(|context| context.name.clone());
        let next = next_active(
            self.active.as_deref(),
            self.pending.as_deref(),
            default,
            exists,
        );
        self.pending = None;
        for name in gone {
            self.sessions.remove(&name);
            self.recent.remove(&name);
        }
        match next {
            Next::Keep => {}
            Next::Connect(name) => self.connect(name, cx),
            Next::None => self.active = None,
        }
        cx.notify();
    }

    /// The loaded contexts, or why there are none. Before the first load
    /// finishes this is an error too; see [`is_loading`](Self::is_loading).
    pub fn contexts(&self) -> Result<&Contexts, &SharedString> {
        match &self.contexts {
            Some(contexts) => contexts.as_ref(),
            None => Err(&LOADING),
        }
    }

    /// Extra kubeconfig files from settings that the contexts were loaded
    /// with (after `KUBECONFIG` / `~/.kube/config`).
    pub fn kubeconfig_paths(&self) -> &[PathBuf] {
        &self.kubeconfig_paths
    }

    /// Security settings of `context`, bound to its name and API server;
    /// strict for an unknown context.
    pub fn policy(&self, context: &str) -> ContextPolicy {
        let server = self
            .contexts()
            .ok()
            .and_then(|contexts| contexts.server(context))
            .unwrap_or_default();
        self.security.policy(&ContextKey::new(context, server))
    }

    /// Whether the user may do `action` with Secrets of `context` (#96).
    pub fn allows_secret(&self, context: &str, action: SecretAction) -> bool {
        self.policy(context).allows_secret(action)
    }

    /// [`allows_secret`](Self::allows_secret) for the active context; with
    /// no context nothing comes from a cluster, so everything is allowed.
    pub fn active_allows_secret(&self, action: SecretAction) -> bool {
        self.active
            .as_deref()
            .is_none_or(|context| self.allows_secret(context, action))
    }

    /// Name of the active context, even before its file is read.
    pub fn active_name(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn active(&self) -> Option<&ContextInfo> {
        let name = self.active.as_deref()?;
        self.contexts().ok()?.get(name)
    }

    fn session(&self) -> Option<&Session> {
        self.sessions.get(self.active.as_deref()?)
    }

    /// State of the active context.
    /// State of the active context; connecting while the kubeconfig is read
    /// with a context waiting for it.
    pub fn state(&self) -> &ConnectionState {
        if self.pending.is_some() && self._loading.is_some() {
            return &CONNECTING;
        }
        self.session()
            .map_or(&DISCONNECTED, |session| &session.state)
    }

    /// State of any context: disconnected unless it is among the recent.
    pub fn state_of(&self, name: &str) -> &ConnectionState {
        self.sessions
            .get(name)
            .map_or(&DISCONNECTED, |session| &session.state)
    }

    pub fn connection(&self) -> Option<&Connection> {
        self.session()?.connection.as_ref()
    }

    /// Connection of any context kept connected, active or not.
    pub fn connection_of(&self, name: &str) -> Option<&Connection> {
        self.sessions.get(name)?.connection.as_ref()
    }

    /// Connected contexts and the object caches of every session, for the
    /// debug overlay (#291).
    #[cfg(debug_assertions)]
    pub fn debug_sessions(&self) -> (usize, Vec<ResourceCache>) {
        let connected = self
            .sessions
            .values()
            .filter(|session| session.connection.is_some())
            .count();
        let caches = self
            .sessions
            .values()
            .filter_map(|session| session.cache.clone())
            .collect();
        (connected, caches)
    }

    /// Object cache of the active connection, once connected.
    pub fn cache(&self) -> Option<&ResourceCache> {
        self.session()?.cache.as_ref()
    }

    /// metrics-server of the active connection, when the cluster serves
    /// it: the table and the details share its polls.
    pub fn metrics_server(&self) -> Option<MetricsServer> {
        let session = self.session()?;
        let (Some(connection), CatalogState::Ready(catalog)) =
            (&session.connection, &session.catalog)
        else {
            return None;
        };
        let api = MetricsApi::new(&connection.client, catalog).ok()?;
        Some(MetricsServer::new(session.metrics.clone(), api))
    }

    pub fn catalog(&self) -> &CatalogState {
        self.session()
            .map_or(&NO_CATALOG, |session| &session.catalog)
    }

    /// Switches to `name`, reusing its connection if it is still open (or
    /// opening), otherwise connecting in the background.
    pub fn connect(&mut self, name: String, cx: &mut Context<Self>) {
        self.active = Some(name.clone());
        self.open(&name, false, cx);
    }

    /// Connects `name` without switching to it.
    pub fn connect_in_background(&mut self, name: String, cx: &mut Context<Self>) {
        self.open(&name, false, cx);
        // The active context stays the most recent.
        if let Some(active) = self.active.clone() {
            self.touch(&active);
        }
    }

    /// Reconnects the active context from scratch.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = self.active.clone() {
            self.open(&name, true, cx);
        }
    }

    /// Closes the connection to `name`; the active context stays selected.
    pub fn disconnect(&mut self, name: &str, cx: &mut Context<Self>) {
        self.sessions.remove(name);
        self.recent.remove(name);
        cx.notify();
    }

    /// Marks `name` recently used and disconnects the contexts pushed out.
    fn touch(&mut self, name: &str) {
        for evicted in self.recent.touch(name, self.keep_connected) {
            self.sessions.remove(&evicted);
        }
    }

    /// Ensures a session for `name`; a failed one (or any with `fresh`) is
    /// started over.
    fn open(&mut self, name: &str, fresh: bool, cx: &mut Context<Self>) {
        // A context picked before its file was read (the first load, or a
        // reload after unlocking the vault) connects once the load is done.
        let unknown = self
            .contexts()
            .is_ok_and(|contexts| contexts.get(name).is_none());
        if self.is_loading() || (self._loading.is_some() && unknown) {
            self.pending = Some(name.to_owned());
            cx.notify();
            return;
        }
        let Some(Ok(contexts)) = &self.contexts else {
            return;
        };
        let reuse = !fresh
            && self
                .sessions
                .get(name)
                .is_some_and(|session| !matches!(session.state, ConnectionState::Error(_)));
        if !reuse {
            let exec_command = contexts.exec_command(name).map(str::to_owned);
            let catalog_file = discovery::cache::dir().map(|dir| {
                discovery::cache::path(&dir, name, contexts.server(name).unwrap_or_default())
            });
            let cached = catalog_file
                .clone()
                .map(|file| cx.background_spawn(async move { discovery::cache::load(&file) }));
            let contexts = contexts.clone();
            let mut session = Session::new(catalog_file);
            let target = name.to_owned();
            let task = runtime::spawn(async move { contexts.connect(&target).await });
            let name_ = name.to_owned();
            session._connecting = Some(cx.spawn(async move |this, cx| {
                // The cached catalog fills the sidebar while connecting;
                // discovery refreshes it once connected (#299).
                if let Some(catalog) = match cached {
                    Some(cached) => cached.await,
                    None => None,
                } {
                    this.update(cx, |this, cx| {
                        if let Some(session) = this.sessions.get_mut(&name_)
                            && matches!(session.catalog, CatalogState::Pending)
                        {
                            session.catalog = CatalogState::Ready(catalog);
                            cx.notify();
                        }
                    })
                    .ok();
                }
                let result = task.await;
                this.update(cx, |this, cx| {
                    let Some(session) = this.sessions.get_mut(&name_) else {
                        return;
                    };
                    // No cached catalog for a context that can't connect.
                    if !matches!(result, Ok(Ok(_))) {
                        session.catalog = CatalogState::Pending;
                    }
                    match result {
                        Ok(Ok(connection)) => {
                            session.state = ConnectionState::Connected {
                                server_version: connection.server_version.clone(),
                            };
                            session.cache = Some(ResourceCache::new(connection.client.clone()));
                            session.metrics = MetricsHub::new();
                            // Node shell pods a crash left behind (#318).
                            let fallback = crate::settings::AppSettings::try_global(cx).map(|s| {
                                s.read(cx).get().general.node_shell_namespace().to_owned()
                            });
                            crate::dock::sweep_node_shells(
                                connection.client.clone(),
                                fallback.unwrap_or_else(|| "default".into()),
                            );
                            session.connection = Some(connection);
                            this.discover(&name_, cx);
                            this.watch_crds(&name_, cx);
                        }
                        Ok(Err(error)) if error.is_exec_not_found() => {
                            session.state =
                                ConnectionState::Error(shell_env::exec_not_found_message(
                                    &error.user_message(),
                                    exec_command.as_deref(),
                                    cx,
                                ));
                        }
                        Ok(Err(error)) => {
                            session.state = ConnectionState::Error(error.user_message());
                        }
                        Err(error) => session.state = ConnectionState::Error(error.to_string()),
                    }
                    cx.notify();
                })
                .ok();
            }));
            self.sessions.insert(name.to_owned(), session);
        }
        self.touch(name);
        cx.notify();
    }

    /// (Re)runs API discovery for `name`. The previous catalog stays visible
    /// until the new one arrives.
    fn discover(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(name) else {
            return;
        };
        let Some(client) = session.connection.as_ref().map(|c| c.client.clone()) else {
            return;
        };
        let task = runtime::spawn(async move { ApiCatalog::discover(&client).await });
        let name = name.to_owned();
        session._discovering = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(session) = this.sessions.get_mut(&name) else {
                    return;
                };
                session.catalog = match result {
                    Ok(Ok(catalog)) => {
                        if matches!(&session.catalog, CatalogState::Ready(shown) if *shown == catalog)
                        {
                            return;
                        }
                        if let Some(file) = session.catalog_file.clone() {
                            let catalog = catalog.clone();
                            cx.background_spawn(async move {
                                discovery::cache::store(&file, &catalog).ok();
                            })
                            .detach();
                        }
                        CatalogState::Ready(catalog)
                    }
                    Ok(Err(error)) => CatalogState::Error(error.to_string()),
                    Err(error) => CatalogState::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reruns discovery for `name` whenever CRDs are added, changed or removed.
    fn watch_crds(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(name) else {
            return;
        };
        let Some(client) = session.connection.as_ref().map(|c| c.client.clone()) else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded();
        let watch = runtime::spawn(async move {
            let mut changes = pin!(discovery::crd_changes(&client));
            while changes.next().await.is_some() {
                if tx.unbounded_send(()).is_err() {
                    break;
                }
            }
        });
        let name_ = name.to_owned();
        let listener = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                cx.background_executor().timer(CRD_DEBOUNCE).await;
                while rx.try_recv().is_ok() {}
                if this
                    .update(cx, |this, cx| this.discover(&name_, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        session._crd_watch = Some((watch, listener));
    }
}

/// What to do with the active context after a load.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// The active context is still there.
    Keep,
    Connect(String),
    /// No context to show.
    None,
}

/// Picks the active context after a load: the `pending` one if it was
/// found (the startup context or one picked while loading), else the
/// current one if it still exists, else the kubeconfig `default`.
fn next_active(
    active: Option<&str>,
    pending: Option<&str>,
    default: Option<String>,
    exists: impl Fn(&str) -> bool,
) -> Next {
    if let Some(pending) = pending.filter(|name| exists(name)) {
        return Next::Connect(pending.to_owned());
    }
    if active.is_some_and(&exists) {
        return Next::Keep;
    }
    default.map_or(Next::None, Next::Connect)
}

#[cfg(test)]
mod tests {
    use super::{Next, RecentContexts, next_active};

    #[test]
    fn recent_keeps_the_latest_and_evicts_the_oldest() {
        let mut recent = RecentContexts::default();
        assert!(recent.touch("a", 3).is_empty());
        assert!(recent.touch("b", 3).is_empty());
        assert!(recent.touch("c", 3).is_empty());
        assert_eq!(recent.names(), ["c", "b", "a"]);

        // Touching a known name reorders without evicting.
        assert!(recent.touch("a", 3).is_empty());
        assert_eq!(recent.names(), ["a", "c", "b"]);

        assert_eq!(recent.touch("d", 3), ["b"]);
        assert_eq!(recent.names(), ["d", "a", "c"]);

        recent.remove("a");
        assert_eq!(recent.names(), ["d", "c"]);
        assert!(recent.touch("e", 3).is_empty());
        assert_eq!(recent.touch("f", 2), ["d", "c"]);
        assert_eq!(recent.names(), ["f", "e"]);
    }

    #[test]
    fn next_active_prefers_the_pending_context() {
        let exists = |name: &str| ["a", "b", "c"].contains(&name);
        // First load: the startup context, else the kubeconfig default.
        assert_eq!(
            next_active(None, Some("b"), Some("a".into()), exists),
            Next::Connect("b".into())
        );
        assert_eq!(
            next_active(None, Some("gone"), Some("a".into()), exists),
            Next::Connect("a".into())
        );
        assert_eq!(next_active(None, None, None, exists), Next::None);
        // Reload: the active context stays while it exists.
        assert_eq!(
            next_active(Some("c"), None, Some("a".into()), exists),
            Next::Keep
        );
        assert_eq!(
            next_active(Some("gone"), None, Some("a".into()), exists),
            Next::Connect("a".into())
        );
        assert_eq!(next_active(Some("gone"), None, None, exists), Next::None);
        // Picked while the load was in flight (e.g. right after unlocking).
        assert_eq!(
            next_active(Some("c"), Some("b"), Some("a".into()), exists),
            Next::Connect("b".into())
        );
    }
}
