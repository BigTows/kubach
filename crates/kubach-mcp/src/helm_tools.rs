//! Helm tools for the agent (#418, ADR 0005 "Helm через агента"), run with
//! the same `helm` binary as Kubach's Helm screens (ADR 0006).
//!
//! Reading: charts of the repositories added in Kubach (and of an `oci://`
//! reference the user allowed), a chart's default values, the releases of a
//! namespace and a release's history. Changing: install, upgrade, rollback
//! and uninstall, each `parse → policy (ceiling, read(ns), modify(ns)) →
//! chart source → server-side dry run → the user approves in Kubach → helm
//! runs with live phases → result`.
//!
//! Boundaries:
//! - the namespace comes from the session: `--namespace` is forced, every
//!   object of the dry run must land in it, `--create-namespace` only for a
//!   namespace the session may change;
//! - helm acts under the namespace's identity: a temporary ServiceAccount's
//!   short-lived token through `HELM_KUBETOKEN` (its Role widened for helm
//!   only while the call runs), the administrator's identity through
//!   `HELM_KUBEASUSER`, or the user's kubeconfig in a client-only
//!   namespace — never the user's credentials where the session has
//!   another identity;
//! - cluster-scoped objects of a chart (CRDs, ClusterRoles, webhooks, PVs,
//!   StorageClasses…) are refused unless the user ticks them explicitly in
//!   the dialog, and always under a ServiceAccount;
//! - charts only from repositories already added in Kubach, or an `oci://`
//!   repository the user allows for the session; never a path or URL;
//! - values only as YAML the agent passes (never a file path), masked by
//!   key name in the dialog, the audit log and answers.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::rbac::v1::PolicyRule;
use kubach_core::discovery::ApiCatalog;
use kubach_core::helm::cli::{
    self as helm_cli, HelmCliError, HelmCommand, HelmTarget, HelmWatch, InstallRequest, SecretEnv,
    ShowKind, UpgradeChart, UpgradeOutcome, UpgradeRequest,
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

/// Values with sensitive keys (`password`, `token`, `auth`…) masked as
/// `*** (N bytes)`, at any depth.
pub(crate) fn mask_values(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive_name(key) && !value.is_object() && !value.is_array() {
                    let size = match &*value {
                        Value::String(text) => text.len(),
                        Value::Null => 0,
                        other => other.to_string().len(),
                    };
                    *value = Value::String(format!("*** ({size} bytes)"));
                } else {
                    mask_values(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_values),
        Value::String(text) => {
            if let std::borrow::Cow::Owned(masked) = mask_text(text) {
                *text = masked;
            }
        }
        _ => {}
    }
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
}

/// Checks the objects a chart renders: each must land in `namespace`;
/// cluster-scoped objects and RBAC inside the namespace are findings the
/// user ticks, and refused outright under a temporary ServiceAccount,
/// whose namespaced Role cannot create them anyway.
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
        let shown = if cluster_scoped {
            format!("{} {} (cluster-scoped)", resource.kind, resource.name)
        } else {
            format!("{} {}", resource.kind, resource.name)
        };
        if !cluster_scoped
            && let Some(other) = resource.namespace.as_deref()
            && other != namespace
        {
            return Err(Denial::new(
                DenialCode::Namespace,
                format!(
                    "the chart puts {} {} into namespace {other}; this call may only touch \
                     {namespace}. Change the chart's values so everything goes into the \
                     release namespace",
                    resource.kind, resource.name
                ),
            ));
        }
        if cluster_scoped {
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::ClusterScoped,
                    format!(
                        "the chart creates the cluster-scoped {} {}; in this namespace helm runs \
                         as the session's ServiceAccount, which cannot create cluster-scoped \
                         objects. Turn it off in the values (CRDs: crds.enabled/installCRDs \
                         false) or ask the user to install it from Kubach's Helm screen",
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
        } else if matches!(resource.kind.as_str(), "Role" | "RoleBinding")
            && group == "rbac.authorization.k8s.io"
        {
            if mode == SessionMode::TemporaryServiceAccount {
                return Err(Denial::new(
                    DenialCode::ForbiddenKind,
                    format!(
                        "the chart creates {} {}; the session's ServiceAccount cannot grant \
                         RBAC rights. Turn it off in the values (rbac.create: false) or ask the \
                         user to install it from Kubach's Helm screen",
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
        if !cluster_scoped && let Some(kind) = served {
            let key = (kind.api.group.clone(), kind.api.plural.clone());
            if !preview.kinds.contains(&key) {
                preview.kinds.push(key);
            }
        }
        preview.objects.push(shown);
    }
    Ok(preview)
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

async fn helm_run(ctx: &Ctx<'_>, env: &HelmEnv, namespace: &str) -> Result<HelmRun, ToolError> {
    let access = ctx.session.helm_access(namespace).await?;
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
    let identity = ctx.session.describe_identity(Some(namespace));
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
/// it back afterwards whatever happened.
async fn with_helm_role<T>(
    ctx: &Ctx<'_>,
    namespace: &str,
    kinds: &[(String, String)],
    work: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    if ctx.session.mode_in(namespace) != SessionMode::TemporaryServiceAccount {
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

/// Manifests diffed with Secret values and sensitive env masked.
fn masked_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let diff = unified_diff(
        &masked_manifest(old),
        &masked_manifest(new),
        old_label,
        new_label,
    );
    truncate(&diff, 64 * 1024).into_owned()
}

/// What the agent is told after a change: status, revision, objects and
/// NOTES (masked, bounded).
fn result_text(
    action: HelmAction,
    release: Option<&Release>,
    preview: &Preview,
    phases: &[String],
    output: &str,
) -> String {
    let mut out = json!({
        "action": action,
        "objects": preview.objects,
        "phases": phases,
    });
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

fn preview_from(ctx: &Ctx<'_>, release: &Release, namespace: &str) -> Result<Preview, ToolError> {
    let mut manifest = release.manifest.expose().clone();
    for hook in &release.hooks {
        manifest.push_str("\n---\n");
        manifest.push_str(hook.manifest.expose());
    }
    check_objects(
        &manifest_resources(&manifest),
        namespace,
        ctx.session.catalog(),
        ctx.session.mode_in(namespace),
    )
    .map_err(ToolError::from)
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
    let run = helm_run(ctx, &env, &namespace).await?;
    let request = InstallRequest {
        release: a.release.clone(),
        chart: a.chart.trim().to_owned(),
        version: a.version.clone(),
        values: Sensitive::new(values.clone()),
        create_namespace: a.create_namespace,
    };
    // The server-side dry run, under the namespace's identity.
    let preview_release = with_helm_role(ctx, &namespace, &[], async {
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
    let mut preview = preview_from(ctx, &preview_release, &namespace)?;
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
        action: HelmAction::Install,
        release: a.release.clone(),
        chart: a.chart.trim().to_owned(),
        version: a
            .version
            .clone()
            .or_else(|| Some(preview_release.chart.version.clone())),
        revision: None,
        values: masked_values_yaml(&values),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        create_namespace: a.create_namespace,
        identity: run.identity.clone(),
    };
    let details = format!(
        "Objects of the server-side dry run:\n{}",
        preview
            .objects
            .iter()
            .map(|object| format!("  + {object}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let output = with_helm_role(ctx, &namespace, &preview.kinds, async {
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
    Ok(changed(result_text(
        HelmAction::Install,
        release.as_ref(),
        &preview,
        &phases,
        &output,
    )))
}

fn changed(text: String) -> ToolOutput {
    ToolOutput {
        text,
        confirmed: true,
        audit: None,
    }
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
    if let Some(values) = &a.values {
        parse_values(values)?;
    }
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
    let run = helm_run(ctx, &env, &namespace).await?;
    let (current, request) = with_helm_role(ctx, &namespace, &[], async {
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
        // Without new values the release keeps its own.
        let values = match &a.values {
            Some(values) => values.clone(),
            None => kubach_core::helm::values_to_yaml(current.user_values.expose()),
        };
        Ok((
            current,
            UpgradeRequest {
                release: a.release.clone(),
                chart,
                values: Sensitive::new(values),
            },
        ))
    })
    .await?;
    let preview_release = with_helm_role(ctx, &namespace, &[], async {
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
    let preview = preview_from(ctx, &preview_release, &namespace)?;
    let diff = masked_diff(
        current.manifest.expose(),
        preview_release.manifest.expose(),
        &format!("revision {}", current.revision),
        "after the upgrade",
    );
    let chart = a.chart.clone().unwrap_or_else(|| {
        format!(
            "the chart kept in the release ({})",
            current.chart.reference()
        )
    });
    let change = HelmChange {
        action: HelmAction::Upgrade,
        release: a.release.clone(),
        chart,
        version: Some(preview_release.chart.version.clone()),
        revision: None,
        values: masked_values_yaml(request.values.expose()),
        objects: preview.objects.clone(),
        cluster_objects: preview.cluster.clone(),
        create_namespace: false,
        identity: run.identity.clone(),
    };
    let details = if diff.trim().is_empty() {
        "The dry run changes no object.".to_owned()
    } else {
        diff
    };
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let output = with_helm_role(ctx, &namespace, &preview.kinds, async {
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
    Ok(changed(result_text(
        HelmAction::Upgrade,
        release.as_ref(),
        &preview,
        &phases,
        &output,
    )))
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
    let run = helm_run(ctx, &env, &namespace).await?;
    let (current, target) = with_helm_role(ctx, &namespace, &[], async {
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
    let preview = preview_from(ctx, &target, &namespace)?;
    let diff = masked_diff(
        current.manifest.expose(),
        target.manifest.expose(),
        &format!("revision {}", current.revision),
        &format!("revision {}", target.revision),
    );
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
        identity: run.identity.clone(),
    };
    let details = if diff.trim().is_empty() {
        "The rollback changes no object.".to_owned()
    } else {
        diff
    };
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let command = HelmCommand::rollback(&run.target, &a.release, a.revision).with_progress();
    let output = with_helm_role(ctx, &namespace, &preview.kinds, async {
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
    Ok(changed(result_text(
        HelmAction::Rollback,
        release.as_ref(),
        &preview,
        &phases,
        &output,
    )))
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
    let run = helm_run(ctx, &env, &namespace).await?;
    let current = with_helm_role(ctx, &namespace, &[], async {
        Releases::new(ctx.session.client_in(&namespace)?)
            .get(&namespace, &a.release, None)
            .await
            .map_err(store_error)
    })
    .await?;
    let preview = preview_from(ctx, &current, &namespace)?;
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
        identity: run.identity.clone(),
    };
    let mut details = String::from("Objects deleted with the release:\n");
    for object in &preview.objects {
        let _ = writeln!(details, "  - {object}");
    }
    if a.keep_history {
        details.push_str("Its revisions stay (--keep-history).\n");
    }
    approve(ctx, &allowed, change, details, preview.findings.clone()).await?;
    // A fresh token: the user may have taken minutes to answer.
    let run = helm_run(ctx, &env, &namespace).await?;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let command = HelmCommand::uninstall(&run.target, &a.release, a.keep_history).with_progress();
    let output = with_helm_role(ctx, &namespace, &preview.kinds, async {
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
    Ok(changed(result_text(
        HelmAction::Uninstall,
        None,
        &preview,
        &phases,
        &output,
    )))
}

#[cfg(test)]
mod tests;
