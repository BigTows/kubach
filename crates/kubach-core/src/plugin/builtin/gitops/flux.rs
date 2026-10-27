//! Flux: `Kustomization` (kustomize.toolkit.fluxcd.io/v1), `HelmRelease`
//! (helm.toolkit.fluxcd.io/v2) and the `GitRepository`, `OCIRepository`,
//! `HelmRepository` sources (source.toolkit.fluxcd.io/v1).

use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::{Value, json};

use super::super::{
    DELETE_ACTION, GenericPlugin, col, creation, kv, name_ns_cells, name_ns_columns, or_none, pair,
    push_kv, relations_with, text, unknown,
};
use super::{
    action, annotation_patch, delete_with_plural, is_suspended, merge_patch, ready_health,
    reference, request_stamp, s, yes_no,
};
use crate::helm::store::object_name;
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Health,
    PluginError, Relation, ResourcePlugin,
};

pub struct FluxKustomizationPlugin;
pub struct HelmReleasePlugin;
/// `GitRepository`, `OCIRepository` and `HelmRepository`.
pub struct FluxSourcePlugin;

const KUSTOMIZE_GROUP: &str = "kustomize.toolkit.fluxcd.io";
const HELM_GROUP: &str = "helm.toolkit.fluxcd.io";
const SOURCE_GROUP: &str = "source.toolkit.fluxcd.io";
const SOURCE_API_VERSION: &str = "source.toolkit.fluxcd.io/v1";
/// Annotation the Flux CLI sets for `flux reconcile`.
/// kstatus conditions that are abnormal but not failed when `True`:
/// the object is being reconciled, or its artifact is out of date.
pub(crate) const WARNING_CONDITIONS: [&str; 2] = ["Reconciling", "ArtifactOutdated"];

const RECONCILE_ANNOTATION: &str = "reconcile.fluxcd.io/requestedAt";

const RECONCILE_ACTION: Action = Action::api(action::RECONCILE, "Reconcile", false);

const SUSPEND_ACTION: Action = Action::api(action::SUSPEND, "Suspend", false);

const RESUME_ACTION: Action = Action::api(action::RESUME, "Resume", false);

fn flux_actions_for(obj: &DynamicObject) -> Vec<Action> {
    if obj.metadata.deletion_timestamp.is_some() {
        Vec::new()
    } else if is_suspended(obj) {
        // A suspended object ignores reconcile requests.
        vec![RESUME_ACTION, DELETE_ACTION]
    } else {
        vec![RECONCILE_ACTION, SUSPEND_ACTION, DELETE_ACTION]
    }
}

/// Merge patch for a Flux action, as the Flux CLI does it. Resume also
/// requests a reconciliation.
pub(crate) fn flux_patch(action_id: ActionId, stamp: &str) -> Option<Value> {
    match action_id {
        action::RECONCILE => Some(annotation_patch(RECONCILE_ANNOTATION, stamp)),
        action::SUSPEND => Some(json!({"spec": {"suspend": true}})),
        action::RESUME => Some(json!({
            "metadata": {"annotations": {RECONCILE_ANNOTATION: stamp}},
            "spec": {"suspend": false},
        })),
        _ => None,
    }
}

fn run_flux<'a>(
    client: &'a Client,
    obj: &'a DynamicObject,
    plural: &'a str,
    action_id: &'a ActionId,
) -> BoxFuture<'a, Result<(), PluginError>> {
    Box::pin(async move {
        if *action_id == action::DELETE {
            return delete_with_plural(client, obj, plural).await;
        }
        let patch = flux_patch(*action_id, &request_stamp()).ok_or_else(|| unknown(action_id))?;
        merge_patch(client, obj, plural, patch).await
    })
}

fn flux_actions() -> Vec<Action> {
    vec![
        RECONCILE_ACTION,
        SUSPEND_ACTION,
        RESUME_ACTION,
        DELETE_ACTION,
    ]
}

fn ready_message(obj: &DynamicObject) -> String {
    ready_health(obj).message
}

