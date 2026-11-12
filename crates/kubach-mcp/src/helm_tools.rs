//! Helm tools for the agent (#418, ADR 0005 "Helm через агента"), run with
//! the same `helm` binary as Kubach's Helm screens (ADR 0006).
//!
//! Reading: charts of the repositories added in Kubach (and of an `oci://`
//! reference the user allowed), a chart's default values, the releases of a
//! namespace and a release's history. Changing: install, upgrade, rollback
//! and uninstall, each `parse → policy (access, read(ns), modify(ns)) →
//! chart source → the chart fetched once → server-side dry run → the user
//! approves in Kubach → helm runs with live phases → the release compared
//! with the dry run → result`.
//!
//! Boundaries:
//! - the namespace comes from the session: `--namespace` is forced,
//!   `--create-namespace` only for the release namespace and only where
//!   the session may change it. An object of the chart in another
//!   namespace (#21) needs the session's rights there (`read(ns)`,
//!   `modify(ns)`, set up as for any tool) and the user's explicit tick,
//!   and under a temporary ServiceAccount the user's credentials as below;
//! - helm acts under the namespace's identity: a temporary ServiceAccount's
//!   short-lived token through `HELM_KUBETOKEN` (its Role widened for helm
//!   only while the call runs), the administrator's identity through
//!   `HELM_KUBEASUSER`, or the user's kubeconfig in a client-only
//!   namespace — never the user's credentials where the session has
//!   another identity;
//! - cluster-scoped objects of a chart (CRDs, ClusterRoles, webhooks, PVs,
//!   StorageClasses…) and RBAC inside the namespace need the user's
//!   explicit tick in the dialog. A temporary ServiceAccount cannot create
//!   them (nor anything in another namespace, #21), so under one Kubach
//!   finds them first without the cluster —
//!   `helm template` for install and upgrade (plus the current revision of
//!   an upgrade), the revision's manifest for rollback and uninstall — and
//!   asks the user to run this one call with their own kubeconfig
//!   credentials (#18): an unticked box, then, for install and upgrade, the
//!   dry run under those credentials and the usual window with its diff.
//!   Without the tick nothing runs and the agent is told which objects.
//!   A preview of such a chart asks nobody, so it shows `helm template`;
//!   when the chart uses `lookup`, that render goes through the namespace's
//!   ServiceAccount, read-only, or the objects it may misstate are marked
//!   (#114);
//! - a preview answers with a summary and keeps the masked diff in the
//!   session for `helm_preview_diff` (#115, `helm_preview`); the
//!   confirmation window keeps the whole diff;
//! - what the agent gets before the user approved anything comes from a
//!   render with no more than the session's own read rights (#173): a
//!   preview is `helm template`, without the cluster or, under a temporary
//!   ServiceAccount, read-only with its rights. The server-side dry run,
//!   which reads the release storage, is for the confirmation window; when
//!   it fails the agent gets a fixed message, never helm's text. In a
//!   client-only namespace the dry run itself waits for the user's yes;
//! - a chart reference is fetched once per call, into a private directory
//!   (#174): the render, the dry run and the change all read that archive,
//!   helm runs in an empty private directory, and the release helm wrote is
//!   compared with the dry run the user approved;
//! - charts only from repositories already added in Kubach, or an `oci://`
//!   repository the user allows for the session; never a path or URL;
//! - values only as YAML the agent passes (never a file path), masked by
//!   key name in the dialog, the audit log and answers;
//! - a release's current values never reach the agent whole (#22):
//!   `helm_get_values` returns them masked by key name and by what looks
//!   like a key or token, and `helm_upgrade` with `valuesPatch` merges the
//!   patch in Kubach and answers with the masked diff of the values only.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::rbac::v1::PolicyRule;
use kubach_core::discovery::ApiCatalog;
use kubach_core::helm::cli::{
    self as helm_cli, HelmCliError, HelmCommand, HelmTarget, HelmWatch, InstallRequest,
    PulledChart, SecretEnv, ShowKind, TemplateRequest, UpgradeChart, UpgradeOutcome,
    UpgradeRequest,
};
use kubach_core::helm::lookup::{lookup_objects, lookup_use};
use kubach_core::helm::repo::{self, HelmPaths, IndexState, SearchOptions};
use kubach_core::helm::{
    Driver, HelmError, ManifestResource, Release, Releases, Sensitive, manifest_resources, oci,
    unified_diff,
};
use kubach_core::kubeconfig::temp::TempKubeconfig;
use kubach_core::secrets::mask::{ends_with_sensitive_word, is_sensitive_name};
use kube::config::Kubeconfig;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::confirm::{ConfirmationDecision, ConfirmationRequest, RequestKind, ask};
use crate::helm_preview::{PreviewInput, changed_values, compare};
use crate::masking::{mask_text, masked_manifest, truncate};
use crate::policy::{Allowed, Denial, DenialCode, Finding, Request};
use crate::scope::SessionMode;
use crate::session::HelmAccess;
use crate::tools::{Ctx, ToolError, ToolOutput, authorize, parse};
use kubach_core::sync;

/// Largest values YAML the agent may pass.
pub const MAX_VALUES_BYTES: usize = 256 * 1024;
/// Most chart rows a search returns.
const MAX_SEARCH_ROWS: usize = 50;
/// NOTES returned to the agent, at most.
const MAX_NOTES_BYTES: usize = 8 * 1024;
/// A values diff in an answer to the agent and in the audit log, at most.
const MAX_AGENT_DIFF_BYTES: usize = 64 * 1024;
/// The manifest diff of a confirmation window, at most (#137). A release
/// is kept in a Secret of 1 MiB compressed, so its manifest is a few MiB
/// at the very most and so is its diff: the window shows it whole. What is
/// larger is cut at a line and the window says how much it does not show
/// ([`DiffCut`]). The request also has to fit the bridge
/// ([`crate::bridge::MAX_CONFIRM_BODY_BYTES`]).
pub const MAX_CONFIRM_DIFF_BYTES: usize = 8 * 1024 * 1024;
/// Objects a [`DiffCut`] names; the rest are counted.
const MAX_CUT_OBJECTS: usize = 50;
/// `helm show values` and other calls without a cluster.
const SHOW_TIMEOUT: Duration = Duration::from_secs(120);

/// Kinds that are cluster-scoped even when the cluster does not serve them
/// yet (a chart's own CRDs), refused by default.
const CLUSTER_KINDS: &[&str] = &[
    "CustomResourceDefinition",
    "ClusterRole",
    "ClusterRoleBinding",
    "MutatingWebhookConfiguration",
    "ValidatingWebhookConfiguration",
    "ValidatingAdmissionPolicy",
    "ValidatingAdmissionPolicyBinding",
    "PersistentVolume",
    "StorageClass",
    "Namespace",
    "PriorityClass",
    "APIService",
    "RuntimeClass",
    "IngressClass",
    "CSIDriver",
];

/// Who runs a call the user allowed under their own credentials (#18), for
/// the dialog and the audit log.
pub const USER_IDENTITY: &str =
    "user: your kubeconfig credentials, allowed for this helm call only";

/// Why such a call runs as the user, for the audit log.
const USER_REASON: &str = "the chart has cluster-scoped objects, RBAC or objects in other \
                           namespaces that the namespace's temporary ServiceAccount cannot create; \
                           the user allowed this one call under their own credentials";

/// What the user reads before allowing a call on a vault context (#113).
const VAULT_NOTE: &str = "This context is in the vault: while helm runs, its credentials are in \
                          a temporary file only you can read, removed as soon as helm exits.";

/// What the session knows about the user's `helm`: the binary Kubach uses
/// (settings, `PATH` or downloaded), the kubeconfig files of the context
/// for client-only namespaces (`None`: the context is not in a file, such
/// as a vault context), a vault context's credentials for a call the user
/// allowed under them (#113), and where the repositories are.
#[derive(Clone, Debug, Default)]
pub struct HelmEnv {
    pub binary: Option<PathBuf>,
    pub kubeconfig: Option<Vec<PathBuf>>,
    pub vault: Option<VaultKubeconfig>,
    pub paths: Option<HelmPaths>,
}

/// A vault context for `helm`, which reads credentials only from a file
/// (#113). It stays in memory, as the session's own client does, and only
/// for a call the user allowed under their credentials goes to a
/// [`TempKubeconfig`] for the time helm runs (ADR 0005, 0015). Only the app
/// has one: a `kubach mcp` process never sees the vault's credentials.
#[derive(Clone)]
pub struct VaultKubeconfig {
    /// One context, its cluster and user.
    config: Arc<Kubeconfig>,
    /// Where temporary kubeconfigs live, so that a file left by a crash is
    /// removed on the next start.
    dir: PathBuf,
}

impl VaultKubeconfig {
    /// `config` as `Contexts::single_context` gives it, `dir` as
    /// `kubeconfig::temp::dir`.
    pub fn new(config: Kubeconfig, dir: PathBuf) -> Self {
        Self {
            config: Arc::new(config),
            dir,
        }
    }

    /// The owner-only file for one helm process, removed when dropped.
    fn write(&self) -> std::io::Result<TempKubeconfig> {
        TempKubeconfig::create(&self.dir, &self.config)
    }
}

impl std::fmt::Debug for VaultKubeconfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultKubeconfig").finish_non_exhaustive()
    }
}

/// What a Helm change does, for the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HelmAction {
    Install,
    Upgrade,
    Rollback,
    Uninstall,
}

impl HelmAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Upgrade => "upgrade",
            Self::Rollback => "roll back",
            Self::Uninstall => "uninstall",
        }
    }
}

/// Whose credentials a Helm change runs with (#18).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HelmCredentials {
    /// The namespace's identity ([`HelmChange::identity`]).
    #[default]
    Session,
    /// The question whether this one call may run with the user's own
    /// kubeconfig credentials: the UI lists [`HelmChange::user_objects`],
    /// warns, and keeps the approve button off until the user ticks an
    /// unticked box. For install and upgrade nothing has touched the
    /// cluster yet: a yes leads to the dry run under the user's
    /// credentials and a second window with its diff.
    AskUser,
    /// The user allowed their credentials for this call already: the
    /// window with the dry run's diff, which the UI marks as running as
    /// the user.
    User,
}

/// A Helm change as the user approves it (#418).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelmChange {
    pub action: HelmAction,
    pub release: String,
    /// `repo/chart`, `oci://…` or the chart kept in the release.
    pub chart: String,
    #[serde(default)]
    pub version: Option<String>,
    /// The revision a rollback returns to.
    #[serde(default)]
    pub revision: Option<u32>,
    /// User values as YAML, values of sensitive keys masked.
    #[serde(default)]
    pub values: String,
    /// `Deployment web`… of the dry run (or of the release, to uninstall).
    #[serde(default)]
    pub objects: Vec<String>,
    /// Cluster-scoped objects among them; each needs the explicit tick.
    #[serde(default)]
    pub cluster_objects: Vec<String>,
    #[serde(default)]
    pub create_namespace: bool,
    /// Under which identity helm runs (`serviceaccount:…`, `user`…).
    #[serde(default)]
    pub identity: String,
    /// Whose credentials run it (#18).
    #[serde(default)]
    pub credentials: HelmCredentials,
    /// Why the user's credentials are asked for: the cluster-scoped
    /// objects, RBAC and objects in other namespaces a temporary
    /// ServiceAccount cannot create or delete.
    #[serde(default)]
    pub user_objects: Vec<String>,
    /// Namespaces besides the release's that the call changes (#21), each
    /// allowed to the session (`modify(ns)`) before the user is asked.
    #[serde(default)]
    pub namespaces: Vec<String>,
    /// How the release's values change (#22): a unified diff, values of
    /// sensitive keys and secret-looking strings masked. Empty when they
    /// stay as they are.
    #[serde(default)]
    pub values_diff: String,
    /// What the diff in `details` leaves out, when it is larger than
    /// [`MAX_CONFIRM_DIFF_BYTES`] (#137). The window must say so.
    #[serde(default)]
    pub diff_cut: Option<DiffCut>,
}

/// The part of a manifest diff the confirmation window does not get
/// (#137): the user has to know they approve more than they see.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffCut {
    pub shown_bytes: usize,
    pub total_bytes: usize,
    /// Lines of the diff after the cut.
    pub hidden_lines: usize,
    /// `Kind name` of the objects with changed lines after the cut, the
    /// first [`MAX_CUT_OBJECTS`].
    #[serde(default)]
    pub objects: Vec<String>,
    /// How many more such objects there are.
    #[serde(default)]
    pub more_objects: usize,
}

