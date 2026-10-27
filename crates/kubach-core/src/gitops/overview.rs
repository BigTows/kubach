//! GitOps overview: every Argo CD `Application`, Flux `Kustomization`,
//! `HelmRelease` and Flux source of the cluster, reduced to one status row
//! each, plus what the details show (revision, errors, managed resources).
//!
//! Sync and health come from the CRD plugins (`plugin::builtin::gitops`), so
//! the overview and the resource browser agree. On top of them, [`Attention`]
//! classifies an object as fine, busy or a problem, which the "problems only"
//! filter and the sort order use.

use kube::api::DynamicObject;
use serde_json::Value;

use crate::discovery::{ApiCatalog, ResourceKind};
use crate::plugin::builtin::gitops::{argo, conditions, flux, is_suspended, ready_health, s};
use crate::plugin::{ConditionInfo, Health, HealthStatus};

const ARGO_GROUP: &str = "argoproj.io";
const KUSTOMIZE_GROUP: &str = "kustomize.toolkit.fluxcd.io";
const HELM_GROUP: &str = "helm.toolkit.fluxcd.io";
const SOURCE_GROUP: &str = "source.toolkit.fluxcd.io";

/// The tool an object belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    ArgoCd,
    Flux,
}

impl Tool {
    pub fn title(self) -> &'static str {
        match self {
            Tool::ArgoCd => "Argo CD",
            Tool::Flux => "Flux",
        }
    }
}

/// A kind shown on the overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitOpsKind {
    ArgoApplication,
    FluxKustomization,
    FluxHelmRelease,
    GitRepository,
    OciRepository,
    HelmRepository,
}

impl GitOpsKind {
    /// In display order.
    pub const ALL: [GitOpsKind; 6] = [
        GitOpsKind::ArgoApplication,
        GitOpsKind::FluxKustomization,
        GitOpsKind::FluxHelmRelease,
        GitOpsKind::GitRepository,
        GitOpsKind::OciRepository,
        GitOpsKind::HelmRepository,
    ];

    pub fn group(self) -> &'static str {
        match self {
            GitOpsKind::ArgoApplication => ARGO_GROUP,
            GitOpsKind::FluxKustomization => KUSTOMIZE_GROUP,
            GitOpsKind::FluxHelmRelease => HELM_GROUP,
            GitOpsKind::GitRepository | GitOpsKind::OciRepository | GitOpsKind::HelmRepository => {
                SOURCE_GROUP
            }
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            GitOpsKind::ArgoApplication => "Application",
            GitOpsKind::FluxKustomization => "Kustomization",
            GitOpsKind::FluxHelmRelease => "HelmRelease",
            GitOpsKind::GitRepository => "GitRepository",
            GitOpsKind::OciRepository => "OCIRepository",
            GitOpsKind::HelmRepository => "HelmRepository",
        }
    }

    pub fn tool(self) -> Tool {
        match self {
            GitOpsKind::ArgoApplication => Tool::ArgoCd,
            _ => Tool::Flux,
        }
    }

    /// Flux sources: they deliver artifacts, nothing is deployed from them
    /// directly.
    pub fn is_source(self) -> bool {
        self.group() == SOURCE_GROUP
    }

    pub fn find(group: &str, kind: &str) -> Option<GitOpsKind> {
        Self::ALL
            .into_iter()
            .find(|k| k.group() == group && k.kind() == kind)
    }
}

/// The overview kinds the cluster serves, in display order. Empty when
/// neither Argo CD nor Flux is installed: the screen is hidden then.
pub fn served(catalog: &ApiCatalog) -> Vec<(GitOpsKind, ResourceKind)> {
    GitOpsKind::ALL
        .into_iter()
        .filter_map(|kind| {
            catalog
                .find(kind.group(), kind.kind())
                .map(|served| (kind, served.clone()))
        })
        .collect()
}

