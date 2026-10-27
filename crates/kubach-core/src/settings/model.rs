//! The settings model and its non-security sections.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::hotbar::HotbarPrefs;
use super::presets::Presets;
use super::security::SecuritySettings;
use crate::helm::Driver;

/// Current schema version, written to the `version` key of the file.
pub const SCHEMA_VERSION: u32 = 1;

/// All application settings. Missing keys take their defaults and unknown
/// keys are ignored, so older and newer files both load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of the file.
    pub version: u32,
    pub general: General,
    pub clusters: Clusters,
    pub tools: Tools,
    pub security: SecuritySettings,
    pub agent: Agent,
    pub helm: HelmPrefs,
    /// Resource presets saved from the editor (#320).
    #[serde(skip_serializing_if = "Presets::is_empty")]
    pub presets: Presets,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            general: General::default(),
            clusters: Clusters::default(),
            tools: Tools::default(),
            security: SecuritySettings::default(),
            agent: Agent::default(),
            helm: HelmPrefs::default(),
            presets: Presets::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub theme: ThemePreference,
    pub language: Language,
    /// Kustomization folders opened last, most recent first (see
    /// [`crate::kustomize::remember_dir`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent_kustomize_dirs: Vec<PathBuf>,
    /// Programs in Shell and Debug tabs may replace the system clipboard
    /// with OSC 52. Off: a pod could put a command there for the user to
    /// paste. Reading the clipboard is never allowed.
    pub terminal_clipboard_write: bool,
    /// Debug opens without asking to add the ephemeral container, which
    /// cannot be removed from the pod (#346): "Don't ask again" in that
    /// confirmation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skip_debug_confirm: bool,
    /// Image of node shell pods (#318); empty or missing takes
    /// [`crate::streams::node_shell::DEFAULT_IMAGE`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_shell_image: Option<String>,
    /// Namespace of node shell pods; empty or missing is `default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_shell_namespace: Option<String>,
}

impl General {
    /// Image node shell pods run.
    pub fn node_shell_image(&self) -> &str {
        non_empty(&self.node_shell_image).unwrap_or(crate::streams::node_shell::DEFAULT_IMAGE)
    }

    /// Namespace node shell pods go to.
    pub fn node_shell_namespace(&self) -> &str {
        non_empty(&self.node_shell_namespace)
            .unwrap_or(crate::streams::node_shell::DEFAULT_NAMESPACE)
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreference {
    #[default]
    Dark,
    Light,
    /// Follows the OS appearance.
    System,
}

/// Interface language (ADR 0018), applied by the app on the fly; partially
/// translated so far.
///
/// Stored as a BCP 47 tag: `en`, `ru`, `zh-Hans`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    En,
    Ru,
    /// Simplified Chinese.
    #[serde(rename = "zh-Hans", alias = "zh-hans")]
    ZhHans,
}

/// Default of [`Clusters::keep_connected`].
pub const DEFAULT_KEEP_CONNECTED: u32 = 3;

/// Default of [`Clusters::vault_lock_minutes`].
pub const DEFAULT_VAULT_LOCK_MINUTES: u32 = 15;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clusters {
    /// Kubeconfig files merged after `KUBECONFIG` (or `~/.kube/config`).
    /// A leading `~/` means the home directory.
    pub kubeconfig_paths: Vec<PathBuf>,
    /// Context to open at startup; `None` uses kubeconfig `current-context`.
    pub startup_context: Option<String>,
    /// Namespace to open per context name.
    pub default_namespaces: BTreeMap<String, String>,
    /// Helm storage driver per context name; a missing context detects it
    /// from the releases found (ADR 0006).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub helm_drivers: BTreeMap<String, Driver>,
    /// Recently used contexts kept connected (the active one included), so
    /// switching back is instant. See [`keep_connected`](Self::keep_connected).
    pub keep_connected: u32,
    /// Arrangement of the cluster hotbar.
    pub hotbar: HotbarPrefs,
    /// Minutes without input after which the vault of imported kubeconfigs
    /// locks itself (ADR 0008); 0 never locks.
    pub vault_lock_minutes: u32,
    /// Lock the vault when the OS screen locks.
    pub vault_lock_on_screen_lock: bool,
    /// Prometheus address typed per context name, as `http(s)://…` or
    /// `namespace/service[:port]`: the [`PrometheusSource::Manual`] source
    /// (ADR 0009).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub prometheus: BTreeMap<String, String>,
    /// How the Prometheus of a context is chosen. A missing context uses
    /// its typed address if it has one (files from before the choice
    /// existed), else [`PrometheusSource::Auto`]; see
    /// [`prometheus_source`](Self::prometheus_source).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub prometheus_sources: BTreeMap<String, PrometheusSource>,
}

/// Where the Prometheus of a context comes from (ADR 0009).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrometheusSource {
    /// The most likely server found in the cluster. The cluster is searched
    /// once and the result remembered in the application state.
    #[default]
    Auto,
    /// One of the servers found, picked by the user:
    /// `namespace/service:port`.
    Found(String),
    /// The address typed in [`Clusters::prometheus`].
    Manual,
    /// No Prometheus: neither searched for nor queried.
    Off,
}