// ------------------------------------------------------------- arguments

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchArgs {
    #[serde(default)]
    query: String,
    oci: Option<String>,
    #[serde(default)]
    versions: bool,
    #[serde(default)]
    devel: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShowValuesArgs {
    chart: String,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleasesArgs {
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryArgs {
    release: String,
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallArgs {
    release: String,
    chart: String,
    version: Option<String>,
    values: Option<String>,
    namespace: Option<String>,
    #[serde(default)]
    create_namespace: bool,
    /// Only show the agent what the install would do (#68).
    #[serde(default)]
    preview: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpgradeArgs {
    release: String,
    chart: Option<String>,
    version: Option<String>,
    values: Option<String>,
    /// Changes merged into the release's values by Kubach (#22).
    values_patch: Option<String>,
    namespace: Option<String>,
    /// Only show the agent what the upgrade would do (#68).
    #[serde(default)]
    preview: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetValuesArgs {
    release: String,
    revision: Option<u32>,
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollbackArgs {
    release: String,
    revision: u32,
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UninstallArgs {
    release: String,
    namespace: Option<String>,
    #[serde(default)]
    keep_history: bool,
}

// ------------------------------------------------------------- pure checks

/// Where a chart comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChartSource {
    /// A repository added in Kubach (`repo/chart`).
    Repository { name: String },
    /// An `oci://` repository: `source` without tag or digest, which the
    /// user allows once per session.
    Oci { source: String },
}

/// Checks a chart reference: `repo/chart` of a repository in
/// `repositories`, or `oci://registry/path[:tag]`. Paths, URLs and other
/// repositories are refused: only the user adds repositories, in Kubach.
pub(crate) fn chart_source(chart: &str, repositories: &[String]) -> Result<ChartSource, Denial> {
    let chart = chart.trim();
    if chart.starts_with("oci://") {
        let reference = oci::parse_reference(chart).ok_or_else(|| {
            Denial::new(
                DenialCode::InvalidInput,
                format!("{chart:?} is not an oci:// chart reference"),
            )
        })?;
        return Ok(ChartSource::Oci {
            source: format!("oci://{}/{}", reference.registry, reference.repository),
        });
    }
    let refused = || {
        Denial::new(
            DenialCode::InvalidInput,
            "pass a chart as repo/chart of a repository added in Kubach, or as an oci:// \
             reference; paths, URLs and archives are not accepted",
        )
    };
    let (name, chart_name) = chart.split_once('/').ok_or_else(refused)?;
    let word = |text: &str| {
        !text.is_empty()
            && text.len() <= 253
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !text.starts_with('.')
    };
    if !word(name) || !word(chart_name) {
        return Err(refused());
    }
    if !repositories.iter().any(|known| known == name) {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "repository {name:?} is not added in Kubach{}. Only the user adds repositories, \
                 in Kubach (Helm → Charts → Repositories); an oci:// reference is asked for \
                 instead",
                if repositories.is_empty() {
                    String::new()
                } else {
                    format!(" (added: {})", repositories.join(", "))
                }
            ),
        ));
    }
    Ok(ChartSource::Repository {
        name: name.to_owned(),
    })
}

/// Parses values YAML from the agent: one mapping or nothing, within
/// [`MAX_VALUES_BYTES`].
pub(crate) fn parse_values(text: &str) -> Result<Value, Denial> {
    if text.len() > MAX_VALUES_BYTES {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("values exceed {MAX_VALUES_BYTES} bytes"),
        ));
    }
    kubach_core::helm::check_values_yaml(text)
        .map_err(|error| Denial::new(DenialCode::InvalidInput, format!("values: {error}")))?;
    let documents = kubach_core::apply::parse(text)
        .map_err(|error| Denial::new(DenialCode::InvalidInput, format!("values: {error:?}")))?;
    Ok(documents
        .into_iter()
        .next()
        .map_or_else(|| Value::Object(Map::new()), |doc| doc.value))
}

/// Shortest string [`looks_like_secret`] takes for a bare key or token.
pub(crate) const SECRET_MIN_LEN: usize = 32;

/// Whether a value that is not under a sensitive name still looks like a
/// secret (#22): a PEM block, a JWT, or one word of at least
/// [`SECRET_MIN_LEN`] characters that is hex (with digits and letters) or
/// base64 / base64url (with upper case, lower case and digits). Names,
/// hosts, image references and paths have dots, colons or a single case,
/// so they stay readable; a false positive only hides a value.
pub(crate) fn looks_like_secret(text: &str) -> bool {
    let text = text.trim();
    if text.contains("-----BEGIN ") {
        return true;
    }
    let base64url = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    };
    let parts: Vec<&str> = text.split('.').collect();
    if text.len() >= 20
        && text.starts_with("eyJ")
        && parts.len() == 3
        && base64url(parts[0])
        && base64url(parts[1])
        && (parts[2].is_empty() || base64url(parts[2]))
    {
        return true;
    }
    if text.len() < SECRET_MIN_LEN {
        return false;
    }
    let digits = text.chars().any(|c| c.is_ascii_digit());
    if digits
        && text.chars().all(|c| c.is_ascii_hexdigit())
        && text.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    let body = text.trim_end_matches('=');
    body.len() >= SECRET_MIN_LEN
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
        && digits
        && body.chars().any(|c| c.is_ascii_uppercase())
        && body.chars().any(|c| c.is_ascii_lowercase())
}

/// `*** (N bytes)` for a masked value.
fn values_placeholder(value: &Value) -> Value {
    let size = match value {
        Value::String(text) => text.len(),
        Value::Null => 0,
        other => other.to_string().len(),
    };
    Value::String(format!("*** ({size} bytes)"))
}

/// Values masked at any depth: every string under a sensitive key
/// (`password`, `token`, `auth`…, a whole word of the key; in a list under
/// one too) and every string that [`looks_like_secret`] becomes
/// `*** (N bytes)`; other strings go through text masking. Booleans and null
/// are never masked (`automountServiceAccountToken: true`), numbers only
/// under a key whose last word is a credential one (`password: 123456`, not
/// `tokenTTL: 3600`) (#59).
pub(crate) fn mask_values(value: &mut Value) {
    mask_values_at(value, "", Key::Plain, &mut Vec::new());
}

/// [`mask_values`], returning the paths it masked (`auth.password`,
/// `tokens[0]`), for `helm_get_values`.
pub(crate) fn mask_values_listed(value: &mut Value) -> Vec<String> {
    let mut masked = Vec::new();
    mask_values_at(value, "", Key::Plain, &mut masked);
    masked
}

/// What the key a value is stored under says about it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Plain,
    /// A sensitive word, but not as its last word (`tokenTTL`).
    Sensitive,
    /// Ends in a sensitive word (`apiToken`, `password`).
    Credential,
}

impl Key {
    fn of(name: &str) -> Self {
        if !is_sensitive_name(name) {
            Key::Plain
        } else if ends_with_sensitive_word(name) {
            Key::Credential
        } else {
            Key::Sensitive
        }
    }
}

fn mask_values_at(value: &mut Value, path: &str, key: Key, masked: &mut Vec<String>) {
    let sensitive = key != Key::Plain;
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                mask_values_at(child, &child_path, Key::of(key), masked);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                mask_values_at(item, &format!("{path}[{index}]"), key, masked);
            }
        }
        Value::String(text) if !sensitive && !looks_like_secret(text) => {
            if let std::borrow::Cow::Owned(text_masked) = mask_text(text) {
                *text = text_masked;
            }
        }
        Value::String(_) => {
            *value = values_placeholder(value);
            masked.push(path.to_owned());
        }
        // A number is a credential only under a key that ends in a
        // credential word (`password: 123456`), not a size or a duration
        // (`tokenTTL: 3600`) (#59).
        Value::Number(_) if key == Key::Credential => {
            *value = values_placeholder(value);
            masked.push(path.to_owned());
        }
        // Booleans and null are switches, never credentials (#59).
        _ => {}
    }
}

/// Merges `patch` into `values` (#22), as a JSON merge patch: mappings
/// merge key by key at any depth, scalars and lists replace the value
/// whole, and `null` removes the key from the release's values (the
/// chart's default applies again).
pub(crate) fn merge_values(values: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *values = patch.clone();
        return;
    };
    if !values.is_object() {
        *values = Value::Object(Map::new());
    }
    let Value::Object(target) = values else {
        unreachable!("set to a mapping above");
    };
    for (key, change) in patch {
        match change {
            Value::Null => {
                target.remove(key);
            }
            Value::Object(_) => merge_values(
                target
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new())),
                change,
            ),
            other => {
                target.insert(key.clone(), other.clone());
            }
        }
    }
}

/// Marks a masked value of `new` that hides a changed value, which would
/// otherwise look the same on both sides: `*** (N bytes, changed)`. The
/// mark says that the two values differ and nothing else about them.
pub(crate) fn mark_changed(old: &Value, new: &Value, old_masked: &Value, new_masked: &mut Value) {
    match (old, new, old_masked, new_masked) {
        (Value::Object(old), Value::Object(new), Value::Object(old_m), Value::Object(new_m)) => {
            for (key, masked) in new_m.iter_mut() {
                if let (Some(o), Some(n), Some(om)) = (old.get(key), new.get(key), old_m.get(key)) {
                    mark_changed(o, n, om, masked);
                }
            }
        }
        (Value::Array(old), Value::Array(new), Value::Array(old_m), Value::Array(new_m)) => {
            // Lists of the same length go item by item. In one that grew or
            // shrank an item is compared with the one of its `name`
            // (`env`, containers), or else with the one at its place.
            let same_length = old.len() == new.len();
            for (index, masked) in new_m.iter_mut().enumerate() {
                let Some(item) = new.get(index) else {
                    continue;
                };
                let at = match item["name"].as_str() {
                    Some(name) if !same_length => {
                        old.iter().position(|other| other["name"] == name)
                    }
                    _ => Some(index),
                };
                if let Some(at) = at
                    && let (Some(o), Some(om)) = (old.get(at), old_m.get(at))
                {
                    mark_changed(o, item, om, masked);
                }
            }
        }
        (old, new, old_masked, Value::String(text))
            if old != new && Some(text.as_str()) == old_masked.as_str() =>
        {
            *text = if text.starts_with("*** (") {
                format!("{}, changed)", text.trim_end_matches(')'))
            } else {
                format!("{text} [changed, masked]")
            };
        }
        _ => {}
    }
}

/// The diff of a release's values (#22), both sides masked as by
/// [`mask_values`]; a masked value that changed is marked so. Empty when
/// nothing changes. What the dialog shows, whole (#137), and the agent gets
/// bounded ([`MAX_AGENT_DIFF_BYTES`]): never the values themselves.
pub(crate) fn masked_values_diff(
    old: &Value,
    new: &Value,
    old_label: &str,
    new_label: &str,
) -> String {
    let mut old_masked = old.clone();
    mask_values(&mut old_masked);
    let mut new_masked = new.clone();
    mask_values(&mut new_masked);
    mark_changed(old, new, &old_masked, &mut new_masked);
    unified_diff(
        &kubach_core::helm::values_to_yaml(&old_masked),
        &kubach_core::helm::values_to_yaml(&new_masked),
        old_label,
        new_label,
    )
}

/// Values YAML as shown and audited: masked, or an outline when it does
/// not parse.
pub(crate) fn masked_values_yaml(text: &str) -> String {
    match parse_values(text) {
        Ok(mut value) => {
            mask_values(&mut value);
            kubach_core::helm::values_to_yaml(&value)
        }
        Err(_) => format!("<values that do not parse, {} bytes>", text.len()),
    }
}

/// The objects of a dry run, checked against the session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Preview {
    /// `Deployment web`, in manifest order.
    pub objects: Vec<String>,
    /// Cluster-scoped ones among them.
    pub cluster: Vec<String>,
    /// What the user has to tick explicitly.
    pub findings: Vec<Finding>,
    /// `(group, plural)` of the namespaced objects, for the Role of a
    /// temporary ServiceAccount.
    pub kinds: Vec<(String, String)>,
    /// Objects in namespaces other than the release's (#21).
    pub elsewhere: Vec<Elsewhere>,
}

/// An object of a chart in a namespace other than the release's (#21).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Elsewhere {
    pub namespace: String,
    /// `Role name (namespace other)`.
    pub object: String,
}

/// What a temporary ServiceAccount cannot do among a call's manifests
/// (#18), and the objects in other namespaces (#21), each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    pub findings: Vec<Finding>,
    pub elsewhere: Vec<Elsewhere>,
    /// The cluster-scoped objects among them.
    pub cluster: Vec<String>,
}

/// The namespaces of `elsewhere`, sorted, each once.
pub(crate) fn other_namespaces<'a>(
    elsewhere: impl IntoIterator<Item = &'a Elsewhere>,
) -> Vec<String> {
    let mut namespaces: Vec<String> = elsewhere
        .into_iter()
        .map(|object| object.namespace.clone())
        .collect();
    namespaces.sort();
    namespaces.dedup();
    namespaces
}

/// The release namespace first, then the others a call changes: what the
/// audit log records (#21).
pub(crate) fn touched(namespace: &str, others: &[String]) -> Vec<String> {
    std::iter::once(namespace.to_owned())
        .chain(others.iter().filter(|other| *other != namespace).cloned())
        .collect()
}

/// What the agent is told when the session may not change `other` (#21).
pub(crate) fn elsewhere_refusal(
    other: &str,
    namespace: &str,
    objects: &[String],
    why: &str,
) -> String {
    format!(
        "the chart puts {} into namespace {other}, outside the release namespace {namespace}; \
         this call needs the right to change {other} and does not have it: {why}. Nothing ran. \
         Change the chart's values so these go into {namespace} (for example the namespace of \
         leader election), or ask the user to do it from Kubach's Helm screen",
        objects.join(", ")
    )
}

/// Checks the objects a chart renders. Cluster-scoped objects, RBAC
/// inside the namespace and objects in other namespaces (#21) are
/// findings the user ticks; under a temporary ServiceAccount, whose
/// namespaced Role cannot create them anyway, they are refused outright.
/// Such a refusal is about a server-side dry run nobody approved yet, so
/// it names no object: a name there is the chart's own text (#173).
pub(crate) fn check_objects(
    resources: &[ManifestResource],
    namespace: &str,
    catalog: &ApiCatalog,
    mode: SessionMode,
) -> Result<Preview, Denial> {
    let mut preview = Preview::default();
    for resource in resources {
        let group = resource
            .api_version
            .rsplit_once('/')
            .map_or("", |(group, _)| group);
        let served = catalog.find(group, &resource.kind);
        let cluster_scoped = match served {
            Some(kind) => !kind.namespaced,
            None => CLUSTER_KINDS.contains(&resource.kind.as_str()),
        };
        let elsewhere = match resource.namespace.as_deref() {
            Some(other) if !cluster_scoped && other != namespace => Some(other),
            _ => None,
        };
        let rbac = !cluster_scoped
            && matches!(resource.kind.as_str(), "Role" | "RoleBinding")
            && group == "rbac.authorization.k8s.io";
        let shown = if cluster_scoped {
            format!("{} {} (cluster-scoped)", resource.kind, resource.name)
        } else if let Some(other) = elsewhere {
            format!("{} {} (namespace {other})", resource.kind, resource.name)
        } else {
            format!("{} {}", resource.kind, resource.name)
        };
        if let Some(other) = elsewhere {
            if !crate::names::is_dns_label(other) {
                return Err(Denial::new(
                    DenialCode::Namespace,
                    "the chart puts an object into a namespace whose name is not a namespace \
                     name",
                ));
            }
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::Namespace,
                    format!(
                        "the server-side dry run shows an object in another namespace, which \
                         helm template did not render; in {namespace} helm runs as the session's \
                         ServiceAccount, which cannot change another namespace. Change the \
                         values so it goes into {namespace}, or ask the user to install it from \
                         Kubach's Helm screen"
                    ),
                ));
            }
            preview.elsewhere.push(Elsewhere {
                namespace: other.to_owned(),
                object: shown.clone(),
            });
            preview.findings.push(Finding {
                document: None,
                path: shown.clone(),
                message: if rbac {
                    format!(
                        "grants rights in namespace {other}, outside the release namespace \
                         {namespace}"
                    )
                } else {
                    format!(
                        "in namespace {other}, outside the release namespace {namespace}: \
                         refused unless you allow it here"
                    )
                },
            });
        } else if cluster_scoped {
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::ClusterScoped,
                    "the server-side dry run shows a cluster-scoped object, which helm template \
                     did not render; in this namespace helm runs as the session's \
                     ServiceAccount, which cannot create cluster-scoped objects. Turn it off in \
                     the values (CRDs: crds.enabled/installCRDs false) or ask the user to install \
                     it from Kubach's Helm screen",
                ));
            }
            preview.cluster.push(shown.clone());
            preview.findings.push(Finding {
                document: None,
                path: format!("{} {}", resource.kind, resource.name),
                message: "cluster-scoped object of the chart: refused unless you allow it here"
                    .into(),
            });
        } else if rbac {
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::ForbiddenKind,
                    "the server-side dry run shows a Role or RoleBinding, which helm template \
                     did not render; the session's ServiceAccount cannot grant RBAC rights. Turn \
                     it off in the values (rbac.create: false) or ask the user to install it from \
                     Kubach's Helm screen",
                ));
            }
            preview.findings.push(Finding {
                document: None,
                path: format!("{} {}", resource.kind, resource.name),
                message: "grants rights inside the namespace".into(),
            });
        }
        if !cluster_scoped
            && elsewhere.is_none()
            && let Some(kind) = served
        {
            let key = (kind.api.group.clone(), kind.api.plural.clone());
            if !preview.kinds.contains(&key) {
                preview.kinds.push(key);
            }
        }
        preview.objects.push(shown);
    }
    Ok(preview)
}

