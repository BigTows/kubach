//! Application state: what Kubach remembers between runs on its own, as
//! opposed to the settings the user edits.
//!
//! Stored as JSON in `state.json` next to `settings.toml` ([`default_path`],
//! honours `KUBACH_CONFIG_DIR`). JSON keys are plain strings, so context names
//! with dots, colons and slashes (EKS ARNs) need no quoting rules. The file is
//! disposable: a missing or broken file, or a broken entry, gives the defaults
//! for what cannot be read, never an error. Writes are atomic.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings::{config_dir, write_atomic};

pub const STATE_FILE: &str = "state.json";

/// Version written to the file. Bump it with a migration in [`parse`] when
/// the format changes incompatibly.
pub const STATE_VERSION: u32 = 1;

/// Recently picked namespaces kept per context.
pub const RECENT_MAX: usize = 5;

/// Path of the state file in [`config_dir`].
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(STATE_FILE))
}

/// Everything remembered between runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppState {
    pub version: u32,
    /// Last namespace selection per context name.
    pub namespaces: BTreeMap<String, NamespaceMemory>,
    /// Column keys in the order the user arranged them, per resource kind
    /// ([`kind_key`]). Kinds in the default order are not stored.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_orders: BTreeMap<String, Vec<String>>,
    /// Result of the last search for Prometheus per context name (ADR 0009),
    /// so the cluster is searched once, not on every start.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub prometheus: BTreeMap<String, PrometheusDiscovery>,
    /// The debug overlay of debug builds is shown (#291). Release builds
    /// ignore it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub debug_overlay: bool,
    /// Widths of resizable side panels the user dragged, in whole pixels,
    /// per panel key such as `browser.details` (#309). Default widths are
    /// not stored.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub panel_widths: BTreeMap<String, u32>,
    /// Local ports the user picked for port-forwards (#464), per context
    /// name, then per [`forward_key`]. Forwards to a random port are not
    /// stored.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub forward_ports: BTreeMap<String, BTreeMap<String, u16>>,
    /// Port-forwards running when Kubach quit, restarted on the next run
    /// (#474). A forward leaves the list only when the user stops or
    /// removes it; one that failed on its own stays.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<SavedForward>,
    /// Column and direction a table is sorted by, per table (#33): a
    /// resource kind ([`kind_key`]) or a screen ([`screen_table_key`]).
    /// Tables in their default order are not stored. Shared by all
    /// contexts.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub table_sorts: BTreeMap<String, TableSort>,
}

/// How a table is sorted: the stable key of a column (never its title or
/// index) and the direction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSort {
    pub column: String,
    pub order: SortOrder,
}

/// Direction of a [`TableSort`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    #[serde(rename = "asc")]
    Ascending,
    #[serde(rename = "desc")]
    Descending,
}

impl TableSort {
    pub fn new(column: impl Into<String>, order: SortOrder) -> Self {
        Self {
            column: column.into(),
            order,
        }
    }

    /// Trims the column; `None` when it is empty.
    fn normalized(self) -> Option<Self> {
        let column = self.column.trim();
        (!column.is_empty()).then(|| Self::new(column, self.order))
    }
}

/// A port-forward to restart on the next run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedForward {
    pub context: String,
    pub namespace: String,
    /// `Pod`, `Service`, `Deployment`, … (as in
    /// [`ForwardSource::kind`](crate::streams::ForwardSource::kind)).
    pub kind: String,
    pub name: String,
    /// The port asked for on the source (a Service port for Services).
    pub port: u16,
    /// The local port the forward listened on; restored exactly.
    pub local_port: u16,
    /// Name of the forwarded port, for "Open in browser".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_name: Option<String>,
    /// Paused by the user (#476): restarted on the next run holding its
    /// local port, but paused. Missing in files written before.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl SavedForward {
    /// Whether `self` and `other` forward the same port of the same source
    /// in the same context; the local port does not matter.
    pub fn same_target(&self, other: &SavedForward) -> bool {
        self.context == other.context
            && self.namespace == other.namespace
            && self.kind == other.kind
            && self.name == other.name
            && self.port == other.port
    }

    /// Trims names; `None` when a name is empty or a port is 0.
    fn normalized(self) -> Option<Self> {
        let trim = |text: String| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
        Some(Self {
            context: trim(self.context)?,
            namespace: trim(self.namespace)?,
            kind: trim(self.kind)?,
            name: trim(self.name)?,
            port: Some(self.port).filter(|port| *port > 0)?,
            local_port: Some(self.local_port).filter(|port| *port > 0)?,
            port_name: self.port_name.and_then(trim),
            paused: self.paused,
        })
    }
}

/// What a search for Prometheus in a cluster found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrometheusDiscovery {
    /// Servers that answered, most likely first, as
    /// `namespace/service:port`; empty when none was found.
    #[serde(default)]
    pub found: Vec<String>,
    /// When the search finished, in Unix seconds.
    #[serde(default)]
    pub at: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            namespaces: BTreeMap::new(),
            column_orders: BTreeMap::new(),
            prometheus: BTreeMap::new(),
            debug_overlay: false,
            panel_widths: BTreeMap::new(),
            forward_ports: BTreeMap::new(),
            forwards: Vec::new(),
            table_sorts: BTreeMap::new(),
        }
    }
}

