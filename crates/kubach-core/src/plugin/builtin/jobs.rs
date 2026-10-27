//! Job and CronJob plugins.

use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind, Patch, PostParams};
use serde_json::{Value, json};

use super::pod::security_notes;
use super::workloads::related;
use super::{
    DELETE_ACTION, OVERVIEW, PORT_FORWARD_ACTION, action, api_for, col, creation, decode,
    decode_or_err, delete, fallback_cells, kv, name_ns_cells, name_ns_columns, or_none, pair,
    patch, push_kv, push_notes, relations_with, section, selector_string, selects, text, time_str,
    unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, RelatedFilter,
    RelatedObjects, RelatedOrder, Relation, RelationKind, ResourcePlugin, SectionBody,
    SelectorRelation, format_age,
};
use crate::security;

pub struct JobPlugin;
pub struct CronJobPlugin;

/// Job conditions that report a problem when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 2] = ["Failed", "FailureTarget"];

const SUSPEND_ACTION: Action = Action::api(action::SUSPEND, "Suspend", false);

const RESUME_ACTION: Action = Action::api(action::RESUME, "Resume", false);

/// Creates a Job, so it is confirmed even on one CronJob (#346).
const TRIGGER_ACTION: Action = Action::api(action::TRIGGER, "Trigger now", false).confirmed();

/// Merge patch toggling `spec.suspend` (Job and CronJob).
pub(crate) fn suspend_patch(suspend: bool) -> Value {
    json!({"spec": {"suspend": suspend}})
}

fn is_suspended(obj: &DynamicObject) -> bool {
    obj.data.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true)
}

fn suspend_actions(obj: &DynamicObject) -> Action {
    if is_suspended(obj) {
        RESUME_ACTION
    } else {
        SUSPEND_ACTION
    }
}

// ----------------------------------------------------------------------- Job

fn job_condition<'a>(job: &'a Job, type_: &str) -> Option<&'a str> {
    job.status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .find(|c| c.type_ == type_ && c.status == "True")
        .map(|c| c.message.as_deref().or(c.reason.as_deref()).unwrap_or(""))
}

/// Status like the kubectl `STATUS` column.
pub(crate) fn job_status(job: &Job) -> &'static str {
    if job_condition(job, "Failed").is_some() {
        "Failed"
    } else if job_condition(job, "Complete").is_some() {
        "Complete"
    } else if job.metadata.deletion_timestamp.is_some() {
        "Terminating"
    } else if job_condition(job, "Suspended").is_some()
        || job.spec.as_ref().and_then(|s| s.suspend) == Some(true)
    {
        "Suspended"
    } else if job_condition(job, "FailureTarget").is_some() {
        "FailureTarget"
    } else {
        "Running"
    }
}

pub(crate) fn job_health(job: &Job) -> Health {
    match job_status(job) {
        "Complete" => Health::new(HealthStatus::Healthy, "Complete"),
        "Failed" => Health::new(
            HealthStatus::Failed,
            job_condition(job, "Failed")
                .filter(|m| !m.is_empty())
                .unwrap_or("Failed"),
        ),
        "Suspended" => Health::phrase(HealthStatus::Unknown, msg!("state-suspended")),
        "FailureTarget" => Health::phrase(HealthStatus::Degraded, msg!("state-job-failing")),
        other => Health::new(HealthStatus::Progressing, other),
    }
}

fn completions(job: &Job) -> String {
    let succeeded = job.status.as_ref().and_then(|s| s.succeeded).unwrap_or(0);
    let spec = job.spec.as_ref();
    match (
        spec.and_then(|s| s.completions),
        spec.and_then(|s| s.parallelism).unwrap_or(1),
    ) {
        (Some(c), _) => format!("{succeeded}/{c}"),
        (None, p) if p > 1 => format!("{succeeded}/1 of {p}"),
        _ => format!("{succeeded}/1"),
    }
}

/// How long the job ran, or has been running at `now`.
fn duration_text(job: &Job, now: Timestamp) -> String {
    let status = job.status.as_ref();
    let Some(start) = status.and_then(|s| s.start_time.as_ref()) else {
        return String::new();
    };
    let end = status
        .and_then(|s| s.completion_time.as_ref())
        .map_or(now, |t| t.0);
    format_age(end.as_second() - start.0.as_second())
}