/// The cluster-scoped objects, RBAC and objects in other namespaces of
/// `manifests` (#18, #21): what a temporary ServiceAccount cannot create
/// or delete, checked as for the user. Each object once, in order.
pub(crate) fn beyond_service_account(
    manifests: &[&str],
    namespace: &str,
    catalog: &ApiCatalog,
) -> Result<Scan, Denial> {
    let mut scan = Scan::default();
    for manifest in manifests {
        let preview = check_objects(
            &manifest_resources(manifest),
            namespace,
            catalog,
            SessionMode::ClientOnly,
        )?;
        for finding in preview.findings {
            if !scan.findings.iter().any(|known| known.path == finding.path) {
                scan.findings.push(finding);
            }
        }
        for object in preview.elsewhere {
            if !scan.elsewhere.contains(&object) {
                scan.elsewhere.push(object);
            }
        }
        for object in preview.cluster {
            if !scan.cluster.contains(&object) {
                scan.cluster.push(object);
            }
        }
    }
    Ok(scan)
}

/// Whether a call must ask for the user's credentials (#18): only under a
/// temporary ServiceAccount, and only for objects it cannot handle —
/// objects in other namespaces among them (#21).
/// Client-only and impersonated namespaces keep their own identity and
/// the usual tick per finding.
pub(crate) fn needs_user_credentials(mode: SessionMode, findings: &[Finding]) -> bool {
    mode == SessionMode::TemporaryServiceAccount && !findings.is_empty()
}

/// What the agent is told when the user does not allow their credentials.
pub(crate) fn user_refusal(objects: &[String], reason: Option<&str>) -> String {
    format!(
        "the user did not allow this helm call to run with their own credentials{}. Nothing ran. \
         The namespace's temporary ServiceAccount cannot create or delete: {}. Turn these off \
         in the values (CRDs: crds.enabled/installCRDs false; RBAC: rbac.create false; objects \
         in other namespaces: move them into the release namespace) and call again, or ask the \
         user to do it from Kubach's Helm screen",
        reason.map(|r| format!(" ({r})")).unwrap_or_default(),
        objects.join(", ")
    )
}

/// Rules a temporary ServiceAccount gets while an approved helm call runs:
/// helm's release storage (Secrets), the deploy profile's kinds,
/// ServiceAccounts, and the namespaced kinds of the chart. Taken back when
/// the call ends.
pub(crate) fn helm_rules(kinds: &[(String, String)]) -> Vec<PolicyRule> {
    const VERBS: &[&str] = &[
        "get", "list", "watch", "create", "update", "patch", "delete",
    ];
    let rule = |group: &str, resources: Vec<String>| PolicyRule {
        api_groups: Some(vec![group.to_owned()]),
        resources: Some(resources),
        verbs: VERBS.iter().map(|verb| (*verb).to_owned()).collect(),
        ..Default::default()
    };
    let mut by_group: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut add = |group: &str, plural: &str| {
        let list = by_group.entry(group.to_owned()).or_default();
        if !list.iter().any(|known| known == plural) {
            list.push(plural.to_owned());
        }
    };
    for plural in [
        "secrets",
        "configmaps",
        "serviceaccounts",
        "services",
        "pods",
        "persistentvolumeclaims",
    ] {
        add("", plural);
    }
    for plural in ["deployments", "statefulsets", "daemonsets", "replicasets"] {
        add("apps", plural);
    }
    for (group, plural) in [
        ("batch", "jobs"),
        ("batch", "cronjobs"),
        ("autoscaling", "horizontalpodautoscalers"),
        ("networking.k8s.io", "ingresses"),
        ("networking.k8s.io", "networkpolicies"),
        ("policy", "poddisruptionbudgets"),
    ] {
        add(group, plural);
    }
    for (group, plural) in kinds {
        if group != "rbac.authorization.k8s.io" {
            add(group, plural);
        }
    }
    by_group
        .into_iter()
        .map(|(group, resources)| rule(&group, resources))
        .collect()
}

/// [`helm_rules`] without a verb that writes and without Secrets: what
/// `lookup` may read in a render nobody approved (#114, #173). Such a
/// render answers the agent, so it sees Secrets only where the session was
/// given them already; a `lookup` of a Secret otherwise fails the render.
pub(crate) fn helm_read_rules() -> Vec<PolicyRule> {
    let mut rules = helm_rules(&[]);
    for rule in &mut rules {
        rule.verbs
            .retain(|verb| matches!(verb.as_str(), "get" | "list" | "watch"));
        if let Some(resources) = &mut rule.resources {
            resources.retain(|resource| resource != "secrets");
        }
    }
    rules
}

// ------------------------------------------------------------- helpers

fn env(ctx: &Ctx<'_>) -> Result<Arc<HelmEnv>, ToolError> {
    ctx.helm
        .clone()
        .ok_or_else(|| ToolError::Failed("Helm is not available in this Kubach session".into()))
}

fn helm_error(error: HelmCliError) -> ToolError {
    match error {
        HelmCliError::Refused(error) => {
            Denial::new(DenialCode::InvalidInput, error.to_string()).into()
        }
        other => ToolError::Failed(format!("helm: {}", mask_text(&other.to_string()))),
    }
}

/// What a `helm template` without the cluster failed on, told from helm's
/// message without quoting it (#173).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderFailure {
    /// A template did not parse or execute, or rendered broken YAML.
    Template,
    /// The values do not fit the chart's schema.
    Schema,
    /// The chart asks for another Kubernetes version.
    KubeVersion,
    Other,
}

impl RenderFailure {
    pub(crate) fn of(error: &HelmCliError) -> Self {
        let HelmCliError::Failed { stderr, .. } = error else {
            return Self::Other;
        };
        let has = |needles: &[&str]| needles.iter().any(|needle| stderr.contains(needle));
        if has(&["values don't meet the specifications of the schema"]) {
            Self::Schema
        } else if has(&["chart requires kubeVersion"]) {
            Self::KubeVersion
        } else if has(&[
            "execution error at",
            "parse error",
            "template: ",
            "error converting YAML to JSON",
            "YAML parse error",
        ]) {
            Self::Template
        } else {
            Self::Other
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Template => {
                "a template error (a template did not parse or execute, or rendered broken YAML)"
            }
            Self::Schema => "the values do not fit the chart's values schema",
            Self::KubeVersion => "the chart requires another Kubernetes version",
            Self::Other => "helm could not render the chart",
        }
    }
}

/// Why helm's own message is not in an error (#173).
const NO_HELM_TEXT: &str = "Helm's message is not passed on: a chart's templates can put what \
                            they read into it.";

/// helm's whole message when the release name is taken: nothing of the
/// chart is in it.
const NAME_IN_USE: &str = "Error: INSTALLATION FAILED: cannot re-use a name that is still in use";

/// A failed server-side dry run (#173). It ran with rights the agent does
/// not have (the release storage is Secrets), and its templates choose how
/// it fails, so the agent gets one message for every failure of helm
/// itself; `rendered` is what the render without the cluster came to, which
/// says nothing of the cluster.
fn dry_run_error(error: HelmCliError, rendered: Option<RenderFailure>) -> ToolError {
    let stderr = match error {
        HelmCliError::Failed { stderr, .. } => stderr,
        other => return helm_error(other),
    };
    if stderr.trim() == NAME_IN_USE {
        return ToolError::Failed(
            "a release with this name exists in the namespace already; helm_upgrade changes \
             it, helm_list_releases shows it"
                .into(),
        );
    }
    let hint = match rendered {
        Some(failure) => format!(
            "helm template without the cluster failed as well: {}",
            failure.text()
        ),
        None => "helm template without the cluster rendered the chart, so the cluster is \
                 involved: its API versions, its validation of an object, what lookup found \
                 or a right the namespace's identity lacks"
            .to_owned(),
    };
    ToolError::Failed(format!(
        "the server-side dry run failed and nothing changed. {NO_HELM_TEXT} {hint}. The user \
         can see helm's message by doing the same from Kubach's Helm screen"
    ))
}

/// A failed change the user approved: helm's message masked as text,
/// except a template's own error.
fn run_error(error: HelmCliError) -> ToolError {
    match &error {
        HelmCliError::Failed { .. } if RenderFailure::of(&error) == RenderFailure::Template => {
            ToolError::Failed(format!("helm failed on a template error. {NO_HELM_TEXT}"))
        }
        _ => helm_error(error),
    }
}

/// A chart that could not be fetched. `helm pull` renders nothing and
/// contacts no cluster, so its message is passed on, masked as text.
fn fetch_error(error: HelmCliError) -> ToolError {
    match error {
        HelmCliError::Failed { .. } | HelmCliError::Output(_) => ToolError::Failed(format!(
            "the chart could not be fetched (wrong name or version, or the registry or \
             repository refused): {}",
            truncate(&mask_text(&error.to_string()), 2000)
        )),
        other => helm_error(other),
    }
}

fn store_error(error: HelmError) -> ToolError {
    if error.is_forbidden() {
        ToolError::Api {
            code: Some(403),
            message: format!(
                "{error}; reading Helm releases needs access to their storage Secrets in the \
                 namespace"
            ),
        }
    } else {
        ToolError::Failed(error.to_string())
    }
}

/// Names of the repositories added in Kubach (the user's helm).
async fn repositories(env: &HelmEnv) -> Vec<repo::LoadedRepository> {
    listed_repositories(env).await.unwrap_or_default()
}

/// The repositories added in Kubach; `None` when their list cannot be read,
/// which is not the same as an empty list.
async fn listed_repositories(env: &HelmEnv) -> Option<Vec<repo::LoadedRepository>> {
    let paths = env.paths.clone()?;
    kubach_core::runtime::blocking(move || repo::load(&paths, &[]).ok()).await
}

fn repository_names(loaded: &[repo::LoadedRepository]) -> Vec<String> {
    loaded
        .iter()
        .map(|loaded| loaded.repository.name.clone())
        .collect()
}

/// Asks the user once per session to use charts from `source` (#418).
async fn ensure_source(ctx: &Ctx<'_>, source: &ChartSource) -> Result<(), ToolError> {
    let ChartSource::Oci { source } = source else {
        return Ok(());
    };
    let _asking = ctx.session.asking().await;
    match ctx.session.chart_source(source) {
        Some(true) => return Ok(()),
        Some(false) => {
            return Err(Denial::new(
                DenialCode::GrantDenied,
                format!(
                    "the user did not allow charts from {source} in this session; Kubach does \
                     not ask again"
                ),
            )
            .into());
        }
        None => {}
    }
    let request = ConfirmationRequest {
        kind: RequestKind::ChartSource {
            source: source.clone(),
        },
        session_id: ctx.session.id().to_owned(),
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: String::new(),
        summary: format!(
            "{} wants charts from {source}, which is not a repository added in Kubach",
            ctx.client_name
        ),
        details: "Until the session ends the agent may search this registry, read its charts' \
                  default values and propose installing them. Every install is still shown \
                  with its objects and approved by you."
            .into(),
        dangerous: Vec::new(),
        mode: ctx.session.mode(),
        allow_remember: false,
        diff_cut: None,
    };
    match ask(ctx.confirmer, &request).await {
        decision if decision.is_approved() => {
            ctx.session.set_chart_source(source, true);
            Ok(())
        }
        ConfirmationDecision::Rejected { reason } => {
            ctx.session.set_chart_source(source, false);
            Err(ToolError::Rejected(Some(format!(
                "the user did not allow charts from {source}{}",
                reason.map(|r| format!(" ({r})")).unwrap_or_default()
            ))))
        }
        other => Err(ToolError::Rejected(other.reason().map(str::to_owned))),
    }
}

/// The private directory of one tool call (#174), removed with the call:
/// an empty directory helm runs in, so that no folder of the application's
/// working directory is taken for a chart, and one for the chart fetched
/// once.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Result<Self, ToolError> {
        let failed = |error: std::io::Error| {
            ToolError::Failed(format!(
                "Kubach could not make a private directory for helm: {}",
                error.kind()
            ))
        };
        let dir = helm_cli::private_dir().map_err(failed)?;
        let work = Self { dir };
        for path in [work.cwd(), work.chart_dir()] {
            std::fs::create_dir(path).map_err(failed)?;
        }
        Ok(work)
    }

    /// Where helm runs: nothing is in it.
    fn cwd(&self) -> PathBuf {
        self.dir.path().join("cwd")
    }

    /// Where the chart is fetched to.
    fn chart_dir(&self) -> PathBuf {
        self.dir.path().join("chart")
    }

    /// Fetches `chart` once for every step of the call (#174).
    async fn fetch(
        &self,
        binary: &Path,
        chart: &str,
        version: Option<&str>,
    ) -> Result<PulledChart, ToolError> {
        helm_cli::pull_chart(
            Some(binary),
            chart,
            version,
            &self.chart_dir(),
            Some(&self.cwd()),
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(fetch_error)
    }
}

/// The archive as helm's chart argument.
fn archive_argument(chart: &PulledChart) -> Result<String, ToolError> {
    chart
        .archive
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| ToolError::Failed("the temporary directory's path is not UTF-8".into()))
}

/// Refuses to go on when the fetched archive is not what it was.
async fn still_fetched(chart: Option<&PulledChart>) -> Result<(), ToolError> {
    match chart {
        Some(chart) if !chart.unchanged().await => Err(ToolError::Failed(
            "the chart archive Kubach fetched for this call changed on disk; nothing ran".into(),
        )),
        _ => Ok(()),
    }
}

/// A path that does not depend on where helm runs.
fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// A `helm` call's connection: the namespace's identity (#418).
struct HelmRun {
    target: HelmTarget,
    identity: String,
    /// Holds a generated kubeconfig (cluster only) while helm runs.
    _dir: Option<tempfile::TempDir>,
    /// Holds a vault context's kubeconfig (#113). Whoever waits for
    /// anything but helm, such as the user's answer, drops the run first.
    _vault: Option<TempKubeconfig>,
}

/// Variables of the user's environment that would change who helm is.
const IDENTITY_VARS: &[&str] = &[
    "HELM_KUBETOKEN",
    "HELM_KUBEASUSER",
    "HELM_KUBEASGROUPS",
    "HELM_KUBEAPISERVER",
    "HELM_KUBECAFILE",
    "HELM_KUBEINSECURE_SKIP_TLS_VERIFY",
    "HELM_KUBETLS_SERVER_NAME",
    "HELM_NAMESPACE",
    "HELM_KUBECONTEXT",
];

