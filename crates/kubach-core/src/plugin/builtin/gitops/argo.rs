//! Argo CD `Application` and `ApplicationSet` (argoproj.io/v1alpha1).

use kubach_i18n::msg;
use std::collections::BTreeMap;

use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind, PostParams};
use serde_json::{Map, Value, json};

use super::super::{
    DELETE_ACTION, GenericPlugin, Polarity, col, creation, kv, name_ns_cells, name_ns_columns,
    name_of, or_none, pair, push_kv, relations_with, text, unknown,
};
use super::{
    action, annotation_patch, api_of, conditions, delete_with_plural, merge_patch, reference, s,
    yes_no,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Health,
    HealthStatus, PluginError, Relation, ResourcePlugin, SectionBody,
};

pub struct ApplicationPlugin;
pub struct ApplicationSetPlugin;

const GROUP: &str = "argoproj.io";
const API_VERSION: &str = "argoproj.io/v1alpha1";
/// ApplicationSet conditions that report a failure when `True`.
pub(crate) const APPSET_NEGATIVE_CONDITIONS: [&str; 1] = ["ErrorOccurred"];

/// Polarity of an Application condition. Argo CD conditions are events with
/// no `status`: present means a problem, `…Error` (`SyncError`,
/// `ComparisonError`) a failure, anything else (`…Warning`) a warning.
pub(crate) fn application_condition_polarity(type_: &str) -> Polarity {
    if type_.ends_with("Error") {
        Polarity::Negative
    } else {
        Polarity::Warning
    }
}

const REFRESH_ANNOTATION: &str = "argocd.argoproj.io/refresh";
const APPSET_REFRESH_ANNOTATION: &str = "argocd.argoproj.io/application-set-refresh";
/// `operation.initiatedBy.username` of syncs started by Kubach.
const SYNC_INITIATOR: &str = "kubach";
/// Address of the cluster Argo CD runs in.
const IN_CLUSTER_SERVER: &str = "https://kubernetes.default.svc";

const REFRESH_ACTION: Action = Action::api(action::REFRESH, "Refresh", false);

const HARD_REFRESH_ACTION: Action = Action::api(action::HARD_REFRESH, "Hard refresh", false);

const SYNC_ACTION: Action = Action::api(action::SYNC, "Sync", true);

/// A managed resource from `status.resources`.
pub(crate) struct Managed<'a> {
    pub(crate) group: &'a str,
    pub(crate) version: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) namespace: &'a str,
    pub(crate) name: &'a str,
    pub(crate) sync: &'a str,
    pub(crate) health: &'a str,
}

pub(crate) fn managed(obj: &DynamicObject) -> Vec<Managed<'_>> {
    fn str_of<'v>(v: &'v Value, p: &str) -> &'v str {
        v.pointer(p).and_then(Value::as_str).unwrap_or_default()
    }
    obj.data
        .pointer("/status/resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| Managed {
            group: str_of(r, "/group"),
            version: str_of(r, "/version"),
            kind: str_of(r, "/kind"),
            namespace: str_of(r, "/namespace"),
            name: str_of(r, "/name"),
            sync: str_of(r, "/status"),
            health: str_of(r, "/health/status"),
        })
        .filter(|r| !r.kind.is_empty() && !r.name.is_empty())
        .collect()
}

fn managed_label(r: &Managed) -> String {
    if r.namespace.is_empty() {
        format!("{} {}", r.kind, r.name)
    } else {
        format!("{} {}/{}", r.kind, r.namespace, r.name)
    }
}

fn managed_relations(obj: &DynamicObject) -> Vec<Relation> {
    managed(obj)
        .iter()
        .map(|r| {
            reference(
                super::api_version(r.group, r.version),
                r.kind,
                r.name,
                (!r.namespace.is_empty()).then(|| r.namespace.to_owned()),
            )
        })
        .collect()
}

/// Sources of an Application or an ApplicationSet template spec.
pub(crate) fn sources(spec: Option<&Value>) -> Vec<&Map<String, Value>> {
    let Some(spec) = spec else {
        return Vec::new();
    };
    match spec.get("sources").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list.iter().filter_map(Value::as_object).collect(),
        _ => spec
            .get("source")
            .and_then(Value::as_object)
            .into_iter()
            .collect(),
    }
}