/// The Duration cell: fixed once the job completed; while it runs, its
/// start, so views count the time live.
fn duration(job: &Job) -> Cell {
    let status = job.status.as_ref();
    match status.and_then(|s| s.start_time.as_ref()) {
        Some(start) if status.and_then(|s| s.completion_time.as_ref()).is_none() => {
            Cell::Age(Some(start.0))
        }
        _ => text(duration_text(job, Timestamp::now())),
    }
}

impl JobPlugin {
    fn cells_at(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(job) = decode::<Job>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let health = job_health(&job);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(Health::new(health.status, job_status(&job))),
            text(completions(&job)),
            duration(&job),
            Cell::Age(creation(obj)),
        ]);
        cells
    }
}

impl ResourcePlugin for JobPlugin {
    fn name(&self) -> &'static str {
        "job"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("batch", "v1", "Job")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("status", "Status", ColumnKind::Status),
            col("completions", "Completions", ColumnKind::Text),
            col("duration", "Duration", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        self.cells_at(obj)
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(job) = decode::<Job>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = job.spec.as_ref();
        let status = job.status.clone().unwrap_or_default();
        let num = |v: Option<i32>| v.map(|v| v.to_string()).unwrap_or_else(|| "<none>".into());
        let mut overview = vec![
            Field::text("Status", job_status(&job)),
            Field::text("Completions", completions(&job)),
            Field::text("Parallelism", num(spec.and_then(|s| s.parallelism))),
            Field::text("Backoff limit", num(spec.and_then(|s| s.backoff_limit))),
            Field::text(
                "Pods",
                format!(
                    "{} active, {} succeeded, {} failed",
                    status.active.unwrap_or(0),
                    status.succeeded.unwrap_or(0),
                    status.failed.unwrap_or(0)
                ),
            ),
        ];
        if let Some(start) = &status.start_time {
            overview.push(Field::new("Started", FieldValue::Time(start.0)));
        }
        if let Some(end) = &status.completion_time {
            overview.push(Field::new("Completed", FieldValue::Time(end.0)));
        }
        overview.push(Field::text(
            "Duration",
            or_none(duration_text(&job, Timestamp::now())),
        ));
        if spec.and_then(|s| s.suspend) == Some(true) {
            overview.push(Field::text("Suspend", "Yes"));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(overview))];
        let pod_spec = spec.and_then(|s| s.template.spec.as_ref());
        push_kv(
            &mut sections,
            "Containers",
            pod_spec
                .map(|s| {
                    s.containers
                        .iter()
                        .map(|c| pair(c.name.clone(), c.image.clone().unwrap_or_default()))
                        .collect()
                })
                .unwrap_or_default(),
        );
        push_notes(
            &mut sections,
            "Security",
            pod_spec
                .map(|s| security_notes(security::check_pod_spec(s)))
                .unwrap_or_default(),
        );
        sections.extend(related(
            obj,
            "v1",
            "Pod",
            spec.and_then(|s| s.selector.as_ref()),
            true,
            RelatedOrder::Newest,
        ));
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            SUSPEND_ACTION,
            RESUME_ACTION,
            PORT_FORWARD_ACTION,
            DELETE_ACTION,
        ]
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        let finished =
            decode::<Job>(obj).is_some_and(|j| matches!(job_status(&j), "Complete" | "Failed"));
        if finished {
            vec![DELETE_ACTION]
        } else {
            vec![suspend_actions(obj), PORT_FORWARD_ACTION, DELETE_ACTION]
        }
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
                action::SUSPEND | action::RESUME => {
                    let p = suspend_patch(*action_id == action::SUSPEND);
                    patch::<Job>(client, obj, &Patch::Merge(p)).await
                }
                action::DELETE => delete::<Job>(client, obj).await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn selector_relations(&self, obj: &DynamicObject) -> Vec<SelectorRelation> {
        decode::<Job>(obj)
            .and_then(|j| j.spec?.selector)
            .and_then(|s| selector_string(&s))
            .map(|s| vec![selects(obj, "v1", "Pod", s)])
            .unwrap_or_default()
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Job>(obj) {
            Some(job) => job_health(&job),
            None => GenericPlugin.health(obj),
        }
    }
}

// ------------------------------------------------------------------- CronJob

