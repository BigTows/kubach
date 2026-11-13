//! Temporary kubeconfig of a local terminal (#319, ADR 0015): one context —
//! the one open in Kubach — with its namespace, written to a private file the
//! terminal's shell gets as `KUBECONFIG`. An agent's helm call on a vault
//! context under the user's credentials uses the same file for as long as
//! helm runs (#113), and so does helm started from Kubach's own screens on
//! such a context ([`ContextKubeconfig`], #201).
//!
//! The file holds credentials (tokens, client keys, materialized vault
//! configs), so it is created readable by the owner only (`0600` from the
//! start on Unix, an owner-only DACL on Windows), lives in
//! `<config dir>/run/kubeconfig` (`0700`) and not in the shared temp folder,
//! and is removed when its tab closes, when Kubach quits and, for files left
//! by a crash, on the next start.
//!
//! Whose file it is: the name carries the writer's process id and a token
//! the process picks once, and the process keeps a lock on a marker file
//! named after both for as long as it lives (in `kubeconfig.owners` next
//! to the folder, which so holds nothing but kubeconfigs). A file is left over when its
//! process id is gone, or when the marker is no longer locked: the system
//! releases the lock with the process, whoever got its process id next.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kube::config::Kubeconfig;
use zeroize::Zeroizing;

/// File name prefix: `terminal-<pid>-<token>-<n>.yaml` (`terminal-<pid>-<n>.yaml`
/// when the marker could not be locked, and from Kubach before the marker).
const PREFIX: &str = "terminal-";
const SUFFIX: &str = ".yaml";
/// Marker of a writer process: `owner-<pid>-<token>.lock` in
/// [`marker_dir`], locked while it lives.
const MARKER_PREFIX: &str = "owner-";
const MARKER_SUFFIX: &str = ".lock";

/// Where the markers of the writers of `dir` are: `<dir>.owners` next to
/// it. `None` for a path without a name.
fn marker_dir(dir: &Path) -> Option<PathBuf> {
    let mut name = dir.file_name()?.to_owned();
    name.push(".owners");
    Some(dir.with_file_name(name))
}

/// `run/kubeconfig` in the config directory (`KUBACH_CONFIG_DIR` overrides
/// it). Not under `cache`, which "Clear cache" removes while tabs are open.
pub fn dir() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join("run").join("kubeconfig"))
}

/// A kubeconfig with only `context`, its cluster and user, and
/// `current-context` pointing at it. `namespace` replaces the context's
/// namespace when given. Exec plugins, tokens and certificates are copied as
/// they are. `None` when the config has no such context.
pub fn single_context(
    config: &Kubeconfig,
    context: &str,
    namespace: Option<&str>,
) -> Option<Kubeconfig> {
    let mut named = config.contexts.iter().find(|c| c.name == context)?.clone();
    let (cluster, user) = match named.context.as_mut() {
        Some(ctx) => {
            if let Some(namespace) = namespace {
                ctx.namespace = Some(namespace.to_owned());
            }
            (Some(ctx.cluster.clone()), ctx.user.clone())
        }
        None => (None, None),
    };
    let clusters = config
        .clusters
        .iter()
        .filter(|c| Some(&c.name) == cluster.as_ref())
        .take(1)
        .cloned()
        .collect();
    let auth_infos = config
        .auth_infos
        .iter()
        .filter(|u| Some(&u.name) == user.as_ref())
        .take(1)
        .cloned()
        .collect();
    Some(Kubeconfig {
        preferences: config.preferences.clone(),
        clusters,
        auth_infos,
        contexts: vec![named],
        current_context: Some(context.to_owned()),
        kind: Some("Config".into()),
        api_version: Some("v1".into()),
        ..Kubeconfig::default()
    })
}

/// `config` as YAML, wiped from memory on drop.
pub fn to_yaml(config: &Kubeconfig) -> io::Result<Zeroizing<String>> {
    serde_saphyr::to_string(config)
        .map(Zeroizing::new)
        .map_err(io::Error::other)
}

