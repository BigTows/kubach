//! Helm tools for the agent (#418, ADR 0005 "Helm через агента"), run with
//! the same `helm` binary as Kubach's Helm screens (ADR 0006).
//!
//! Reading: charts of the repositories added in Kubach (and of an `oci://`
//! reference the user allowed), a chart's default values, the releases of a
//! namespace and a release's history. Changing: install, upgrade, rollback
//! and uninstall, each `parse → policy (access, read(ns), modify(ns)) →
//! chart source → server-side dry run → the user approves in Kubach → helm
//! runs with live phases → result`.
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
//!   Without the tick nothing runs and the agent is told which objects;
//! - charts only from repositories already added in Kubach, or an `oci://`
//!   repository the user allows for the session; never a path or URL;
//! - values only as YAML the agent passes (never a file path), masked by
//!   key name in the dialog, the audit log and answers;
//! - a release's current values never reach the agent whole (#22):
//!   `helm_get_values` returns them masked by key name and by what looks
//!   like a key or token, and `helm_upgrade` with `valuesPatch` merges the
//!   patch in Kubach and answers with the masked diff of the values only.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::rbac::v1::PolicyRule;
use kubach_core::discovery::ApiCatalog;
use kubach_core::helm::cli::{
    self as helm_cli, HelmCliError, HelmCommand, HelmTarget, HelmWatch, InstallRequest, SecretEnv,
    ShowKind, TemplateRequest, UpgradeChart, UpgradeOutcome, UpgradeRequest,
};
use kubach_core::helm::repo::{self, HelmPaths, IndexState, SearchOptions};
use kubach_core::helm::{
    Driver, HelmError, ManifestResource, Release, Releases, Sensitive, manifest_resources, oci,
    unified_diff,
};
use kubach_core::secrets::mask::is_sensitive_name;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::confirm::{ConfirmationDecision, ConfirmationRequest, RequestKind, ask};
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