impl Default for Clusters {
    fn default() -> Self {
        Self {
            kubeconfig_paths: Vec::new(),
            startup_context: None,
            default_namespaces: BTreeMap::new(),
            helm_drivers: BTreeMap::new(),
            keep_connected: DEFAULT_KEEP_CONNECTED,
            hotbar: HotbarPrefs::default(),
            vault_lock_minutes: DEFAULT_VAULT_LOCK_MINUTES,
            vault_lock_on_screen_lock: true,
            prometheus: BTreeMap::new(),
            prometheus_sources: BTreeMap::new(),
        }
    }
}

impl Clusters {
    /// `kubeconfig_paths` with `~/` expanded.
    pub fn resolved_kubeconfig_paths(&self) -> Vec<PathBuf> {
        self.kubeconfig_paths
            .iter()
            .map(|path| expand_home(path))
            .collect()
    }

    /// Idle time before the vault locks, `None` for never.
    pub fn vault_lock_after(&self) -> Option<std::time::Duration> {
        (self.vault_lock_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.vault_lock_minutes) * 60))
    }

    /// `keep_connected`, at least 1 (the active context).
    pub fn keep_connected(&self) -> usize {
        self.keep_connected.max(1) as usize
    }

    pub fn default_namespace(&self, context: &str) -> Option<&str> {
        self.default_namespaces.get(context).map(String::as_str)
    }

    /// Prometheus address set for `context`, if any.
    pub fn prometheus(&self, context: &str) -> Option<&str> {
        self.prometheus.get(context).map(String::as_str)
    }

    /// How the Prometheus of `context` is chosen: the stored choice, else
    /// [`PrometheusSource::Manual`] when an address is typed (settings from
    /// before the choice existed), else [`PrometheusSource::Auto`].
    pub fn prometheus_source(&self, context: &str) -> PrometheusSource {
        match self.prometheus_sources.get(context) {
            Some(source) => source.clone(),
            None if self.prometheus.contains_key(context) => PrometheusSource::Manual,
            None => PrometheusSource::Auto,
        }
    }

    /// Chooses how the Prometheus of `context` is found. The typed address
    /// is kept for switching back to [`PrometheusSource::Manual`].
    pub fn set_prometheus_source(&mut self, context: &str, source: PrometheusSource) {
        let implied = source == PrometheusSource::Auto && !self.prometheus.contains_key(context);
        if implied {
            self.prometheus_sources.remove(context);
        } else {
            self.prometheus_sources.insert(context.to_owned(), source);
        }
    }

    /// Back to the default for `context`: [`PrometheusSource::Auto`], no
    /// typed address.
    pub fn reset_prometheus(&mut self, context: &str) {
        self.prometheus.remove(context);
        self.prometheus_sources.remove(context);
    }

    /// Sets or (with `None` or an empty address) clears the typed Prometheus
    /// address of `context`. The source is not changed.
    pub fn set_prometheus(&mut self, context: &str, address: Option<String>) {
        match address.map(|a| a.trim().to_owned()) {
            Some(address) if !address.is_empty() => {
                self.prometheus.insert(context.to_owned(), address);
            }
            _ => {
                // An old-style entry is Manual only through its address:
                // keep the source when the address goes.
                let source = self.prometheus_source(context);
                self.prometheus.remove(context);
                self.set_prometheus_source(context, source);
            }
        }
    }

    /// Helm storage driver picked for `context`; `None` detects it.
    pub fn helm_driver(&self, context: &str) -> Option<Driver> {
        self.helm_drivers.get(context).copied()
    }

    /// Picks the Helm storage driver of `context`; `None` goes back to
    /// detecting it.
    pub fn set_helm_driver(&mut self, context: &str, driver: Option<Driver>) {
        match driver {
            Some(driver) => {
                self.helm_drivers.insert(context.to_owned(), driver);
            }
            None => {
                self.helm_drivers.remove(context);
            }
        }
    }

    /// Sets or (with `None` or an empty name) clears the namespace of `context`.
    pub fn set_default_namespace(&mut self, context: &str, namespace: Option<String>) {
        match namespace.map(|ns| ns.trim().to_owned()) {
            Some(ns) if !ns.is_empty() => {
                self.default_namespaces.insert(context.to_owned(), ns);
            }
            _ => {
                self.default_namespaces.remove(context);
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    /// Explicit `helm` binary; `None` searches `PATH` (ADR 0006).
    pub helm_path: Option<PathBuf>,
    /// Release line of the Helm Kubach downloads (#403): `"4"` or `"3"`.
    pub helm_line: crate::tools::helm::HelmLine,
}

impl Tools {
    /// The `helm` to run: see [`locate_helm`](Self::locate_helm).
    pub fn helm(&self) -> Option<PathBuf> {
        self.locate_helm().map(|helm| helm.path)
    }

    /// The `helm` to run and where it was found: the path set in settings,
    /// else `helm` on `PATH`, else the copy Kubach downloaded (#131).
    pub fn locate_helm(&self) -> Option<HelmBinary> {
        resolve_helm(
            self.helm_path.as_deref().map(expand_home),
            || find_on_path("helm"),
            crate::tools::helm::downloaded_path,
        )
    }
}

/// Where the `helm` in use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelmSource {
    /// `tools.helm_path` in settings.
    Settings,
    /// Found on `PATH`.
    Path,
    /// Downloaded by Kubach into its data directory.
    Downloaded,
}

/// A `helm` binary found by [`Tools::locate_helm`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelmBinary {
    pub path: PathBuf,
    pub source: HelmSource,
}