/// `as_user`: the user allowed this one call under their own kubeconfig
/// credentials (#18), whatever the namespace's identity; it then runs as
/// in a client-only namespace.
async fn helm_run(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    work: &Workspace,
    namespace: &str,
    as_user: bool,
) -> Result<HelmRun, ToolError> {
    let access = if as_user {
        HelmAccess::User
    } else {
        ctx.session.helm_access(namespace).await?
    };
    let mut vars: Vec<(&'static str, Option<OsString>)> =
        IDENTITY_VARS.iter().map(|name| (*name, None)).collect();
    let set =
        |vars: &mut Vec<(&'static str, Option<OsString>)>, name: &'static str, value: String| {
            vars.retain(|(known, _)| *known != name);
            vars.push((name, Some(value.into())));
        };
    let no_file = || {
        ToolError::Failed(
            "Helm needs this context in a kubeconfig file in this namespace (the session uses \
             your own credentials here); a vault context works with Helm where the session has \
             a ServiceAccount, and Kubach writes its credentials to a temporary file only for a \
             call the user allowed under them"
                .into(),
        )
    };
    let identity = if as_user {
        USER_IDENTITY.to_owned()
    } else {
        ctx.session.describe_identity(Some(namespace))
    };
    let (context, kubeconfig, dir, vault) = match access {
        HelmAccess::Token { kubeconfig, token } => {
            let dir = tempfile::Builder::new()
                .prefix("kubach-helm-")
                .tempdir()
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            let path = dir.path().join("kubeconfig");
            write_private(&path, &kubeconfig)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            set(&mut vars, "HELM_KUBETOKEN", token);
            ("kubach".to_owned(), vec![path], Some(dir), None)
        }
        HelmAccess::Impersonate { user, groups } => {
            set(&mut vars, "HELM_KUBEASUSER", user);
            if !groups.is_empty() {
                set(&mut vars, "HELM_KUBEASGROUPS", groups.join(","));
            }
            let files = env.kubeconfig.clone().ok_or_else(no_file)?;
            (ctx.session.scope().context.clone(), files, None, None)
        }
        HelmAccess::User => match (&env.kubeconfig, &env.vault) {
            (Some(files), _) => (
                ctx.session.scope().context.clone(),
                files.clone(),
                None,
                None,
            ),
            (None, Some(vault)) if as_user => {
                // Locking the vault ends the session: nothing is written
                // after that.
                if !ctx.session.is_active() {
                    return Err(Denial::new(
                        DenialCode::SessionEnded,
                        "the session ended before helm could run",
                    )
                    .into());
                }
                // The kind only: the file's text never reaches a message.
                let file = vault.write().map_err(|error| {
                    ToolError::Failed(format!(
                        "Kubach could not write the temporary kubeconfig for helm: {}",
                        error.kind()
                    ))
                })?;
                let path = file.path().to_owned();
                (
                    ctx.session.scope().context.clone(),
                    vec![path],
                    None,
                    Some(file),
                )
            }
            _ => return Err(no_file()),
        },
    };
    Ok(HelmRun {
        target: HelmTarget {
            context,
            kubeconfig: kubeconfig.into_iter().map(absolute).collect(),
            namespace: namespace.to_owned(),
            driver: Driver::Secret,
            secret_env: SecretEnv::new(vars),
            working_dir: Some(work.cwd()),
        },
        identity,
        _dir: dir,
        _vault: vault,
    })
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

/// Widens the ServiceAccount's Role for helm while `work` runs, and puts
/// it back afterwards whatever happened. Nothing to widen when helm runs
/// as the user (`as_user`, #18).
async fn with_helm_role<T>(
    ctx: &Ctx<'_>,
    namespace: &str,
    kinds: &[(String, String)],
    as_user: bool,
    work: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    if as_user {
        return work.await;
    }
    with_role(ctx, namespace, helm_rules(kinds), work).await
}

/// Gives the namespace's temporary ServiceAccount `rules` while `work`
/// runs; other identities have no Role to widen.
async fn with_role<T>(
    ctx: &Ctx<'_>,
    namespace: &str,
    rules: Vec<PolicyRule>,
    work: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    if ctx.session.mode_in(namespace) != SessionMode::TemporaryServiceAccount {
        return work.await;
    }
    ctx.session
        .widen_role(namespace, rules)
        .await
        .map_err(|error| {
            ToolError::Failed(format!(
                "Kubach could not give the session's ServiceAccount the rights helm needs: {error}"
            ))
        })?;
    // RBAC changes reach the authorizer within moments.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let result = work.await;
    let restored = ctx.session.restore_role(namespace).await;
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(ToolError::Failed(format!(
            "helm finished, but the ServiceAccount's Role could not be narrowed again: {error}"
        ))),
        (Err(error), _) => Err(error),
    }
}

/// Phases of a running helm, collected for the answer and passed on live.
fn watch(ctx: &Ctx<'_>, phases: &Arc<Mutex<Vec<String>>>) -> HelmWatch {
    let phases = Arc::clone(phases);
    let live = ctx.progress.clone();
    HelmWatch {
        on_phase: Some(Arc::new(move |phase: String| {
            if let Some(live) = &live {
                live(phase.clone());
            }
            let mut phases = sync::lock(&phases);
            if phases.last() != Some(&phase) && phases.len() < 50 {
                phases.push(phase);
            }
        })),
        cancel: None,
    }
}

/// A release name as Helm takes it, checked before anything is asked.
fn release_name(name: &str) -> Result<(), ToolError> {
    helm_cli::check_release_name(name)
        .map_err(|error| Denial::new(DenialCode::InvalidName, error.to_string()).into())
}

fn require_binary(env: &HelmEnv) -> Result<PathBuf, ToolError> {
    env.binary
        .clone()
        .map(absolute)
        .ok_or_else(|| helm_error(HelmCliError::NotInstalled))
}

/// Manifests diffed with Secret values and sensitive env masked. Parsed
/// on the blocking pool: the deep schemas of a chart's CRDs overflowed the
/// async worker's stack on upgrade (#21), as in [`preview_from`].
async fn masked_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> WindowDiff {
    let (old, new) = (old.to_owned(), new.to_owned());
    let (old_label, new_label) = (old_label.to_owned(), new_label.to_owned());
    kubach_core::runtime::blocking(move || {
        window_diff(&old, &new, &old_label, &new_label, MAX_CONFIRM_DIFF_BYTES)
    })
    .await
}

/// The diff of two manifests for the confirmation window, both masked
/// ([`masked_sides`]), within `max` bytes ([`cut_diff`]).
pub(crate) fn window_diff(
    old: &str,
    new: &str,
    old_label: &str,
    new_label: &str,
    max: usize,
) -> WindowDiff {
    let (old, new) = masked_sides(old, new);
    let diff = unified_diff(&old, &new, old_label, new_label);
    cut_diff(diff, &old, &new, max)
}

/// What an object of a manifest is compared with on the other side: its
/// `apiVersion`, `kind`, namespace and name.
fn object_identity(object: &Value) -> [&str; 4] {
    let text = |pointer: &str| {
        object
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    [
        text("/apiVersion"),
        text("/kind"),
        text("/metadata/namespace"),
        text("/metadata/name"),
    ]
}

/// Both manifests as [`masked_manifest`] writes them, for a diff. A masked
/// value of `new` that differs from the one of the same object of `old`
/// says so ([`mark_changed`], as in a preview and in the values' diff):
/// two values of the same length have the same mask, and the diff would
/// have no line for a Secret value that changes (#147). The mark is made
/// from the comparison alone and holds nothing of either value.
fn masked_sides(old: &str, new: &str) -> (String, String) {
    let (Ok(old_objects), Ok(new_objects)) = (
        kubach_core::apply::yaml::parse(old),
        kubach_core::apply::yaml::parse(new),
    ) else {
        // Outlines: kinds, names and a size. Two texts of the same size
        // must not look like no change.
        let (old_masked, mut new_masked) = (masked_manifest(old), masked_manifest(new));
        if old != new && old_masked == new_masked {
            new_masked.push_str("\n<differs from the other side in text that is not shown>");
        }
        return (old_masked, new_masked);
    };
    let masked = |objects: &[kubach_core::apply::yaml::Manifest]| -> Vec<Value> {
        objects
            .iter()
            .map(|manifest| {
                let mut value = manifest.value.clone();
                crate::masking::mask_object(&mut value);
                value
            })
            .collect()
    };
    let (old_masked, mut new_masked) = (masked(&old_objects), masked(&new_objects));
    // Objects of `old` by identity, in order: a manifest may hold the same
    // one twice, and then the first goes with the first.
    let mut places: HashMap<[&str; 4], VecDeque<usize>> = HashMap::new();
    for (index, object) in old_objects.iter().enumerate() {
        places
            .entry(object_identity(&object.value))
            .or_default()
            .push_back(index);
    }
    for (object, object_masked) in new_objects.iter().zip(&mut new_masked) {
        if let Some(at) = places
            .get_mut(&object_identity(&object.value))
            .and_then(VecDeque::pop_front)
        {
            mark_changed(
                &old_objects[at].value,
                &object.value,
                &old_masked[at],
                object_masked,
            );
        }
    }
    (
        kubach_core::apply::yaml::to_yaml_multi(&old_masked),
        kubach_core::apply::yaml::to_yaml_multi(&new_masked),
    )
}

/// A manifest diff for the confirmation window: whole, or with what it
/// leaves out (#137).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowDiff {
    pub text: String,
    pub cut: Option<DiffCut>,
}

/// `diff` of the masked manifests `old` and `new` within `max` bytes: as it
/// is when it fits, else cut after a whole line, with the size of the rest
/// and the objects whose changes are in it.
fn cut_diff(diff: String, old: &str, new: &str, max: usize) -> WindowDiff {
    cut_at(diff, max, |diff, from| hidden_objects(diff, from, old, new))
}

/// `diff` within `max` bytes for a confirmation window: as it is when it
/// fits, else cut after a whole line, with the size of the rest and the
/// objects `hidden` names for the whole diff and the byte the cut is at.
pub(crate) fn cut_at(
    mut diff: String,
    max: usize,
    hidden: impl FnOnce(&str, usize) -> Vec<String>,
) -> WindowDiff {
    if diff.len() <= max {
        return WindowDiff {
            text: diff,
            cut: None,
        };
    }
    let mut end = max;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    // After a whole line; a single line longer than `max` is cut inside.
    if let Some(line_end) = diff[..end].rfind('\n') {
        end = line_end + 1;
    }
    let mut objects = hidden(&diff, end);
    let more_objects = objects.len().saturating_sub(MAX_CUT_OBJECTS);
    objects.truncate(MAX_CUT_OBJECTS);
    let cut = DiffCut {
        shown_bytes: end,
        total_bytes: diff.len(),
        hidden_lines: diff[end..].lines().count(),
        objects,
        more_objects,
    };
    diff.truncate(end);
    WindowDiff {
        text: diff,
        cut: Some(cut),
    }
}

/// Where each side of a hunk starts: `@@ -12,7 +14,8 @@` is `(12, 14)`.
fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split([' ', ',']).next()?;
    let old = old.split(',').next()?;
    Some((old.parse().ok()?, new.parse().ok()?))
}

/// The documents of a manifest as [`masked_manifest`] writes it: the line
/// each starts at (from 1) and its text.
fn documents(manifest: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let (mut start_line, mut start) = (1, 0);
    let (mut line, mut offset) = (1, 0);
    for text in manifest.split_inclusive('\n') {
        if text.trim_end() == "---" {
            found.push((start_line, &manifest[start..offset]));
            start_line = line + 1;
            start = offset + text.len();
        }
        line += 1;
        offset += text.len();
    }
    found.push((start_line, &manifest[start..]));
    found
}

/// `Kind name` of the objects of the masked manifests whose added or
/// removed lines lie in `diff` from byte `from` on: those of `new` in its
/// order, then those only `old` has.
fn hidden_objects(diff: &str, from: usize, old: &str, new: &str) -> Vec<String> {
    // Lines (from 1) of each side that change after the cut.
    let (mut old_lines, mut new_lines) = (Vec::new(), Vec::new());
    let (mut old_line, mut new_line) = (0usize, 0usize);
    let mut in_hunk = false;
    let mut offset = 0;
    for text in diff.split_inclusive('\n') {
        let hidden = offset >= from;
        offset += text.len();
        if let Some((old_start, new_start)) = hunk_start(text) {
            (old_line, new_line) = (old_start, new_start);
            in_hunk = true;
        } else if !in_hunk {
            // The `---` and `+++` header.
        } else if text.starts_with('+') {
            if hidden {
                new_lines.push(new_line);
            }
            new_line += 1;
        } else if text.starts_with('-') {
            if hidden {
                old_lines.push(old_line);
            }
            old_line += 1;
        } else {
            old_line += 1;
            new_line += 1;
        }
    }
    let mut labels: Vec<String> = Vec::new();
    for (manifest, lines) in [(new, new_lines), (old, old_lines)] {
        let documents = documents(manifest);
        let mut last = None;
        for line in lines {
            let index = documents
                .partition_point(|(start, _)| *start <= line)
                .saturating_sub(1);
            if last == Some(index) {
                continue;
            }
            last = Some(index);
            let label = document_label(documents[index].1, index);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    labels
}

/// `Kind name` of one document, or its number when it has none.
fn document_label(document: &str, index: usize) -> String {
    let object = kubach_core::apply::yaml::parse(document)
        .ok()
        .and_then(|mut manifests| manifests.pop())
        .map(|manifest| manifest.value);
    let field = |pointer: &str| {
        object
            .as_ref()
            .and_then(|object| object.pointer(pointer))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    match (field("/kind"), field("/metadata/name")) {
        (Some(kind), Some(name)) => format!("{kind} {name}"),
        _ => format!("document {}", index + 1),
    }
}

/// How the release helm wrote compares with the dry run the user approved
/// (#174), by object: `Kind name` of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Comparison {
    /// In the release, not in the dry run.
    pub not_in_dry_run: Vec<String>,
    /// In the dry run, not in the release.
    pub missing: Vec<String>,
    /// In both with different content. A chart that generates values (a
    /// password, a certificate) or reads the cluster has these honestly.
    pub changed: Vec<String>,
    /// Another chart or version than the dry run's.
    pub chart_differs: bool,
}

impl Comparison {
    /// The release has the objects the user saw, of the chart they saw.
    pub fn same_objects(&self) -> bool {
        self.not_in_dry_run.is_empty() && self.missing.is_empty() && !self.chart_differs
    }

    fn json(&self) -> Value {
        let listed = |objects: &[String]| -> Vec<String> {
            let mut shown: Vec<String> = objects.iter().take(MAX_CUT_OBJECTS).cloned().collect();
            if objects.len() > MAX_CUT_OBJECTS {
                shown.push(format!("… and {} more", objects.len() - MAX_CUT_OBJECTS));
            }
            shown
        };
        json!({
            "sameObjects": self.same_objects(),
            "chartDiffers": self.chart_differs,
            "notInDryRun": listed(&self.not_in_dry_run),
            "missing": listed(&self.missing),
            "changed": listed(&self.changed),
        })
    }
}

/// Compares the manifests of the dry run (`expected`) and of the release
/// (`actual`) object by object; `None` when one of them does not parse.
pub(crate) fn compare_manifests(
    expected: &str,
    actual: &str,
    namespace: &str,
) -> Option<Comparison> {
    let expected = kubach_core::apply::yaml::parse(expected).ok()?;
    let actual = kubach_core::apply::yaml::parse(actual).ok()?;
    // An object without a namespace is in the release's.
    let key = |object: &Value| -> [String; 4] {
        let [api_version, kind, other, name] = object_identity(object);
        let other = if other.is_empty() { namespace } else { other };
        [api_version, kind, other, name].map(str::to_owned)
    };
    let label = |key: &[String; 4]| {
        if key[2] == namespace {
            format!("{} {}", key[1], key[3])
        } else {
            format!("{} {} (namespace {})", key[1], key[3], key[2])
        }
    };
    let mut places: HashMap<[String; 4], VecDeque<usize>> = HashMap::new();
    for (index, object) in expected.iter().enumerate() {
        places
            .entry(key(&object.value))
            .or_default()
            .push_back(index);
    }
    let mut found = vec![false; expected.len()];
    let mut comparison = Comparison::default();
    for object in &actual {
        let key = key(&object.value);
        match places.get_mut(&key).and_then(VecDeque::pop_front) {
            Some(at) => {
                found[at] = true;
                if expected[at].value != object.value {
                    comparison.changed.push(label(&key));
                }
            }
            None => comparison.not_in_dry_run.push(label(&key)),
        }
    }
    for (object, found) in expected.iter().zip(found) {
        if !found {
            comparison.missing.push(label(&key(&object.value)));
        }
    }
    Some(comparison)
}

/// The release after a change against what the user approved.
#[derive(Debug, Default)]
enum Compared {
    /// Nothing to compare (an uninstall).
    #[default]
    Nothing,
    /// The release could not be read afterwards.
    Unreadable,
    Done(Comparison),
}

/// What is known after a change the user approved (#174): the archive all
/// its steps read, and how the release compares with the dry run.
#[derive(Debug, Default)]
struct AfterChange {
    chart_sha256: Option<String>,
    compared: Compared,
}

impl AfterChange {
    /// `expected`: the release of the dry run, or the revision rolled back
    /// to. Compared on the blocking pool, as every manifest is.
    async fn compare(
        chart: Option<&PulledChart>,
        expected: &Release,
        actual: Option<&Release>,
        namespace: &str,
    ) -> Self {
        let chart_sha256 = chart.map(|chart| chart.sha256.clone());
        let Some(actual) = actual else {
            return Self {
                chart_sha256,
                compared: Compared::Unreadable,
            };
        };
        let chart_differs = expected.chart.reference() != actual.chart.reference();
        let (expected, actual) = (release_manifest(expected), release_manifest(actual));
        let namespace = namespace.to_owned();
        let comparison = kubach_core::runtime::blocking(move || {
            compare_manifests(&expected, &actual, &namespace)
        })
        .await;
        Self {
            chart_sha256,
            compared: match comparison {
                Some(comparison) => Compared::Done(Comparison {
                    chart_differs,
                    ..comparison
                }),
                None => Compared::Unreadable,
            },
        }
    }

    /// For the agent.
    fn answer(&self) -> Option<Value> {
        match &self.compared {
            Compared::Nothing => None,
            Compared::Unreadable => Some(json!(
                "not compared: the release could not be read after the change"
            )),
            Compared::Done(comparison) => {
                let mut out = comparison.json();
                out["note"] = json!(if !comparison.same_objects() {
                    "The release differs from the dry run the user approved: tell the user \
                     which objects."
                } else if comparison.changed.is_empty() {
                    "The release has the objects of the dry run the user approved."
                } else {
                    "The release has the objects of the dry run the user approved; those \
                     under changed differ in content, as with values a chart generates or \
                     reads from the cluster."
                });
                Some(out)
            }
        }
    }

    /// For the audit log: the archive, and a mismatch when there is one.
    fn audit(&self, audit: &mut Value) {
        if let Some(sha256) = &self.chart_sha256 {
            audit["chartSha256"] = json!(sha256);
        }
        match &self.compared {
            Compared::Nothing => {}
            Compared::Unreadable => audit["comparedWithDryRun"] = json!("not compared"),
            Compared::Done(comparison) => {
                audit["comparedWithDryRun"] = if comparison.same_objects() {
                    json!({"sameObjects": true, "changed": comparison.changed.len()})
                } else {
                    comparison.json()
                };
            }
        }
    }
}

/// What the agent is told after a change: status, revision, objects and
/// NOTES (masked, bounded).
fn result_text(
    action: HelmAction,
    release: Option<&Release>,
    preview: &Preview,
    phases: &[String],
    output: &str,
    values_diff: Option<&str>,
    after: &AfterChange,
) -> String {
    let mut out = json!({
        "action": action,
        "objects": preview.objects,
        "phases": phases,
    });
    if let Some(sha256) = &after.chart_sha256 {
        out["chartArchive"] = json!({ "sha256": sha256 });
    }
    if let Some(compared) = after.answer() {
        out["comparedWithDryRun"] = compared;
    }
    if let Some(diff) = values_diff {
        out["valuesDiff"] = json!(if diff.trim().is_empty() {
            "the values did not change"
        } else {
            diff
        });
    }
    if let Some(release) = release {
        out["release"] = json!(release.name);
        out["namespace"] = json!(release.namespace);
        out["revision"] = json!(release.revision);
        out["status"] = json!(release.status.as_str());
        out["chart"] = json!(release.chart.reference());
        out["appVersion"] = json!(release.chart.app_version);
        out["notes"] = json!(truncate(&mask_text(&release.notes), MAX_NOTES_BYTES));
    } else if !output.trim().is_empty() {
        out["output"] = json!(truncate(&mask_text(output.trim()), MAX_NOTES_BYTES));
    }
    serde_json::to_string_pretty(&out).unwrap_or_default()
}

// ------------------------------------------------------------- read tools

pub(crate) async fn search(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: SearchArgs = parse(args)?;
    authorize(ctx, &Request::HelmCharts).await?;
    let env = env(ctx)?;
    if let Some(reference) = a.oci {
        let source = chart_source(&reference, &[])?;
        ensure_source(ctx, &source).await?;
        let versions = oci::list_versions(reference.trim(), a.devel)
            .await
            .map_err(|error| ToolError::Failed(format!("registry: {error}")))?;
        let out = json!({
            "chart": reference.trim(),
            "latest": oci::newest_stable(&versions),
            "versions": versions.iter().take(MAX_SEARCH_ROWS).collect::<Vec<_>>(),
            "howToInstall": "helm_install with chart set to this oci:// reference and version",
        });
        return Ok(ToolOutput::read(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        ));
    }
    let listed = listed_repositories(&env).await;
    let loaded = listed.as_deref().unwrap_or_default();
    let hits = repo::search(
        loaded,
        &a.query,
        SearchOptions {
            all_versions: a.versions,
            devel: a.devel,
            repository: None,
        },
    );
    let rows: Vec<Value> = hits
        .iter()
        .take(MAX_SEARCH_ROWS)
        .map(|hit| {
            json!({
                "chart": hit.reference(),
                "version": hit.version.version,
                "appVersion": hit.version.app_version,
                "description": hit.version.description.as_deref().map(|d| truncate(d, 200).into_owned()),
                "deprecated": hit.version.deprecated,
            })
        })
        .collect();
    let repositories: Vec<Value> = loaded
        .iter()
        .map(|loaded| {
            json!({
                "name": loaded.repository.name,
                "index": match &loaded.index {
                    IndexState::Ready(_) => "ready",
                    IndexState::Missing => "missing: update it in Kubach",
                    _ => "unreadable",
                },
            })
        })
        .collect();
    let mut out = json!({
        "charts": rows,
        "more": hits.len() > MAX_SEARCH_ROWS,
        "repositories": repositories,
        "note": "Charts of the repositories added in Kubach. An oci:// chart can be looked at \
                 with oci; the user is asked to allow its registry.",
    });
    if hits.is_empty() {
        explain_empty_search(&mut out, listed.as_deref());
    }
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

/// Says in `out` why a chart search found nothing (#118); `listed` is what
/// [`listed_repositories`] returned. Names only: a repository's URL may hold
/// a login, and a parse error of the list or an index quotes the file.
fn explain_empty_search(out: &mut Value, listed: Option<&[repo::LoadedRepository]>) {
    const ONLY_THE_USER: &str = "Only the user adds repositories and updates their indexes, in \
                                 Kubach (Helm → Charts → Repositories).";
    let loaded = listed.unwrap_or_default();
    let names = |ready: bool| -> Vec<&str> {
        loaded
            .iter()
            .filter(|loaded| loaded.charts().is_some() == ready)
            .map(|loaded| loaded.repository.name.as_str())
            .collect()
    };
    let searched = names(true);
    let without_index = names(false);
    let (reason, note) = if listed.is_none() {
        (
            "repositoryListUnreadable",
            "Kubach could not read Helm's repository list, so nothing was searched; repositories \
             may still be added. Ask the user to check them in Kubach (Helm → Charts → \
             Repositories)."
                .to_owned(),
        )
    } else if loaded.is_empty() {
        (
            "noRepositories",
            format!("No repositories are added in Kubach. {ONLY_THE_USER}"),
        )
    } else if searched.is_empty() {
        (
            "noLoadedIndex",
            format!(
                "Nothing was searched: no repository has a loaded index ({}). {ONLY_THE_USER}",
                without_index.join(", ")
            ),
        )
    } else {
        let skipped = if without_index.is_empty() {
            String::new()
        } else {
            format!(
                " Not searched, no loaded index: {}. {ONLY_THE_USER}",
                without_index.join(", ")
            )
        };
        (
            "noMatch",
            format!(
                "No chart matched in {}. The query is one substring of repo/chart, a description \
                 or a keyword: a shorter one may match, and an empty one lists every chart.\
                 {skipped}",
                searched.join(", ")
            ),
        )
    };
    out["reason"] = json!(reason);
    out["searched"] = json!(searched);
    out["withoutIndex"] = json!(without_index);
    out["note"] = json!(note);
    out["ociCharts"] = json!(
        "An oci:// chart is in no index, so a query never finds it. If its oci://registry/path \
         is known, pass that as oci to list its versions, or as chart to helm_install; the user \
         is asked to allow its registry."
    );
}

pub(crate) async fn show_values(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: ShowValuesArgs = parse(args)?;
    let env = env(ctx)?;
    let loaded = repositories(&env).await;
    let source = chart_source(&a.chart, &repository_names(&loaded))?;
    authorize(ctx, &Request::HelmCharts).await?;
    ensure_source(ctx, &source).await?;
    let binary = require_binary(&env)?;
    let chart = repo::helm_chart_reference(&loaded, &a.chart);
    let work = Workspace::new()?;
    let command =
        HelmCommand::show(ShowKind::Values, &chart, a.version.as_deref()).in_dir(Some(&work.cwd()));
    let text = helm_cli::run(Some(&binary), &command, SHOW_TIMEOUT)
        .await
        .map_err(helm_error)?;
    Ok(ToolOutput::read(
        truncate(&mask_text(&text), crate::tools::MAX_OUTPUT_BYTES).into_owned(),
    ))
}

pub(crate) async fn list_releases(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: ReleasesArgs = parse(args)?;
    let allowed = authorize(
        ctx,
        &Request::HelmRead {
            namespace: a.namespace,
        },
    )
    .await?;
    let client = ctx.session.client_in(&allowed.namespace)?;
    let releases = Releases::new(client)
        .list(Some(&allowed.namespace))
        .await
        .map_err(store_error)?;
    let rows: Vec<Value> = releases
        .iter()
        .map(|release| {
            json!({
                "release": release.name,
                "revision": release.revision,
                "status": release.status.as_str(),
                "chart": release.chart.reference(),
                "appVersion": release.chart.app_version,
                "updated": release.updated.map(|t| t.to_string()),
            })
        })
        .collect();
    let out = json!({"namespace": allowed.namespace, "releases": rows});
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

pub(crate) async fn history(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: HistoryArgs = parse(args)?;
    let allowed = authorize(
        ctx,
        &Request::HelmRead {
            namespace: a.namespace,
        },
    )
    .await?;
    helm_cli::check_release_name(&a.release)
        .map_err(|error| Denial::new(DenialCode::InvalidName, error.to_string()))?;
    let client = ctx.session.client_in(&allowed.namespace)?;
    let revisions = Releases::new(client)
        .history(&allowed.namespace, &a.release)
        .await
        .map_err(store_error)?;
    let rows: Vec<Value> = revisions
        .iter()
        .map(|release| {
            json!({
                "revision": release.revision,
                "status": release.status.as_str(),
                "chart": release.chart.reference(),
                "appVersion": release.chart.app_version,
                "updated": release.last_deployed.map(|t| t.to_string()),
                "description": truncate(&mask_text(&release.description), 500),
            })
        })
        .collect();
    let out = json!({
        "release": a.release,
        "namespace": allowed.namespace,
        "revisions": rows,
    });
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

/// The values a release (or one revision of it) was given (#22), masked as
/// by [`mask_values`]: by key name, and strings that look like a key or a
/// token. Read under the same identity as [`history`]; where it cannot
/// read Helm's storage Secrets the agent is told so, and Kubach does not
/// try other credentials.
pub(crate) async fn get_values(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: GetValuesArgs = parse(args)?;
    helm_cli::check_release_name(&a.release)
        .map_err(|error| Denial::new(DenialCode::InvalidName, error.to_string()))?;
    let allowed = authorize(
        ctx,
        &Request::HelmRead {
            namespace: a.namespace,
        },
    )
    .await?;
    let namespace = allowed.namespace.clone();
    let client = ctx.session.client_in(&namespace)?;
    let release = Releases::new(client)
        .get(&namespace, &a.release, a.revision)
        .await
        .map_err(|error| match store_error(error) {
            ToolError::Api { code, message } => ToolError::Api {
                code,
                message: format!(
                    "{message}. The session's identity in {namespace} ({}) cannot read them, \
                     and Kubach does not read them with other credentials. Ask the user to look \
                     at the values in Kubach's Helm screen",
                    ctx.session.describe_identity(Some(&namespace))
                ),
            },
            other => other,
        })?;
    let values = release.user_values.expose().clone();
    let (yaml, masked) = kubach_core::runtime::blocking(move || {
        let mut values = values;
        let mut masked = mask_values_listed(&mut values);
        masked.sort();
        (kubach_core::helm::values_to_yaml(&values), masked)
    })
    .await;
    let out = json!({
        "release": release.name,
        "namespace": namespace,
        "revision": release.revision,
        "chart": release.chart.reference(),
        "values": truncate(&yaml, crate::tools::MAX_OUTPUT_BYTES),
        "maskedKeys": masked,
        "note": "Values the release was given (not the chart's defaults: see helm_show_values). \
                 Values under sensitive names and strings that look like keys or tokens are \
                 masked as *** (N bytes) and cannot be read. To change a few keys, pass only \
                 them to helm_upgrade as valuesPatch; Kubach merges it into these values.",
    });
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

// ------------------------------------------------------------- changes

/// Asks the user to approve `change`; `dangerous` holds the cluster-scoped
/// objects and RBAC the user must tick on their own.
async fn approve(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    change: HelmChange,
    details: String,
    findings: Vec<Finding>,
) -> Result<(), ToolError> {
    let summary = format!(
        "{} {} release {} in {}{}",
        ctx.client_name,
        match change.action {
            HelmAction::Install => "wants to install",
            HelmAction::Upgrade => "wants to upgrade",
            HelmAction::Rollback => "wants to roll back",
            HelmAction::Uninstall => "wants to uninstall",
        },
        change.release,
        allowed.namespace,
        match (&change.action, change.revision) {
            (HelmAction::Rollback, Some(revision)) => format!(" to revision {revision}"),
            _ => format!(" ({})", change.chart),
        }
    );
    let summary = if change.namespaces.is_empty() {
        summary
    } else {
        format!("{summary}, also changing {}", change.namespaces.join(", "))
    };
    let summary = match change.credentials {
        HelmCredentials::Session => summary,
        HelmCredentials::AskUser => format!("{summary} with your own kubeconfig credentials"),
        HelmCredentials::User => format!("{summary}, running with your kubeconfig credentials"),
    };
    let request = ConfirmationRequest {
        kind: RequestKind::Helm {
            helm: Box::new(change),
        },
        session_id: ctx.session.id().to_owned(),
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: allowed.namespace.clone(),
        summary,
        details,
        dangerous: findings,
        mode: ctx.session.mode_in(&allowed.namespace),
        allow_remember: false,
        diff_cut: None,
    };
    // Under the user's credentials, beyond the namespace, uninstalling or
    // with cluster-scoped objects: always asked (#30).
    crate::tools::approve_change(ctx, request, "the session ended while waiting for the user").await
}

/// The latest revision after a change, under the namespace's identity.
async fn current_release(ctx: &Ctx<'_>, namespace: &str, release: &str) -> Option<Release> {
    let client = ctx.session.client_in(namespace).ok()?;
    Releases::new(client)
        .get(namespace, release, None)
        .await
        .ok()
}

/// The latest revision after a change, for the comparison with the dry
/// run (#174): read as before it, with the storage open to a temporary
/// ServiceAccount for the read.
async fn release_after(
    ctx: &Ctx<'_>,
    namespace: &str,
    release: &str,
) -> Result<Option<Release>, ToolError> {
    with_helm_role(ctx, namespace, &[], false, async {
        Ok(current_release(ctx, namespace, release).await)
    })
    .await
}

/// A release's manifest with its hooks.
fn release_manifest(release: &Release) -> String {
    let mut manifest = release.manifest.expose().clone();
    for hook in &release.hooks {
        manifest.push_str("\n---\n");
        manifest.push_str(hook.manifest.expose());
    }
    manifest
}

/// The objects of `release` checked for the call: as for the user when
/// the user allowed their credentials (`as_user`, #18), otherwise for the
/// namespace's identity.
///
/// Manifests are parsed on the blocking pool, not the async worker's
/// stack: the deeply nested schemas of CRDs recurse deep (#18).
async fn preview_from(
    ctx: &Ctx<'_>,
    release: &Release,
    namespace: &str,
    as_user: bool,
) -> Result<Preview, ToolError> {
    let mode = if as_user {
        SessionMode::ClientOnly
    } else {
        ctx.session.mode_in(namespace)
    };
    let manifest = release_manifest(release);
    let namespace = namespace.to_owned();
    let catalog = ctx.session.catalog().clone();
    kubach_core::runtime::blocking(move || {
        check_objects(&manifest_resources(&manifest), &namespace, &catalog, mode)
    })
    .await
    .map_err(ToolError::from)
}

/// What a temporary ServiceAccount could not do among `manifests` (#18)
/// and their objects in other namespaces (#21), whatever the namespace's
/// identity: [`needs_user_credentials`] decides by the mode. Parsed on the
/// blocking pool, as in [`preview_from`].
async fn scan(ctx: &Ctx<'_>, namespace: &str, manifests: &[&str]) -> Result<Scan, ToolError> {
    let manifests: Vec<String> = manifests.iter().map(|text| (*text).to_owned()).collect();
    let namespace = namespace.to_owned();
    let catalog = ctx.session.catalog().clone();
    kubach_core::runtime::blocking(move || {
        let manifests: Vec<&str> = manifests.iter().map(String::as_str).collect();
        beyond_service_account(&manifests, &namespace, &catalog)
    })
    .await
    .map_err(ToolError::from)
}

/// Sets up the session's rights in each namespace besides the release's
/// that the chart changes (#21): the same `read(ns)` and `modify(ns)` as
/// any tool in that namespace, through [`authorize`] (a pinned session
/// refuses it). Done before the user is asked about the call and before
/// anything changes; a refusal names the objects when they are known from
/// a render without the cluster (`named`). The call's namespace for the
/// audit log stays the release's.
pub(crate) async fn authorize_elsewhere(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    release: &str,
    elsewhere: &[Elsewhere],
    named: bool,
) -> Result<(), ToolError> {
    for other in other_namespaces(elsewhere) {
        let result = authorize(
            ctx,
            &Request::HelmChange {
                namespace: Some(other.clone()),
                release: release.to_owned(),
            },
        )
        .await;
        *sync::lock(&ctx.namespace) = Some(allowed.namespace.clone());
        let Err(error) = result else {
            continue;
        };
        let objects: Vec<String> = elsewhere
            .iter()
            .filter(|object| object.namespace == other)
            .map(|object| object.object.clone())
            .collect();
        // After a dry run nobody approved, the names are the chart's own
        // text and stay out of the refusal (#173).
        let message = if named {
            elsewhere_refusal(&other, &allowed.namespace, &objects, &error.message())
        } else {
            format!(
                "the server-side dry run puts objects into another namespace, which helm \
                 template did not render, and this call may not change that namespace. Nothing \
                 ran. Change the chart's values so they go into {}, or ask the user to do it \
                 from Kubach's Helm screen",
                allowed.namespace
            )
        };
        return Err(match error {
            ToolError::Denied(denial) => Denial::new(denial.code, message).into(),
            ToolError::Rejected(_) => ToolError::Rejected(Some(message)),
            ToolError::Api { code, .. } => ToolError::Api { code, message },
            ToolError::Failed(_) => ToolError::Failed(message),
        });
    }
    Ok(())
}

/// `Kind name`, with the namespace of an object outside `namespace`.
pub(crate) fn object_label(object: &ManifestResource, namespace: &str) -> String {
    match object.namespace.as_deref() {
        Some(other) if other != namespace => {
            format!("{} {} (namespace {other})", object.kind, object.name)
        }
        _ => format!("{} {}", object.kind, object.name),
    }
}

/// [`object_label`] of each object `helm template` rendered, parsed on the
/// blocking pool.
async fn rendered_objects(rendered: Option<&str>, namespace: &str) -> Vec<String> {
    let text = rendered.unwrap_or_default().to_owned();
    let namespace = namespace.to_owned();
    kubach_core::runtime::blocking(move || {
        manifest_resources(&text)
            .iter()
            .map(|object| object_label(object, &namespace))
            .collect()
    })
    .await
}

/// The chart as `helm template` renders it on the client, without the
/// cluster (#18, #173): all that is known before anybody is asked. An error
/// when it does not render there (a chart that needs the cluster's
/// capabilities): the server-side dry run then still shows its objects.
async fn rendered(binary: &Path, request: &TemplateRequest) -> Result<String, HelmCliError> {
    helm_cli::template(Some(binary), request, helm_cli::DEFAULT_TIMEOUT).await
}

/// What the first of two windows shows (#18, #173): the objects known
/// without the cluster, and what a yes leads to.
fn first_window(objects: &[String], failure: Option<RenderFailure>) -> String {
    match failure {
        None => format!(
            "Objects of helm template (rendered without the cluster):\n{}\n\n{DRY_RUN_NEXT}",
            bullet_list("+", objects)
        ),
        Some(failure) => format!(
            "helm template could not render this chart without the cluster ({}), so its \
             objects are not known yet.\n\n{DRY_RUN_NEXT}",
            failure.text()
        ),
    }
}

/// Asks the user of a client-only namespace before the server-side dry run
/// (#173). There helm has nothing but the user's own kubeconfig credentials,
/// and a dry run runs the chart's templates with them, so it waits for a
/// yes as a call under the user's credentials does anywhere else (#18): the
/// same unticked box, then the window with the dry run's diff.
async fn ask_dry_run(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    mut change: HelmChange,
    details: String,
) -> Result<(), ToolError> {
    change.credentials = HelmCredentials::AskUser;
    change.identity = ctx.session.describe_identity(Some(&allowed.namespace));
    approve(ctx, allowed, change, details, Vec::new()).await
}

/// Asks the user to run this one call under their own kubeconfig
/// credentials (#18); `findings` are why. A no, or no answer, is a refusal
/// that names the objects; a yes is recorded for the audit log.
async fn ask_user_credentials(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    allowed: &Allowed,
    mut change: HelmChange,
    mut details: String,
    findings: &[Finding],
) -> Result<(), ToolError> {
    let objects: Vec<String> = findings.iter().map(|f| f.path.clone()).collect();
    if env.kubeconfig.is_none() && env.vault.is_none() {
        return Err(Denial::new(
            DenialCode::ClusterScoped,
            format!(
                "the namespace's temporary ServiceAccount cannot create or delete: {}; running \
                 helm with the user's credentials needs this context's kubeconfig, and this \
                 session has none. Turn these off in the values or ask the user to do it from \
                 Kubach's Helm screen",
                objects.join(", ")
            ),
        )
        .into());
    }
    if env.kubeconfig.is_none() {
        let _ = write!(details, "\n\n{VAULT_NOTE}");
    }
    change.credentials = HelmCredentials::AskUser;
    change.identity = USER_IDENTITY.to_owned();
    change.user_objects = objects.clone();
    match approve(ctx, allowed, change, details, Vec::new()).await {
        Ok(()) => {
            *sync::lock(&ctx.identity) = Some(USER_IDENTITY.to_owned());
            Ok(())
        }
        Err(ToolError::Rejected(reason)) => Err(ToolError::Rejected(Some(user_refusal(
            &objects,
            reason.as_deref(),
        )))),
        Err(other) => Err(other),
    }
}

/// What `preview: true` shows the agent (#68): a summary, with the diff
/// kept in the session (#115).
struct PreviewText<'a> {
    release: &'a str,
    namespace: &'a str,
    cluster: &'a [String],
    findings: &'a [Finding],
    /// Objects in other namespaces, needed again to read the diff.
    elsewhere: &'a [Elsewhere],
    /// The manifests compared, raw, each with what it is.
    old: (&'a str, &'a str),
    new: (&'a str, &'a str),
    /// Masked values diff of an upgrade and its rows.
    values: Option<(&'a str, &'a [Value])>,
    /// A render by `helm template` of a chart that uses `lookup` (#114).
    lookup: Option<&'a LookupNote>,
}

/// The answer to a preview. Compared on the blocking pool (#21).
async fn preview_output(
    ctx: &Ctx<'_>,
    preview: &PreviewText<'_>,
    note: &str,
) -> Result<ToolOutput, ToolError> {
    let (values_diff, values) = preview.values.unwrap_or_default();
    let input = PreviewInput {
        old: preview.old.0.to_owned(),
        new: preview.new.0.to_owned(),
        old_label: preview.old.1.to_owned(),
        new_label: preview.new.1.to_owned(),
        namespace: preview.namespace.to_owned(),
        values_diff: values_diff.to_owned(),
        values: values.to_vec(),
        lookup_objects: preview
            .lookup
            .map(|lookup| lookup.objects.clone())
            .unwrap_or_default(),
        lookup_whole: preview.lookup.is_some_and(|lookup| lookup.whole_preview),
    };
    let mut compared = kubach_core::runtime::blocking(move || compare(&input)).await;
    let mut out = json!({
        "preview": true,
        "note": note,
        "clusterObjects": preview.cluster,
        "needsTheUsersAttention": preview
            .findings
            .iter()
            .map(|f| format!("{}: {}", f.path, f.message))
            .collect::<Vec<_>>(),
        "summary": compared.summary.take(),
    });
    if let Some(lookup) = preview.lookup {
        out["lookup"] = json!({
            "status": lookup.status,
            "note": lookup.note,
            "wholePreview": lookup.whole_preview,
            "objects": lookup.objects,
        });
    }
    out["fullDiff"] = sync::lock(ctx.session.helm_previews()).keep(
        preview.release,
        preview.namespace,
        preview.elsewhere.to_vec(),
        compared,
    );
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

/// A preview from `helm template` without the cluster (#68, #173).
const TEMPLATE_NOTE: &str = "Rendered with helm template, without the cluster: nothing was \
     installed or changed and the user was not asked. This render does not see the cluster's API \
     versions, its validation of the objects or lookup. Call again without preview to do it: the \
     server-side dry run runs then, and the user sees its objects and diff before approving.";

/// [`TEMPLATE_NOTE`] of a render that read the cluster (#114).
const TEMPLATE_READ_NOTE: &str = "Rendered with helm template under the session's \
     ServiceAccount, read-only: nothing was installed or changed and the user was not asked. \
     lookup read what the session may read. Call again without preview to do it: the \
     server-side dry run runs then, and the user sees its objects and diff before approving.";

const LOOKUP_READ: &str = "The chart uses lookup, and it read the namespace's objects in this \
     render, with the session's own read rights: the diff has no false changes from it.";
const LOOKUP_MISSING: &str = "The chart uses lookup, which found nothing in this render: it ran \
     without the cluster (a preview reads no more than the session may, and no Secret it was \
     not given). A change to these objects may be false (a generated TLS Secret looks \
     recreated, for example) and is not proof of a real one. The diff the user sees after \
     allowing the call has lookup working.";
const LOOKUP_MISSING_WHOLE: &str = "The chart uses lookup, which found nothing in this render: it \
     ran without the cluster. Kubach cannot tell which objects depend on it, so any change in \
     this diff may be false (a generated TLS Secret looks recreated, for example). The diff the \
     user sees after allowing the call has lookup working.";
const LOOKUP_UNKNOWN: &str = "Kubach could not read the chart's templates, so it cannot tell \
     whether the chart uses lookup. If it does, any change in this diff may be false. The diff \
     the user sees after allowing the call has lookup working.";

/// What a `helm template` preview says about `lookup` (#114).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LookupNote {
    /// `read`: the render saw the cluster. `unavailable`: it did not.
    /// `unknown`: Kubach could not read the templates.
    pub status: &'static str,
    pub note: &'static str,
    /// Any object may be misstated, not only `objects`.
    pub whole_preview: bool,
    /// Objects that depend on `lookup`, when not read.
    pub objects: Vec<String>,
}

impl LookupNote {
    fn read() -> Self {
        Self {
            status: "read",
            note: LOOKUP_READ,
            whole_preview: false,
            objects: Vec::new(),
        }
    }

    fn unknown() -> Self {
        Self {
            status: "unknown",
            note: LOOKUP_UNKNOWN,
            whole_preview: true,
            objects: Vec::new(),
        }
    }

    fn unavailable(whole_preview: bool, objects: Vec<String>) -> Self {
        Self {
            status: "unavailable",
            note: if whole_preview {
                LOOKUP_MISSING_WHOLE
            } else {
                LOOKUP_MISSING
            },
            whole_preview,
            objects,
        }
    }
}

/// `helm template` on the cluster, under the namespace's temporary
/// ServiceAccount, its Role widened only to read and never to Secrets
/// (#114, #173): `--dry-run=server` without `--validate`. Never the user's
/// credentials, which nobody allowed here, so nothing of a vault context
/// reaches the disk either: helm gets the kubeconfig with the cluster only
/// and the ServiceAccount's token. The ServiceAccount has no rights on
/// cluster-scoped objects, in other namespaces or on Secrets the session
/// was not given, so a `lookup` of those fails the render.
async fn render_on_cluster(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    binary: &Path,
    work: &Workspace,
    template: &TemplateRequest,
) -> Result<String, ToolError> {
    let namespace = &template.namespace;
    let run = helm_run(ctx, env, work, namespace, false).await?;
    with_role(ctx, namespace, helm_read_rules(), async {
        helm_cli::template_on_cluster(
            Some(binary),
            &run.target,
            template,
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(helm_error)
    })
    .await
}

/// What `lookup` means for a preview: under a temporary ServiceAccount a
/// chart that uses it is rendered on the cluster with the session's read
/// rights; elsewhere, and when that render fails, the render without the
/// cluster stands and the objects `lookup` may misstate are marked. The reason a render failed is not passed on: helm's errors
/// quote templates, and the agent needs only the marks.
async fn lookup_render(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    binary: &Path,
    work: &Workspace,
    template: &TemplateRequest,
    rendered: Option<&str>,
    previous: &str,
) -> (Option<String>, Option<LookupNote>) {
    let templates =
        helm_cli::chart_templates(Some(binary), &template.chart, helm_cli::DEFAULT_TIMEOUT).await;
    let Ok(files) = templates else {
        return (None, Some(LookupNote::unknown()));
    };
    let used = kubach_core::runtime::blocking(move || lookup_use(&files)).await;
    if !used.is_used() {
        return (None, None);
    }
    // Only the namespace's ServiceAccount renders on the cluster: another
    // identity may read more than the session does.
    if ctx.session.mode_in(&template.namespace) == SessionMode::TemporaryServiceAccount
        && let Ok(text) = render_on_cluster(ctx, env, binary, work, template).await
    {
        return (Some(text), Some(LookupNote::read()));
    }
    let Some(text) = rendered.map(str::to_owned) else {
        return (None, Some(LookupNote::unavailable(true, Vec::new())));
    };
    let namespace = template.namespace.clone();
    let previous = previous.to_owned();
    let note = kubach_core::runtime::blocking(move || {
        let found = lookup_objects(&text, &used);
        // An object of such a template the render lost looks removed.
        let gone = lookup_objects(&previous, &used);
        let mut objects: Vec<String> = Vec::new();
        for object in found.objects.iter().chain(&gone.objects) {
            let label = object_label(object, &namespace);
            if !objects.contains(&label) {
                objects.push(label);
            }
        }
        LookupNote::unavailable(found.whole_render, objects)
    })
    .await;
    (None, Some(note))
}

/// What a preview from `helm template` compares.
struct TemplatePreview<'a> {
    template: &'a TemplateRequest,
    /// `helm template` without the cluster, or why there is none.
    rendered: Result<&'a str, RenderFailure>,
    /// What the scan of that render found.
    scan: &'a Scan,
    previous_manifest: &'a str,
    previous_label: &'a str,
    /// Manifests scanned besides the render.
    scanned: &'a [&'a str],
    /// `--create-namespace` of an install.
    create_namespace: bool,
    values: Option<(&'a str, &'a [Value])>,
}

/// What `preview: true` answers (#68): `helm template`, without asking
/// anyone and with no more than the session's own read rights (#173). The
/// server-side dry run is not for a preview: it reads the release storage,
/// which is Secrets, and in a client-only namespace runs with the user's
/// credentials.
async fn template_preview(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    binary: &Path,
    work: &Workspace,
    preview: &TemplatePreview<'_>,
) -> Result<ToolOutput, ToolError> {
    let namespace = &preview.template.namespace;
    let (live, lookup) = lookup_render(
        ctx,
        env,
        binary,
        work,
        preview.template,
        preview.rendered.ok(),
        preview.previous_manifest,
    )
    .await;
    let rendered = match (live.as_deref(), preview.rendered) {
        (Some(live), _) => live,
        (None, Ok(rendered)) => rendered,
        (None, Err(failure)) => {
            return Err(ToolError::Failed(format!(
                "helm template could not render the chart without the cluster: {}. \
                 {NO_HELM_TEXT} A chart that needs the cluster to render shows its objects in \
                 the server-side dry run, which runs when you call without preview and the \
                 user is asked",
                failure.text()
            )));
        }
    };
    // What lookup found may change what the chart renders.
    let (mut findings, mut cluster, mut elsewhere) = match &live {
        Some(live) => {
            let mut manifests = vec![live.as_str()];
            manifests.extend(preview.scanned);
            let scan = scan(ctx, namespace, &manifests).await?;
            (scan.findings, scan.cluster, scan.elsewhere)
        }
        None => (
            preview.scan.findings.clone(),
            preview.scan.cluster.clone(),
            Vec::new(),
        ),
    };
    for object in &preview.scan.elsewhere {
        if !elsewhere.contains(object) {
            elsewhere.push(object.clone());
        }
    }
    if preview.create_namespace {
        cluster.push(format!("Namespace {namespace} (cluster-scoped)"));
        findings.push(Finding {
            document: None,
            path: format!("Namespace {namespace}"),
            message: "created by --create-namespace if missing".into(),
        });
    }
    preview_output(
        ctx,
        &PreviewText {
            release: &preview.template.release,
            namespace,
            cluster: &cluster,
            findings: &findings,
            elsewhere: &elsewhere,
            old: (preview.previous_manifest, preview.previous_label),
            new: (rendered, "helm template"),
            values: preview.values,
            lookup: lookup.as_ref(),
        },
        if live.is_some() {
            TEMPLATE_READ_NOTE
        } else {
            TEMPLATE_NOTE
        },
    )
    .await
}

const DRY_RUN_NEXT: &str = "If you allow it, Kubach runs the server-side dry run with your \
                            credentials and shows its objects and diff in the next window; \
                            nothing changes until you approve that one.";

fn bullet_list(prefix: &str, objects: &[String]) -> String {
    objects
        .iter()
        .map(|object| format!("  {prefix} {object}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A changed release, with its audit record: every namespace the call
/// changed (#21), the archive it read and how the release compares with
/// the dry run (#174) and, for a call under the user's credentials, why
/// (#18).
fn changed_as(
    text: String,
    as_user: bool,
    findings: &[Finding],
    namespaces: Vec<String>,
    values_diff: Option<&str>,
    after: &AfterChange,
) -> ToolOutput {
    let mut audit = json!({ "namespaces": namespaces });
    after.audit(&mut audit);
    if let Some(diff) = values_diff {
        audit["valuesDiff"] = json!(diff);
    }
    if as_user {
        audit["ranAs"] = json!("user");
        audit["why"] = json!(USER_REASON);
        audit["objects"] = json!(findings.iter().map(|f| f.path.as_str()).collect::<Vec<_>>());
    }
    ToolOutput {
        text,
        confirmed: true,
        audit: Some(audit),
    }
}

/// The user's objects of a change: shown only when asked about (#18).
fn user_objects(as_user: bool, findings: &[Finding]) -> Vec<String> {
    if as_user {
        findings.iter().map(|f| f.path.clone()).collect()
    } else {
        Vec::new()
    }
}

pub(crate) async fn install(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    // On the heap: the call's state is large, and the caller's stack is an
    // async worker's.
    Box::pin(install_call(ctx, args)).await
}

async fn install_call(ctx: &Ctx<'_>, args: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
    let a: InstallArgs = parse(args)?;
    release_name(&a.release)?;
    // Inputs first: a bad chart or values never bothers the user.
    let env = env(ctx)?;
    let loaded = repositories(&env).await;
    let source = chart_source(&a.chart, &repository_names(&loaded))?;
    let helm_chart = repo::helm_chart_reference(&loaded, &a.chart);
    let values = a.values.unwrap_or_default();
    parse_values(&values)?;
    let allowed = authorize(
        ctx,
        &Request::HelmChange {
            namespace: a.namespace.clone(),
            release: a.release.clone(),
        },
    )
    .await?;
    let namespace = allowed.namespace.clone();
    if a.create_namespace && ctx.session.mode_in(&namespace) == SessionMode::TemporaryServiceAccount
    {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "namespace {namespace} exists already (the session's ServiceAccount lives in it); \
                 leave createNamespace out"
            ),
        )
        .into());
    }
    ensure_source(ctx, &source).await?;
    let binary = require_binary(&env)?;
    // The chart is fetched once: every step below reads this archive (#174).
    let work = Workspace::new()?;
    let pulled = work
        .fetch(&binary, &helm_chart, a.version.as_deref())
        .await?;
    let request = InstallRequest {
        release: a.release.clone(),
        chart: archive_argument(&pulled)?,
        version: None,
        values: Sensitive::new(values.clone()),
        create_namespace: a.create_namespace,
    };
    let base_change = HelmChange {
        action: HelmAction::Install,
        release: a.release.clone(),
        chart: a.chart.trim().to_owned(),
        version: a.version.clone(),
        revision: None,
        values: masked_values_yaml(&values),
        objects: Vec::new(),
        cluster_objects: Vec::new(),
        create_namespace: a.create_namespace,
        identity: String::new(),
        credentials: HelmCredentials::Session,
        user_objects: Vec::new(),
        namespaces: Vec::new(),
        values_diff: String::new(),
        diff_cut: None,
    };
    // What the chart renders without the cluster: its cluster-scoped
    // objects, RBAC and other namespaces are found before anything else
    // (#18), and a preview is this render (#173).
    let template = TemplateRequest {
        release: a.release.clone(),
        namespace: namespace.clone(),
        chart: pulled.chart(),
        values: request.values.clone(),
        is_upgrade: false,
        working_dir: Some(work.cwd()),
    };
    let rendered = rendered(&binary, &template).await;
    let render_failure = rendered.as_ref().err().map(RenderFailure::of);
    let rendered = rendered.ok();
    let scan = scan(ctx, &namespace, &[rendered.as_deref().unwrap_or_default()]).await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere, true).await?;
    let mode = ctx.session.mode_in(&namespace);
    let as_user = needs_user_credentials(mode, &scan.findings);
    if a.preview {
        return Box::pin(template_preview(
            ctx,
            &env,
            &binary,
            &work,
            &TemplatePreview {
                template: &template,
                rendered: rendered
                    .as_deref()
                    .ok_or(render_failure.unwrap_or(RenderFailure::Other)),
                scan: &scan,
                previous_manifest: "",
                previous_label: "nothing installed",
                scanned: &[],
                create_namespace: a.create_namespace,
                values: None,
            },
        ))
        .await;
    }
    // The dry run waits for a yes where it runs with the user's own
    // credentials: allowed for this call (#18), or the only ones there are
    // in a client-only namespace (#173).
    let asked_first = as_user || mode == SessionMode::ClientOnly;
    if asked_first {
        let objects = rendered_objects(rendered.as_deref(), &namespace).await;
        let details = first_window(&objects, render_failure);
        let change = HelmChange {
            objects,
            namespaces: other_namespaces(&scan.elsewhere),
            ..base_change.clone()
        };
        if as_user {
            ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
        } else {
            ask_dry_run(ctx, &allowed, change, details).await?;
        }
    }
    still_fetched(Some(&pulled)).await?;
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    // The server-side dry run, under the namespace's identity or, allowed
    // above, the user's. What it fails on is not for the agent (#173).
    let preview_release = with_helm_role(ctx, &namespace, &[], as_user, async {
        match helm_cli::install(
            Some(&binary),
            &run.target,
            &request,
            true,
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(|error| dry_run_error(error, render_failure))?
        {
            UpgradeOutcome::Preview(release) => Ok(release),
            UpgradeOutcome::Applied(_) => Err(ToolError::Failed("helm did not dry-run".into())),
        }
    })
    .await?;
    let identity = run.identity.clone();
    // Nothing of the run stays on disk while the user decides.
    drop(run);
    let mut preview = preview_from(ctx, &preview_release, &namespace, as_user).await?;
    // The dry run may show more than helm template did (#21).
    authorize_elsewhere(ctx, &allowed, &a.release, &preview.elsewhere, false).await?;
    let others = other_namespaces(scan.elsewhere.iter().chain(&preview.elsewhere));
    if a.create_namespace {
        preview
            .cluster
            .push(format!("Namespace {namespace} (cluster-scoped)"));
        preview.findings.push(Finding {
            document: None,
            path: format!("Namespace {namespace}"),
            message: "created by --create-namespace if missing".into(),
        });
    }
    let change = HelmChange {
        version: a
            .version
            .clone()
            .or_else(|| Some(preview_release.chart.version.clone())),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        identity,
        credentials: if asked_first {
            HelmCredentials::User
        } else {
            HelmCredentials::Session
        },
        user_objects: user_objects(as_user, &scan.findings),
        namespaces: others.clone(),
        ..base_change
    };
    let details = format!(
        "Objects of the server-side dry run:\n{}",
        bullet_list("+", &preview.objects)
    );
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    still_fetched(Some(&pulled)).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let output = with_helm_role(ctx, &namespace, &preview.kinds, as_user, async {
        helm_cli::install_watched(
            Some(&binary),
            &run.target,
            &request,
            false,
            helm_cli::DEFAULT_TIMEOUT,
            Some(watch(ctx, &phases)),
        )
        .await
        .map_err(run_error)
    })
    .await?;
    drop(run);
    let output = match output {
        UpgradeOutcome::Applied(output) => output,
        UpgradeOutcome::Preview(_) => String::new(),
    };
    let release = release_after(ctx, &namespace, &a.release).await?;
    let after = AfterChange::compare(
        Some(&pulled),
        &preview_release,
        release.as_ref(),
        &namespace,
    )
    .await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Install,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            None,
            &after,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
        &after,
    ))
}

/// How an upgrade changes the release's values (#22).
#[derive(Debug)]
pub(crate) enum ValuesEdit {
    /// Neither `values` nor `valuesPatch`: the release keeps its own.
    Keep,
    /// `values`: they replace the release's.
    Replace(String),
    /// `valuesPatch`: merged into the release's by Kubach.
    Patch(Value),
}

impl ValuesEdit {
    fn is_patch(&self) -> bool {
        matches!(self, Self::Patch(_))
    }
}

/// Checks the values arguments of an upgrade before anything is asked:
/// `values` and `valuesPatch` together are an input error (#22).
pub(crate) fn values_edit(values: Option<&str>, patch: Option<&str>) -> Result<ValuesEdit, Denial> {
    match (values, patch) {
        (Some(_), Some(_)) => Err(Denial::new(
            DenialCode::InvalidInput,
            "pass either values (they replace the release's values) or valuesPatch (only the \
             changes, merged into the release's values), not both",
        )),
        (Some(values), None) => {
            parse_values(values)?;
            Ok(ValuesEdit::Replace(values.to_owned()))
        }
        (None, Some(patch)) => parse_values(patch)
            .map(ValuesEdit::Patch)
            .map_err(|denial| {
                Denial::new(
                    denial.code,
                    denial.message.replacen("values", "valuesPatch", 1),
                )
            }),
        (None, None) => Ok(ValuesEdit::Keep),
    }
}

/// An upgrade's values: what helm gets, what the dialog shows (masked) and
/// the masked diff against the current revision.
pub(crate) struct UpgradeValues {
    pub yaml: Sensitive<String>,
    pub masked: String,
    /// Whole, for the confirmation window (#137).
    pub diff: String,
    /// [`changed_values`], for a preview's summary (#115).
    pub changes: Vec<Value>,
}

/// The values of an upgrade from `edit` and the current revision, merged,
/// masked and diffed on the blocking pool (#22).
pub(crate) fn upgrade_values_now(
    edit: ValuesEdit,
    current: &Value,
    revision: u32,
) -> UpgradeValues {
    let (new, yaml) = match edit {
        ValuesEdit::Keep => (current.clone(), None),
        // The agent's text goes to helm as it is.
        ValuesEdit::Replace(text) => (
            parse_values(&text).unwrap_or_else(|_| Value::Object(Map::new())),
            Some(text),
        ),
        ValuesEdit::Patch(patch) => {
            let mut merged = current.clone();
            merge_values(&mut merged, &patch);
            (merged, None)
        }
    };
    let yaml = yaml.unwrap_or_else(|| kubach_core::helm::values_to_yaml(&new));
    let mut masked = new.clone();
    mask_values(&mut masked);
    UpgradeValues {
        masked: kubach_core::helm::values_to_yaml(&masked),
        diff: masked_values_diff(
            current,
            &new,
            &format!("values of revision {revision}"),
            "values after the upgrade",
        ),
        changes: changed_values(current, &new),
        yaml: Sensitive::new(yaml),
    }
}

async fn upgrade_values(edit: ValuesEdit, current: &Release) -> UpgradeValues {
    let values = current.user_values.expose().clone();
    let revision = current.revision;
    kubach_core::runtime::blocking(move || upgrade_values_now(edit, &values, revision)).await
}

pub(crate) async fn upgrade(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    // On the heap: the call's state is large, and the caller's stack is an
    // async worker's.
    Box::pin(upgrade_call(ctx, args)).await
}

async fn upgrade_call(ctx: &Ctx<'_>, args: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
    let a: UpgradeArgs = parse(args)?;
    release_name(&a.release)?;
    // Inputs first: a bad chart or values never bothers the user.
    let env = env(ctx)?;
    let (source, helm_chart) = match &a.chart {
        Some(chart) => {
            let loaded = repositories(&env).await;
            (
                Some(chart_source(chart, &repository_names(&loaded))?),
                Some(repo::helm_chart_reference(&loaded, chart)),
            )
        }
        None => (None, None),
    };
    let edit = values_edit(a.values.as_deref(), a.values_patch.as_deref())?;
    let allowed = authorize(
        ctx,
        &Request::HelmChange {
            namespace: a.namespace.clone(),
            release: a.release.clone(),
        },
    )
    .await?;
    let namespace = allowed.namespace.clone();
    if let Some(source) = &source {
        ensure_source(ctx, source).await?;
    }
    let binary = require_binary(&env)?;
    // A chart reference is fetched once: every step below reads this
    // archive (#174). The chart kept in the release comes from its storage.
    let work = Workspace::new()?;
    let pulled = match &helm_chart {
        Some(chart) => Some(work.fetch(&binary, chart, a.version.as_deref()).await?),
        None => None,
    };
    // The release's storage is read under the namespace's identity.
    let (current, upgrade_chart) = with_helm_role(ctx, &namespace, &[], false, async {
        let releases = Releases::new(ctx.session.client_in(&namespace)?);
        let current = releases
            .get(&namespace, &a.release, None)
            .await
            .map_err(store_error)?;
        let chart = match &pulled {
            Some(pulled) => pulled.chart(),
            None => UpgradeChart::Stored(
                releases
                    .stored_chart(&namespace, &a.release, current.revision)
                    .await
                    .map_err(store_error)?,
            ),
        };
        Ok((current, chart))
    })
    .await?;
    let patched = edit.is_patch();
    let values = upgrade_values(edit, &current).await;
    let request = UpgradeRequest {
        release: a.release.clone(),
        chart: upgrade_chart,
        values: values.yaml,
    };
    let chart = a.chart.clone().unwrap_or_else(|| {
        format!(
            "the chart kept in the release ({})",
            current.chart.reference()
        )
    });
    let base_change = HelmChange {
        action: HelmAction::Upgrade,
        release: a.release.clone(),
        chart,
        version: a.version.clone(),
        revision: None,
        values: values.masked,
        objects: Vec::new(),
        cluster_objects: Vec::new(),
        create_namespace: false,
        identity: String::new(),
        credentials: HelmCredentials::Session,
        user_objects: Vec::new(),
        namespaces: Vec::new(),
        values_diff: values.diff.clone(),
        diff_cut: None,
    };
    // What the new chart renders without the cluster and what the current
    // revision holds (an upgrade deletes what the chart no longer has)
    // (#18); a preview is this render (#173).
    let template = TemplateRequest {
        release: a.release.clone(),
        namespace: namespace.clone(),
        chart: request.chart.clone(),
        values: request.values.clone(),
        is_upgrade: true,
        working_dir: Some(work.cwd()),
    };
    let rendered = rendered(&binary, &template).await;
    let render_failure = rendered.as_ref().err().map(RenderFailure::of);
    let rendered = rendered.ok();
    let current_manifest = release_manifest(&current);
    let previous_label = format!("revision {}", current.revision);
    // The agent, a preview and the audit log get the values diff bounded.
    let agent_values_diff = truncate(&values.diff, MAX_AGENT_DIFF_BYTES).into_owned();
    let preview_values = Some((agent_values_diff.as_str(), values.changes.as_slice()));
    let scan = scan(
        ctx,
        &namespace,
        &[rendered.as_deref().unwrap_or_default(), &current_manifest],
    )
    .await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere, true).await?;
    let mode = ctx.session.mode_in(&namespace);
    let as_user = needs_user_credentials(mode, &scan.findings);
    if a.preview {
        return Box::pin(template_preview(
            ctx,
            &env,
            &binary,
            &work,
            &TemplatePreview {
                template: &template,
                rendered: rendered
                    .as_deref()
                    .ok_or(render_failure.unwrap_or(RenderFailure::Other)),
                scan: &scan,
                // `helm template` prints hooks: the revision's count too.
                previous_manifest: &current_manifest,
                previous_label: &previous_label,
                scanned: &[&current_manifest],
                create_namespace: false,
                values: preview_values,
            },
        ))
        .await;
    }
    // As for an install: the dry run waits for a yes where it runs with
    // the user's own credentials (#18, #173).
    let asked_first = as_user || mode == SessionMode::ClientOnly;
    if asked_first {
        let objects = rendered_objects(rendered.as_deref(), &namespace).await;
        let details = first_window(&objects, render_failure);
        let change = HelmChange {
            objects,
            namespaces: other_namespaces(&scan.elsewhere),
            ..base_change.clone()
        };
        if as_user {
            ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
        } else {
            ask_dry_run(ctx, &allowed, change, details).await?;
        }
    }
    still_fetched(pulled.as_ref()).await?;
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    // What the dry run fails on is not for the agent (#173).
    let preview_release = with_helm_role(ctx, &namespace, &[], as_user, async {
        match helm_cli::upgrade(
            Some(&binary),
            &run.target,
            &request,
            true,
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(|error| dry_run_error(error, render_failure))?
        {
            UpgradeOutcome::Preview(release) => Ok(release),
            UpgradeOutcome::Applied(_) => Err(ToolError::Failed("helm did not dry-run".into())),
        }
    })
    .await?;
    let identity = run.identity.clone();
    // Nothing of the run stays on disk while the user decides.
    drop(run);
    let preview = preview_from(ctx, &preview_release, &namespace, as_user).await?;
    // The dry run may show more than helm template did (#21).
    authorize_elsewhere(ctx, &allowed, &a.release, &preview.elsewhere, false).await?;
    let others = other_namespaces(scan.elsewhere.iter().chain(&preview.elsewhere));
    let change = HelmChange {
        version: Some(preview_release.chart.version.clone()),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        identity,
        credentials: if asked_first {
            HelmCredentials::User
        } else {
            HelmCredentials::Session
        },
        user_objects: user_objects(as_user, &scan.findings),
        namespaces: others.clone(),
        ..base_change
    };
    let diff = masked_diff(
        current.manifest.expose(),
        preview_release.manifest.expose(),
        &format!("revision {}", current.revision),
        "after the upgrade",
    )
    .await;
    let change = HelmChange {
        diff_cut: diff.cut,
        ..change
    };
    let details = if diff.text.trim().is_empty() {
        "The dry run changes no object.".to_owned()
    } else {
        diff.text
    };
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    still_fetched(pulled.as_ref()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let output = with_helm_role(ctx, &namespace, &preview.kinds, as_user, async {
        helm_cli::upgrade_watched(
            Some(&binary),
            &run.target,
            &request,
            false,
            helm_cli::DEFAULT_TIMEOUT,
            Some(watch(ctx, &phases)),
        )
        .await
        .map_err(run_error)
    })
    .await?;
    drop(run);
    let output = match output {
        UpgradeOutcome::Applied(output) => output,
        UpgradeOutcome::Preview(_) => String::new(),
    };
    let release = release_after(ctx, &namespace, &a.release).await?;
    let after = AfterChange::compare(
        pulled.as_ref(),
        &preview_release,
        release.as_ref(),
        &namespace,
    )
    .await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Upgrade,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            patched.then_some(agent_values_diff.as_str()),
            &after,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        patched.then_some(agent_values_diff.as_str()),
        &after,
    ))
}

pub(crate) async fn rollback(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    // On the heap: the call's state is large, and the caller's stack is an
    // async worker's.
    Box::pin(rollback_call(ctx, args)).await
}

async fn rollback_call(ctx: &Ctx<'_>, args: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
    let a: RollbackArgs = parse(args)?;
    release_name(&a.release)?;
    let allowed = authorize(
        ctx,
        &Request::HelmChange {
            namespace: a.namespace.clone(),
            release: a.release.clone(),
        },
    )
    .await?;
    let namespace = allowed.namespace.clone();
    let env = env(ctx)?;
    let binary = require_binary(&env)?;
    let (current, target) = with_helm_role(ctx, &namespace, &[], false, async {
        let releases = Releases::new(ctx.session.client_in(&namespace)?);
        let current = releases
            .get(&namespace, &a.release, None)
            .await
            .map_err(store_error)?;
        let target = releases
            .get(&namespace, &a.release, Some(a.revision))
            .await
            .map_err(store_error)?;
        Ok((current, target))
    })
    .await?;
    // The revision rolled back to, and the current one: a rollback deletes
    // what the target no longer has (#18).
    let scan = scan(
        ctx,
        &namespace,
        &[&release_manifest(&target), &release_manifest(&current)],
    )
    .await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere, true).await?;
    let others = other_namespaces(&scan.elsewhere);
    let as_user = needs_user_credentials(ctx.session.mode_in(&namespace), &scan.findings);
    let preview = preview_from(ctx, &target, &namespace, as_user).await?;
    let diff = masked_diff(
        current.manifest.expose(),
        target.manifest.expose(),
        &format!("revision {}", current.revision),
        &format!("revision {}", target.revision),
    )
    .await;
    let identity = if as_user {
        USER_IDENTITY.to_owned()
    } else {
        ctx.session.describe_identity(Some(&namespace))
    };
    let change = HelmChange {
        action: HelmAction::Rollback,
        release: a.release.clone(),
        chart: target.chart.reference(),
        version: Some(target.chart.version.clone()),
        revision: Some(a.revision),
        values: {
            let mut values = target.user_values.expose().clone();
            mask_values(&mut values);
            kubach_core::helm::values_to_yaml(&values)
        },
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        create_namespace: false,
        identity,
        credentials: HelmCredentials::Session,
        user_objects: Vec::new(),
        namespaces: others.clone(),
        values_diff: String::new(),
        diff_cut: diff.cut,
    };
    let details = if diff.text.trim().is_empty() {
        "The rollback changes no object.".to_owned()
    } else {
        diff.text
    };
    // Nothing runs before this window: one question covers both the
    // user's credentials and the rollback.
    if as_user {
        ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
    } else {
        approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    }
    // A fresh token: the user may have taken minutes to answer. helm runs
    // in an empty private directory (#174).
    let work = Workspace::new()?;
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let command = HelmCommand::rollback(&run.target, &a.release, a.revision).with_progress();
    let output = with_helm_role(ctx, &namespace, &preview.kinds, as_user, async {
        helm_cli::run_watched(
            Some(&binary),
            &command,
            helm_cli::DEFAULT_TIMEOUT,
            watch(ctx, &phases),
        )
        .await
        .map_err(run_error)
    })
    .await?;
    drop(run);
    // The revision helm wrote against the one the user saw (#174).
    let release = release_after(ctx, &namespace, &a.release).await?;
    let after = AfterChange::compare(None, &target, release.as_ref(), &namespace).await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Rollback,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            None,
            &after,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
        &after,
    ))
}