/// What the session knows about the user's `helm`: the binary Kubach uses
/// (settings, `PATH` or downloaded), the kubeconfig files of the context
/// for client-only namespaces (`None`: the context is not in a file, such
/// as a vault context), and where the repositories are.
#[derive(Clone, Debug, Default)]
pub struct HelmEnv {
    pub binary: Option<PathBuf>,
    pub kubeconfig: Option<Vec<PathBuf>>,
    pub paths: Option<HelmPaths>,
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

/// Values masked at any depth: every scalar under a sensitive key
/// (`password`, `token`, `auth`…; in a list under one too) and every string
/// that [`looks_like_secret`] becomes `*** (N bytes)`; other strings go
/// through text masking.
pub(crate) fn mask_values(value: &mut Value) {
    mask_values_at(value, "", false, &mut Vec::new());
}

/// [`mask_values`], returning the paths it masked (`auth.password`,
/// `tokens[0]`), for `helm_get_values`.
pub(crate) fn mask_values_listed(value: &mut Value) -> Vec<String> {
    let mut masked = Vec::new();
    mask_values_at(value, "", false, &mut masked);
    masked
}

fn mask_values_at(value: &mut Value, path: &str, sensitive: bool, masked: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                mask_values_at(child, &child_path, is_sensitive_name(key), masked);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                mask_values_at(item, &format!("{path}[{index}]"), sensitive, masked);
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
        _ if sensitive => {
            *value = values_placeholder(value);
            masked.push(path.to_owned());
        }
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
/// otherwise look the same on both sides: `*** (N bytes, changed)`.
fn mark_changed(old: &Value, new: &Value, old_masked: &Value, new_masked: &mut Value) {
    match (old, new, old_masked, new_masked) {
        (Value::Object(old), Value::Object(new), Value::Object(old_m), Value::Object(new_m)) => {
            for (key, masked) in new_m.iter_mut() {
                if let (Some(o), Some(n), Some(om)) = (old.get(key), new.get(key), old_m.get(key)) {
                    mark_changed(o, n, om, masked);
                }
            }
        }
        (Value::Array(old), Value::Array(new), Value::Array(old_m), Value::Array(new_m))
            if old.len() == new.len() =>
        {
            for (index, masked) in new_m.iter_mut().enumerate() {
                mark_changed(&old[index], &new[index], &old_m[index], masked);
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
/// nothing changes. What the dialog shows and the agent gets: never the
/// values themselves.
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
    let diff = unified_diff(
        &kubach_core::helm::values_to_yaml(&old_masked),
        &kubach_core::helm::values_to_yaml(&new_masked),
        old_label,
        new_label,
    );
    truncate(&diff, 64 * 1024).into_owned()
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
                    format!(
                        "the chart puts {} {} into {other:?}, which is not a namespace name",
                        resource.kind, resource.name
                    ),
                ));
            }
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::Namespace,
                    format!(
                        "the server-side dry run shows {} {} in namespace {other}, which helm \
                         template did not render; in {namespace} helm runs as the session's \
                         ServiceAccount, which cannot change another namespace. Change the \
                         values so it goes into {namespace}, or ask the user to install it from \
                         Kubach's Helm screen",
                        resource.kind, resource.name
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
                    format!(
                        "the server-side dry run shows the cluster-scoped {} {}, which helm \
                         template did not render; in this namespace helm runs as the session's \
                         ServiceAccount, which cannot create cluster-scoped objects. Turn it off \
                         in the values (CRDs: crds.enabled/installCRDs false) or ask the user to \
                         install it from Kubach's Helm screen",
                        resource.kind, resource.name
                    ),
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
                    format!(
                        "the server-side dry run shows {} {}, which helm template did not \
                         render; the session's ServiceAccount cannot grant RBAC rights. Turn it \
                         off in the values (rbac.create: false) or ask the user to install it \
                         from Kubach's Helm screen",
                        resource.kind, resource.name
                    ),
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
    let Some(paths) = env.paths.clone() else {
        return Vec::new();
    };
    kubach_core::runtime::blocking(move || repo::load(&paths, &[]).unwrap_or_default()).await
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

/// A `helm` call's connection: the namespace's identity (#418).
struct HelmRun {
    target: HelmTarget,
    identity: String,
    /// Holds a generated kubeconfig (cluster only) while helm runs.
    _dir: Option<tempfile::TempDir>,
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
             your own credentials here, and Kubach does not write them to disk); a vault context \
             works with Helm only where the session has a ServiceAccount"
                .into(),
        )
    };
    let identity = if as_user {
        USER_IDENTITY.to_owned()
    } else {
        ctx.session.describe_identity(Some(namespace))
    };
    let (context, kubeconfig, dir) = match access {
        HelmAccess::Token { kubeconfig, token } => {
            let dir = tempfile::Builder::new()
                .prefix("kubach-helm-")
                .tempdir()
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            let path = dir.path().join("kubeconfig");
            write_private(&path, &kubeconfig)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            set(&mut vars, "HELM_KUBETOKEN", token);
            ("kubach".to_owned(), vec![path], Some(dir))
        }
        HelmAccess::Impersonate { user, groups } => {
            set(&mut vars, "HELM_KUBEASUSER", user);
            if !groups.is_empty() {
                set(&mut vars, "HELM_KUBEASGROUPS", groups.join(","));
            }
            let files = env.kubeconfig.clone().ok_or_else(no_file)?;
            (ctx.session.scope().context.clone(), files, None)
        }
        HelmAccess::User => {
            let files = env.kubeconfig.clone().ok_or_else(no_file)?;
            (ctx.session.scope().context.clone(), files, None)
        }
    };
    Ok(HelmRun {
        target: HelmTarget {
            context,
            kubeconfig,
            namespace: namespace.to_owned(),
            driver: Driver::Secret,
            secret_env: SecretEnv::new(vars),
        },
        identity,
        _dir: dir,
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
    if as_user || ctx.session.mode_in(namespace) != SessionMode::TemporaryServiceAccount {
        return work.await;
    }
    ctx.session
        .widen_role(namespace, helm_rules(kinds))
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
        .ok_or_else(|| helm_error(HelmCliError::NotInstalled))
}

/// Manifests diffed with Secret values and sensitive env masked. Parsed
/// on the blocking pool: the deep schemas of a chart's CRDs overflowed the
/// async worker's stack on upgrade (#21), as in [`preview_from`].
async fn masked_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let (old, new) = (old.to_owned(), new.to_owned());
    let (old_label, new_label) = (old_label.to_owned(), new_label.to_owned());
    kubach_core::runtime::blocking(move || {
        let diff = unified_diff(
            &masked_manifest(&old),
            &masked_manifest(&new),
            &old_label,
            &new_label,
        );
        truncate(&diff, 64 * 1024).into_owned()
    })
    .await
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
) -> String {
    let mut out = json!({
        "action": action,
        "objects": preview.objects,
        "phases": phases,
    });
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
    let loaded = repositories(&env).await;
    let hits = repo::search(
        &loaded,
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
    let out = json!({
        "charts": rows,
        "more": hits.len() > MAX_SEARCH_ROWS,
        "repositories": repositories,
        "note": if loaded.is_empty() {
            "No repositories are added in Kubach. Only the user adds them (Kubach → Helm → \
             Charts → Repositories); or pass oci to look at an oci:// chart, which the user is \
             asked to allow."
        } else {
            "Charts of the repositories added in Kubach. An oci:// chart can be looked at with \
             oci; the user is asked to allow its registry."
        },
    });
    Ok(ToolOutput::read(
        serde_json::to_string_pretty(&out).unwrap_or_default(),
    ))
}

pub(crate) async fn show_values(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: ShowValuesArgs = parse(args)?;
    let env = env(ctx)?;
    let source = chart_source(&a.chart, &repository_names(&repositories(&env).await))?;
    authorize(ctx, &Request::HelmCharts).await?;
    ensure_source(ctx, &source).await?;
    let binary = require_binary(&env)?;
    let command = HelmCommand::show(ShowKind::Values, &a.chart, a.version.as_deref());
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
                     and Kubach does not read them with other credentials. With access to \
                     Secrets for the session (list_secrets asks the user for it) it can; \
                     otherwise ask the user to look at the values in Kubach's Helm screen",
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
    };
    let decision = ask(ctx.confirmer, &request).await;
    if !ctx.session.is_active() {
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended while waiting for the user",
        )
        .into());
    }
    match decision {
        ConfirmationDecision::Approved | ConfirmationDecision::ApprovedAlways => Ok(()),
        ConfirmationDecision::Denied { reason } | ConfirmationDecision::Rejected { reason } => {
            Err(ToolError::Rejected(reason))
        }
        ConfirmationDecision::Provided { .. } => Err(ToolError::Rejected(Some(
            "unexpected answer to a confirmation".into(),
        ))),
    }
}