impl AppState {
    /// Remembered namespace selection of `context`.
    pub fn namespaces(&self, context: &str) -> Option<&NamespaceMemory> {
        self.namespaces.get(context)
    }

    /// Remembers the namespace selection of `context`; returns whether the
    /// state changed. A memory that holds nothing usable forgets the context.
    pub fn remember_namespaces(&mut self, context: &str, memory: NamespaceMemory) -> bool {
        match memory.normalized() {
            Some(memory) => {
                if self.namespaces.get(context) == Some(&memory) {
                    return false;
                }
                self.namespaces.insert(context.to_owned(), memory);
                true
            }
            None => self.namespaces.remove(context).is_some(),
        }
    }

    /// Remembered column order of `kind` ([`kind_key`]), possibly stale: pass
    /// it through [`column_order`] before use.
    pub fn column_order(&self, kind: &str) -> Option<&[String]> {
        self.column_orders.get(kind).map(Vec::as_slice)
    }

    /// Remembers the column order of `kind` after the user moved a column;
    /// returns whether the state changed.
    ///
    /// `shown` is the order of the columns on screen, `default` the plugin
    /// order of every column the kind can show. Columns that are hidden now
    /// (Namespace in a single namespace, metrics) keep their place from the
    /// previous order. An order equal to `default` is forgotten.
    pub fn remember_column_order(
        &mut self,
        kind: &str,
        shown: &[impl AsRef<str>],
        default: &[impl AsRef<str>],
    ) -> bool {
        let previous = self.column_orders.get(kind).cloned().unwrap_or_default();
        let order = interleave(&previous, clean(owned(shown), usize::MAX));
        if column_order(default, Some(&order)) == owned(default) {
            return self.forget_column_order(kind);
        }
        if self.column_orders.get(kind) == Some(&order) {
            return false;
        }
        self.column_orders.insert(kind.to_owned(), order);
        true
    }

    /// Result of the last search for Prometheus in `context`; `None` when
    /// the cluster was never searched.
    pub fn prometheus(&self, context: &str) -> Option<&PrometheusDiscovery> {
        self.prometheus.get(context)
    }

    /// Remembers a search for Prometheus in `context`; returns whether the
    /// state changed.
    pub fn remember_prometheus(&mut self, context: &str, discovery: PrometheusDiscovery) -> bool {
        let discovery = PrometheusDiscovery {
            found: clean(discovery.found, usize::MAX),
            at: discovery.at,
        };
        if self.prometheus.get(context) == Some(&discovery) {
            return false;
        }
        self.prometheus.insert(context.to_owned(), discovery);
        true
    }

    /// Width the user gave the side panel `key`, if any.
    pub fn panel_width(&self, key: &str) -> Option<u32> {
        self.panel_widths.get(key).copied()
    }

    /// Remembers the width of the side panel `key`, or forgets it with
    /// `None` (back to the default); returns whether the state changed.
    /// A zero width is forgotten.
    pub fn remember_panel_width(&mut self, key: &str, width: Option<u32>) -> bool {
        match width.filter(|width| *width > 0) {
            Some(width) => self.panel_widths.insert(key.to_owned(), width) != Some(width),
            None => self.panel_widths.remove(key).is_some(),
        }
    }

    /// Local port picked for the forward [`forward_key`] in `context`.
    pub fn forward_port(&self, context: &str, key: &str) -> Option<u16> {
        self.forward_ports.get(context)?.get(key).copied()
    }

    /// Remembers the local port picked for the forward `key` in `context`,
    /// or forgets it with `None` (a random port); returns whether the state
    /// changed. Port 0 is forgotten.
    pub fn remember_forward_port(&mut self, context: &str, key: &str, port: Option<u16>) -> bool {
        match port.filter(|port| *port > 0) {
            Some(port) => {
                let ports = self.forward_ports.entry(context.to_owned()).or_default();
                ports.insert(key.to_owned(), port) != Some(port)
            }
            None => {
                let Some(ports) = self.forward_ports.get_mut(context) else {
                    return false;
                };
                let removed = ports.remove(key).is_some();
                if ports.is_empty() {
                    self.forward_ports.remove(context);
                }
                removed
            }
        }
    }

    /// Forwards to restart on this run (#474).
    pub fn saved_forwards(&self) -> &[SavedForward] {
        &self.forwards
    }

    /// Remembers a started forward, replacing one to the same target;
    /// returns whether the state changed. A forward with an empty name or
    /// port 0 is not remembered.
    pub fn remember_forward(&mut self, forward: SavedForward) -> bool {
        let Some(forward) = forward.normalized() else {
            return false;
        };
        match self.forwards.iter_mut().find(|f| f.same_target(&forward)) {
            Some(saved) if *saved == forward => false,
            Some(saved) => {
                *saved = forward;
                true
            }
            None => {
                self.forwards.push(forward);
                true
            }
        }
    }