pub(crate) fn field<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Path inside the repo, or the Helm chart name.
pub(crate) fn source_path(m: &Map<String, Value>) -> &str {
    match field(m, "path") {
        "" => field(m, "chart"),
        path => path,
    }
}

/// `HEAD` when the target revision is empty, like Argo CD.
pub(crate) fn target_revision(m: &Map<String, Value>) -> &str {
    match field(m, "targetRevision") {
        "" => "HEAD",
        rev => rev,
    }
}

/// First source's value plus `(+N)` for multi-source apps.
pub(crate) fn first_and_more(
    spec: Option<&Value>,
    f: impl Fn(&Map<String, Value>) -> &str,
) -> String {
    let all = sources(spec);
    match all.as_slice() {
        [] => String::new(),
        [one] => f(one).to_owned(),
        [first, rest @ ..] => format!("{} (+{})", f(first), rest.len()),
    }
}

pub(crate) fn destination(spec: Option<&Value>) -> String {
    let dest = spec.and_then(|s| s.get("destination"));
    let get = |k: &str| {
        dest.and_then(|d| d.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let cluster = match (get("name"), get("server")) {
        ("", IN_CLUSTER_SERVER) => "in-cluster",
        ("", server) => server,
        (name, _) => name,
    };
    match get("namespace") {
        "" => cluster.to_owned(),
        ns => format!("{cluster}/{ns}"),
    }
}

fn in_cluster(obj: &DynamicObject) -> bool {
    let server = s(obj, "/spec/destination/server");
    let name = s(obj, "/spec/destination/name");
    server == IN_CLUSTER_SERVER || (server.is_empty() && name == "in-cluster")
}

/// Sync status cell: `Synced`, `OutOfSync`, or `Syncing` while an operation runs.
pub(crate) fn sync_health(obj: &DynamicObject) -> Health {
    if operation_running(obj) {
        return Health::new(HealthStatus::Progressing, "Syncing");
    }
    match s(obj, "/status/sync/status") {
        "Synced" => Health::new(HealthStatus::Healthy, "Synced"),
        "OutOfSync" => Health::new(HealthStatus::Degraded, "OutOfSync"),
        _ => Health::new(HealthStatus::Unknown, "Unknown"),
    }
}

fn operation_running(obj: &DynamicObject) -> bool {
    obj.data.get("operation").is_some_and(|o| !o.is_null())
        || s(obj, "/status/operationState/phase") == "Running"
}

/// Maps Argo CD health (`status.health.status`).
pub(crate) fn app_health(obj: &DynamicObject) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-deleting"));
    }
    let state = s(obj, "/status/health/status");
    let message = match s(obj, "/status/health/message") {
        "" => state,
        m => m,
    };
    let status = match state {
        "Healthy" => HealthStatus::Healthy,
        "Progressing" => HealthStatus::Progressing,
        "Degraded" | "Missing" => HealthStatus::Degraded,
        "Suspended" => HealthStatus::Unknown,
        _ => {
            // Without health, an error condition (ComparisonError, …) explains why.
            if let Some(c) = conditions(obj).iter().find(|c| c.type_.ends_with("Error")) {
                return Health::new(
                    HealthStatus::Degraded,
                    c.message.clone().unwrap_or_else(|| c.type_.clone()),
                );
            }
            return Health::new(HealthStatus::Unknown, or_unknown(state));
        }
    };
    Health::new(status, message)
}

fn or_unknown(s: &str) -> &str {
    if s.is_empty() { "Unknown" } else { s }
}

