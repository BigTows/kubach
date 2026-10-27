//! Kubeconfig files contexts come from: the default ones (`KUBECONFIG` or
//! `~/.kube/config`) and the files added in settings
//! (`clusters.kubeconfig_paths`).
//!
//! Added files are linked, not copied: they are read in place on every load,
//! so `aws eks update-kubeconfig`, `gcloud` or OIDC plugins rewriting them
//! are picked up and no credentials are duplicated. A linked file can go
//! missing; its problem is reported per file instead of failing the load.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kubach_i18n::{Message, msg};
use kube::config::{Kubeconfig, KubeconfigError};

use crate::cluster::Contexts;

pub mod review;
pub mod temp;

use self::review::ConfigReview;

/// Environment variable with the default kubeconfig files.
pub const KUBECONFIG_ENV: &str = "KUBECONFIG";

/// Where a kubeconfig file (and so a context) comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Listed in `KUBECONFIG`.
    Env(PathBuf),
    /// `~/.kube/config`, used when `KUBECONFIG` is unset or empty.
    Home(PathBuf),
    /// Added in settings and read in place.
    Linked(PathBuf),
    /// Imported into the encrypted vault at `vault` (ADR 0008).
    Imported {
        vault: PathBuf,
        id: String,
        name: String,
    },
}

impl Source {
    pub fn path(&self) -> &Path {
        match self {
            Source::Env(path) | Source::Home(path) | Source::Linked(path) => path,
            Source::Imported { vault, .. } => vault,
        }
    }

    /// Imported into the vault.
    pub fn is_imported(&self) -> bool {
        matches!(self, Source::Imported { .. })
    }

    /// Added by the user (can be relocated or removed), as opposed to the
    /// default kubeconfig.
    pub fn is_linked(&self) -> bool {
        matches!(self, Source::Linked(_))
    }

    /// Short name of the origin: `KUBECONFIG`, `~/.kube/config` or
    /// `Linked file`.
    pub fn origin(&self) -> &'static str {
        match self {
            Source::Env(_) => "KUBECONFIG",
            Source::Home(_) => "~/.kube/config",
            Source::Linked(_) => "Linked file",
            Source::Imported { .. } => "Imported",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Home(_) => f.write_str(self.origin()),
            Source::Imported { name, .. } => write!(f, "{}: {name}", self.origin()),
            _ => write!(f, "{}: {}", self.origin(), self.path().display()),
        }
    }
}

/// The default kubeconfig files, by kubectl rules: every file in
/// `KUBECONFIG`, else `~/.kube/config`; both from the login shell's
/// environment when it was captured ([`crate::env`]).
pub fn default_sources() -> Vec<Source> {
    sources_from(crate::env::var_os(KUBECONFIG_ENV), crate::env::home_dir())
}

fn sources_from(env: Option<OsString>, home: Option<PathBuf>) -> Vec<Source> {
    let listed: Vec<Source> = env
        .iter()
        .flat_map(std::env::split_paths)
        .filter(|path| !path.as_os_str().is_empty())
        .map(Source::Env)
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    home.map(|home| Source::Home(home.join(".kube").join("config")))
        .into_iter()
        .collect()
}

/// Default sources followed by `linked` files (paths already expanded).
pub fn sources_with(linked: &[PathBuf]) -> Vec<Source> {
    let mut sources = default_sources();
    sources.extend(linked.iter().cloned().map(Source::Linked));
    sources
}

/// Why a kubeconfig file gave no contexts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileProblem {
    /// Nothing at the path: moved, deleted, or on a disk not mounted.
    Missing,
    /// There but not readable (permissions, a directory, …).
    Unreadable(String),
    /// Read but not a valid kubeconfig, or conflicts with the files before
    /// it (different `kind` or `apiVersion`).
    Invalid(String),
    /// Imported into the vault, which is locked.
    Locked,
}