    /// Forgets the forward to the target of `forward` (stopped or removed by
    /// the user); returns whether the state changed.
    pub fn forget_forward(&mut self, forward: &SavedForward) -> bool {
        let before = self.forwards.len();
        self.forwards.retain(|f| !f.same_target(forward));
        self.forwards.len() != before
    }

    /// Goes back to the default column order of `kind`; returns whether the
    /// state changed.
    pub fn forget_column_order(&mut self, kind: &str) -> bool {
        self.column_orders.remove(kind).is_some()
    }

    /// Remembered sort of the table `key` ([`kind_key`] or
    /// [`screen_table_key`]). Its column may be gone: the table then keeps
    /// its default order.
    pub fn table_sort(&self, key: &str) -> Option<&TableSort> {
        self.table_sorts.get(key)
    }

    /// Remembers the sort of the table `key`, or forgets it with `None`
    /// (the default order); returns whether the state changed. A sort
    /// without a column is forgotten.
    pub fn remember_table_sort(&mut self, key: &str, sort: Option<TableSort>) -> bool {
        match sort.and_then(TableSort::normalized) {
            Some(sort) => self.table_sorts.insert(key.to_owned(), sort.clone()) != Some(sort),
            None => self.table_sorts.remove(key).is_some(),
        }
    }
}

/// Key of a table outside the resource browser in [`AppState::table_sorts`]:
/// `screen:gitops`. The prefix keeps it apart from [`kind_key`]s.
pub fn screen_table_key(screen: &str) -> String {
    format!("screen:{screen}")
}

/// Key of a resource kind in [`AppState::column_orders`]: `Pod`,
/// `Deployment.apps`; stable across API versions.
pub fn kind_key(group: &str, kind: &str) -> String {
    if group.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}.{group}")
    }
}

/// Order to show the `default` columns in, given a `saved` order of keys.
///
/// The result holds exactly the `default` keys. Saved keys come in saved
/// order; keys the saved order does not know (a column added by a newer
/// plugin) go to their default place, right after the column that precedes
/// them by default. Saved keys that are gone are skipped.
pub fn column_order(default: &[impl AsRef<str>], saved: Option<&[String]>) -> Vec<String> {
    let default = owned(default);
    let preferred = saved
        .unwrap_or_default()
        .iter()
        .filter(|key| default.contains(key))
        .cloned()
        .collect();
    interleave(&default, clean(preferred, usize::MAX))
}

/// `preferred`, plus the keys of `base` missing from it, each inserted right
/// after the key preceding it in `base` (or first).
fn interleave(base: &[String], mut preferred: Vec<String>) -> Vec<String> {
    for (ix, key) in base.iter().enumerate() {
        if preferred.contains(key) {
            continue;
        }
        let at = match ix.checked_sub(1) {
            // Every earlier `base` key is in `preferred` by now.
            Some(prev) => preferred
                .iter()
                .position(|k| k == &base[prev])
                .map_or(preferred.len(), |pos| pos + 1),
            None => 0,
        };
        preferred.insert(at, key.clone());
    }
    preferred
}

fn owned(keys: &[impl AsRef<str>]) -> Vec<String> {
    keys.iter().map(|key| key.as_ref().to_owned()).collect()
}

/// Namespaces shown for one context and the recently picked ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceMemory {
    pub selection: NamespaceChoice,
    /// Most recent first.
    #[serde(default)]
    pub recent: Vec<String>,
}

/// All namespaces or a list of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamespaceChoice {
    All,
    Only(Vec<String>),
}

impl NamespaceMemory {
    /// Trims names, drops empty ones and duplicates, caps the recent list.
    /// `None` when no namespace is selected (an empty `Only`).
    fn normalized(self) -> Option<Self> {
        let selection = match self.selection {
            NamespaceChoice::All => NamespaceChoice::All,
            NamespaceChoice::Only(names) => match clean(names, usize::MAX) {
                names if names.is_empty() => return None,
                names => NamespaceChoice::Only(names),
            },
        };
        Some(Self {
            selection,
            recent: clean(self.recent, RECENT_MAX),
        })
    }
}

fn clean(names: Vec<String>, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out.truncate(max);
    out
}

/// Namespaces a context opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartNamespaces {
    pub selection: NamespaceChoice,
    pub recent: Vec<String>,
    /// Shown when the selection becomes empty: the configured namespace,
    /// else the kubeconfig one, else `default`.
    pub default: String,
}