pub(crate) async fn uninstall(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    // On the heap: the call's state is large, and the caller's stack is an
    // async worker's.
    Box::pin(uninstall_call(ctx, args)).await
}

async fn uninstall_call(ctx: &Ctx<'_>, args: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
    let a: UninstallArgs = parse(args)?;
    release_name(&a.release)?;
    let allowed = authorize(
        ctx,
        &Request::HelmChange {
            namespace: a.namespace.clone(),
            release: a.release.clone(),
        },
    )
    .await?;
    let namespace = allowed.namespace.clone();
    let env = env(ctx)?;
    let binary = require_binary(&env)?;
    let current = with_helm_role(ctx, &namespace, &[], false, async {
        Releases::new(ctx.session.client_in(&namespace)?)
            .get(&namespace, &a.release, None)
            .await
            .map_err(store_error)
    })
    .await?;
    // Uninstalling deletes the revision's cluster-scoped objects too (#18).
    let scan = scan(ctx, &namespace, &[&release_manifest(&current)]).await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere, true).await?;
    let others = other_namespaces(&scan.elsewhere);
    let as_user = needs_user_credentials(ctx.session.mode_in(&namespace), &scan.findings);
    let preview = preview_from(ctx, &current, &namespace, as_user).await?;
    let identity = if as_user {
        USER_IDENTITY.to_owned()
    } else {
        ctx.session.describe_identity(Some(&namespace))
    };
    let change = HelmChange {
        action: HelmAction::Uninstall,
        release: a.release.clone(),
        chart: current.chart.reference(),
        version: Some(current.chart.version.clone()),
        revision: None,
        values: String::new(),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        create_namespace: false,
        identity,
        credentials: HelmCredentials::Session,
        user_objects: Vec::new(),
        namespaces: others.clone(),
        values_diff: String::new(),
        diff_cut: None,
    };
    let mut details = String::from("Objects deleted with the release:\n");
    for object in &preview.objects {
        let _ = writeln!(details, "  - {object}");
    }
    if a.keep_history {
        details.push_str("Its revisions stay (--keep-history).\n");
    }
    if as_user {
        ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
    } else {
        approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    }
    // A fresh token: the user may have taken minutes to answer. helm runs
    // in an empty private directory (#174).
    let work = Workspace::new()?;
    let run = helm_run(ctx, &env, &work, &namespace, as_user).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let command = HelmCommand::uninstall(&run.target, &a.release, a.keep_history).with_progress();
    let output = with_helm_role(ctx, &namespace, &preview.kinds, as_user, async {
        helm_cli::run_watched(
            Some(&binary),
            &command,
            helm_cli::DEFAULT_TIMEOUT,
            watch(ctx, &phases),
        )
        .await
        .map_err(run_error)
    })
    .await?;
    drop(run);
    let after = AfterChange::default();
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Uninstall,
            None,
            &preview,
            &phases,
            &output,
            None,
            &after,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
        &after,
    ))
}

#[cfg(test)]
mod tests;
