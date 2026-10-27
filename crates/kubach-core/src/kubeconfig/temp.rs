//! Temporary kubeconfig of a local terminal (#319, ADR 0015): one context —
//! the one open in Kubach — with its namespace, written to a private file the
//! terminal's shell gets as `KUBECONFIG`.
//!
//! The file holds credentials (tokens, client keys, materialized vault
//! configs), so it is created readable by the owner only (`0600` from the
//! start on Unix, an owner-only DACL on Windows), lives in
//! `<config dir>/run/kubeconfig` (`0700`) and not in the shared temp folder,
//! and is removed when its tab closes, when Kubach quits and, for files left
//! by a crash, on the next start.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kube::config::Kubeconfig;
use zeroize::Zeroizing;

/// File name prefix: `terminal-<pid>-<n>.yaml`.
const PREFIX: &str = "terminal-";
const SUFFIX: &str = ".yaml";

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
        let path = dir.join(format!("{PREFIX}{}-{n}{SUFFIX}", std::process::id()));
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

/// Process id in a file name written by [`TempKubeconfig::create`].
fn owner_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (pid, n) = rest.split_once('-')?;
    n.parse::<u64>().ok()?;
    pid.parse().ok()
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

/// Removes the files in `dir` whose writer process is `stale`; returns how
/// many were removed.
fn remove_where(dir: &Path, stale: impl Fn(u32) -> bool) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(owner_pid)
                .is_some_and(&stale)
        })
        .filter(|entry| fs::remove_file(entry.path()).is_ok())
        .count()
}

/// At startup: removes files of Kubach processes that are gone.
pub fn remove_orphans(dir: &Path) -> usize {
    remove_where(dir, |pid| !is_alive(pid))
}

/// At quit: removes every file this process wrote.
pub fn remove_own(dir: &Path) -> usize {
    let own = std::process::id();
    remove_where(dir, |pid| pid == own)
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

    #[test]
    fn removes_orphans_and_own_files_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let own = std::process::id();
        // No process has this id: pids are below 2^22 on Linux, 99999 on macOS.
        let gone = 4_000_000_000u32;
        let write = |name: &str| fs::write(dir.join(name), "x").unwrap();
        write(&format!("terminal-{own}-0.yaml"));
        write(&format!("terminal-{gone}-3.yaml"));
        write("terminal-x-1.yaml");
        write("notes.txt");

        assert_eq!(remove_orphans(dir), 1);
        assert!(!dir.join(format!("terminal-{gone}-3.yaml")).exists());
        assert!(dir.join(format!("terminal-{own}-0.yaml")).exists());
        assert_eq!(remove_own(dir), 1);
        assert!(!dir.join(format!("terminal-{own}-0.yaml")).exists());
        assert!(dir.join("terminal-x-1.yaml").exists());
        assert!(dir.join("notes.txt").exists());
        assert_eq!(remove_orphans(&dir.join("missing")), 0);
    }

    #[test]
    fn parses_owner_pid() {
        assert_eq!(owner_pid("terminal-42-7.yaml"), Some(42));
        assert_eq!(owner_pid("terminal-42.yaml"), None);
        assert_eq!(owner_pid("other-42-7.yaml"), None);
    }
}