/// `Kind/name` or `Kind/ns/name` of a `sourceRef`-style reference.
pub(crate) fn ref_label(r: Option<&Value>) -> String {
    let Some(r) = r else {
        return String::new();
    };
    let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
    match g("namespace") {
        "" => format!("{}/{}", g("kind"), g("name")),
        ns => format!("{}/{ns}/{}", g("kind"), g("name")),
    }
}

/// Relation to a `sourceRef`-style reference; namespace defaults to the object's.
fn ref_relation(obj: &DynamicObject, r: Option<&Value>, api_version: &str) -> Option<Relation> {
    let r = r?;
    let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
    if g("kind").is_empty() || g("name").is_empty() {
        return None;
    }
    let api_version = match g("apiVersion") {
        "" => api_version,
        v => v,
    };
    let ns = match g("namespace") {
        "" => obj.metadata.namespace.clone(),
        ns => Some(ns.to_owned()),
    };
    Some(reference(api_version, g("kind"), g("name"), ns))
}

/// `dependsOn` entries as relations to the same kind.
fn depends_on(obj: &DynamicObject, api_version: &str, kind: &str) -> Vec<Relation> {
    obj.data
        .pointer("/spec/dependsOn")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let name = d.get("name").and_then(Value::as_str)?;
            let ns = d
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| obj.metadata.namespace.clone());
            Some(reference(api_version, kind, name, ns))
        })
        .collect()
}

fn suspended_cell(obj: &DynamicObject) -> Cell {
    text(yes_no(is_suspended(obj)))
}

// -------------------------------------------------------------- Kustomization

/// An entry of `status.inventory.entries`: id `<ns>_<name>_<group>_<kind>`.
pub(crate) struct InventoryEntry {
    pub(crate) api_version: String,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) namespace: Option<String>,
}

pub(crate) fn inventory(obj: &DynamicObject) -> Vec<InventoryEntry> {
    obj.data
        .pointer("/status/inventory/entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let id = e.get("id").and_then(Value::as_str)?;
            let version = e.get("v").and_then(Value::as_str).unwrap_or("v1");
            // Names and groups never contain `_`, so the split is unambiguous.
            let [ns, name, group, kind] = id.splitn(4, '_').collect::<Vec<_>>()[..] else {
                return None;
            };
            Some(InventoryEntry {
                api_version: super::api_version(group, version),
                kind: kind.to_owned(),
                name: name.to_owned(),
                namespace: (!ns.is_empty()).then(|| ns.to_owned()),
            })
        })
        .collect()
}

/// Recent reconciliations from `status.history` (Flux 2.5+, newest first):
/// `last reconciled` → `status revision (N runs)`.
fn reconcile_history(obj: &DynamicObject) -> Vec<(String, String)> {
    obj.data
        .pointer("/status/history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|h| {
            let g = |p: &str| h.pointer(p).and_then(Value::as_str).unwrap_or_default();
            let runs = h
                .get("totalReconciliations")
                .and_then(Value::as_i64)
                .map(|n| format!(" ({n} runs)"))
                .unwrap_or_default();
            let value = [g("/lastReconciledStatus"), g("/metadata/revision")]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            pair(
                or_none(g("/lastReconciled")),
                format!("{}{runs}", or_none(value)),
            )
        })
        .collect()
}