/// How much an object needs looking at, least urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    /// Synced / reconciled and healthy.
    Ok,
    /// No status yet (no controller, or not reconciled yet).
    Unknown,
    /// Suspended by the user: reconciliation is off.
    Suspended,
    /// A sync or reconciliation is running, or a rollout is progressing.
    Pending,
    /// Drifted from Git (OutOfSync) or a resource is missing.
    Warning,
    /// The sync or reconciliation failed, or the app is degraded.
    Error,
}

impl Attention {
    /// Shown by the "problems only" filter.
    pub fn is_problem(self) -> bool {
        matches!(self, Attention::Warning | Attention::Error)
    }

    pub fn title(self) -> &'static str {
        match self {
            Attention::Ok => "OK",
            Attention::Unknown => "Unknown",
            Attention::Suspended => "Suspended",
            Attention::Pending => "In progress",
            Attention::Warning => "Warning",
            Attention::Error => "Error",
        }
    }
}

/// An object the details link to, e.g. a managed resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLink {
    /// API group, `""` for the core group.
    pub group: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    /// What the GitOps tool says about it, e.g. `Synced · Healthy`.
    pub status: Option<String>,
    pub attention: Attention,
}

impl ResourceLink {
    fn new(group: &str, kind: &str, namespace: Option<String>, name: &str) -> Self {
        Self {
            group: group.to_owned(),
            kind: kind.to_owned(),
            namespace: namespace.filter(|ns| !ns.is_empty()),
            name: name.to_owned(),
            status: None,
            attention: Attention::Ok,
        }
    }

    /// `Kind ns/name` or `Kind name`.
    pub fn label(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{} {ns}/{}", self.kind, self.name),
            None => format!("{} {}", self.kind, self.name),
        }
    }
}

/// One GitOps object as the overview shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOpsItem {
    pub kind: GitOpsKind,
    pub namespace: Option<String>,
    pub name: String,
    /// Argo CD sync status, or Flux `Ready` (reconciled).
    pub sync: Health,
    /// Argo CD health, Flux Kustomization `Healthy`, HelmRelease `Released`.
    /// `None` when the object has no such notion (sources) or no condition.
    pub health: Option<Health>,
    pub attention: Attention,
    pub suspended: bool,
    /// Last synced / applied revision (or the chart version of a release).
    pub revision: String,
    /// Revision last tried when it differs from the applied one (Flux).
    pub attempted_revision: Option<String>,
    /// Where it comes from: repo URL and path, chart, source URL.
    pub source: String,
    /// Where it goes: Argo CD destination, Flux target namespace, source ref.
    pub target: String,
    /// When the status last changed (Argo CD `reconciledAt`, Flux `Ready`
    /// transition).
    pub updated: Option<String>,
    /// Why it is not fine: sync errors, failed conditions, operation message.
    pub errors: Vec<String>,
    pub conditions: Vec<ConditionInfo>,
    /// The Flux source it reads from.
    pub source_ref: Option<ResourceLink>,
    /// Resources it manages (Argo CD `status.resources`, Flux inventory).
    pub managed: Vec<ResourceLink>,
}

impl GitOpsItem {
    pub fn from_object(kind: GitOpsKind, obj: &DynamicObject) -> Self {
        let mut item = Self {
            kind,
            namespace: obj.metadata.namespace.clone(),
            name: obj.metadata.name.clone().unwrap_or_default(),
            sync: Health::unknown(),
            health: None,
            attention: Attention::Unknown,
            suspended: is_suspended(obj),
            revision: String::new(),
            attempted_revision: None,
            source: String::new(),
            target: String::new(),
            updated: None,
            errors: Vec::new(),
            conditions: conditions(obj),
            source_ref: None,
            managed: Vec::new(),
        };
        match kind {
            GitOpsKind::ArgoApplication => item.fill_argo(obj),
            GitOpsKind::FluxKustomization => item.fill_kustomization(obj),
            GitOpsKind::FluxHelmRelease => item.fill_helm_release(obj),
            GitOpsKind::GitRepository | GitOpsKind::OciRepository | GitOpsKind::HelmRepository => {
                item.fill_source(obj)
            }
        }
        if obj.metadata.deletion_timestamp.is_some() {
            item.attention = Attention::Pending;
        }
        item
    }