/// A temporary kubeconfig file, removed when dropped.
#[derive(Debug)]
pub struct TempKubeconfig {
    path: PathBuf,
}

impl TempKubeconfig {
    /// Writes `config` to a new file in `dir` (created `0700` if missing),
    /// readable by the owner only.
    pub fn create(dir: &Path, config: &Kubeconfig) -> io::Result<Self> {
        let yaml = to_yaml(config)?;
        Self::write(dir, yaml.as_bytes())
    }

    fn write(dir: &Path, bytes: &[u8]) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        crate::settings::create_private_dir(dir)?;
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        // Without the marker (a file system without locks) the file is
        // named the old way and only the process id tells whose it is.
        let writer = match hold_marker(dir) {
            Ok(()) => Writer::own(),
            Err(_) => Writer {
                token: None,
                ..Writer::own()
            },
        };
        let path = dir.join(writer.file_name(n));
        let mut file = create_private_file(&path)?;
        // From here on the file is ours to remove, also on errors below.
        let this = Self { path };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(this)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempKubeconfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The kubeconfig of one context that is in no file (a context imported
/// into the vault), for a tool that reads its configuration only from a
/// file, such as `helm` (#113, #201). It stays in memory and goes to a
/// [`TempKubeconfig`] only for as long as one process of the tool runs
/// ([`write`](Self::write)).
#[derive(Clone)]
pub struct ContextKubeconfig(Arc<Kept>);

struct Kept {
    /// One context, its cluster and user.
    config: Kubeconfig,
    /// Where the temporary files live ([`dir`]), so that one left by a
    /// crash is removed on the next start.
    dir: PathBuf,
}

impl ContextKubeconfig {
    /// `context` of `config` alone ([`single_context`]), for files in
    /// `dir`. `None` when the config has no such context.
    pub fn of(config: &Kubeconfig, context: &str, dir: PathBuf) -> Option<Self> {
        let config = single_context(config, context, None)?;
        Some(Self(Arc::new(Kept { config, dir })))
    }

    /// The owner-only file for one process, removed when dropped.
    pub fn write(&self) -> io::Result<TempKubeconfig> {
        TempKubeconfig::create(&self.0.dir, &self.0.config)
    }
}

/// Never the contents: they hold credentials.
impl std::fmt::Debug for ContextKubeconfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextKubeconfig").finish_non_exhaustive()
    }
}

/// The same loaded config, not an equal one: contents are not compared.
impl PartialEq for ContextKubeconfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ContextKubeconfig {}

/// Creates `path` (failing if it exists) with no access for anyone but the
/// owner before anything is written to it.
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(not(unix))]
    if let Err(error) = super::restrict_permissions(path) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

/// The process that wrote a file, from its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Writer {
    pid: u32,
    /// Picked once per process; `None` for a file without a marker.
    token: Option<u64>,
}