impl ResourcePlugin for FluxKustomizationPlugin {
    fn name(&self) -> &'static str {
        "flux-kustomization"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(
            KUSTOMIZE_GROUP,
            "v1",
            "Kustomization",
        )]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("ready", "Ready", ColumnKind::Status),
            col("source", "Source", ColumnKind::Text),
            col("path", "Path", ColumnKind::Text),
            col("revision", "Revision", ColumnKind::Text),
            col("suspended", "Suspended", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(ready_health(obj)),
            text(ref_label(obj.data.pointer("/spec/sourceRef"))),
            text(s(obj, "/spec/path")),
            text(s(obj, "/status/lastAppliedRevision")),
            suspended_cell(obj),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let prune = obj.data.pointer("/spec/prune").and_then(Value::as_bool);
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Ready", or_none(ready_message(obj))),
                pair(
                    "Source",
                    or_none(ref_label(obj.data.pointer("/spec/sourceRef"))),
                ),
                pair("Path", or_none(s(obj, "/spec/path"))),
                pair("Interval", or_none(s(obj, "/spec/interval"))),
                pair("Prune", yes_no(prune == Some(true))),
                pair("Target namespace", or_none(s(obj, "/spec/targetNamespace"))),
                pair("Suspended", yes_no(is_suspended(obj))),
                pair(
                    "Last applied revision",
                    or_none(s(obj, "/status/lastAppliedRevision")),
                ),
                pair(
                    "Last attempted revision",
                    or_none(s(obj, "/status/lastAttemptedRevision")),
                ),
                pair(
                    "Last handled reconcile",
                    or_none(s(obj, "/status/lastHandledReconcileAt")),
                ),
            ],
        )];
        push_kv(
            &mut sections,
            "Inventory",
            inventory(obj)
                .into_iter()
                .map(|e| {
                    let key = match &e.namespace {
                        Some(ns) => format!("{} {ns}/{}", e.kind, e.name),
                        None => format!("{} {}", e.kind, e.name),
                    };
                    pair(key, e.api_version)
                })
                .collect(),
        );
        push_kv(&mut sections, "History", reconcile_history(obj));
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        flux_actions()
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        flux_actions_for(obj)
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_flux(client, obj, "kustomizations", action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra: Vec<Relation> =
            ref_relation(obj, obj.data.pointer("/spec/sourceRef"), SOURCE_API_VERSION)
                .into_iter()
                .collect();
        extra.extend(depends_on(
            obj,
            "kustomize.toolkit.fluxcd.io/v1",
            "Kustomization",
        ));
        extra.extend(
            inventory(obj)
                .into_iter()
                .map(|e| reference(e.api_version, e.kind, e.name, e.namespace)),
        );
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        ready_health(obj)
    }
}

// ---------------------------------------------------------------- HelmRelease

/// Chart as `name@version` from `spec.chart`, or the `spec.chartRef` reference.
pub(crate) fn chart(obj: &DynamicObject) -> String {
    let name = s(obj, "/spec/chart/spec/chart");
    if name.is_empty() {
        return ref_label(obj.data.pointer("/spec/chartRef"));
    }
    match s(obj, "/spec/chart/spec/version") {
        "" | "*" => name.to_owned(),
        v => format!("{name}@{v}"),
    }
}

/// Latest release snapshot (`status.history` is newest first).
fn latest(obj: &DynamicObject) -> Option<&Value> {
    obj.data.pointer("/status/history/0")
}