/// The lookup order of [`Tools::locate_helm`]; later sources are only
/// asked when earlier ones have nothing.
fn resolve_helm(
    explicit: Option<PathBuf>,
    on_path: impl FnOnce() -> Option<PathBuf>,
    downloaded: impl FnOnce() -> Option<PathBuf>,
) -> Option<HelmBinary> {
    let found = |source| move |path| HelmBinary { path, source };
    explicit
        .map(found(HelmSource::Settings))
        .or_else(|| on_path().map(found(HelmSource::Path)))
        .or_else(|| downloaded().map(found(HelmSource::Downloaded)))
}

/// Finds an executable on `PATH` (with `.exe` on Windows).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    find_in(&file, &crate::env::var_os("PATH")?)
}

/// `file` in the absolute directories of `path`: empty and relative `PATH`
/// entries would resolve against the working directory (#202).
fn find_in(file: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    crate::which::search_dirs(path)
        .map(|dir| dir.join(file))
        .find(|path| path.is_file())
}

/// Replaces a leading `~/` (or a lone `~`) with the home directory.
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_owned();
    };
    match crate::env::home_dir() {
        Some(home) => home.join(rest),
        None => path.to_owned(),
    }
}

/// Helm pages (ADR 0006).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HelmPrefs {
    /// Download chart icons and README images from the URLs charts give
    /// (`kubach_core::images`); off, Kubach sends no such request.
    pub load_chart_icons: bool,
}