impl FileProblem {
    /// The problem for the screen; [`Display`](fmt::Display) is its English
    /// text (logs, MCP).
    pub fn message(&self) -> Message {
        match self {
            FileProblem::Missing => msg!("kubeconfig-file-missing"),
            FileProblem::Unreadable(error) => {
                msg!("kubeconfig-file-unreadable", error = error.as_str())
            }
            FileProblem::Invalid(error) => msg!("kubeconfig-file-invalid", error = error.as_str()),
            FileProblem::Locked => msg!("kubeconfig-file-locked"),
        }
    }
}

impl fmt::Display for FileProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message().english())
    }
}

fn io_problem(error: &io::Error) -> FileProblem {
    match error.kind() {
        io::ErrorKind::NotFound => FileProblem::Missing,
        _ => FileProblem::Unreadable(error.to_string()),
    }
}

/// Reads one kubeconfig file (relative certificate paths are made absolute).
pub fn read(path: &Path) -> Result<Kubeconfig, FileProblem> {
    let metadata = std::fs::metadata(path).map_err(|error| io_problem(&error))?;
    if metadata.is_dir() {
        return Err(FileProblem::Unreadable("it is a directory".into()));
    }
    Kubeconfig::read_from(path).map_err(|error| match error {
        KubeconfigError::ReadConfig(io, _) => io_problem(&io),
        other => FileProblem::Invalid(other.to_string()),
    })
}

/// Largest file accepted when adding a kubeconfig; real ones are a few KiB,
/// so anything bigger was dropped or picked by mistake.
pub const MAX_ADDED_SIZE: u64 = 4 * 1024 * 1024;

/// Reads a file the user adds as a kubeconfig (picked or dragged onto the
/// window): like [`read`], but also turns away files that are clearly
/// something else, so the preview says so instead of showing an empty
/// config.
pub fn read_added(path: &Path) -> Result<Kubeconfig, FileProblem> {
    let metadata = std::fs::metadata(path).map_err(|error| io_problem(&error))?;
    if metadata.is_dir() {
        return Err(FileProblem::Unreadable(
            "it is a folder, not a kubeconfig file".into(),
        ));
    }
    if metadata.len() == 0 {
        return Err(FileProblem::Invalid("the file is empty".into()));
    }
    if metadata.len() > MAX_ADDED_SIZE {
        return Err(FileProblem::Invalid(format!(
            "the file is too large ({} KiB) for a kubeconfig",
            metadata.len() / 1024
        )));
    }
    let config = Kubeconfig::read_from(path).map_err(|error| match error {
        KubeconfigError::ReadConfig(io, _) if io.kind() == io::ErrorKind::InvalidData => {
            FileProblem::Invalid("not a text file".into())
        }
        KubeconfigError::ReadConfig(io, _) => io_problem(&io),
        other => FileProblem::Invalid(other.to_string()),
    })?;
    check_kubeconfig(&config)?;
    Ok(config)
}

/// Whether parsed YAML is a kubeconfig: `kind: Config`, or (without a
/// `kind`) at least one cluster, context or user. Other YAML, such as a
/// manifest, parses into an empty config because every field is optional.
pub fn check_kubeconfig(config: &Kubeconfig) -> Result<(), FileProblem> {
    match config.kind.as_deref() {
        Some("Config") => Ok(()),
        Some(kind) => Err(FileProblem::Invalid(format!(
            "it is a {kind}, expected kind: Config"
        ))),
        None if !config.clusters.is_empty()
            || !config.contexts.is_empty()
            || !config.auth_infos.is_empty() =>
        {
            Ok(())
        }
        None => Err(FileProblem::Invalid(
            "no clusters, contexts or users in it".into(),
        )),
    }
}

/// Paths dragged onto the window, each once, in the order given.
pub fn dropped_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(*path))
        .map(Path::to_path_buf)
        .collect()
}