/// Revision shown in the table: chart version of the latest release, or the
/// last attempted revision.
pub(crate) fn release_revision(obj: &DynamicObject) -> String {
    latest(obj)
        .and_then(|h| h.get("chartVersion"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| s(obj, "/status/lastAttemptedRevision").to_owned())
}

/// Helm release name: `spec.releaseName`, else `[<targetNamespace>-]<name>`.
fn release_name(obj: &DynamicObject) -> String {
    let explicit = s(obj, "/spec/releaseName");
    if !explicit.is_empty() {
        return explicit.to_owned();
    }
    let name = obj.metadata.name.clone().unwrap_or_default();
    match s(obj, "/spec/targetNamespace") {
        "" => name,
        ns => format!("{ns}-{name}"),
    }
}

fn storage_namespace(obj: &DynamicObject) -> Option<String> {
    match s(obj, "/spec/storageNamespace") {
        "" => obj.metadata.namespace.clone(),
        ns => Some(ns.to_owned()),
    }
}

impl ResourcePlugin for HelmReleasePlugin {
    fn name(&self) -> &'static str {
        "flux-helmrelease"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(HELM_GROUP, "v2", "HelmRelease")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("ready", "Ready", ColumnKind::Status),
            col("chart", "Chart", ColumnKind::Text),
            col("revision", "Revision", ColumnKind::Text),
            col("suspended", "Suspended", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(ready_health(obj)),
            text(chart(obj)),
            text(release_revision(obj)),
            suspended_cell(obj),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let source = match obj.data.pointer("/spec/chart/spec/sourceRef") {
            Some(r) => ref_label(Some(r)),
            None => ref_label(obj.data.pointer("/spec/chartRef")),
        };
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Ready", or_none(ready_message(obj))),
                pair("Chart", or_none(chart(obj))),
                pair("Source", or_none(source)),
                pair("Release name", release_name(obj)),
                pair("Target namespace", or_none(s(obj, "/spec/targetNamespace"))),
                pair("Interval", or_none(s(obj, "/spec/interval"))),
                pair("Suspended", yes_no(is_suspended(obj))),
                pair(
                    "Last attempted revision",
                    or_none(s(obj, "/status/lastAttemptedRevision")),
                ),
                pair(
                    "Last handled reconcile",
                    or_none(s(obj, "/status/lastHandledReconcileAt")),
                ),
            ],
        )];
        push_kv(
            &mut sections,
            "History",
            obj.data
                .pointer("/status/history")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|h| {
                    let g = |k: &str| h.get(k).and_then(Value::as_str).unwrap_or_default();
                    let revision = h.get("version").and_then(Value::as_i64).unwrap_or(0);
                    pair(
                        format!("Revision {revision}"),
                        format!(
                            "{} {}@{} ({})",
                            g("status"),
                            g("chartName"),
                            g("chartVersion"),
                            g("lastDeployed")
                        ),
                    )
                })
                .collect(),
        );
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        flux_actions()
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        flux_actions_for(obj)
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_flux(client, obj, "helmreleases", action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra: Vec<Relation> = [
            ref_relation(
                obj,
                obj.data.pointer("/spec/chart/spec/sourceRef"),
                SOURCE_API_VERSION,
            ),
            ref_relation(obj, obj.data.pointer("/spec/chartRef"), SOURCE_API_VERSION),
        ]
        .into_iter()
        .flatten()
        .collect();
        extra.extend(
            obj.data
                .pointer("/spec/valuesFrom")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| ref_relation(obj, Some(v), "v1")),
        );
        // The Helm storage Secret of the current release (see `crate::helm`).
        if let Some(rev) = latest(obj)
            .and_then(|h| h.get("version"))
            .and_then(Value::as_u64)
        {
            extra.push(reference(
                "v1",
                "Secret",
                object_name(&release_name(obj), rev as u32),
                storage_namespace(obj),
            ));
        }
        extra.extend(depends_on(obj, "helm.toolkit.fluxcd.io/v2", "HelmRelease"));
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        ready_health(obj)
    }
}

// -------------------------------------------------------------------- Sources

fn plural(obj: &DynamicObject) -> &'static str {
    match obj.types.as_ref().map(|t| t.kind.as_str()) {
        Some("OCIRepository") => "ocirepositories",
        Some("HelmRepository") => "helmrepositories",
        _ => "gitrepositories",
    }
}