/// The `operation` Argo CD's API server writes for `argocd app sync`
/// (`SetAppOperation`): sync to the target revision(s) with the app's sync
/// options and retry policy, no prune override.
pub(crate) fn sync_operation(app: &DynamicObject) -> Result<Value, PluginError> {
    if app.metadata.deletion_timestamp.is_some() {
        return Err(PluginError::InvalidObject("application is deleting".into()));
    }
    if app.data.get("operation").is_some_and(|o| !o.is_null()) {
        return Err(PluginError::InvalidObject(
            "another operation is already in progress".into(),
        ));
    }
    let spec = app.data.get("spec");
    let mut sync = Map::new();
    match spec
        .and_then(|s| s.get("sources"))
        .and_then(Value::as_array)
    {
        Some(list) if !list.is_empty() => {
            let revisions: Vec<&str> = sources(spec).into_iter().map(target_revision).collect();
            sync.insert("sources".into(), Value::Array(list.clone()));
            sync.insert("revisions".into(), json!(revisions));
        }
        _ => {
            let source = spec
                .and_then(|s| s.get("source"))
                .and_then(Value::as_object)
                .ok_or_else(|| PluginError::InvalidObject("application has no source".into()))?;
            sync.insert("source".into(), Value::Object(source.clone()));
            sync.insert("revision".into(), json!(target_revision(source)));
        }
    }
    let policy = spec.and_then(|s| s.get("syncPolicy"));
    if let Some(options) = policy.and_then(|p| p.get("syncOptions")) {
        sync.insert("syncOptions".into(), options.clone());
    }
    let mut op = json!({
        "sync": sync,
        "initiatedBy": {"username": SYNC_INITIATOR},
    });
    if let Some(retry) = policy.and_then(|p| p.get("retry")) {
        op["retry"] = retry.clone();
    }
    Ok(op)
}

/// Sets `operation` and clears `status.operationState`, retrying on conflicts.
async fn sync(client: &Client, obj: &DynamicObject) -> Result<(), PluginError> {
    let api = api_of(client, obj, "applications")?;
    let name = name_of(obj)?;
    let mut attempts = 0;
    loop {
        let mut app = api.get(name).await?;
        app.data["operation"] = sync_operation(&app)?;
        if let Some(status) = app.data.get_mut("status").and_then(Value::as_object_mut) {
            status.remove("operationState");
        }
        match api.replace(name, &PostParams::default(), &app).await {
            Err(kube::Error::Api(status)) if status.is_conflict() && attempts < 3 => {
                attempts += 1;
            }
            result => return result.map(|_| ()).map_err(Into::into),
        }
    }
}

fn counts<'a>(items: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items.filter(|i| !i.is_empty()) {
        *by.entry(item).or_default() += 1;
    }
    by.into_iter()
        .map(|(k, n)| pair(k, n.to_string()))
        .collect()
}

fn source_pairs(spec: Option<&Value>) -> Vec<(String, String)> {
    let all = sources(spec);
    let multi = all.len() > 1;
    let mut out = Vec::new();
    for (i, src) in all.iter().enumerate() {
        let key = |k: &str| {
            if multi {
                format!("#{} {k}", i + 1)
            } else {
                k.to_owned()
            }
        };
        out.push(pair(key("Repo"), field(src, "repoURL")));
        if !field(src, "chart").is_empty() {
            out.push(pair(key("Chart"), field(src, "chart")));
        } else {
            out.push(pair(key("Path"), or_none(field(src, "path"))));
        }
        out.push(pair(key("Target revision"), target_revision(src)));
        if !field(src, "ref").is_empty() {
            out.push(pair(key("Ref"), field(src, "ref")));
        }
    }
    out
}

fn operation_pairs(obj: &DynamicObject) -> Vec<(String, String)> {
    let Some(op) = obj.data.pointer("/status/operationState") else {
        return Vec::new();
    };
    let str_of = |p: &str| op.pointer(p).and_then(Value::as_str).unwrap_or_default();
    let initiator = if op.pointer("/operation/initiatedBy/automated") == Some(&json!(true)) {
        "automated"
    } else {
        str_of("/operation/initiatedBy/username")
    };
    let revision = match str_of("/syncResult/revision") {
        "" => str_of("/operation/sync/revision"),
        r => r,
    };
    let mut out = vec![
        pair("Phase", or_none(str_of("/phase"))),
        pair("Message", or_none(str_of("/message"))),
        pair("Revision", or_none(revision)),
        pair("Initiated by", or_none(initiator)),
        pair("Started", or_none(str_of("/startedAt"))),
        pair("Finished", or_none(str_of("/finishedAt"))),
    ];
    if let Some(n) = op.get("retryCount").and_then(Value::as_i64) {
        out.push(pair("Retries", n.to_string()));
    }
    out
}