/// The latest revision after a change, under the namespace's identity.
async fn current_release(ctx: &Ctx<'_>, namespace: &str, release: &str) -> Option<Release> {
    let client = ctx.session.client_in(namespace).ok()?;
    Releases::new(client)
        .get(namespace, release, None)
        .await
        .ok()
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
/// anything changes; a refusal names the objects. The call's namespace
/// for the audit log stays the release's.
async fn authorize_elsewhere(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    release: &str,
    elsewhere: &[Elsewhere],
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
        let message = elsewhere_refusal(&other, &allowed.namespace, &objects, &error.message());
        return Err(match error {
            ToolError::Denied(denial) => Denial::new(denial.code, message).into(),
            ToolError::Rejected(_) => ToolError::Rejected(Some(message)),
            ToolError::Api { code, .. } => ToolError::Api { code, message },
            ToolError::Failed(_) => ToolError::Failed(message),
        });
    }
    Ok(())
}

/// `Kind name` of each object `helm template` rendered, with the
/// namespace of one outside `namespace`, parsed on the blocking pool.
async fn rendered_objects(rendered: Option<&str>, namespace: &str) -> Vec<String> {
    let text = rendered.unwrap_or_default().to_owned();
    let namespace = namespace.to_owned();
    kubach_core::runtime::blocking(move || {
        manifest_resources(&text)
            .iter()
            .map(|r| match r.namespace.as_deref() {
                Some(other) if other != namespace => {
                    format!("{} {} (namespace {other})", r.kind, r.name)
                }
                _ => format!("{} {}", r.kind, r.name),
            })
            .collect()
    })
    .await
}

/// The chart as `helm template` renders it on the client (#18), under a
/// temporary ServiceAccount only. `None` when it does not render there (a
/// chart that needs the cluster's capabilities): the server-side dry run
/// under the ServiceAccount then still refuses what it cannot create.
async fn rendered(ctx: &Ctx<'_>, binary: &Path, request: &TemplateRequest) -> Option<String> {
    if ctx.session.mode_in(&request.namespace) != SessionMode::TemporaryServiceAccount {
        return None;
    }
    helm_cli::template(Some(binary), request, helm_cli::DEFAULT_TIMEOUT)
        .await
        .ok()
}