    /// Unique row key: `group/Kind/namespace/name`.
    pub fn key(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.kind.group(),
            self.kind.kind(),
            self.namespace.as_deref().unwrap_or_default(),
            self.name
        )
    }

    /// `namespace/name`, or the name of a cluster-scoped object.
    pub fn display_name(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}/{}", self.name),
            None => self.name.clone(),
        }
    }

    pub fn is_problem(&self) -> bool {
        self.attention.is_problem()
    }

    /// Short sync label for a badge: Argo CD's sync status, or the Flux
    /// `Ready` state in the words of `flux get`.
    pub fn sync_label(&self) -> &str {
        if self.kind.tool() == Tool::ArgoCd {
            return &self.sync.message;
        }
        match self.sync.status {
            HealthStatus::Healthy => "Ready",
            HealthStatus::Progressing => "Reconciling",
            HealthStatus::Degraded => "Not ready",
            HealthStatus::Failed => "Stalled",
            HealthStatus::Unknown if self.suspended => "Suspended",
            HealthStatus::Unknown => "Unknown",
        }
    }

    fn push_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        let message = message.trim();
        if !message.is_empty() && !self.errors.iter().any(|e| e == message) {
            self.errors.push(message.to_owned());
        }
    }

    fn fill_argo(&mut self, obj: &DynamicObject) {
        let spec = obj.data.get("spec");
        self.sync = argo::sync_health(obj);
        let health = argo::app_health(obj);
        let health_state = s(obj, "/status/health/status");
        self.revision = match obj
            .data
            .pointer("/status/sync/revisions")
            .and_then(Value::as_array)
        {
            Some(list) if !list.is_empty() => list
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            _ => s(obj, "/status/sync/revision").to_owned(),
        };
        let repo = argo::first_and_more(spec, |m| argo::field(m, "repoURL"));
        let path = argo::first_and_more(spec, argo::source_path);
        let target_revision = argo::first_and_more(spec, argo::target_revision);
        self.source = [repo, path, target_revision]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        self.target = argo::destination(spec);
        self.updated = non_empty(s(obj, "/status/reconciledAt"));
        self.suspended = health_state == "Suspended";

        let mut attention = match self.sync.message.as_str() {
            "Synced" => Attention::Ok,
            "OutOfSync" => Attention::Warning,
            "Syncing" => Attention::Pending,
            _ => Attention::Unknown,
        };
        attention = attention.max(match health_state {
            "Healthy" => Attention::Ok,
            "Progressing" => Attention::Pending,
            "Suspended" => Attention::Suspended,
            "Missing" => Attention::Warning,
            "Degraded" => Attention::Error,
            _ => Attention::Unknown,
        });
        // Error conditions (SyncError, ComparisonError, …) and warnings
        // (OrphanedResourceWarning, …); Argo CD conditions have no status.
        for c in self.conditions.clone() {
            let text = c.message.clone().unwrap_or_else(|| c.type_.clone());
            if c.type_.ends_with("Error") {
                attention = Attention::Error;
                self.push_error(format!("{}: {text}", c.type_));
            } else if c.type_.ends_with("Warning") {
                attention = attention.max(Attention::Warning);
                self.push_error(format!("{}: {text}", c.type_));
            }
        }
        let phase = s(obj, "/status/operationState/phase");
        if matches!(phase, "Failed" | "Error") {
            attention = Attention::Error;
            let message = s(obj, "/status/operationState/message");
            self.push_error(format!("Sync {}: {message}", phase.to_lowercase()));
            for r in obj
                .data
                .pointer("/status/operationState/syncResult/resources")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
                if matches!(g("status"), "SyncFailed" | "PruneSkipped") && !g("message").is_empty()
                {
                    let link = ResourceLink::new(
                        g("group"),
                        g("kind"),
                        non_empty(g("namespace")),
                        g("name"),
                    );
                    self.push_error(format!("{}: {}", link.label(), g("message")));
                }
            }
        }
        if matches!(health.status, HealthStatus::Degraded | HealthStatus::Failed)
            && !health.message.is_empty()
            && health.message != health_state
        {
            self.push_error(health.message.clone());
        }
        self.health = Some(health);
        self.attention = attention;

        self.managed = argo::managed(obj)
            .into_iter()
            .map(|r| {
                let mut link = ResourceLink::new(r.group, r.kind, non_empty(r.namespace), r.name);
                let parts: Vec<&str> = [r.sync, r.health]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect();
                link.status = (!parts.is_empty()).then(|| parts.join(" · "));
                link.attention = match (r.sync, r.health) {
                    (_, "Degraded") => Attention::Error,
                    ("OutOfSync", _) | (_, "Missing") => Attention::Warning,
                    (_, "Progressing") => Attention::Pending,
                    (_, "Suspended") => Attention::Suspended,
                    _ => Attention::Ok,
                };
                link
            })
            .collect();
    }

    /// Sync and attention from the kstatus `Ready` condition, plus failure
    /// messages of the conditions (Flux).
    fn fill_flux_status(&mut self, obj: &DynamicObject) {
        self.sync = ready_health(obj);
        let mut attention = match self.sync.status {
            HealthStatus::Healthy => Attention::Ok,
            HealthStatus::Progressing => Attention::Pending,
            HealthStatus::Degraded | HealthStatus::Failed => Attention::Error,
            HealthStatus::Unknown if self.suspended => Attention::Suspended,
            HealthStatus::Unknown => Attention::Unknown,
        };
        if self.suspended && attention == Attention::Ok {
            attention = Attention::Suspended;
        }
        for c in self.conditions.clone() {
            // kstatus: `Stalled` and `…Failed` / `…Unavailable` are
            // abnormal when true, the others (`Ready`, `Healthy`, …) when false.
            let t = c.type_.as_str();
            let failed = if t == "Stalled" || t.ends_with("Failed") || t.ends_with("Unavailable") {
                c.status == "True"
            } else {
                t != "Reconciling" && c.status == "False"
            };
            if failed {
                let text = c
                    .message
                    .clone()
                    .or_else(|| c.reason.clone())
                    .unwrap_or_default();
                self.push_error(format!("{}: {text}", c.type_));
            }
        }
        self.attention = attention;
        self.updated = obj
            .data
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|c| c.get("type").and_then(Value::as_str) == Some("Ready"))
            .and_then(|c| c.get("lastTransitionTime"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    /// Health from a positive condition (`Healthy`, `Released`), `None`
    /// when the object does not have it.
    fn condition_health(&mut self, type_: &str) {
        let Some(c) = self.conditions.iter().find(|c| c.type_ == type_) else {
            return;
        };
        let text = c
            .message
            .clone()
            .or_else(|| c.reason.clone())
            .unwrap_or_else(|| c.type_.clone());
        let health = match c.status.as_str() {
            "True" => Health::new(HealthStatus::Healthy, text),
            "False" => Health::new(HealthStatus::Degraded, text),
            _ => Health::new(HealthStatus::Progressing, text),
        };
        if health.status == HealthStatus::Degraded && !self.suspended {
            self.attention = Attention::Error;
        }
        self.health = Some(health);
    }

    fn fill_kustomization(&mut self, obj: &DynamicObject) {
        self.fill_flux_status(obj);
        self.condition_health("Healthy");
        self.revision = s(obj, "/status/lastAppliedRevision").to_owned();
        self.attempted_revision = non_empty(s(obj, "/status/lastAttemptedRevision"))
            .filter(|attempted| *attempted != self.revision);
        let source = flux::ref_label(obj.data.pointer("/spec/sourceRef"));
        self.source = [source.as_str(), s(obj, "/spec/path")]
            .into_iter()
            .filter(|p| !p.is_empty() && *p != "/")
            .collect::<Vec<_>>()
            .join(" · ");
        self.target = s(obj, "/spec/targetNamespace").to_owned();
        self.source_ref = source_link(obj, obj.data.pointer("/spec/sourceRef"));
        self.managed = flux::inventory(obj)
            .into_iter()
            .map(|e| {
                let group = e
                    .api_version
                    .rsplit_once('/')
                    .map(|(g, _)| g)
                    .unwrap_or_default()
                    .to_owned();
                ResourceLink::new(&group, &e.kind, e.namespace, &e.name)
            })
            .collect();
    }

    fn fill_helm_release(&mut self, obj: &DynamicObject) {
        self.fill_flux_status(obj);
        self.condition_health("Released");
        let latest = obj.data.pointer("/status/history/0");
        let g = |k: &str| {
            latest
                .and_then(|h| h.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        self.revision = match (g("chartName"), g("chartVersion")) {
            ("", _) | (_, "") => flux::release_revision(obj),
            (name, version) => format!("{name}@{version}"),
        };
        self.attempted_revision = non_empty(s(obj, "/status/lastAttemptedRevision"))
            .filter(|attempted| !self.revision.ends_with(&format!("@{attempted}")))
            .filter(|attempted| *attempted != self.revision);
        self.source = flux::chart(obj);
        self.target = s(obj, "/spec/targetNamespace").to_owned();
        let chart_source = obj
            .data
            .pointer("/spec/chart/spec/sourceRef")
            .or_else(|| obj.data.pointer("/spec/chartRef"));
        self.source_ref = source_link(obj, chart_source);
    }

    fn fill_source(&mut self, obj: &DynamicObject) {
        self.fill_flux_status(obj);
        self.revision = s(obj, "/status/artifact/revision").to_owned();
        self.source = s(obj, "/spec/url").to_owned();
        self.target = flux::source_ref(obj);
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// A Flux `sourceRef` / `chartRef`; the namespace defaults to the object's.
fn source_link(obj: &DynamicObject, r: Option<&Value>) -> Option<ResourceLink> {
    let r = r?;
    let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
    if g("kind").is_empty() || g("name").is_empty() {
        return None;
    }
    let group = match g("apiVersion").rsplit_once('/') {
        Some((group, _)) => group,
        None => SOURCE_GROUP,
    };
    let namespace = non_empty(g("namespace")).or_else(|| obj.metadata.namespace.clone());
    Some(ResourceLink::new(group, g("kind"), namespace, g("name")))
}

/// Overview rows of the watched objects: the most urgent first, then by
/// kind, namespace and name.
pub fn collect<'a>(
    objects: impl IntoIterator<Item = (GitOpsKind, &'a DynamicObject)>,
) -> Vec<GitOpsItem> {
    let mut items: Vec<GitOpsItem> = objects
        .into_iter()
        .map(|(kind, obj)| GitOpsItem::from_object(kind, obj))
        .collect();
    items.sort_by(|a, b| {
        b.attention
            .cmp(&a.attention)
            .then(a.kind.cmp(&b.kind))
            .then(a.namespace.cmp(&b.namespace))
            .then(a.name.cmp(&b.name))
    });
    items
}

/// Revision for a table cell: commit hashes cut to 7 characters, as Git
/// prints them (`53e28ff…` → `53e28ff`, `main@sha1:6f1d2c3b…` →
/// `main@sha1:6f1d2c3`). Versions and tags stay as they are.
pub fn short_revision(revision: &str) -> String {
    const SHORT: usize = 7;
    let is_hash = |s: &str| s.len() > SHORT && s.chars().all(|c| c.is_ascii_hexdigit());
    revision
        .split(", ")
        .map(|part| match part.rsplit_once(':') {
            Some((prefix, hash)) if prefix.ends_with("sha1") || prefix.ends_with("sha256") => {
                if is_hash(hash) {
                    format!("{prefix}:{}", &hash[..SHORT])
                } else {
                    part.to_owned()
                }
            }
            _ if part.len() >= 40 && is_hash(part) => part[..SHORT].to_owned(),
            _ => part.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Counts for the summary line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub errors: usize,
    pub warnings: usize,
    pub pending: usize,
    pub suspended: usize,
    pub ok: usize,
}

impl Summary {
    pub fn problems(&self) -> usize {
        self.errors + self.warnings
    }
}

pub fn summarize(items: &[GitOpsItem]) -> Summary {
    let mut summary = Summary {
        total: items.len(),
        ..Summary::default()
    };
    for item in items {
        match item.attention {
            Attention::Error => summary.errors += 1,
            Attention::Warning => summary.warnings += 1,
            Attention::Pending => summary.pending += 1,
            Attention::Suspended => summary.suspended += 1,
            Attention::Ok => summary.ok += 1,
            Attention::Unknown => {}
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::yaml;

    /// Objects of a multi-document fixture, by name.
    fn fixture(text: &str, name: &str) -> DynamicObject {
        let docs = yaml::parse(text).expect("fixture parses");
        let value = docs
            .into_iter()
            .map(|m| m.value)
            .find(|v| v.pointer("/metadata/name").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("no fixture {name}"));
        serde_json::from_value(value).unwrap()
    }

    const ARGO: &str = include_str!("fixtures/argo.yaml");
    const FLUX: &str = include_str!("fixtures/flux.yaml");

    fn argo(name: &str) -> GitOpsItem {
        GitOpsItem::from_object(GitOpsKind::ArgoApplication, &fixture(ARGO, name))
    }

    fn flux(kind: GitOpsKind, name: &str) -> GitOpsItem {
        GitOpsItem::from_object(kind, &fixture(FLUX, name))
    }

    #[test]
    fn argo_synced_and_healthy() {
        let item = argo("guestbook");
        assert_eq!(item.attention, Attention::Ok);
        assert_eq!(item.sync.message, "Synced");
        assert_eq!(item.sync_label(), "Synced");
        assert_eq!(item.health.as_ref().unwrap().status, HealthStatus::Healthy);
        assert_eq!(item.revision, "53e28ff20cc530b9ada2173fbbd64d48338583ba");
        assert_eq!(
            item.source,
            "https://github.com/argoproj/argocd-example-apps.git · guestbook · HEAD"
        );
        assert_eq!(item.target, "in-cluster/guestbook");
        assert_eq!(item.updated.as_deref(), Some("2026-10-20T09:12:41Z"));
        assert!(item.errors.is_empty());
        assert_eq!(item.managed.len(), 2);
        assert_eq!(item.managed[1].group, "apps");
        assert_eq!(item.managed[1].label(), "Deployment guestbook/guestbook-ui");
        assert_eq!(item.managed[1].status.as_deref(), Some("Synced · Healthy"));
        assert_eq!(item.key(), "argoproj.io/Application/argocd/guestbook");
    }

    #[test]
    fn argo_failed_sync_is_an_error_with_reasons() {
        let item = argo("payments");
        assert_eq!(item.attention, Attention::Error);
        assert!(item.is_problem());
        assert_eq!(item.sync.message, "OutOfSync");
        assert_eq!(item.health.as_ref().unwrap().status, HealthStatus::Degraded);
        assert_eq!(
            item.errors,
            [
                "SyncError: Failed sync attempt to 9a1c2e4: one or more objects failed to apply \
                 (dry run) (retried 2 times).",
                "Sync failed: one or more objects failed to apply (retried 2 times)",
                "Deployment payments/payments-api: Deployment.apps \"payments-api\" is invalid: \
                 spec.selector: Invalid value: field is immutable",
            ]
        );
        let deployment = &item.managed[1];
        assert_eq!(deployment.attention, Attention::Error);
        assert_eq!(deployment.status.as_deref(), Some("OutOfSync · Degraded"));
        assert_eq!(item.managed[0].attention, Attention::Warning);
    }

    #[test]
    fn argo_drift_progress_and_multi_source() {
        let drift = argo("monitoring-stack");
        assert_eq!(drift.attention, Attention::Warning);
        assert_eq!(drift.revision, "4.1.0, 2b8d0c1");
        assert!(drift.source.contains("(+1)"), "{}", drift.source);

        let syncing = argo("shop");
        assert_eq!(syncing.sync.message, "Syncing");
        assert_eq!(syncing.attention, Attention::Pending);
        assert!(!syncing.is_problem());

        // Never reconciled (no controller): unknown, not a problem.
        let fresh = argo("fresh");
        assert_eq!(fresh.attention, Attention::Unknown);
        assert!(fresh.health.is_some());
    }

    #[test]
    fn flux_kustomization() {
        let ok = flux(GitOpsKind::FluxKustomization, "apps");
        assert_eq!(ok.attention, Attention::Ok);
        assert_eq!(ok.revision, "main@sha1:6f1d2c3b");
        assert_eq!(ok.attempted_revision, None);
        assert_eq!(ok.source, "GitRepository/flux-system · ./apps/production");
        assert_eq!(ok.health.as_ref().unwrap().status, HealthStatus::Healthy);
        assert_eq!(
            ok.source_ref,
            Some(ResourceLink::new(
                SOURCE_GROUP,
                "GitRepository",
                Some("flux-system".into()),
                "flux-system"
            ))
        );
        assert_eq!(ok.managed.len(), 3);
        assert_eq!(ok.managed[0].label(), "Namespace podinfo");
        assert_eq!(ok.managed[0].group, "");
        assert_eq!(ok.managed[2].label(), "Deployment podinfo/podinfo");
        assert_eq!(ok.managed[2].group, "apps");
        assert_eq!(ok.updated.as_deref(), Some("2026-10-21T08:00:03Z"));

        let failed = flux(GitOpsKind::FluxKustomization, "infra");
        assert_eq!(failed.attention, Attention::Error);
        assert_eq!(failed.sync.status, HealthStatus::Failed);
        assert_eq!(failed.sync_label(), "Stalled");
        assert_eq!(ok.sync_label(), "Ready");
        assert_eq!(failed.revision, "main@sha1:aa11bb22");
        assert_eq!(
            failed.attempted_revision.as_deref(),
            Some("main@sha1:cc33dd44")
        );
        assert_eq!(
            failed.errors,
            [
                "Ready: kustomization path not found: stat /tmp/kustomization-1/infra/prod: \
                 no such file or directory",
                "Stalled: kustomization path not found: stat /tmp/kustomization-1/infra/prod: \
                 no such file or directory",
            ]
        );

        let suspended = flux(GitOpsKind::FluxKustomization, "legacy");
        assert!(suspended.suspended);
        assert_eq!(suspended.attention, Attention::Suspended);
        assert_eq!(suspended.sync_label(), "Suspended");
    }

    #[test]
    fn flux_helm_release() {
        let failed = flux(GitOpsKind::FluxHelmRelease, "podinfo");
        assert_eq!(failed.attention, Attention::Error);
        assert_eq!(failed.revision, "podinfo@6.7.0");
        assert_eq!(failed.attempted_revision.as_deref(), Some("6.7.1"));
        assert_eq!(failed.source, "podinfo@6.7.x");
        assert_eq!(
            failed.health.as_ref().unwrap().status,
            HealthStatus::Degraded
        );
        assert_eq!(
            failed.source_ref.as_ref().map(ResourceLink::label),
            Some("HelmRepository flux-system/podinfo-repo".into())
        );
        assert!(
            failed.errors[0].starts_with("Ready: Helm upgrade failed for release podinfo"),
            "{:?}",
            failed.errors
        );
        assert!(failed.errors.iter().any(|e| e.starts_with("Released:")));
    }

    #[test]
    fn flux_sources() {
        let git = flux(GitOpsKind::GitRepository, "flux-system");
        assert_eq!(git.attention, Attention::Ok);
        assert_eq!(git.revision, "main@sha1:6f1d2c3b");
        assert_eq!(git.source, "ssh://git@github.com/acme/fleet");
        assert_eq!(git.target, "branch: main");
        assert_eq!(git.health, None);

        let helm = flux(GitOpsKind::HelmRepository, "podinfo-repo");
        assert_eq!(helm.attention, Attention::Error);
        assert!(helm.errors[0].contains("failed to fetch Helm repository index"));
    }

    #[test]
    fn collect_sorts_problems_first_and_summarizes() {
        let a = fixture(ARGO, "guestbook");
        let b = fixture(ARGO, "payments");
        let c = fixture(FLUX, "legacy");
        let d = fixture(ARGO, "shop");
        let items = collect([
            (GitOpsKind::ArgoApplication, &a),
            (GitOpsKind::FluxKustomization, &c),
            (GitOpsKind::ArgoApplication, &b),
            (GitOpsKind::ArgoApplication, &d),
        ]);
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["payments", "shop", "legacy", "guestbook"]);
        let summary = summarize(&items);
        assert_eq!(
            summary,
            Summary {
                total: 4,
                errors: 1,
                warnings: 0,
                pending: 1,
                suspended: 1,
                ok: 1,
            }
        );
        assert_eq!(summary.problems(), 1);
    }

    #[test]
    fn short_revisions() {
        assert_eq!(
            short_revision("53e28ff20cc530b9ada2173fbbd64d48338583ba"),
            "53e28ff"
        );
        assert_eq!(
            short_revision("main@sha1:6f1d2c3b4a5e6f708192a3b4c5d6e7f8091a2b3c"),
            "main@sha1:6f1d2c3"
        );
        assert_eq!(
            short_revision(
                "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0"
            ),
            "sha256:0f1e2d3"
        );
        assert_eq!(short_revision("4.1.0, 2b8d0c1"), "4.1.0, 2b8d0c1");
        assert_eq!(short_revision("podinfo@6.7.0"), "podinfo@6.7.0");
        assert_eq!(short_revision(""), "");
    }

    #[test]
    fn kinds() {
        assert_eq!(
            GitOpsKind::find("source.toolkit.fluxcd.io", "OCIRepository"),
            Some(GitOpsKind::OciRepository)
        );
        assert_eq!(GitOpsKind::find("apps", "Deployment"), None);
        assert!(GitOpsKind::HelmRepository.is_source());
        assert!(!GitOpsKind::FluxHelmRelease.is_source());
        assert_eq!(GitOpsKind::ArgoApplication.tool(), Tool::ArgoCd);
        assert_eq!(GitOpsKind::FluxKustomization.tool().title(), "Flux");
    }

    #[test]
    fn served_follows_the_catalog() {
        use kube::api::{ApiResource, GroupVersionKind};
        use kube::discovery::{ApiCapabilities, Scope, verbs};

        let catalog = |kinds: &[(&str, &str)]| {
            ApiCatalog::from_resources(kinds.iter().map(|(group, kind)| {
                let gvk = GroupVersionKind::gvk(group, "v1", kind);
                (
                    ApiResource::from_gvk(&gvk),
                    ApiCapabilities {
                        scope: Scope::Namespaced,
                        subresources: Vec::new(),
                        operations: vec![verbs::LIST.into(), verbs::WATCH.into()],
                    },
                )
            }))
        };
        assert!(served(&catalog(&[("", "Pod"), ("apps", "Deployment")])).is_empty());
        let kinds: Vec<_> = served(&catalog(&[
            (HELM_GROUP, "HelmRelease"),
            (ARGO_GROUP, "Application"),
            (ARGO_GROUP, "AppProject"),
        ]))
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
        assert_eq!(
            kinds,
            [GitOpsKind::ArgoApplication, GitOpsKind::FluxHelmRelease]
        );
    }
}