/// Picks the starting namespaces of a context: the remembered selection,
/// else `clusters.default_namespaces` (`configured`), else the kubeconfig
/// namespace, else all namespaces. Remembered namespaces that no longer exist
/// are dropped later, once the cluster's namespaces are listed.
pub fn start_namespaces(
    remembered: Option<&NamespaceMemory>,
    configured: Option<&str>,
    kubeconfig: Option<&str>,
) -> StartNamespaces {
    fn nonempty(ns: Option<&str>) -> Option<&str> {
        ns.map(str::trim).filter(|ns| !ns.is_empty())
    }
    let configured = nonempty(configured);
    let kubeconfig = nonempty(kubeconfig);
    let fallback = configured.or(kubeconfig);
    let remembered = remembered.cloned().and_then(NamespaceMemory::normalized);
    let selection = match (&remembered, fallback) {
        (Some(memory), _) => memory.selection.clone(),
        (None, Some(ns)) => NamespaceChoice::Only(vec![ns.to_owned()]),
        (None, None) => NamespaceChoice::All,
    };
    StartNamespaces {
        selection,
        recent: remembered.map(|memory| memory.recent).unwrap_or_default(),
        default: fallback.unwrap_or("default").to_owned(),
    }
}

/// Key of a forward in [`AppState::forward_ports`]: `namespace/svc/web:80`
/// (the source as [`ForwardSource::label`](crate::streams::ForwardSource::label),
/// the port asked for on it).
pub fn forward_key(namespace: &str, source: &str, port: u16) -> String {
    format!("{namespace}/{source}:{port}")
}

/// Loads the state from `path`, never failing: anything unreadable gives the
/// defaults, a broken entry drops only itself.
pub fn load(path: &Path) -> AppState {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => AppState::default(),
    }
}

fn parse(text: &str) -> AppState {
    let mut state = AppState::default();
    let Ok(Value::Object(mut root)) = serde_json::from_str::<Value>(text) else {
        return state;
    };
    if let Some(Value::Object(namespaces)) = root.remove("namespaces") {
        for (context, entry) in namespaces {
            if let Ok(memory) = serde_json::from_value::<NamespaceMemory>(entry) {
                state.remember_namespaces(&context, memory);
            }
        }
    }
    if let Some(Value::Object(orders)) = root.remove("column_orders") {
        for (kind, entry) in orders {
            if let Ok(order) = serde_json::from_value::<Vec<String>>(entry) {
                let order = clean(order, usize::MAX);
                if !order.is_empty() {
                    state.column_orders.insert(kind, order);
                }
            }
        }
    }
    if let Some(Value::Object(found)) = root.remove("prometheus") {
        for (context, entry) in found {
            if let Ok(discovery) = serde_json::from_value::<PrometheusDiscovery>(entry) {
                state.remember_prometheus(&context, discovery);
            }
        }
    }
    if let Some(Value::Bool(shown)) = root.remove("debug_overlay") {
        state.debug_overlay = shown;
    }
    if let Some(Value::Object(widths)) = root.remove("panel_widths") {
        for (key, entry) in widths {
            let width = entry.as_u64().and_then(|width| u32::try_from(width).ok());
            state.remember_panel_width(&key, width);
        }
    }
    if let Some(Value::Object(contexts)) = root.remove("forward_ports") {
        for (context, entry) in contexts {
            let Value::Object(ports) = entry else {
                continue;
            };
            for (key, port) in ports {
                let port = port.as_u64().and_then(|port| u16::try_from(port).ok());
                if port.is_some() {
                    state.remember_forward_port(&context, &key, port);
                }
            }
        }
    }
    if let Some(Value::Object(sorts)) = root.remove("table_sorts") {
        for (key, entry) in sorts {
            if let Ok(sort) = serde_json::from_value::<TableSort>(entry) {
                state.remember_table_sort(&key, Some(sort));
            }
        }
    }
    if let Some(Value::Array(forwards)) = root.remove("forwards") {
        for entry in forwards {
            let Ok(forward) = serde_json::from_value::<SavedForward>(entry) else {
                continue;
            };
            let Some(forward) = forward.normalized() else {
                continue;
            };
            // The first of duplicates wins.
            if !state.forwards.iter().any(|f| f.same_target(&forward)) {
                state.forwards.push(forward);
            }
        }
    }
    state
}