/// Builds a Job from the CronJob template, like
/// `kubectl create job --from=cronjob/<name>`. The name is generated by the API
/// server from `<cronjob>-manual-`.
pub(crate) fn job_from_cronjob(cj: &CronJob) -> Result<Job, PluginError> {
    let name = cj
        .metadata
        .name
        .clone()
        .ok_or_else(|| PluginError::InvalidObject("CronJob has no name".into()))?;
    let uid = cj
        .metadata
        .uid
        .clone()
        .ok_or_else(|| PluginError::InvalidObject("CronJob has no uid".into()))?;
    let template = &cj.spec.job_template;
    let template_meta = template.metadata.clone().unwrap_or_default();
    let mut annotations = template_meta.annotations.unwrap_or_default();
    annotations.insert("cronjob.kubernetes.io/instantiate".into(), "manual".into());
    // Job names are used as label values, so keep them within 63 characters.
    let prefix: String = name.chars().take(52).collect();
    Ok(Job {
        metadata: ObjectMeta {
            generate_name: Some(format!("{prefix}-manual-")),
            namespace: cj.metadata.namespace.clone(),
            labels: template_meta.labels,
            annotations: Some(annotations),
            owner_references: Some(vec![OwnerReference {
                api_version: "batch/v1".into(),
                kind: "CronJob".into(),
                name,
                uid,
                controller: Some(true),
                block_owner_deletion: Some(true),
            }]),
            ..Default::default()
        },
        spec: template.spec.clone(),
        status: None,
    })
}

pub(crate) fn cronjob_health(cj: &CronJob) -> Health {
    if cj.spec.suspend == Some(true) {
        return Health::phrase(HealthStatus::Unknown, msg!("state-suspended"));
    }
    let status = cj.status.clone().unwrap_or_default();
    let active = status.active.as_ref().map_or(0, Vec::len);
    if active > 0 {
        return Health::phrase(
            HealthStatus::Progressing,
            msg!("state-cronjob-active", count = active),
        );
    }
    match (status.last_schedule_time, status.last_successful_time) {
        (Some(s), Some(ok)) if ok.0 < s.0 => {
            Health::phrase(HealthStatus::Degraded, msg!("state-cronjob-last-failed"))
        }
        (Some(_), None) => Health::phrase(
            HealthStatus::Degraded,
            msg!("state-cronjob-never-succeeded"),
        ),
        (None, _) => Health::phrase(HealthStatus::Healthy, msg!("state-cronjob-not-scheduled")),
        _ => Health::phrase(HealthStatus::Healthy, msg!("state-cronjob-scheduled")),
    }
}

impl CronJobPlugin {
    fn cells_at(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(cj) = decode::<CronJob>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = cj.spec.clone();
        let status = cj.status.clone().unwrap_or_default();
        let mut cells = name_ns_cells(obj);
        cells.extend([
            text(spec.schedule.clone()),
            text(or_none(spec.time_zone.clone().unwrap_or_default())),
            text(if spec.suspend == Some(true) {
                "True"
            } else {
                "False"
            }),
            Cell::Number(status.active.as_ref().map_or(0, Vec::len) as i64),
            Cell::Age(status.last_schedule_time.map(|t| t.0)),
            Cell::Status(cronjob_health(&cj)),
            Cell::Age(creation(obj)),
        ]);
        cells
    }
}