/// Git: branch/tag/semver/name/commit; OCI: tag/semver/digest; Helm: repo type.
pub(crate) fn source_ref(obj: &DynamicObject) -> String {
    let kind = obj.types.as_ref().map(|t| t.kind.as_str()).unwrap_or("");
    if kind == "HelmRepository" {
        return match s(obj, "/spec/type") {
            "" => "default".into(),
            t => t.into(),
        };
    }
    ["commit", "digest", "name", "semver", "tag", "branch"]
        .iter()
        .find_map(|k| {
            obj.data
                .pointer(&format!("/spec/ref/{k}"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}: {v}"))
        })
        .unwrap_or_default()
}

impl ResourcePlugin for FluxSourcePlugin {
    fn name(&self) -> &'static str {
        "flux-source"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        ["GitRepository", "OCIRepository", "HelmRepository"]
            .into_iter()
            .map(|kind| GroupVersionKind::gvk(SOURCE_GROUP, "v1", kind))
            .collect()
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("ready", "Ready", ColumnKind::Status),
            col("url", "URL", ColumnKind::Text),
            col("ref", "Ref", ColumnKind::Text),
            col("revision", "Revision", ColumnKind::Text),
            col("suspended", "Suspended", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(ready_health(obj)),
            text(s(obj, "/spec/url")),
            text(source_ref(obj)),
            text(s(obj, "/status/artifact/revision")),
            suspended_cell(obj),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Ready", or_none(ready_message(obj))),
                pair("URL", or_none(s(obj, "/spec/url"))),
                pair("Ref", or_none(source_ref(obj))),
                pair("Interval", or_none(s(obj, "/spec/interval"))),
                pair("Provider", or_none(s(obj, "/spec/provider"))),
                pair("Secret", or_none(s(obj, "/spec/secretRef/name"))),
                pair("Suspended", yes_no(is_suspended(obj))),
                pair(
                    "Artifact revision",
                    or_none(s(obj, "/status/artifact/revision")),
                ),
                pair(
                    "Artifact updated",
                    or_none(s(obj, "/status/artifact/lastUpdateTime")),
                ),
                pair(
                    "Last handled reconcile",
                    or_none(s(obj, "/status/lastHandledReconcileAt")),
                ),
            ],
        )];
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        flux_actions()
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        flux_actions_for(obj)
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        run_flux(client, obj, plural(obj), action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let extra = ["/spec/secretRef/name", "/spec/certSecretRef/name"]
            .into_iter()
            .map(|p| s(obj, p))
            .filter(|n| !n.is_empty())
            .map(|n| reference("v1", "Secret", n, obj.metadata.namespace.clone()))
            .collect();
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        ready_health(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::HealthStatus;
    use crate::plugin::builtin::test_util::{obj, pairs};

    fn kustomization() -> DynamicObject {
        obj(json!({
            "apiVersion": "kustomize.toolkit.fluxcd.io/v1",
            "kind": "Kustomization",
            "metadata": {"name": "apps", "namespace": "flux-system", "generation": 3},
            "spec": {
                "interval": "10m",
                "path": "./apps/production",
                "prune": true,
                "sourceRef": {"kind": "GitRepository", "name": "flux-system"},
                "dependsOn": [{"name": "infra"}]
            },
            "status": {
                "observedGeneration": 3,
                "lastAppliedRevision": "main@sha1:5f2b7c1",
                "lastAttemptedRevision": "main@sha1:5f2b7c1",
                "conditions": [{
                    "type": "Ready", "status": "True", "reason": "ReconciliationSucceeded",
                    "message": "Applied revision: main@sha1:5f2b7c1"
                }],
                "inventory": {"entries": [
                    {"id": "_podinfo__Namespace", "v": "v1"},
                    {"id": "podinfo_podinfo_apps_Deployment", "v": "v1"}
                ]}
            }
        }))
    }

    #[test]
    fn kustomization_plugin() {
        let p = FluxKustomizationPlugin;
        let o = kustomization();
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
        assert_eq!(cells[3], text("GitRepository/flux-system"));
        assert_eq!(cells[4], text("./apps/production"));
        assert_eq!(cells[5], text("main@sha1:5f2b7c1"));
        assert_eq!(cells[6], text("No"));

        let sections = p.details(&o);
        assert!(pairs(&sections, "Overview").contains(&pair("Prune", "Yes")));
        assert_eq!(
            pairs(&sections, "Inventory"),
            [
                pair("Namespace podinfo", "v1"),
                pair("Deployment podinfo/podinfo", "apps/v1")
            ]
        );
        assert!(sections.iter().all(|s| s.title != "History"));

        let mut with_history = o.clone();
        with_history.data["status"]["history"] = json!([
            {"digest": "sha256:1", "firstReconciled": "2026-10-01T00:00:00Z",
             "lastReconciled": "2026-10-02T10:00:00Z", "lastReconciledStatus": "ReconciliationSucceeded",
             "totalReconciliations": 12, "metadata": {"revision": "main@sha1:5f2b7c1"}},
            {"lastReconciled": "2026-09-30T08:00:00Z", "lastReconciledStatus": "HealthCheckFailed"}
        ]);
        assert_eq!(
            pairs(&p.details(&with_history), "History"),
            [
                pair(
                    "2026-10-02T10:00:00Z",
                    "ReconciliationSucceeded main@sha1:5f2b7c1 (12 runs)"
                ),
                pair("2026-09-30T08:00:00Z", "HealthCheckFailed"),
            ]
        );

        let rels = p.relations(&o);
        let targets: Vec<_> = rels
            .iter()
            .map(|r| {
                (
                    r.target.api_version.as_str(),
                    r.target.kind.as_str(),
                    r.target.namespace.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                (SOURCE_API_VERSION, "GitRepository", Some("flux-system")),
                (
                    "kustomize.toolkit.fluxcd.io/v1",
                    "Kustomization",
                    Some("flux-system")
                ),
                ("v1", "Namespace", None),
                ("apps/v1", "Deployment", Some("podinfo")),
            ]
        );
    }

    #[test]
    fn suspend_and_reconcile() {
        let mut o = kustomization();
        assert_eq!(
            flux_actions_for(&o),
            [RECONCILE_ACTION, SUSPEND_ACTION, DELETE_ACTION]
        );
        o.data["spec"]["suspend"] = json!(true);
        assert_eq!(flux_actions_for(&o), [RESUME_ACTION, DELETE_ACTION]);
        assert_eq!(FluxKustomizationPlugin.cells(&o)[6], text("Yes"));
        assert_eq!(
            FluxKustomizationPlugin.health(&o),
            Health::new(HealthStatus::Unknown, "Suspended")
        );

        let stamp = "2026-10-22T10:00:00.123456789Z";
        assert_eq!(
            flux_patch(action::RECONCILE, stamp),
            Some(json!({"metadata": {"annotations": {RECONCILE_ANNOTATION: stamp}}}))
        );
        assert_eq!(
            flux_patch(action::SUSPEND, stamp),
            Some(json!({"spec": {"suspend": true}}))
        );
        assert_eq!(
            flux_patch(action::RESUME, stamp),
            Some(json!({
                "metadata": {"annotations": {RECONCILE_ANNOTATION: stamp}},
                "spec": {"suspend": false}
            }))
        );
        assert_eq!(flux_patch(action::SYNC, stamp), None);
    }

    fn helm_release() -> DynamicObject {
        obj(json!({
            "apiVersion": "helm.toolkit.fluxcd.io/v2",
            "kind": "HelmRelease",
            "metadata": {"name": "podinfo", "namespace": "apps"},
            "spec": {
                "interval": "5m",
                "targetNamespace": "web",
                "chart": {"spec": {
                    "chart": "podinfo", "version": "6.x",
                    "sourceRef": {"kind": "HelmRepository", "name": "podinfo", "namespace": "flux-system"}
                }},
                "valuesFrom": [{"kind": "ConfigMap", "name": "podinfo-values"}]
            },
            "status": {
                "lastAttemptedRevision": "6.5.4",
                "history": [
                    {"chartName": "podinfo", "chartVersion": "6.5.4", "version": 2,
                     "status": "deployed", "lastDeployed": "2026-10-02T10:00:00Z"},
                    {"chartName": "podinfo", "chartVersion": "6.5.3", "version": 1,
                     "status": "superseded", "lastDeployed": "2026-10-01T10:00:00Z"}
                ],
                "conditions": [
                    {"type": "Ready", "status": "False", "reason": "UpgradeFailed",
                     "message": "Helm upgrade failed: timed out waiting for the condition"}
                ]
            }
        }))
    }

    #[test]
    fn helm_release_plugin() {
        let p = HelmReleasePlugin;
        let o = helm_release();
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            p.health(&o),
            Health::new(
                HealthStatus::Degraded,
                "Helm upgrade failed: timed out waiting for the condition"
            )
        );
        assert_eq!(cells[3], text("podinfo@6.x"));
        assert_eq!(cells[4], text("6.5.4"));

        let sections = p.details(&o);
        let overview = pairs(&sections, "Overview");
        assert!(overview.contains(&pair("Release name", "web-podinfo")));
        assert!(overview.contains(&pair("Source", "HelmRepository/flux-system/podinfo")));
        assert_eq!(
            pairs(&sections, "History")[0],
            pair(
                "Revision 2",
                "deployed podinfo@6.5.4 (2026-10-02T10:00:00Z)"
            )
        );

        let rels = p.relations(&o);
        let targets: Vec<_> = rels
            .iter()
            .map(|r| (r.target.kind.as_str(), r.target.name.as_str()))
            .collect();
        assert_eq!(
            targets,
            [
                ("HelmRepository", "podinfo"),
                ("ConfigMap", "podinfo-values"),
                ("Secret", "sh.helm.release.v1.web-podinfo.v2"),
            ]
        );
        assert_eq!(rels[0].target.namespace.as_deref(), Some("flux-system"));
        assert_eq!(rels[2].target.namespace.as_deref(), Some("apps"));
    }

    #[test]
    fn helm_release_with_chart_ref() {
        let mut o = helm_release();
        o.data["spec"].as_object_mut().unwrap().remove("chart");
        o.data["spec"]["chartRef"] = json!({"kind": "OCIRepository", "name": "podinfo"});
        o.data["status"] = json!({"lastAttemptedRevision": "6.5.4@sha256:abc"});
        let cells = HelmReleasePlugin.cells(&o);
        assert_eq!(cells[3], text("OCIRepository/podinfo"));
        assert_eq!(cells[4], text("6.5.4@sha256:abc"));
        assert_eq!(
            HelmReleasePlugin.relations(&o)[0].target.kind,
            "OCIRepository"
        );
    }

    #[test]
    fn sources() {
        let p = FluxSourcePlugin;
        let git = obj(json!({
            "apiVersion": "source.toolkit.fluxcd.io/v1",
            "kind": "GitRepository",
            "metadata": {"name": "flux-system", "namespace": "flux-system"},
            "spec": {
                "url": "ssh://git@github.com/acme/fleet", "interval": "1m",
                "ref": {"branch": "main"}, "secretRef": {"name": "flux-system"}
            },
            "status": {
                "artifact": {"revision": "main@sha1:5f2b7c1", "lastUpdateTime": "2026-10-02T10:00:00Z"},
                "conditions": [{"type": "Ready", "status": "True", "message": "stored artifact"}]
            }
        }));
        let cells = p.cells(&git);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[3], text("ssh://git@github.com/acme/fleet"));
        assert_eq!(cells[4], text("branch: main"));
        assert_eq!(cells[5], text("main@sha1:5f2b7c1"));
        assert_eq!(p.health(&git).status, HealthStatus::Healthy);
        assert_eq!(plural(&git), "gitrepositories");
        assert_eq!(p.relations(&git)[0].target.kind, "Secret");

        let oci = obj(json!({
            "apiVersion": "source.toolkit.fluxcd.io/v1",
            "kind": "OCIRepository",
            "metadata": {"name": "podinfo", "namespace": "flux-system"},
            "spec": {"url": "oci://ghcr.io/stefanprodan/manifests/podinfo", "ref": {"tag": "latest"}}
        }));
        assert_eq!(p.cells(&oci)[4], text("tag: latest"));
        assert_eq!(plural(&oci), "ocirepositories");
        assert_eq!(p.health(&oci), Health::unknown());

        let helm = obj(json!({
            "apiVersion": "source.toolkit.fluxcd.io/v1",
            "kind": "HelmRepository",
            "metadata": {"name": "podinfo", "namespace": "flux-system"},
            "spec": {"url": "oci://ghcr.io/stefanprodan/charts", "type": "oci", "suspend": true}
        }));
        assert_eq!(p.cells(&helm)[4], text("oci"));
        assert_eq!(p.cells(&helm)[6], text("Yes"));
        assert_eq!(plural(&helm), "helmrepositories");
        assert_eq!(p.actions_for(&helm), [RESUME_ACTION, DELETE_ACTION]);
    }
}