/// Writes `state` atomically, creating the directory if needed.
pub fn save(path: &Path, state: &AppState) -> io::Result<()> {
    let mut state = state.clone();
    state.version = STATE_VERSION;
    let mut text = serde_json::to_string_pretty(&state).map_err(io::Error::other)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EKS: &str = "arn:aws:eks:eu-central-1:123456789012:cluster/prod.eu";

    fn only(names: &[&str]) -> NamespaceChoice {
        NamespaceChoice::Only(names.iter().map(|&n| n.to_owned()).collect())
    }

    fn memory(selection: NamespaceChoice, recent: &[&str]) -> NamespaceMemory {
        NamespaceMemory {
            selection,
            recent: recent.iter().map(|&n| n.to_owned()).collect(),
        }
    }

    #[test]
    fn remembers_the_debug_overlay() {
        let mut state = AppState::default();
        // Not written while off.
        assert!(
            !serde_json::to_string(&state)
                .unwrap()
                .contains("debug_overlay")
        );
        state.debug_overlay = true;
        let text = serde_json::to_string(&state).unwrap();
        assert!(parse(&text).debug_overlay);
        // Anything but a boolean is the default.
        assert!(!parse(r#"{"debug_overlay": "yes"}"#).debug_overlay);
    }

    #[test]
    fn round_trips_context_names_with_dots_colons_and_slashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kubach").join(STATE_FILE);
        let mut state = AppState::default();
        state.remember_namespaces(EKS, memory(only(&["web", "api"]), &["api", "web"]));
        state.remember_namespaces(
            "gke_proj_europe-west1_main",
            memory(NamespaceChoice::All, &[]),
        );
        state.remember_namespaces("admin@kind.local:6443", memory(only(&["kube-system"]), &[]));
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        // No temp file is left behind.
        let files = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn file_format() {
        let mut state = AppState::default();
        state.remember_namespaces(EKS, memory(only(&["web"]), &["web"]));
        state.remember_namespaces("dev", memory(NamespaceChoice::All, &[]));
        let value: Value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "namespaces": {
                    "dev": { "selection": "all", "recent": [] },
                    EKS: { "selection": { "only": ["web"] }, "recent": ["web"] },
                },
            })
        );
    }

    #[test]
    fn missing_or_broken_files_give_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        assert_eq!(load(&path), AppState::default());
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), AppState::default());
        fs::write(&path, "[1, 2]").unwrap();
        assert_eq!(load(&path), AppState::default());
    }

    #[test]
    fn broken_entries_drop_only_themselves() {
        let text = r#"{
            "version": 7,
            "future": true,
            "namespaces": {
                "good": { "selection": { "only": [" web ", "web", ""] }, "recent": ["a", "a"] },
                "empty": { "selection": { "only": [] } },
                "bad": { "selection": 42 },
                "no-recent": { "selection": "all" }
            }
        }"#;
        let state = parse(text);
        assert_eq!(state.namespaces.len(), 2);
        assert_eq!(
            state.namespaces("good"),
            Some(&memory(only(&["web"]), &["a"]))
        );
        assert_eq!(
            state.namespaces("no-recent"),
            Some(&memory(NamespaceChoice::All, &[]))
        );
        assert_eq!(state.version, STATE_VERSION);
    }

    #[test]
    fn remembering_reports_changes_and_caps_recent() {
        let mut state = AppState::default();
        let picked = memory(only(&["web"]), &["a", "b", "c", "d", "e", "f"]);
        assert!(state.remember_namespaces(EKS, picked.clone()));
        assert!(!state.remember_namespaces(EKS, picked));
        assert_eq!(state.namespaces(EKS).unwrap().recent.len(), RECENT_MAX);
        // Nothing selected forgets the context.
        assert!(state.remember_namespaces(EKS, memory(only(&[]), &[])));
        assert_eq!(state.namespaces(EKS), None);
        assert!(!state.remember_namespaces(EKS, memory(only(&[]), &[])));
    }

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|&k| k.to_owned()).collect()
    }

    const POD: [&str; 6] = ["name", "namespace", "containers", "status", "age", "node"];

    #[test]
    fn column_order_without_saved_one_is_the_default() {
        assert_eq!(column_order(&POD, None), keys(&POD));
        assert_eq!(column_order(&POD, Some(&[])), keys(&POD));
    }

    #[test]
    fn column_order_follows_the_saved_one() {
        let saved = keys(&["age", "name", "namespace", "node", "containers", "status"]);
        assert_eq!(column_order(&POD, Some(&saved)), saved);
    }

    #[test]
    fn new_columns_take_their_default_place_and_gone_ones_are_skipped() {
        // Saved before the plugin added Containers and Node; IP is gone.
        let saved = keys(&["status", "name", "ip", "namespace", "age"]);
        assert_eq!(
            column_order(&POD, Some(&saved)),
            keys(&["status", "name", "namespace", "containers", "age", "node"])
        );
        // A new first column stays first.
        let saved = keys(&["status", "namespace", "containers", "age", "node"]);
        assert_eq!(
            column_order(&POD, Some(&saved)),
            keys(&["name", "status", "namespace", "containers", "age", "node"])
        );
    }

    #[test]
    fn duplicate_saved_keys_count_once() {
        let saved = keys(&["age", "age", "name"]);
        assert_eq!(
            column_order(&["name", "age"], Some(&saved)),
            keys(&["age", "name"])
        );
    }

    #[test]
    fn remembering_column_order_keeps_hidden_columns_and_forgets_the_default() {
        let mut state = AppState::default();
        // Single namespace: Namespace is hidden.
        let shown = ["age", "name", "containers", "status", "node"];
        assert!(state.remember_column_order("Pod", &shown, &POD));
        assert!(!state.remember_column_order("Pod", &shown, &POD));
        assert_eq!(state.column_order("Pod"), Some(keys(&shown).as_slice()));
        // All namespaces: Namespace goes after Name by default.
        assert_eq!(
            column_order(&POD, state.column_order("Pod")),
            keys(&["age", "name", "namespace", "containers", "status", "node"])
        );
        // Moving Namespace first while it is shown...
        let shown = ["namespace", "age", "name", "containers", "status", "node"];
        assert!(state.remember_column_order("Pod", &shown, &POD));
        // ...then moving Age in a single namespace keeps Namespace first.
        let hidden = ["name", "containers", "age", "status", "node"];
        assert!(state.remember_column_order("Pod", &hidden, &POD));
        assert_eq!(
            column_order(&POD, state.column_order("Pod")),
            keys(&["namespace", "name", "containers", "age", "status", "node"])
        );
        // Back to the default order: nothing to remember.
        assert!(state.remember_column_order("Pod", &POD, &POD));
        assert_eq!(state.column_order("Pod"), None);
        assert!(!state.forget_column_order("Pod"));
    }

    #[test]
    fn column_orders_round_trip_and_broken_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        let kind = kind_key("apps", "Deployment");
        state.remember_column_order(&kind, &["age", "name"], &["name", "age"]);
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        assert_eq!(
            state.column_order("Deployment.apps"),
            Some(keys(&["age", "name"]).as_slice())
        );

        let text = r#"{
            "column_orders": {
                "Pod": ["age", " name ", "age", ""],
                "Node": 42,
                "Service": []
            }
        }"#;
        let state = parse(text);
        assert_eq!(state.column_orders.len(), 1);
        assert_eq!(
            state.column_order("Pod"),
            Some(keys(&["age", "name"]).as_slice())
        );
    }

    #[test]
    fn kind_keys() {
        assert_eq!(kind_key("", "Pod"), "Pod");
        assert_eq!(kind_key("apps", "Deployment"), "Deployment.apps");
    }

    #[test]
    fn remembered_selection_comes_first() {
        let remembered = memory(only(&["api", "web"]), &["web"]);
        let start = start_namespaces(Some(&remembered), Some("payments"), Some("kube"));
        assert_eq!(start.selection, only(&["api", "web"]));
        assert_eq!(start.recent, vec!["web".to_owned()]);
        assert_eq!(start.default, "payments");

        let all = memory(NamespaceChoice::All, &[]);
        let start = start_namespaces(Some(&all), Some("payments"), Some("kube"));
        assert_eq!(start.selection, NamespaceChoice::All);
    }

    #[test]
    fn then_the_configured_then_the_kubeconfig_namespace_then_all() {
        let start = start_namespaces(None, Some("payments"), Some("kube"));
        assert_eq!(start.selection, only(&["payments"]));
        assert!(start.recent.is_empty());

        let start = start_namespaces(None, Some("  "), Some("kube"));
        assert_eq!(start.selection, only(&["kube"]));
        assert_eq!(start.default, "kube");

        let empty = memory(only(&[]), &["web"]);
        let start = start_namespaces(Some(&empty), None, None);
        assert_eq!(start.selection, NamespaceChoice::All);
        assert_eq!(start.default, "default");
        assert!(start.recent.is_empty());
    }

    #[test]
    fn prometheus_searches_round_trip_per_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        let found = PrometheusDiscovery {
            found: vec![
                "monitoring/prometheus-operated:web".into(),
                " monitoring/prometheus-operated:web ".into(),
                "monitoring/kube-prometheus-stack-prometheus:http-web".into(),
            ],
            at: 1_790_000_000,
        };
        assert!(state.remember_prometheus(EKS, found.clone()));
        assert!(!state.remember_prometheus(EKS, found));
        assert_eq!(state.prometheus(EKS).unwrap().found.len(), 2);
        let none = PrometheusDiscovery {
            found: Vec::new(),
            at: 1_790_000_100,
        };
        assert!(state.remember_prometheus("kind", none.clone()));
        assert_eq!(state.prometheus("kind"), Some(&none));
        assert_eq!(state.prometheus("never"), None);

        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
    }

    #[test]
    fn panel_widths_round_trip_and_broken_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        assert!(state.remember_panel_width("browser.details", Some(512)));
        assert!(!state.remember_panel_width("browser.details", Some(512)));
        assert_eq!(state.panel_width("browser.details"), Some(512));
        assert!(state.remember_panel_width("gitops.details", Some(300)));
        assert!(state.remember_panel_width("gitops.details", None));
        assert!(!state.remember_panel_width("gitops.details", None));
        assert!(!state.remember_panel_width("access.details", Some(0)));
        assert_eq!(state.panel_width("gitops.details"), None);
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);

        let state = parse(r#"{"panel_widths": {"a": 400, "b": "wide", "c": -5, "d": 333.3}}"#);
        assert_eq!(state.panel_width("a"), Some(400));
        assert_eq!(state.panel_width("b"), None);
        assert_eq!(state.panel_width("c"), None);
        assert_eq!(state.panel_width("d"), None);
    }

    #[test]
    fn forward_ports_round_trip_and_broken_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        let key = forward_key("monitoring", "svc/grafana", 80);
        assert_eq!(key, "monitoring/svc/grafana:80");
        assert!(state.remember_forward_port(EKS, &key, Some(3000)));
        assert!(!state.remember_forward_port(EKS, &key, Some(3000)));
        assert_eq!(state.forward_port(EKS, &key), Some(3000));
        assert_eq!(state.forward_port("minikube", &key), None);
        assert!(state.remember_forward_port("minikube", &key, Some(4000)));
        assert!(state.remember_forward_port("minikube", &key, None));
        assert!(!state.remember_forward_port("minikube", &key, None));
        assert!(!state.remember_forward_port("minikube", &key, Some(0)));
        assert!(!state.forward_ports.contains_key("minikube"));
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);

        let state =
            parse(r#"{"forward_ports": {"a": {"x": 8080, "y": 70000, "z": "web"}, "b": 5}}"#);
        assert_eq!(state.forward_port("a", "x"), Some(8080));
        assert_eq!(state.forward_port("a", "y"), None);
        assert_eq!(state.forward_port("a", "z"), None);
        assert!(!state.forward_ports.contains_key("b"));
    }

    fn saved(context: &str, name: &str, port: u16, local_port: u16) -> SavedForward {
        SavedForward {
            context: context.into(),
            namespace: "monitoring".into(),
            kind: "Service".into(),
            name: name.into(),
            port,
            local_port,
            port_name: Some("http".into()),
            paused: false,
        }
    }

    #[test]
    fn paused_forwards_round_trip_and_old_files_are_not_paused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        let grafana = saved(EKS, "grafana", 80, 3000);
        state.remember_forward(grafana.clone());
        assert!(!serde_json::to_string(&state).unwrap().contains("paused"));
        // Pausing replaces the saved forward of the same target.
        let paused = SavedForward {
            paused: true,
            ..grafana.clone()
        };
        assert!(state.remember_forward(paused.clone()));
        assert_eq!(state.saved_forwards(), std::slice::from_ref(&paused));
        save(&path, &state).unwrap();
        assert_eq!(load(&path).saved_forwards(), [paused]);
        // Resuming clears the flag again.
        assert!(state.remember_forward(grafana.clone()));
        assert_eq!(state.saved_forwards(), [grafana]);
        // A file without the field: not paused.
        let old = parse(
            r#"{ "version": 1, "forwards": [ { "context": "a", "namespace": "ns",
                 "kind": "Pod", "name": "web", "port": 8080, "local_port": 18080 } ] }"#,
        );
        assert!(!old.saved_forwards()[0].paused);
    }

    #[test]
    fn saved_forwards_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = AppState::default();
        assert!(!serde_json::to_string(&state).unwrap().contains("forwards"));
        let grafana = saved(EKS, "grafana", 80, 3000);
        assert!(state.remember_forward(grafana.clone()));
        assert!(!state.remember_forward(grafana.clone()));
        assert!(state.remember_forward(saved("minikube", "prometheus", 9090, 9090)));
        save(&path, &state).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded, state);
        assert_eq!(loaded.saved_forwards()[0], grafana);
        assert_eq!(loaded.saved_forwards().len(), 2);
    }

    #[test]
    fn a_restarted_forward_replaces_the_saved_one() {
        let mut state = AppState::default();
        assert!(state.remember_forward(saved(EKS, "grafana", 80, 3000)));
        // Same target, another local port: one entry, the new port.
        assert!(state.remember_forward(saved(EKS, "grafana", 80, 3001)));
        assert_eq!(state.saved_forwards(), [saved(EKS, "grafana", 80, 3001)]);
        // Another port of the same source is another forward.
        assert!(state.remember_forward(saved(EKS, "grafana", 443, 3443)));
        assert_eq!(state.saved_forwards().len(), 2);
        // Nothing to restart without a local port.
        assert!(!state.remember_forward(saved(EKS, "loki", 3100, 0)));
    }

    #[test]
    fn stopped_forwards_are_forgotten() {
        let mut state = AppState::default();
        state.remember_forward(saved(EKS, "grafana", 80, 3000));
        state.remember_forward(saved("minikube", "grafana", 80, 3000));
        // Stopping matches the target whatever the local port.
        assert!(state.forget_forward(&saved(EKS, "grafana", 80, 0)));
        assert!(!state.forget_forward(&saved(EKS, "grafana", 80, 3000)));
        assert_eq!(
            state.saved_forwards(),
            [saved("minikube", "grafana", 80, 3000)]
        );
        assert!(state.forget_forward(&saved("minikube", "grafana", 80, 3000)));
        let text = serde_json::to_string(&state).unwrap();
        assert!(parse(&text).saved_forwards().is_empty());
    }

    #[test]
    fn broken_saved_forwards_drop_only_themselves() {
        let state = parse(
            r#"{
                "version": 1,
                "forwards": [
                    { "context": "a", "namespace": "ns", "kind": "Pod", "name": "web",
                      "port": 8080, "local_port": 18080 },
                    { "context": "a", "namespace": "ns", "kind": "Pod", "name": "web",
                      "port": 8080, "local_port": 28080 },
                    { "context": "a", "namespace": "ns", "kind": "Pod", "name": "web",
                      "port": 70000, "local_port": 1 },
                    { "context": "", "namespace": "ns", "kind": "Pod", "name": "x",
                      "port": 1, "local_port": 1 },
                    { "context": "a", "namespace": "ns", "kind": "Pod", "name": "y",
                      "port": 80, "local_port": 0 },
                    { "context": "a", "namespace": "ns", "kind": "Pod" },
                    42,
                    { "context": " b ", "namespace": "ns", "kind": "Service", "name": "db",
                      "port": 5432, "local_port": 5432, "port_name": " ", "future": 1 }
                ]
            }"#,
        );
        let forwards = state.saved_forwards();
        assert_eq!(forwards.len(), 2);
        assert_eq!(forwards[0].local_port, 18080);
        assert_eq!(forwards[1].context, "b");
        assert_eq!(forwards[1].port_name, None);
        // Not a list: nothing restored.
        assert!(
            parse(r#"{"forwards": {"a": 1}}"#)
                .saved_forwards()
                .is_empty()
        );
    }

    #[test]
    fn broken_prometheus_entries_drop_only_themselves() {
        let state = parse(
            r#"{
                "version": 1,
                "prometheus": {
                    "a": { "found": ["m/p:web"], "at": 5 },
                    "b": { "found": "m/p:web" },
                    "c": {}
                }
            }"#,
        );
        assert_eq!(state.prometheus("a").unwrap().found, ["m/p:web"]);
        assert_eq!(state.prometheus("b"), None);
        // Nothing found, time unknown: still a search that happened.
        assert_eq!(state.prometheus("c"), Some(&PrometheusDiscovery::default()));
    }

    #[test]
    fn table_sorts_are_remembered_per_table() {
        let mut state = AppState::default();
        let pods = kind_key("", "Pod");
        let deployments = kind_key("apps", "Deployment");
        let gitops = screen_table_key("gitops");
        assert!(
            state.remember_table_sort(&pods, Some(TableSort::new("age", SortOrder::Descending)))
        );
        assert!(state.remember_table_sort(
            &deployments,
            Some(TableSort::new("name", SortOrder::Ascending))
        ));
        assert!(state.remember_table_sort(
            &gitops,
            Some(TableSort::new("status", SortOrder::Ascending))
        ));
        // The same sort again changes nothing.
        assert!(
            !state.remember_table_sort(&pods, Some(TableSort::new("age", SortOrder::Descending)))
        );
        // Changing one table leaves the others alone.
        assert!(
            state.remember_table_sort(&pods, Some(TableSort::new("age", SortOrder::Ascending)))
        );
        assert_eq!(
            state.table_sort(&pods),
            Some(&TableSort::new("age", SortOrder::Ascending))
        );
        assert_eq!(
            state.table_sort(&deployments),
            Some(&TableSort::new("name", SortOrder::Ascending))
        );
        assert_eq!(
            state.table_sort(&gitops),
            Some(&TableSort::new("status", SortOrder::Ascending))
        );
        assert_eq!(state.table_sort("Service"), None);
        // Back to the default order forgets the table.
        assert!(state.remember_table_sort(&pods, None));
        assert!(!state.remember_table_sort(&pods, None));
        assert_eq!(state.table_sort(&pods), None);
        assert!(state.table_sort(&deployments).is_some());
        // A sort without a column is no sort.
        assert!(
            state.remember_table_sort(&gitops, Some(TableSort::new("  ", SortOrder::Ascending)))
        );
        assert_eq!(state.table_sort(&gitops), None);
    }

    #[test]
    fn table_sorts_round_trip_and_broken_ones_are_dropped() {
        let mut state = AppState::default();
        assert!(
            !serde_json::to_string(&state)
                .unwrap()
                .contains("table_sorts")
        );
        state.remember_table_sort(
            "Pod",
            Some(TableSort::new("restarts", SortOrder::Descending)),
        );
        state.remember_table_sort(
            &screen_table_key("helm.releases"),
            Some(TableSort::new("name", SortOrder::Ascending)),
        );
        let text = serde_json::to_string(&state).unwrap();
        assert!(
            text.contains(r#""Pod":{"column":"restarts","order":"desc"}"#),
            "{text}"
        );
        assert!(
            text.contains(r#""screen:helm.releases":{"column":"name","order":"asc"}"#),
            "{text}"
        );
        assert_eq!(parse(&text), state);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(STATE_FILE);
        save(&file, &state).unwrap();
        assert_eq!(load(&file), state);

        let text = r#"{
            "version": 1,
            "table_sorts": {
                "Pod": {"column": "age", "order": "desc"},
                "Service": {"column": "name", "order": "sideways"},
                "Secret": {"order": "asc"},
                "ConfigMap": {"column": "", "order": "asc"},
                "Node": "name",
                "Job.batch": {"column": " status ", "order": "asc", "extra": 1}
            }
        }"#;
        let state = parse(text);
        assert_eq!(state.table_sorts.len(), 2);
        assert_eq!(
            state.table_sort("Pod"),
            Some(&TableSort::new("age", SortOrder::Descending))
        );
        assert_eq!(
            state.table_sort("Job.batch"),
            Some(&TableSort::new("status", SortOrder::Ascending))
        );
        // Files from before #33 have no sorts.
        assert!(
            parse(r#"{"version": 1, "column_orders": {}}"#)
                .table_sorts
                .is_empty()
        );
    }
}