impl Default for HelmPrefs {
    fn default() -> Self {
        Self {
            load_chart_icons: true,
        }
    }
}

/// Default port of the MCP HTTP server ([`Agent::http_port`]).
pub const DEFAULT_MCP_PORT: u16 = 47821;

/// The MCP server of the running app (#86). Which contexts an agent may use
/// and how is in [`SecuritySettings`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agent {
    /// Serve MCP over HTTP on `127.0.0.1` while Kubach runs. `kubach mcp`
    /// (stdio) also reaches the app through it to ask for confirmations.
    pub http_enabled: bool,
    /// Fixed, so agent configs keep working across restarts.
    pub http_port: u16,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            http_enabled: false,
            http_port: DEFAULT_MCP_PORT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_shell_defaults_and_overrides() {
        let mut general = General::default();
        assert_eq!(general.node_shell_namespace(), "default");
        assert_eq!(
            general.node_shell_image(),
            crate::streams::node_shell::DEFAULT_IMAGE
        );
        general.node_shell_namespace = Some("  ".into());
        assert_eq!(general.node_shell_namespace(), "default");
        general.node_shell_namespace = Some(" tools ".into());
        general.node_shell_image = Some("alpine:3.20".into());
        assert_eq!(general.node_shell_namespace(), "tools");
        assert_eq!(general.node_shell_image(), "alpine:3.20");
    }

    #[test]
    fn find_in_skips_empty_and_relative_path_entries() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("i202-helm");
        std::fs::write(&file, "").unwrap();
        let relative = dir
            .path()
            .strip_prefix(dir.path().ancestors().last().unwrap())
            .unwrap();
        let path = std::env::join_paths([Path::new(""), Path::new("."), relative]).unwrap();
        assert_eq!(find_in("i202-helm", &path), None);
        let path = std::env::join_paths([Path::new(""), dir.path()]).unwrap();
        assert_eq!(find_in("i202-helm", &path), Some(file));
    }

    fn located(
        explicit: Option<&str>,
        on_path: Option<&str>,
        downloaded: Option<&str>,
    ) -> Option<(String, HelmSource)> {
        resolve_helm(
            explicit.map(PathBuf::from),
            || on_path.map(PathBuf::from),
            || downloaded.map(PathBuf::from),
        )
        .map(|helm| (helm.path.display().to_string(), helm.source))
    }

    #[test]
    fn helm_lookup_order() {
        let all = located(Some("/opt/helm"), Some("/usr/bin/helm"), Some("/data/helm"));
        assert_eq!(all, Some(("/opt/helm".into(), HelmSource::Settings)));
        let no_setting = located(None, Some("/usr/bin/helm"), Some("/data/helm"));
        assert_eq!(no_setting, Some(("/usr/bin/helm".into(), HelmSource::Path)));
        let only_downloaded = located(None, None, Some("/data/helm"));
        assert_eq!(
            only_downloaded,
            Some(("/data/helm".into(), HelmSource::Downloaded))
        );
        assert_eq!(located(None, None, None), None);
    }

    #[test]
    fn later_sources_are_not_asked() {
        let found = resolve_helm(
            Some(PathBuf::from("/opt/helm")),
            || panic!("PATH searched"),
            || panic!("downloads searched"),
        );
        assert_eq!(found.unwrap().source, HelmSource::Settings);
    }

    #[test]
    fn terminal_clipboard_write_is_off_unless_set() {
        assert!(!Settings::default().general.terminal_clipboard_write);
        let older: Settings = toml::from_str("[general]\ntheme = \"light\"\n").unwrap();
        assert!(!older.general.terminal_clipboard_write);
        let allowed: Settings =
            toml::from_str("[general]\nterminal_clipboard_write = true\n").unwrap();
        assert!(allowed.general.terminal_clipboard_write);
    }
}