fn sync_result_pairs(obj: &DynamicObject) -> Vec<(String, String)> {
    obj.data
        .pointer("/status/operationState/syncResult/resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| {
            let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
            let label = if g("namespace").is_empty() {
                format!("{} {}", g("kind"), g("name"))
            } else {
                format!("{} {}/{}", g("kind"), g("namespace"), g("name"))
            };
            let status = match (g("status"), g("hookPhase")) {
                (s, "") => s.to_owned(),
                (s, hook) => format!("{s} ({hook})"),
            };
            let value = match g("message") {
                "" => status,
                m => format!("{status}: {m}"),
            };
            pair(label, value)
        })
        .collect()
}

/// How many past syncs the details list.
const HISTORY_SHOWN: usize = 10;

/// Past syncs from `status.history` (oldest first there), newest first:
/// `#id at` → `revision(s) by initiator`.
fn history_pairs(obj: &DynamicObject) -> Vec<(String, String)> {
    let Some(history) = obj
        .data
        .pointer("/status/history")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    history
        .iter()
        .rev()
        .take(HISTORY_SHOWN)
        .map(|h| {
            let g = |k: &str| h.get(k).and_then(Value::as_str).unwrap_or_default();
            let revisions: Vec<&str> = match h.get("revisions").and_then(Value::as_array) {
                Some(list) if !list.is_empty() => list.iter().filter_map(Value::as_str).collect(),
                _ => vec![g("revision")],
            };
            let id = h.get("id").and_then(Value::as_i64).unwrap_or_default();
            let at = match g("deployedAt") {
                "" => String::new(),
                at => format!(" {at}"),
            };
            let initiator = if h.pointer("/initiatedBy/automated") == Some(&json!(true)) {
                "automated"
            } else {
                h.pointer("/initiatedBy/username")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            let by = match initiator {
                "" => String::new(),
                who => format!(" by {who}"),
            };
            pair(
                format!("#{id}{at}"),
                format!("{}{by}", or_none(revisions.join(", "))),
            )
        })
        .collect()
}

impl ResourcePlugin for ApplicationPlugin {
    fn name(&self) -> &'static str {
        "argocd-application"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1alpha1", "Application")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("sync", "Sync status", ColumnKind::Status),
            col("health", "Health", ColumnKind::Status),
            col("repo", "Repo", ColumnKind::Text),
            col("path", "Path", ColumnKind::Text),
            col("revision", "Revision", ColumnKind::Text),
            col("destination", "Destination", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let spec = obj.data.get("spec");
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(sync_health(obj)),
            Cell::Status(app_health(obj)),
            text(first_and_more(spec, |m| field(m, "repoURL"))),
            text(first_and_more(spec, source_path)),
            text(first_and_more(spec, target_revision)),
            text(destination(spec)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let spec = obj.data.get("spec");
        let health = app_health(obj);
        let automated = obj.data.pointer("/spec/syncPolicy/automated");
        let auto_sync = match automated {
            Some(a) if a.get("enabled") != Some(&json!(false)) => {
                let flag = |k: &str| a.get(k) == Some(&json!(true));
                format!(
                    "Yes (prune: {}, self-heal: {})",
                    yes_no(flag("prune")),
                    yes_no(flag("selfHeal"))
                )
            }
            _ => "No".into(),
        };
        let synced = match obj
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
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Project", or_none(s(obj, "/spec/project"))),
                pair("Sync status", sync_health(obj).message),
                pair("Synced revision", or_none(synced)),
                pair("Health", or_unknown(s(obj, "/status/health/status"))),
                pair("Health message", or_none(health.message)),
                pair("Auto-sync", auto_sync),
                pair("Destination", or_none(destination(spec))),
                pair("Reconciled at", or_none(s(obj, "/status/reconciledAt"))),
            ],
        )];
        push_kv(&mut sections, "Sources", source_pairs(spec));
        push_kv(&mut sections, "Operation", operation_pairs(obj));
        push_kv(&mut sections, "Sync result", sync_result_pairs(obj));
        push_kv(&mut sections, "Sync history", history_pairs(obj));
        let resources = managed(obj);
        if !resources.is_empty() {
            let mut summary = vec![pair("Total", resources.len().to_string())];
            summary.extend(counts(resources.iter().map(|r| r.sync)));
            summary.extend(counts(resources.iter().map(|r| r.health)));
            sections.push(kv("Resources summary", summary));
            sections.push(kv(
                "Resources",
                resources
                    .iter()
                    .map(|r| {
                        let state = match r.health {
                            "" => r.sync.to_owned(),
                            h => format!("{} / {h}", r.sync),
                        };
                        pair(managed_label(r), state)
                    })
                    .collect(),
            ));
        }
        let conditions = conditions(obj);
        if !conditions.is_empty() {
            sections.push(DetailSection::new(
                "Conditions",
                SectionBody::Conditions(conditions),
            ));
        }
        let images: Vec<&str> = obj
            .data
            .pointer("/status/summary/images")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        push_kv(
            &mut sections,
            "Images",
            images.iter().map(|i| pair(*i, "")).collect(),
        );
        // Argo CD conditions have no status, so the generic part adds none.
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            REFRESH_ACTION,
            HARD_REFRESH_ACTION,
            SYNC_ACTION,
            DELETE_ACTION,
        ]
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        if obj.metadata.deletion_timestamp.is_some() {
            return Vec::new();
        }
        if operation_running(obj) {
            return vec![REFRESH_ACTION, HARD_REFRESH_ACTION, DELETE_ACTION];
        }
        self.actions()
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            match *action_id {
                action::REFRESH | action::HARD_REFRESH => {
                    let mode = if *action_id == action::HARD_REFRESH {
                        "hard"
                    } else {
                        "normal"
                    };
                    let patch = annotation_patch(REFRESH_ANNOTATION, mode);
                    merge_patch(client, obj, "applications", patch).await
                }
                action::SYNC => sync(client, obj).await,
                action::DELETE => delete_with_plural(client, obj, "applications").await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra = Vec::new();
        let project = s(obj, "/spec/project");
        if !project.is_empty() {
            extra.push(reference(
                API_VERSION,
                "AppProject",
                project,
                obj.metadata.namespace.clone(),
            ));
        }
        // Destination objects live in this cluster only for in-cluster apps.
        if in_cluster(obj) {
            let ns = s(obj, "/spec/destination/namespace");
            if !ns.is_empty() {
                extra.push(reference("v1", "Namespace", ns, None));
            }
            extra.extend(managed_relations(obj));
        }
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        app_health(obj)
    }
}