impl ResourcePlugin for CronJobPlugin {
    fn name(&self) -> &'static str {
        "cronjob"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("batch", "v1", "CronJob")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("schedule", "Schedule", ColumnKind::Text),
            col("timezone", "Timezone", ColumnKind::Text),
            col("suspend", "Suspend", ColumnKind::Text),
            col("active", "Active", ColumnKind::Number),
            col("last_schedule", "Last schedule", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        self.cells_at(obj)
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(cj) = decode::<CronJob>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = cj.spec.clone();
        let status = cj.status.clone().unwrap_or_default();
        let num = |v: Option<i32>| v.map(|v| v.to_string()).unwrap_or_else(|| "<none>".into());
        let mut sections = vec![kv(
            "Overview",
            vec![
                pair("Schedule", spec.schedule.clone()),
                pair(
                    "Timezone",
                    or_none(spec.time_zone.clone().unwrap_or_default()),
                ),
                pair("Suspend", (spec.suspend == Some(true)).to_string()),
                pair(
                    "Concurrency policy",
                    spec.concurrency_policy
                        .clone()
                        .unwrap_or_else(|| "Allow".into()),
                ),
                pair(
                    "Starting deadline",
                    spec.starting_deadline_seconds
                        .map(|s| format!("{s}s"))
                        .unwrap_or_else(|| "<none>".into()),
                ),
                pair(
                    "History (successful)",
                    num(spec.successful_jobs_history_limit),
                ),
                pair("History (failed)", num(spec.failed_jobs_history_limit)),
                pair(
                    "Last schedule",
                    or_none(time_str(status.last_schedule_time.as_ref())),
                ),
                pair(
                    "Last successful",
                    or_none(time_str(status.last_successful_time.as_ref())),
                ),
            ],
        )];
        push_kv(
            &mut sections,
            "Active jobs",
            status
                .active
                .iter()
                .flatten()
                .filter_map(|r| r.name.clone())
                .map(|n| pair(n, "Running"))
                .collect(),
        );
        let pod_spec = spec
            .job_template
            .spec
            .as_ref()
            .and_then(|s| s.template.spec.as_ref());
        push_notes(
            &mut sections,
            "Security",
            pod_spec
                .map(|s| security_notes(security::check_pod_spec(s)))
                .unwrap_or_default(),
        );
        sections.push(section(
            "Jobs",
            SectionBody::Related(RelatedObjects {
                api_version: "batch/v1".into(),
                kind: "Job".into(),
                namespace: obj.metadata.namespace.clone(),
                filter: RelatedFilter::Selector {
                    labels: None,
                    owner_uid: obj.metadata.uid.clone(),
                },
                order: RelatedOrder::Newest,
            }),
        ));
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![TRIGGER_ACTION, SUSPEND_ACTION, RESUME_ACTION, DELETE_ACTION]
    }

    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        vec![TRIGGER_ACTION, suspend_actions(obj), DELETE_ACTION]
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
                action::TRIGGER => {
                    let job = job_from_cronjob(&decode_or_err::<CronJob>(obj)?)?;
                    let job: DynamicObject = serde_json::to_value(&job)
                        .and_then(serde_json::from_value)
                        .map_err(|e| PluginError::InvalidObject(e.to_string()))?;
                    api_for::<Job>(client, obj)
                        .create(&PostParams::default(), &job)
                        .await?;
                    Ok(())
                }
                action::SUSPEND | action::RESUME => {
                    let p = suspend_patch(*action_id == action::SUSPEND);
                    patch::<CronJob>(client, obj, &Patch::Merge(p)).await
                }
                action::DELETE => delete::<CronJob>(client, obj).await,
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let active = decode::<CronJob>(obj)
            .and_then(|c| c.status?.active)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                Some(Relation {
                    kind: RelationKind::References,
                    target: ObjectRef {
                        api_version: r.api_version.unwrap_or_else(|| "batch/v1".into()),
                        kind: r.kind.unwrap_or_else(|| "Job".into()),
                        name: r.name?,
                        namespace: r.namespace.or_else(|| obj.metadata.namespace.clone()),
                    },
                })
            })
            .collect();
        relations_with(obj, active)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<CronJob>(obj) {
            Some(cj) => cronjob_health(&cj),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, ts};

    fn job(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {"name": "migrate", "namespace": "default"},
            "spec": {
                "completions": 1, "backoffLimit": 3,
                "selector": {"matchLabels": {"batch.kubernetes.io/controller-uid": "abc"}},
                "template": {"spec": {"restartPolicy": "Never",
                    "containers": [{"name": "m", "image": "migrate:1"}]}}
            },
            "status": status
        }))
    }

    #[test]
    fn job_complete() {
        let o = job(json!({
            "startTime": "2026-10-22T11:00:00Z", "completionTime": "2026-10-22T11:03:12Z",
            "succeeded": 1,
            "conditions": [{"type": "Complete", "status": "True"}]
        }));
        let p = JobPlugin;
        let cells = p.cells_at(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[2],
            Cell::Status(Health::new(HealthStatus::Healthy, "Complete"))
        );
        assert_eq!(cells[3], text("1/1"));
        assert_eq!(cells[4], text("3m12s"));
        let ids: Vec<_> = p.actions_for(&o).iter().map(|a| a.id).collect();
        assert_eq!(ids, ["delete"]);
        assert_eq!(p.selector_relations(&o)[0].target_kind, "Pod");
    }

    #[test]
    fn job_running_failed_suspended() {
        let p = JobPlugin;
        let running = job(json!({"startTime": "2026-10-22T11:59:00Z", "active": 1}));
        let cells = p.cells_at(&running);
        assert_eq!(cells[3], text("0/1"));
        assert_eq!(cells[4], Cell::Age(Some(ts("2026-10-22T11:59:00Z"))));
        assert_eq!(p.health(&running).status, HealthStatus::Progressing);
        assert_eq!(p.actions_for(&running)[0].id, "suspend");

        let failed = job(json!({"failed": 4, "conditions": [{
            "type": "Failed", "status": "True", "reason": "BackoffLimitExceeded",
            "message": "Job has reached the specified backoff limit"
        }]}));
        assert_eq!(
            p.health(&failed),
            Health::new(
                HealthStatus::Failed,
                "Job has reached the specified backoff limit"
            )
        );

        let mut suspended = job(json!({}));
        suspended.data["spec"]["suspend"] = json!(true);
        assert_eq!(p.health(&suspended).message, "Suspended");
        assert_eq!(p.actions_for(&suspended)[0].id, "resume");
        assert_eq!(suspend_patch(false), json!({"spec": {"suspend": false}}));
    }

    fn cronjob(suspend: bool, status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": {"name": "backup", "namespace": "ops", "uid": "cj-uid"},
            "spec": {
                "schedule": "*/5 * * * *", "suspend": suspend,
                "jobTemplate": {
                    "metadata": {"labels": {"app": "backup"}},
                    "spec": {"template": {"spec": {"restartPolicy": "OnFailure",
                        "containers": [{"name": "b", "image": "backup:2"}]}}}
                }
            },
            "status": status
        }))
    }

    #[test]
    fn cronjob_cells_and_health() {
        let o = cronjob(
            false,
            json!({
                "lastScheduleTime": "2026-10-22T11:55:00Z",
                "lastSuccessfulTime": "2026-10-22T11:55:30Z",
                "active": [{"apiVersion": "batch/v1", "kind": "Job", "name": "backup-1"}]
            }),
        );
        let p = CronJobPlugin;
        let cells = p.cells_at(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[2], text("*/5 * * * *"));
        assert_eq!(cells[3], text("<none>"));
        assert_eq!(cells[4], text("False"));
        assert_eq!(cells[5], Cell::Number(1));
        assert_eq!(cells[6], Cell::Age(Some(ts("2026-10-22T11:55:00Z"))));
        assert_eq!(p.health(&o).status, HealthStatus::Progressing);
        let rels = p.relations(&o);
        assert_eq!(rels[0].target.name, "backup-1");
        assert_eq!(rels[0].target.namespace.as_deref(), Some("ops"));
        assert!(pairs(&p.details(&o), "Active jobs").contains(&pair("backup-1", "Running")));

        let failing = cronjob(
            false,
            json!({"lastScheduleTime": "2026-10-22T11:55:00Z",
                   "lastSuccessfulTime": "2026-10-22T11:50:00Z"}),
        );
        assert_eq!(p.health(&failing).status, HealthStatus::Degraded);

        let suspended = cronjob(true, json!({}));
        assert_eq!(p.cells_at(&suspended)[4], text("True"));
        assert_eq!(p.health(&suspended).message, "Suspended");
        let ids: Vec<_> = p.actions_for(&suspended).iter().map(|a| a.id).collect();
        assert_eq!(ids, ["trigger", "resume", "delete"]);
    }

    #[test]
    fn job_from_cronjob_template() {
        let cj: CronJob = decode(&cronjob(false, json!({}))).unwrap();
        let job = job_from_cronjob(&cj).unwrap();
        assert_eq!(
            job.metadata.generate_name.as_deref(),
            Some("backup-manual-")
        );
        assert_eq!(job.metadata.namespace.as_deref(), Some("ops"));
        assert_eq!(
            job.metadata.labels.unwrap().get("app").map(String::as_str),
            Some("backup")
        );
        assert_eq!(
            job.metadata.annotations.unwrap()["cronjob.kubernetes.io/instantiate"],
            "manual"
        );
        let owner = &job.metadata.owner_references.unwrap()[0];
        assert_eq!(owner.kind, "CronJob");
        assert_eq!(owner.uid, "cj-uid");
        assert_eq!(owner.controller, Some(true));
        let spec = job.spec.unwrap();
        assert_eq!(
            spec.template.spec.unwrap().containers[0].image.as_deref(),
            Some("backup:2")
        );

        let mut no_uid = cj.clone();
        no_uid.metadata.uid = None;
        assert!(matches!(
            job_from_cronjob(&no_uid),
            Err(PluginError::InvalidObject(_))
        ));
    }
}