impl Writer {
    /// This process.
    fn own() -> Self {
        static TOKEN: OnceLock<u64> = OnceLock::new();
        let token = *TOKEN.get_or_init(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64)
        });
        Self {
            pid: std::process::id(),
            token: Some(token),
        }
    }

    fn file_name(self, n: u64) -> String {
        match self.token {
            Some(token) => format!("{PREFIX}{}-{token}-{n}{SUFFIX}", self.pid),
            None => format!("{PREFIX}{}-{n}{SUFFIX}", self.pid),
        }
    }

    /// The writer of a file named by [`Writer::file_name`].
    fn of_file(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
        let parts: Vec<&str> = rest.split('-').collect();
        let (pid, token, n) = match parts.as_slice() {
            [pid, n] => (pid, None, n),
            [pid, token, n] => (pid, Some(token.parse().ok()?), n),
            _ => return None,
        };
        n.parse::<u64>().ok()?;
        Some(Self {
            pid: pid.parse().ok()?,
            token,
        })
    }

    /// `owner-<pid>-<token>.lock`; `None` for a writer without a token.
    fn marker_name(self) -> Option<String> {
        self.token
            .map(|token| format!("{MARKER_PREFIX}{}-{token}{MARKER_SUFFIX}", self.pid))
    }

    /// The writer a marker file is named after.
    fn of_marker(name: &str) -> Option<Self> {
        let rest = name
            .strip_prefix(MARKER_PREFIX)?
            .strip_suffix(MARKER_SUFFIX)?;
        let (pid, token) = rest.split_once('-')?;
        Some(Self {
            pid: pid.parse().ok()?,
            token: Some(token.parse().ok()?),
        })
    }

    /// Whether the writer is gone, so its files are left over. `alive`
    /// tells whether a process id is in use.
    ///
    /// A live Kubach always holds the lock on its marker, so its files are
    /// never taken for leftovers: a marker that is locked, missing or
    /// unreadable leaves the answer to the process id alone, as for files
    /// without a token.
    fn is_gone(self, dir: &Path, alive: impl Fn(u32) -> bool) -> bool {
        let own = Self::own();
        if self == own {
            return false;
        }
        if !alive(self.pid) {
            return true;
        }
        match self.marker_name() {
            // This process id with another token: an earlier process.
            Some(_) if self.pid == own.pid => true,
            Some(marker) => {
                marker_dir(dir).is_some_and(|markers| marker_released(&markers.join(marker)))
            }
            None => false,
        }
    }
}

/// Markers this process holds locked, by path, until it exits.
static HELD: Mutex<Vec<(PathBuf, fs::File)>> = Mutex::new(Vec::new());

/// Makes sure this process holds the lock on its marker for `dir`.
fn hold_marker(dir: &Path) -> io::Result<()> {
    let (Some(markers), Some(name)) = (marker_dir(dir), Writer::own().marker_name()) else {
        return Err(io::Error::other("no place for a marker"));
    };
    crate::settings::create_private_dir(&markers)?;
    let path = markers.join(name);
    let mut held = crate::sync::lock(&HELD);
    if held.iter().any(|(held, _)| held == &path) && path.exists() {
        return Ok(());
    }
    held.retain(|(held, _)| held != &path);
    // A cleanup of another Kubach may look at the marker between its
    // creation and its lock, and remove it: then the lock is on a file
    // nobody finds, and the marker is made again.
    for _ in 0..5 {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) if path.exists() => {
                held.push((path, file));
                return Ok(());
            }
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    Err(io::Error::other("the marker of this process stays locked"))
}

/// Whether nobody holds the lock on the marker at `path`: the process that
/// made it is gone. A marker that is missing or can't be opened says
/// nothing (`false`).
fn marker_released(path: &Path) -> bool {
    fs::File::open(path).is_ok_and(|file| file.try_lock().is_ok())
}

/// Removes the marker at `path` if its process is gone (checked under the
/// lock, so a marker being taken is not removed).
fn remove_released_marker(path: &Path) {
    if let Ok(file) = fs::File::open(path)
        && file.try_lock().is_ok()
    {
        let _ = fs::remove_file(path);
    }
}

/// Whether process `pid` still runs on this machine.
pub(crate) fn is_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is checked and closed once.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    false
}

/// Removes the files in `dir` whose writer is `stale`; returns how many
/// were removed.
fn remove_where(dir: &Path, stale: impl Fn(Writer) -> bool) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(Writer::of_file)
                .is_some_and(&stale)
        })
        .filter(|entry| fs::remove_file(entry.path()).is_ok())
        .count()
}

/// At startup: removes files of Kubach processes that are gone (see the
/// module docs for how that is told), then their markers.
pub fn remove_orphans(dir: &Path) -> usize {
    remove_orphans_with(dir, is_alive)
}