/// Asks the user to run this one call under their own kubeconfig
/// credentials (#18); `findings` are why. A no, or no answer, is a refusal
/// that names the objects; a yes is recorded for the audit log.
async fn ask_user_credentials(
    ctx: &Ctx<'_>,
    env: &HelmEnv,
    allowed: &Allowed,
    mut change: HelmChange,
    details: String,
    findings: &[Finding],
) -> Result<(), ToolError> {
    let objects: Vec<String> = findings.iter().map(|f| f.path.clone()).collect();
    if env.kubeconfig.is_none() {
        return Err(Denial::new(
            DenialCode::ClusterScoped,
            format!(
                "the namespace's temporary ServiceAccount cannot create or delete: {}; running \
                 helm with the user's credentials needs this context in a kubeconfig file, and \
                 it is not (a vault context). Turn these off in the values or ask the user to do \
                 it from Kubach's Helm screen",
                objects.join(", ")
            ),
        )
        .into());
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

/// What the next step does after a yes to the user's credentials.
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
/// changed (#21) and, for a call under the user's credentials, why (#18).
fn changed_as(
    text: String,
    as_user: bool,
    findings: &[Finding],
    namespaces: Vec<String>,
    values_diff: Option<&str>,
) -> ToolOutput {
    let mut audit = json!({ "namespaces": namespaces });
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
    let a: InstallArgs = parse(args)?;
    release_name(&a.release)?;
    // Inputs first: a bad chart or values never bothers the user.
    let env = env(ctx)?;
    let source = chart_source(&a.chart, &repository_names(&repositories(&env).await))?;
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
    let request = InstallRequest {
        release: a.release.clone(),
        chart: a.chart.trim().to_owned(),
        version: a.version.clone(),
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
    };
    // Under a temporary ServiceAccount, the chart's cluster-scoped objects
    // and RBAC are found on the client first (#18).
    let template = TemplateRequest {
        release: a.release.clone(),
        namespace: namespace.clone(),
        chart: UpgradeChart::Reference {
            chart: request.chart.clone(),
            version: a.version.clone(),
        },
        values: request.values.clone(),
        is_upgrade: false,
    };
    let rendered = rendered(ctx, &binary, &template).await;
    let scan = scan(ctx, &namespace, &[rendered.as_deref().unwrap_or_default()]).await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere).await?;
    let as_user = needs_user_credentials(ctx.session.mode_in(&namespace), &scan.findings);
    if as_user {
        let objects = rendered_objects(rendered.as_deref(), &namespace).await;
        let details = format!(
            "Objects of helm template (rendered without the cluster):\n{}\n\n{DRY_RUN_NEXT}",
            bullet_list("+", &objects)
        );
        let change = HelmChange {
            objects,
            namespaces: other_namespaces(&scan.elsewhere),
            ..base_change.clone()
        };
        ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
    }
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
    // The server-side dry run, under the namespace's identity or, allowed
    // above, the user's.
    let preview_release = with_helm_role(ctx, &namespace, &[], as_user, async {
        match helm_cli::install(
            Some(&binary),
            &run.target,
            &request,
            true,
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(helm_error)?
        {
            UpgradeOutcome::Preview(release) => Ok(release),
            UpgradeOutcome::Applied(_) => Err(ToolError::Failed("helm did not dry-run".into())),
        }
    })
    .await?;
    let mut preview = preview_from(ctx, &preview_release, &namespace, as_user).await?;
    // The dry run may show more than helm template did (#21).
    authorize_elsewhere(ctx, &allowed, &a.release, &preview.elsewhere).await?;
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
        identity: run.identity.clone(),
        credentials: if as_user {
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
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
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
        .map_err(helm_error)
    })
    .await?;
    let output = match output {
        UpgradeOutcome::Applied(output) => output,
        UpgradeOutcome::Preview(_) => String::new(),
    };
    let release = current_release(ctx, &namespace, &a.release).await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Install,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            None,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
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
    pub diff: String,
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
    let a: UpgradeArgs = parse(args)?;
    release_name(&a.release)?;
    // Inputs first: a bad chart or values never bothers the user.
    let env = env(ctx)?;
    let source = match &a.chart {
        Some(chart) => Some(chart_source(
            chart,
            &repository_names(&repositories(&env).await),
        )?),
        None => None,
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
    // The release's storage is read under the namespace's identity.
    let (current, upgrade_chart) = with_helm_role(ctx, &namespace, &[], false, async {
        let releases = Releases::new(ctx.session.client_in(&namespace)?);
        let current = releases
            .get(&namespace, &a.release, None)
            .await
            .map_err(store_error)?;
        let chart = match &a.chart {
            Some(chart) => UpgradeChart::Reference {
                chart: chart.trim().to_owned(),
                version: a.version.clone(),
            },
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
    };
    // Under a temporary ServiceAccount: what the new chart renders on the
    // client and what the current revision holds (an upgrade deletes what
    // the chart no longer has) (#18).
    let template = TemplateRequest {
        release: a.release.clone(),
        namespace: namespace.clone(),
        chart: request.chart.clone(),
        values: request.values.clone(),
        is_upgrade: true,
    };
    let rendered = rendered(ctx, &binary, &template).await;
    let current_manifest = release_manifest(&current);
    let scan = scan(
        ctx,
        &namespace,
        &[rendered.as_deref().unwrap_or_default(), &current_manifest],
    )
    .await?;
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere).await?;
    let as_user = needs_user_credentials(ctx.session.mode_in(&namespace), &scan.findings);
    if as_user {
        let objects = rendered_objects(rendered.as_deref(), &namespace).await;
        let details = format!(
            "Objects of helm template (rendered without the cluster):\n{}\n\n{DRY_RUN_NEXT}",
            bullet_list("+", &objects)
        );
        let change = HelmChange {
            objects,
            namespaces: other_namespaces(&scan.elsewhere),
            ..base_change.clone()
        };
        ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
    }
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
    let preview_release = with_helm_role(ctx, &namespace, &[], as_user, async {
        match helm_cli::upgrade(
            Some(&binary),
            &run.target,
            &request,
            true,
            helm_cli::DEFAULT_TIMEOUT,
        )
        .await
        .map_err(helm_error)?
        {
            UpgradeOutcome::Preview(release) => Ok(release),
            UpgradeOutcome::Applied(_) => Err(ToolError::Failed("helm did not dry-run".into())),
        }
    })
    .await?;
    let preview = preview_from(ctx, &preview_release, &namespace, as_user).await?;
    // The dry run may show more than helm template did (#21).
    authorize_elsewhere(ctx, &allowed, &a.release, &preview.elsewhere).await?;
    let others = other_namespaces(scan.elsewhere.iter().chain(&preview.elsewhere));
    let diff = masked_diff(
        current.manifest.expose(),
        preview_release.manifest.expose(),
        &format!("revision {}", current.revision),
        "after the upgrade",
    )
    .await;
    let change = HelmChange {
        version: Some(preview_release.chart.version.clone()),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        identity: run.identity.clone(),
        credentials: if as_user {
            HelmCredentials::User
        } else {
            HelmCredentials::Session
        },
        user_objects: user_objects(as_user, &scan.findings),
        namespaces: others.clone(),
        ..base_change
    };
    let details = if diff.trim().is_empty() {
        "The dry run changes no object.".to_owned()
    } else {
        diff
    };
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
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
        .map_err(helm_error)
    })
    .await?;
    let output = match output {
        UpgradeOutcome::Applied(output) => output,
        UpgradeOutcome::Preview(_) => String::new(),
    };
    let release = current_release(ctx, &namespace, &a.release).await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Upgrade,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            patched.then_some(values.diff.as_str()),
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        patched.then_some(values.diff.as_str()),
    ))
}

pub(crate) async fn rollback(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
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
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere).await?;
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
    };
    let details = if diff.trim().is_empty() {
        "The rollback changes no object.".to_owned()
    } else {
        diff
    };
    // Nothing runs before this window: one question covers both the
    // user's credentials and the rollback.
    if as_user {
        ask_user_credentials(ctx, &env, &allowed, change, details, &scan.findings).await?;
    } else {
        approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    }
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
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
        .map_err(helm_error)
    })
    .await?;
    let release = current_release(ctx, &namespace, &a.release).await;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Rollback,
            release.as_ref(),
            &preview,
            &phases,
            &output,
            None,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
    ))
}

pub(crate) async fn uninstall(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
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
    authorize_elsewhere(ctx, &allowed, &a.release, &scan.elsewhere).await?;
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
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace, as_user).await?;
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
        .map_err(helm_error)
    })
    .await?;
    let phases = sync::lock(&phases).clone();
    Ok(changed_as(
        result_text(
            HelmAction::Uninstall,
            None,
            &preview,
            &phases,
            &output,
            None,
        ),
        as_user,
        &scan.findings,
        touched(&namespace, &others),
        None,
    ))
}

#[cfg(test)]
mod tests;