// ------------------------------------------------------------ ApplicationSet

fn generator_kinds(obj: &DynamicObject) -> String {
    obj.data
        .pointer("/spec/generators")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|g| g.keys().cloned())
        .collect::<Vec<_>>()
        .join(", ")
}

fn appset_health(obj: &DynamicObject) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        return Health::phrase(HealthStatus::Progressing, msg!("state-deleting"));
    }
    let conditions = conditions(obj);
    let find = |t: &str, status: &str| {
        conditions
            .iter()
            .find(|c| c.type_ == t && c.status == status)
    };
    let msg = |c: &crate::plugin::ConditionInfo| {
        c.message
            .clone()
            .or_else(|| c.reason.clone())
            .unwrap_or_else(|| c.type_.clone())
    };
    if let Some(c) = find("ErrorOccurred", "True").or(find("ParametersGenerated", "False")) {
        return Health::new(HealthStatus::Degraded, msg(c));
    }
    if let Some(c) = find("RolloutProgressing", "True") {
        return Health::new(HealthStatus::Progressing, msg(c));
    }
    if let Some(c) = find("ResourcesUpToDate", "True") {
        return Health::new(HealthStatus::Healthy, msg(c));
    }
    Health::unknown()
}

impl ResourcePlugin for ApplicationSetPlugin {
    fn name(&self) -> &'static str {
        "argocd-applicationset"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1alpha1", "ApplicationSet")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("status", "Status", ColumnKind::Status),
            col("generators", "Generators", ColumnKind::Text),
            col("applications", "Applications", ColumnKind::Number),
            col("project", "Project", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(appset_health(obj)),
            text(generator_kinds(obj)),
            Cell::Number(managed(obj).len() as i64),
            text(s(obj, "/spec/template/spec/project")),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let template = obj.data.pointer("/spec/template/spec");
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Generators", or_none(generator_kinds(obj))),
                pair(
                    "Template name",
                    or_none(s(obj, "/spec/template/metadata/name")),
                ),
                pair("Project", or_none(s(obj, "/spec/template/spec/project"))),
                pair("Destination", or_none(destination(template))),
                pair(
                    "Applications sync policy",
                    or_none(s(obj, "/spec/syncPolicy/applicationsSync")),
                ),
            ],
        )];
        push_kv(&mut sections, "Template sources", source_pairs(template));
        push_kv(
            &mut sections,
            "Applications",
            managed(obj)
                .iter()
                .map(|r| {
                    let state = match r.health {
                        "" => r.sync.to_owned(),
                        h => format!("{} / {h}", r.sync),
                    };
                    pair(r.name, state)
                })
                .collect(),
        );
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![REFRESH_ACTION, DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            match *action_id {
                action::REFRESH => {
                    let patch = annotation_patch(APPSET_REFRESH_ANNOTATION, "true");
                    merge_patch(client, obj, "applicationsets", patch).await
                }
                action::DELETE => delete_with_plural(client, obj, "applicationsets").await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        relations_with(obj, managed_relations(obj))
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        appset_health(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::RelationKind;
    use crate::plugin::builtin::test_util::{obj, pairs, section};

    fn app() -> DynamicObject {
        obj(json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {
                "name": "guestbook", "namespace": "argocd",
                "creationTimestamp": "2026-10-01T00:00:00Z"
            },
            "spec": {
                "project": "default",
                "source": {
                    "repoURL": "https://github.com/argoproj/argocd-example-apps.git",
                    "path": "guestbook",
                    "targetRevision": "main"
                },
                "destination": {"server": "https://kubernetes.default.svc", "namespace": "guestbook"},
                "syncPolicy": {
                    "automated": {"prune": true},
                    "syncOptions": ["CreateNamespace=true"],
                    "retry": {"limit": 2}
                }
            },
            "status": {
                "sync": {"status": "OutOfSync", "revision": "4f2c1a9"},
                "health": {"status": "Progressing", "message": "Waiting for rollout"},
                "reconciledAt": "2026-10-02T10:00:00Z",
                "resources": [
                    {"version": "v1", "kind": "Service", "namespace": "guestbook",
                     "name": "guestbook-ui", "status": "Synced", "health": {"status": "Healthy"}},
                    {"group": "apps", "version": "v1", "kind": "Deployment", "namespace": "guestbook",
                     "name": "guestbook-ui", "status": "OutOfSync", "health": {"status": "Progressing"}}
                ],
                "conditions": [
                    {"type": "SyncError", "message": "one or more objects failed to apply",
                     "lastTransitionTime": "2026-10-02T10:00:00Z"}
                ],
                "operationState": {
                    "phase": "Failed",
                    "message": "one or more objects failed to apply",
                    "startedAt": "2026-10-02T09:59:00Z",
                    "finishedAt": "2026-10-02T10:00:00Z",
                    "operation": {"initiatedBy": {"automated": true}, "sync": {"revision": "4f2c1a9"}},
                    "syncResult": {
                        "revision": "4f2c1a9",
                        "resources": [
                            {"kind": "Deployment", "namespace": "guestbook", "name": "guestbook-ui",
                             "status": "SyncFailed", "message": "field is immutable"}
                        ]
                    }
                },
                "summary": {"images": ["gcr.io/heptio-images/ks-guestbook-demo:0.2"]}
            }
        }))
    }

    #[test]
    fn cells() {
        let p = ApplicationPlugin;
        let cells = p.cells(&app());
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[2],
            Cell::Status(Health::new(HealthStatus::Degraded, "OutOfSync"))
        );
        assert_eq!(
            cells[3],
            Cell::Status(Health::new(
                HealthStatus::Progressing,
                "Waiting for rollout"
            ))
        );
        assert_eq!(
            cells[4],
            text("https://github.com/argoproj/argocd-example-apps.git")
        );
        assert_eq!(cells[5], text("guestbook"));
        assert_eq!(cells[6], text("main"));
        assert_eq!(cells[7], text("in-cluster/guestbook"));
    }

    #[test]
    fn health_mapping() {
        let with = |state: &str| {
            let mut o = app();
            o.data["status"]["health"] = json!({"status": state});
            app_health(&o).status
        };
        assert_eq!(with("Healthy"), HealthStatus::Healthy);
        assert_eq!(with("Progressing"), HealthStatus::Progressing);
        assert_eq!(with("Degraded"), HealthStatus::Degraded);
        assert_eq!(with("Missing"), HealthStatus::Degraded);
        assert_eq!(with("Suspended"), HealthStatus::Unknown);
        // Unknown health with an error condition is degraded.
        assert_eq!(with("Unknown"), HealthStatus::Degraded);
        let mut o = app();
        o.data["status"] = json!({});
        assert_eq!(
            app_health(&o),
            Health::new(HealthStatus::Unknown, "Unknown")
        );
    }

    #[test]
    fn multi_source() {
        let mut o = app();
        o.data["spec"]["sources"] = json!([
            {"repoURL": "https://charts.example.com", "chart": "redis", "targetRevision": "18.1.0"},
            {"repoURL": "https://git.example.com/values.git", "ref": "values"}
        ]);
        let cells = ApplicationPlugin.cells(&o);
        assert_eq!(cells[4], text("https://charts.example.com (+1)"));
        assert_eq!(cells[5], text("redis (+1)"));
        assert_eq!(cells[6], text("18.1.0 (+1)"));

        let op = sync_operation(&o).unwrap();
        assert_eq!(op["sync"]["revisions"], json!(["18.1.0", "HEAD"]));
        assert_eq!(op["sync"]["sources"].as_array().unwrap().len(), 2);
        assert!(op["sync"].get("source").is_none());
    }

    #[test]
    fn details() {
        let sections = ApplicationPlugin.details(&app());
        let overview = pairs(&sections, "Overview");
        assert!(overview.contains(&pair("Synced revision", "4f2c1a9")));
        assert!(overview.contains(&pair("Auto-sync", "Yes (prune: Yes, self-heal: No)")));
        let op = pairs(&sections, "Operation");
        assert!(op.contains(&pair("Phase", "Failed")));
        assert!(op.contains(&pair("Initiated by", "automated")));
        assert_eq!(
            pairs(&sections, "Sync result"),
            [pair(
                "Deployment guestbook/guestbook-ui",
                "SyncFailed: field is immutable"
            )]
        );
        let summary = pairs(&sections, "Resources summary");
        assert!(summary.contains(&pair("Total", "2")));
        assert!(summary.contains(&pair("OutOfSync", "1")));
        assert!(summary.contains(&pair("Healthy", "1")));
        let SectionBody::Conditions(c) = section(&sections, "Conditions") else {
            panic!("conditions expected");
        };
        assert_eq!(c[0].type_, "SyncError");
        assert_eq!(
            pairs(&sections, "Images"),
            [pair("gcr.io/heptio-images/ks-guestbook-demo:0.2", "")]
        );
        assert!(sections.iter().all(|s| s.title != "Sync history"));
    }

    #[test]
    fn sync_history() {
        let mut app = app();
        app.data["status"]["history"] = json!([
            {"id": 0, "revision": "1a2b3c", "deployedAt": "2026-10-01T10:00:00Z",
             "initiatedBy": {"username": "admin"}},
            {"id": 1, "revisions": ["4f2c1a9", "9.1.0"], "deployedAt": "2026-10-02T10:00:00Z",
             "initiatedBy": {"automated": true}}
        ]);
        let sections = ApplicationPlugin.details(&app);
        assert_eq!(
            pairs(&sections, "Sync history"),
            [
                pair("#1 2026-10-02T10:00:00Z", "4f2c1a9, 9.1.0 by automated"),
                pair("#0 2026-10-01T10:00:00Z", "1a2b3c by admin"),
            ]
        );
    }

    #[test]
    fn relations() {
        let rels = ApplicationPlugin.relations(&app());
        let targets: Vec<_> = rels
            .iter()
            .map(|r| (r.target.api_version.as_str(), r.target.kind.as_str()))
            .collect();
        assert_eq!(
            targets,
            [
                ("argoproj.io/v1alpha1", "AppProject"),
                ("v1", "Namespace"),
                ("v1", "Service"),
                ("apps/v1", "Deployment"),
            ]
        );
        assert!(rels.iter().all(|r| r.kind == RelationKind::References));
        assert_eq!(rels[3].target.namespace.as_deref(), Some("guestbook"));

        // Remote clusters: only the project.
        let mut o = app();
        o.data["spec"]["destination"] = json!({"name": "prod", "namespace": "guestbook"});
        assert_eq!(ApplicationPlugin.relations(&o).len(), 1);
    }

    #[test]
    fn sync_operation_matches_argocd_server() {
        let op = sync_operation(&app()).unwrap();
        assert_eq!(
            op,
            json!({
                "sync": {
                    "source": {
                        "repoURL": "https://github.com/argoproj/argocd-example-apps.git",
                        "path": "guestbook",
                        "targetRevision": "main"
                    },
                    "revision": "main",
                    "syncOptions": ["CreateNamespace=true"]
                },
                "initiatedBy": {"username": "kubach"},
                "retry": {"limit": 2}
            })
        );

        let mut busy = app();
        busy.data["operation"] = json!({"sync": {}});
        assert!(sync_operation(&busy).is_err());
        assert_eq!(
            ApplicationPlugin.actions_for(&busy),
            [REFRESH_ACTION, HARD_REFRESH_ACTION, DELETE_ACTION]
        );
        assert_eq!(
            sync_health(&busy),
            Health::new(HealthStatus::Progressing, "Syncing")
        );
        assert!(ApplicationPlugin.actions_for(&app()).contains(&SYNC_ACTION));
        assert!(
            ApplicationPlugin
                .actions()
                .iter()
                .any(|a| a.id == action::SYNC && a.dangerous)
        );
    }

    fn appset() -> DynamicObject {
        obj(json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "ApplicationSet",
            "metadata": {"name": "guestbook", "namespace": "argocd"},
            "spec": {
                "generators": [{"list": {"elements": [{"cluster": "a"}]}}, {"git": {}}],
                "template": {
                    "metadata": {"name": "{{cluster}}-guestbook"},
                    "spec": {
                        "project": "default",
                        "source": {"repoURL": "https://git.example.com/apps.git", "path": "guestbook"},
                        "destination": {"server": "https://kubernetes.default.svc", "namespace": "guestbook"}
                    }
                }
            },
            "status": {
                "conditions": [
                    {"type": "ErrorOccurred", "status": "False", "reason": "ApplicationSetUpToDate"},
                    {"type": "ResourcesUpToDate", "status": "True", "reason": "ApplicationSetUpToDate",
                     "message": "All applications have been generated successfully"}
                ],
                "resources": [
                    {"group": "argoproj.io", "version": "v1alpha1", "kind": "Application",
                     "namespace": "argocd", "name": "a-guestbook", "status": "Synced",
                     "health": {"status": "Healthy"}}
                ]
            }
        }))
    }

    #[test]
    fn applicationset() {
        let p = ApplicationSetPlugin;
        let o = appset();
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(p.health(&o).status, HealthStatus::Healthy);
        assert_eq!(cells[3], text("list, git"));
        assert_eq!(cells[4], Cell::Number(1));
        let sections = p.details(&o);
        assert!(
            pairs(&sections, "Overview").contains(&pair("Destination", "in-cluster/guestbook"))
        );
        assert_eq!(
            pairs(&sections, "Applications"),
            [pair("a-guestbook", "Synced / Healthy")]
        );
        let rels = p.relations(&o);
        assert_eq!(rels[0].target.kind, "Application");

        let mut broken = appset();
        broken.data["status"]["conditions"][0] =
            json!({"type": "ErrorOccurred", "status": "True", "message": "bad template"});
        assert_eq!(
            p.health(&broken),
            Health::new(HealthStatus::Degraded, "bad template")
        );
    }
}