/// Permission bits of `path` (e.g. `0o644`) when other users may read or
/// write it, i.e. anything wider than `0600`. Always `None` on Windows,
/// where files are protected by ACLs instead.
pub fn loose_permissions(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
        (mode & 0o077 != 0).then_some(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Makes `path` readable and writable by its owner only: `0600` on Unix,
/// on Windows a DACL with only the current user (inherited entries removed).
pub fn restrict_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        crate::windows_acl::restrict_to_current_user(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

/// `0o644` → `0644`.
pub fn format_mode(mode: u32) -> String {
    format!("{mode:04o}")
}

/// One kubeconfig file as of the last load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub source: Source,
    /// Why the file gave no contexts; `None` when it was read.
    pub problem: Option<FileProblem>,
    /// Permission bits when wider than `0600`; see [`loose_permissions`].
    pub loose_mode: Option<u32>,
    /// Names of the contexts the file defines (in file order), including
    /// ones shadowed by an earlier file with the same name. Known for a
    /// locked vault entry too.
    pub contexts: Vec<String>,
}

impl FileState {
    pub fn path(&self) -> &Path {
        self.source.path()
    }

    pub fn is_ok(&self) -> bool {
        self.problem.is_none()
    }
}

/// A file looked at before adding it.
#[derive(Clone, Debug)]
pub struct Preview {
    pub contexts: Contexts,
    pub loose_mode: Option<u32>,
    /// Programs the file runs and local files it reads on connect.
    pub review: ConfigReview,
}

/// Reads `path` to show its contexts before it is added; see
/// [`read_added`].
pub fn preview(path: &Path) -> Result<Preview, FileProblem> {
    let config = read_added(path)?;
    Ok(Preview {
        review: ConfigReview::of(&config, path.parent()),
        contexts: Contexts::from_kubeconfig(config),
        loose_mode: loose_permissions(path),
    })
}

/// Stops watching when dropped.
pub struct FilesWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// Calls `on_change` (from a background thread) once any of `paths` is
/// created, changed, replaced or removed and things settled for `debounce`.
///
/// Watches the parent directories, so a file that disappears and comes back
/// is noticed. Directories that don't exist are skipped.
pub fn watch(
    paths: &[PathBuf],
    debounce: Duration,
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<FilesWatcher> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut names: HashSet<OsString> = HashSet::new();
    for path in paths {
        let Some(name) = path.file_name() else {
            continue;
        };
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_owned();
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
        names.insert(OsStr::to_owned(name));
    }
    let watcher =
        crate::settings::watch_names(&dirs, names, debounce, "kubach-kubeconfig-watch", on_change)?;
    Ok(FilesWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: dev
clusters:
  - name: dev-cluster
    cluster: { server: "https://dev.example.com" }
contexts:
  - name: dev
    context: { cluster: dev-cluster }
  - name: dev-admin
    context: { cluster: dev-cluster }
"#;

    #[test]
    fn default_sources_follow_kubectl_rules() {
        let home = PathBuf::from("/home/me");
        assert_eq!(
            sources_from(None, Some(home.clone())),
            [Source::Home(home.join(".kube").join("config"))]
        );
        // An empty KUBECONFIG counts as unset.
        assert_eq!(
            sources_from(Some(OsString::new()), Some(home.clone())),
            [Source::Home(home.join(".kube").join("config"))]
        );
        let list = std::env::join_paths(["/a/one", "", "/b/two"]).unwrap();
        assert_eq!(
            sources_from(Some(list), Some(home)),
            [Source::Env("/a/one".into()), Source::Env("/b/two".into())]
        );
        assert!(sources_from(None, None).is_empty());
    }

    #[test]
    fn source_labels() {
        let linked = Source::Linked("/x/eks.yaml".into());
        assert!(linked.is_linked());
        assert_eq!(linked.to_string(), "Linked file: /x/eks.yaml");
        let home = Source::Home("/home/me/.kube/config".into());
        assert!(!home.is_linked());
        assert_eq!(home.to_string(), "~/.kube/config");
        assert_eq!(
            Source::Env("/a".into()).to_string(),
            "KUBECONFIG: /a".to_owned()
        );
    }

    #[test]
    fn read_tells_missing_from_invalid() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read(&dir.path().join("nope.yaml")).err(),
            Some(FileProblem::Missing)
        );
        assert!(matches!(read(dir.path()), Err(FileProblem::Unreadable(_))));

        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "contexts: [ this is: not yaml").unwrap();
        assert!(matches!(read(&bad), Err(FileProblem::Invalid(_))));

        let good = dir.path().join("good.yaml");
        std::fs::write(&good, CONFIG).unwrap();
        assert_eq!(read(&good).unwrap().contexts.len(), 2);
    }

    #[test]
    fn preview_lists_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, CONFIG).unwrap();
        let preview = preview(&path).unwrap();
        let names: Vec<_> = preview
            .contexts
            .list()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["dev", "dev-admin"]);
        assert_eq!(
            preview.contexts.server("dev"),
            Some("https://dev.example.com")
        );
    }

    #[test]
    fn added_file_must_be_a_kubeconfig() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, data: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            path
        };
        let invalid = |path: &Path| match read_added(path) {
            Err(FileProblem::Invalid(reason)) => reason,
            other => panic!("{}: {other:?}", path.display()),
        };

        assert_eq!(
            read_added(&write("ok", CONFIG.as_bytes()))
                .unwrap()
                .contexts
                .len(),
            2
        );
        // No `kind`, but clusters: still a kubeconfig.
        let bare = write(
            "bare",
            b"clusters:\n  - name: c\n    cluster: { server: https://c }\n",
        );
        assert!(read_added(&bare).is_ok());
        // `kind: Config` with nothing in it is an empty kubeconfig.
        assert!(read_added(&write("empty-config", b"apiVersion: v1\nkind: Config\n")).is_ok());

        let manifest = write(
            "deploy.yaml",
            b"apiVersion: apps/v1\nkind: Deployment\nmetadata: { name: web }\n",
        );
        assert!(invalid(&manifest).contains("Deployment"));
        assert!(invalid(&write("other.yaml", b"foo: bar\n")).contains("no clusters"));
        assert!(!invalid(&write("notes.txt", b"just some text")).is_empty());
        assert_eq!(invalid(&write("empty", b"")), "the file is empty");
        assert_eq!(
            invalid(&write(
                "image.png",
                &[0x89, b'P', b'N', b'G', 0xff, 0xfe, 0]
            )),
            "not a text file"
        );
        let big = write("big", &vec![b'#'; MAX_ADDED_SIZE as usize + 1]);
        assert!(invalid(&big).contains("too large"));

        assert!(matches!(
            read_added(dir.path()),
            Err(FileProblem::Unreadable(_))
        ));
        assert_eq!(
            read_added(&dir.path().join("gone")).err(),
            Some(FileProblem::Missing)
        );
        // Previews of added files use the same checks.
        assert!(preview(&manifest).is_err());
    }

    #[test]
    fn dropped_paths_are_deduplicated_in_order() {
        let paths = [
            PathBuf::from("/a/eks.yaml"),
            PathBuf::from("/b/config"),
            PathBuf::from("/a/eks.yaml"),
        ];
        assert_eq!(
            dropped_paths(paths.iter().map(PathBuf::as_path)),
            [PathBuf::from("/a/eks.yaml"), PathBuf::from("/b/config")]
        );
        assert!(dropped_paths([]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn detects_and_fixes_loose_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, CONFIG).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(loose_permissions(&path), Some(0o644));
        assert_eq!(format_mode(0o644), "0644");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(loose_permissions(&path), Some(0o640));

        restrict_permissions(&path).unwrap();
        assert_eq!(loose_permissions(&path), None);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(loose_permissions(&path), None);
        assert_eq!(loose_permissions(&dir.path().join("missing")), None);
    }

    #[test]
    fn watch_reports_a_file_coming_and_going() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eks.yaml");
        let missing_dir = dir.path().join("gone").join("config");
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = changes.clone();
        let watcher = watch(
            &[path.clone(), missing_dir],
            Duration::from_millis(100),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let wait = |at_least: usize| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while changes.load(Ordering::SeqCst) < at_least && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            changes.load(Ordering::SeqCst)
        };
        std::fs::write(&path, CONFIG).unwrap();
        assert!(wait(1) >= 1);
        let before = changes.load(Ordering::SeqCst);
        std::fs::remove_file(&path).unwrap();
        assert!(wait(before + 1) > before);
        drop(watcher);
    }
}