fn remove_orphans_with(dir: &Path, alive: impl Fn(u32) -> bool) -> usize {
    let removed = remove_where(dir, |writer| writer.is_gone(dir, &alive));
    let own = Writer::own();
    let markers = marker_dir(dir).and_then(|markers| fs::read_dir(markers).ok());
    for entry in markers.into_iter().flatten().flatten() {
        let other = entry
            .file_name()
            .to_str()
            .and_then(Writer::of_marker)
            .is_some_and(|writer| writer != own);
        if other {
            remove_released_marker(&entry.path());
        }
    }
    removed
}

/// At quit: removes every file this process wrote, and its marker.
pub fn remove_own(dir: &Path) -> usize {
    let own = Writer::own();
    let removed = remove_where(dir, |writer| writer.pid == own.pid);
    if let (Some(markers), Some(name)) = (marker_dir(dir), own.marker_name()) {
        let path = markers.join(name);
        // Closed before it is removed: Windows keeps an open file's name.
        crate::sync::lock(&HELD).retain(|(held, _)| held != &path);
        let _ = fs::remove_file(path);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: prod
clusters:
  - name: prod-cluster
    cluster: { server: "https://prod.example.com", certificate-authority-data: Q0E= }
  - name: dev-cluster
    cluster: { server: "https://dev.example.com" }
users:
  - name: alice
    user: { token: alice-token }
  - name: eks
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: aws
        args: [eks, get-token, --cluster-name, dev]
        env: [{ name: AWS_PROFILE, value: dev }]
contexts:
  - name: prod
    context: { cluster: prod-cluster, user: alice, namespace: web }
  - name: dev
    context: { cluster: dev-cluster, user: eks }
"#;

    fn config() -> Kubeconfig {
        Kubeconfig::from_yaml(CONFIG).unwrap()
    }

    #[test]
    fn keeps_only_the_context_with_its_cluster_and_user() {
        let single = single_context(&config(), "dev", Some("kube-system")).unwrap();
        let yaml = to_yaml(&single).unwrap();
        let back = Kubeconfig::from_yaml(&yaml).unwrap();
        assert_eq!(back.current_context.as_deref(), Some("dev"));
        assert_eq!(back.contexts.len(), 1);
        let context = back.contexts[0].context.as_ref().unwrap();
        assert_eq!(context.namespace.as_deref(), Some("kube-system"));
        assert_eq!(context.cluster, "dev-cluster");
        let clusters: Vec<_> = back.clusters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(clusters, ["dev-cluster"]);
        let users: Vec<_> = back.auth_infos.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(users, ["eks"]);
        assert!(!yaml.contains("alice-token"));
        assert!(!yaml.contains("prod.example.com"));
    }

    #[test]
    fn a_context_kept_in_memory_is_written_alone_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("kubeconfig");
        assert!(ContextKubeconfig::of(&config(), "missing", folder.clone()).is_none());
        let kept = ContextKubeconfig::of(&config(), "dev", folder.clone()).unwrap();
        assert!(!format!("{kept:?}").contains("aws"));
        assert_eq!(kept, kept.clone());
        assert_ne!(
            kept,
            ContextKubeconfig::of(&config(), "dev", folder.clone()).unwrap()
        );
        let (first, second) = (kept.write().unwrap(), kept.write().unwrap());
        assert_ne!(first.path(), second.path());
        let yaml = fs::read_to_string(first.path()).unwrap();
        let back = Kubeconfig::from_yaml(&yaml).unwrap();
        assert_eq!(back.contexts.len(), 1);
        assert_eq!(back.current_context.as_deref(), Some("dev"));
        assert!(!yaml.contains("alice-token"));
        let paths = [first.path().to_owned(), second.path().to_owned()];
        drop((first, second));
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn keeps_exec_plugins_and_tokens_as_they_are() {
        let dev = single_context(&config(), "dev", None).unwrap();
        let exec = dev.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap();
        assert_eq!(exec.command.as_deref(), Some("aws"));
        assert_eq!(
            exec.args.as_deref().unwrap(),
            ["eks", "get-token", "--cluster-name", "dev"]
        );
        let yaml = to_yaml(&dev).unwrap();
        let back = Kubeconfig::from_yaml(&yaml).unwrap();
        let env = back.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap()
            .env
            .clone()
            .unwrap();
        assert_eq!(env[0]["name"], "AWS_PROFILE");

        let prod = single_context(&config(), "prod", None).unwrap();
        let yaml = to_yaml(&prod).unwrap();
        assert!(yaml.contains("alice-token"));
        assert!(yaml.contains("Q0E="));
        let back = Kubeconfig::from_yaml(&yaml).unwrap();
        let context = back.contexts[0].context.as_ref().unwrap();
        assert_eq!(context.namespace.as_deref(), Some("web"));
    }

    #[test]
    fn unknown_context_is_none() {
        assert!(single_context(&config(), "missing", None).is_none());
    }

    #[test]
    fn file_is_private_and_removed_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("run").join("kubeconfig");
        let single = single_context(&config(), "prod", None).unwrap();
        let file = TempKubeconfig::create(&dir, &single).unwrap();
        let path = file.path().to_owned();
        assert!(path.starts_with(&dir));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("current-context: prod"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        let second = TempKubeconfig::create(&dir, &single).unwrap();
        assert_ne!(second.path(), path);
        drop(file);
        assert!(!path.exists());
        assert!(second.path().exists());
    }

    /// Names of the files in `dir`.
    fn kubeconfigs(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn file_is_removed_when_a_panic_unwinds() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("kubeconfig");
        let single = single_context(&config(), "prod", None).unwrap();
        let panicked = std::panic::catch_unwind(|| {
            let file = TempKubeconfig::create(&dir, &single).unwrap();
            assert!(file.path().exists());
            panic!("helm's caller panicked");
        });
        assert!(panicked.is_err());
        assert!(kubeconfigs(&dir).is_empty());
    }

    #[test]
    fn removes_orphans_and_own_files_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = &root.path().join("kubeconfig");
        fs::create_dir(dir).unwrap();
        let own = std::process::id();
        // No process has this id: pids are below 2^22 on Linux, 99999 on macOS.
        let gone = 4_000_000_000u32;
        let write = |name: &str| fs::write(dir.join(name), "x").unwrap();
        write(&format!("terminal-{own}-0.yaml"));
        write(&format!("terminal-{gone}-3.yaml"));
        write(&format!("terminal-{gone}-17-4.yaml"));
        write("terminal-x-1.yaml");
        write("notes.txt");

        assert_eq!(remove_orphans(dir), 2);
        assert!(!dir.join(format!("terminal-{gone}-3.yaml")).exists());
        assert!(!dir.join(format!("terminal-{gone}-17-4.yaml")).exists());
        assert!(dir.join(format!("terminal-{own}-0.yaml")).exists());
        assert_eq!(remove_own(dir), 1);
        assert!(!dir.join(format!("terminal-{own}-0.yaml")).exists());
        assert!(dir.join("terminal-x-1.yaml").exists());
        assert!(dir.join("notes.txt").exists());
        assert_eq!(remove_orphans(&dir.join("missing")), 0);
    }

    /// A process id that is in use again does not keep the files of the
    /// process that had it before: its marker is no longer locked.
    #[test]
    fn a_reused_process_id_does_not_keep_leftovers() {
        let root = tempfile::tempdir().unwrap();
        let dir = &root.path().join("kubeconfig");
        let markers = &marker_dir(dir).unwrap();
        assert_eq!(markers, &root.path().join("kubeconfig.owners"));
        fs::create_dir(dir).unwrap();
        fs::create_dir(markers).unwrap();
        let write = |name: &str| fs::write(dir.join(name), "x").unwrap();
        let mark = |name: &str| fs::write(markers.join(name), "").unwrap();
        // Every process id "runs": only the markers tell.
        let alive = |_: u32| true;

        // Gone: the marker is there and nobody holds it.
        write("terminal-700-11-0.yaml");
        write("terminal-700-11-1.yaml");
        mark("owner-700-11.lock");
        // Live: its marker is locked.
        write("terminal-701-12-0.yaml");
        mark("owner-701-12.lock");
        let live = fs::File::open(markers.join("owner-701-12.lock")).unwrap();
        live.try_lock().unwrap();
        // No marker and no token: the process id alone decides, as before.
        write("terminal-702-13-0.yaml");
        write("terminal-703-0.yaml");
        // This process id under another token: an earlier process.
        write(&format!("terminal-{}-1-0.yaml", std::process::id()));

        assert_eq!(remove_orphans_with(dir, alive), 3);
        assert_eq!(
            kubeconfigs(dir),
            [
                "terminal-701-12-0.yaml",
                "terminal-702-13-0.yaml",
                "terminal-703-0.yaml"
            ]
        );
        assert!(!markers.join("owner-700-11.lock").exists());
        assert!(markers.join("owner-701-12.lock").exists());

        // The live process exits: the system drops its lock.
        drop(live);
        assert_eq!(remove_orphans_with(dir, alive), 1);
        assert!(!markers.join("owner-701-12.lock").exists());
        // A process id that is gone needs no marker.
        assert_eq!(remove_orphans_with(dir, |_| false), 2);
        assert!(kubeconfigs(dir).is_empty());
    }

    #[test]
    fn own_files_are_marked_and_kept() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("kubeconfig");
        let single = single_context(&config(), "prod", None).unwrap();
        let file = TempKubeconfig::create(&dir, &single).unwrap();
        let name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let own = Writer::own();
        assert_eq!(Writer::of_file(&name), Some(own));
        let marker = marker_dir(&dir).unwrap().join(own.marker_name().unwrap());
        assert!(marker.exists());
        // The folder itself holds nothing but the kubeconfig.
        assert_eq!(kubeconfigs(&dir), std::slice::from_ref(&name));
        // Locked: another look at the marker does not get the lock.
        assert!(!marker_released(&marker));

        assert_eq!(remove_orphans_with(&dir, |_| true), 0);
        assert_eq!(remove_orphans_with(&dir, |_| false), 0);
        assert!(file.path().exists() && marker.exists());

        // Quit removes the files and the marker; a later file marks again.
        assert_eq!(remove_own(&dir), 1);
        assert!(!file.path().exists() && !marker.exists());
        let again = TempKubeconfig::create(&dir, &single).unwrap();
        assert!(marker.exists());
        assert!(!marker_released(&marker));
        drop(again);
        assert_eq!(remove_own(&dir), 0);
    }

    #[test]
    fn parses_writers_from_names() {
        let writer = |pid, token| Some(Writer { pid, token });
        assert_eq!(Writer::of_file("terminal-42-7.yaml"), writer(42, None));
        assert_eq!(
            Writer::of_file("terminal-42-99-7.yaml"),
            writer(42, Some(99))
        );
        assert_eq!(Writer::of_file("terminal-42.yaml"), None);
        assert_eq!(Writer::of_file("terminal-42-x-7.yaml"), None);
        assert_eq!(Writer::of_file("terminal-1-2-3-4.yaml"), None);
        assert_eq!(Writer::of_file("other-42-7.yaml"), None);
        assert_eq!(Writer::of_marker("owner-42-99.lock"), writer(42, Some(99)));
        assert_eq!(Writer::of_marker("owner-42.lock"), None);
        assert_eq!(Writer::of_marker("terminal-42-99-7.yaml"), None);
        let own = Writer::own();
        assert_eq!(Writer::of_file(&own.file_name(3)), Some(own));
        assert_eq!(Writer::of_marker(&own.marker_name().unwrap()), Some(own));
    }
}
