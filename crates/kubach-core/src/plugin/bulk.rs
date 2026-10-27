//! Running one plugin action over many objects: which actions apply to a
//! whole selection, how the user must confirm them, and a runner with bounded
//! concurrency that turns per-object results into a readable summary.
//!
//! Actions with parameters (scale) are not offered in bulk: one shared value
//! for objects with different current states (e.g. replica counts) is rarely
//! what is meant, and per-object values need a form of their own.

use std::future::Future;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream;
use kubach_i18n::{Message, msg};
use kube::Client;
use kube::api::{ApiResource, DynamicObject};

use super::builtin::{NO_PARAMS, action};
use super::{Action, ActionId, ActionKind, PluginError, ResourcePlugin};

/// Objects processed at once by [`run_bulk`].
pub const DEFAULT_CONCURRENCY: usize = 8;

/// Deleting more objects than this asks to type their count.
pub const TYPED_COUNT_THRESHOLD: usize = 5;

/// Actions offered for every one of `objects`, in the order the plugin lists
/// them for the first object. Actions with parameters and interactive ones
/// (streams: logs, exec, port-forward) are left out.
pub fn common_actions<O: AsRef<DynamicObject>>(
    plugin: &dyn ResourcePlugin,
    objects: &[O],
) -> Vec<Action> {
    let Some((first, rest)) = objects.split_first() else {
        return Vec::new();
    };
    let mut actions: Vec<Action> = plugin
        .actions_for(first.as_ref())
        .into_iter()
        .filter(|a| a.kind == ActionKind::Api)
        .collect();
    for object in rest {
        let available = plugin.actions_for(object.as_ref());
        actions.retain(|a| available.iter().any(|b| b.id == a.id));
    }
    actions.retain(|a| {
        objects
            .iter()
            .all(|object| plugin.action_params(object.as_ref(), &a.id).is_empty())
    });
    actions
}

/// How the user confirms a bulk action before it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// Runs right away.
    None,
    /// A dialog listing the objects.
    List,
    /// The list, plus typing the number of objects.
    TypedCount,
}

/// Confirmation for running `action` on `count` objects. A run on several
/// objects is always confirmed with their list (#346): Cmd-A then Cordon
/// must not cordon every node at once. One object needs it only for
/// dangerous actions and those marked [`Action::confirm_single`]
/// (Trigger now); reversible ones (scale, restart, cordon, suspend) run
/// right away. Deleting more than [`TYPED_COUNT_THRESHOLD`] objects asks to
/// type their count. `strict` is the context's strict safety mode (#39):
/// every action is confirmed and every delete needs the typed count.
pub fn confirmation(action: &Action, count: usize, strict: bool) -> Confirmation {
    let delete = action.id == action::DELETE;
    let several = count > 1 && action.kind == ActionKind::Api;
    if delete && (strict || count > TYPED_COUNT_THRESHOLD) {
        Confirmation::TypedCount
    } else if action.dangerous || action.confirm_single || several || strict {
        Confirmation::List
    } else {
        Confirmation::None
    }
}

/// Title of a run of `action` over `count` objects: `Delete 1 Pod`,
/// `Delete 3 Pods`. `one` is the kind (`Pod`), `many` its plural title.
/// The UI translates it, like the other texts of confirmations here.
pub fn run_title(action: impl Into<String>, count: usize, one: &str, many: &str) -> Message {
    msg!(
        "action-run-title",
        count = count,
        action = action.into(),
        kind = one,
        title = many
    )
}

/// Why an action failed on one object.
#[derive(Clone, Debug, PartialEq)]
pub struct BulkFailure {
    /// `namespace/name` or `name`.
    pub object: String,
    pub reason: Message,
    /// Refused by RBAC (HTTP 403).
    pub forbidden: bool,
}

/// Result of a bulk run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BulkSummary {
    pub succeeded: usize,
    /// Sorted by object.
    pub failures: Vec<BulkFailure>,
}

impl BulkSummary {
    pub fn total(&self) -> usize {
        self.succeeded + self.failures.len()
    }

    /// Whether any failure is an RBAC refusal.
    pub fn any_forbidden(&self) -> bool {
        self.failures.iter().any(|f| f.forbidden)
    }
}

/// Readable reason for a failed action, and whether RBAC refused it.
pub fn describe_error(error: &PluginError) -> (Message, bool) {
    match error {
        PluginError::Kube(kube::Error::Api(status)) => {
            let message = if status.message.is_empty() {
                error.to_string()
            } else {
                status.message.clone()
            };
            if status.is_forbidden() {
                (msg!("action-error-forbidden", error = message), true)
            } else if status.is_not_found() {
                (msg!("action-error-not-found", error = message), false)
            } else {
                (error_text(message), false)
            }
        }
        other => (error_text(other.to_string()), false),
    }
}

/// An error's own text (the API server's, English) as a message.
pub fn error_text(text: impl Into<String>) -> Message {
    msg!("action-error-text", error = text.into())
}

/// Runs `run` on every item, at most `limit` at a time, calling `on_done`
/// as each finishes. Results come back in completion order.
pub async fn run_bounded<T, E, F, Fut>(
    items: Vec<T>,
    limit: usize,
    run: F,
    mut on_done: impl FnMut(&T, &Result<(), E>),
) -> Vec<(T, Result<(), E>)>
where
    T: Clone,
    F: Fn(T) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    let mut results = Vec::with_capacity(items.len());
    let mut pending = stream::iter(items.into_iter().map(|item| {
        let future = run(item.clone());
        async move { (item, future.await) }
    }))
    .buffer_unordered(limit.max(1));
    while let Some((item, result)) = pending.next().await {
        on_done(&item, &result);
        results.push((item, result));
    }
    results
}

/// Turns per-object results into a summary. A delete of an object that is
/// already gone counts as done.
pub fn summarize(
    action_id: ActionId,
    results: Vec<(String, Result<(), PluginError>)>,
) -> BulkSummary {
    let mut summary = BulkSummary::default();
    for (object, result) in results {
        match result {
            Ok(()) => summary.succeeded += 1,
            Err(PluginError::Kube(kube::Error::Api(status)))
                if action_id == action::DELETE && status.is_not_found() =>
            {
                summary.succeeded += 1
            }
            Err(error) => {
                let (reason, forbidden) = describe_error(&error);
                summary.failures.push(BulkFailure {
                    object,
                    reason,
                    forbidden,
                });
            }
        }
    }
    summary.failures.sort_by(|a, b| a.object.cmp(&b.object));
    summary
}

/// `namespace/name`, or `name` for cluster-scoped objects.
pub fn object_label(object: &DynamicObject) -> String {
    let name = object.metadata.name.as_deref().unwrap_or_default();
    match object.metadata.namespace.as_deref() {
        Some(namespace) => format!("{namespace}/{name}"),
        None => name.to_owned(),
    }
}

/// The `kubectl delete` equivalent of deleting `objects` of `api`, shown in
/// confirmation dialogs; `None` when they span several namespaces (no
/// single command does that).
pub fn delete_command<O: AsRef<DynamicObject>>(api: &ApiResource, objects: &[O]) -> Option<String> {
    let first = objects.first()?.as_ref();
    let namespace = first.metadata.namespace.as_deref();
    if objects
        .iter()
        .any(|o| o.as_ref().metadata.namespace.as_deref() != namespace)
    {
        return None;
    }
    let mut command = format!("kubectl delete {}", resource_arg(api));
    for object in objects {
        command.push(' ');
        command.push_str(object.as_ref().metadata.name.as_deref().unwrap_or_default());
    }
    if let Some(namespace) = namespace {
        command.push_str(&format!(" -n {namespace}"));
    }
    Some(command)
}

/// What evicting `objects` (pods) does, for confirmation dialogs.
pub fn evict_note<O: AsRef<DynamicObject>>(objects: &[O]) -> Message {
    match objects {
        [one] => msg!("action-evict-one", object = object_label(one.as_ref())),
        many => msg!("action-evict-many", count = many.len()),
    }
}

/// Command lines shown in confirmations, at most this many.
pub const COMMAND_LINES: usize = 8;

/// The `kubectl` equivalent of running `action_id` on `objects` of `api`,
/// for confirmation dialogs: one line per namespace (or per CronJob for
/// Trigger now), at most [`COMMAND_LINES`] plus a `# … more` comment.
/// Empty when kubectl has no such command (the action has none, or a
/// delete across namespaces, see [`delete_command`]).
pub fn action_commands<O: AsRef<DynamicObject>>(
    api: &ApiResource,
    action_id: ActionId,
    objects: &[O],
) -> Vec<String> {
    let resource = resource_arg(api);
    let mut lines: Vec<String> = match action_id {
        action::DELETE => delete_command(api, objects).into_iter().collect(),
        action::RESTART => by_namespace(objects)
            .into_iter()
            .map(|(namespace, names)| {
                with_namespace(
                    format!("kubectl rollout restart {resource} {}", names.join(" ")),
                    namespace,
                )
            })
            .collect(),
        action::CORDON | action::UNCORDON => {
            let verb = if action_id == action::CORDON {
                "cordon"
            } else {
                "uncordon"
            };
            let names: Vec<&str> = objects.iter().map(|o| name_of(o.as_ref())).collect();
            if names.is_empty() {
                Vec::new()
            } else {
                vec![format!("kubectl {verb} {}", names.join(" "))]
            }
        }
        action::TRIGGER => objects
            .iter()
            .map(|object| {
                let object = object.as_ref();
                let name = name_of(object);
                // The Job gets a generated name (`<name>-manual-xxxxx`);
                // kubectl needs one spelled out.
                let prefix: String = name.chars().take(52).collect();
                with_namespace(
                    format!("kubectl create job --from=cronjob/{name} {prefix}-manual-$(date +%s)"),
                    object.metadata.namespace.as_deref(),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    if lines.len() > COMMAND_LINES {
        let more = lines.len() - COMMAND_LINES;
        lines.truncate(COMMAND_LINES);
        lines.push(format!("# … and {more} more"));
    }
    lines
}

/// What running `action_id` on `objects` does, when the dialog should say
/// more than "<Action> N <kind>?".
pub fn action_note<O: AsRef<DynamicObject>>(action_id: ActionId, objects: &[O]) -> Option<Message> {
    match (action_id, objects) {
        (action::TRIGGER, [one]) => Some(msg!(
            "action-trigger-one",
            object = object_label(one.as_ref())
        )),
        (action::TRIGGER, many) => Some(msg!("action-trigger-many", count = many.len())),
        (action::CORDON, many) if many.len() > 1 => {
            Some(msg!("action-cordon-many", count = many.len()))
        }
        _ => None,
    }
}

/// `kubectl scale` of `object` to `replicas`, for the confirmation of a
/// scale to zero (#347).
pub fn scale_command(api: &ApiResource, object: &DynamicObject, replicas: i32) -> String {
    with_namespace(
        format!(
            "kubectl scale {}/{} --replicas={replicas}",
            resource_arg(api),
            name_of(object)
        ),
        object.metadata.namespace.as_deref(),
    )
}

/// `plural` or `plural.group`, as kubectl takes a resource type.
fn resource_arg(api: &ApiResource) -> String {
    if api.group.is_empty() {
        api.plural.clone()
    } else {
        format!("{}.{}", api.plural, api.group)
    }
}

fn name_of(object: &DynamicObject) -> &str {
    object.metadata.name.as_deref().unwrap_or_default()
}

/// Names of `objects` by namespace, in order of first appearance.
fn by_namespace<O: AsRef<DynamicObject>>(objects: &[O]) -> Vec<(Option<&str>, Vec<&str>)> {
    let mut groups: Vec<(Option<&str>, Vec<&str>)> = Vec::new();
    for object in objects {
        let object = object.as_ref();
        let namespace = object.metadata.namespace.as_deref();
        match groups.iter_mut().find(|(ns, _)| *ns == namespace) {
            Some((_, names)) => names.push(name_of(object)),
            None => groups.push((namespace, vec![name_of(object)])),
        }
    }
    groups
}

fn with_namespace(command: String, namespace: Option<&str>) -> String {
    match namespace {
        Some(namespace) => format!("{command} -n {namespace}"),
        None => command,
    }
}

/// Runs `action_id` on every object with [`DEFAULT_CONCURRENCY`]; `on_done`
/// is called after each object (for progress). Run it on the I/O runtime.
pub async fn run_bulk(
    plugin: Arc<dyn ResourcePlugin>,
    client: Client,
    objects: Vec<Arc<DynamicObject>>,
    action_id: ActionId,
    on_done: impl FnMut(&Arc<DynamicObject>, &Result<(), PluginError>),
) -> BulkSummary {
    let results = run_bounded(
        objects,
        DEFAULT_CONCURRENCY,
        |object| {
            let plugin = plugin.clone();
            let client = client.clone();
            async move {
                plugin
                    .run_action(&client, &object, &action_id, &NO_PARAMS)
                    .await
            }
        },
        on_done,
    )
    .await;
    let results = results
        .into_iter()
        .map(|(object, result)| (object_label(&object), result))
        .collect();
    summarize(action_id, results)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::{CronJobPlugin, DeploymentPlugin, PodPlugin};

    fn object(value: serde_json::Value) -> Arc<DynamicObject> {
        Arc::new(serde_json::from_value(value).unwrap())
    }

    fn cronjob(name: &str, suspended: bool) -> Arc<DynamicObject> {
        object(json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": {"name": name, "namespace": "ns"},
            "spec": {"schedule": "* * * * *", "suspend": suspended,
                     "jobTemplate": {"spec": {"template": {"spec": {"containers": []}}}}}
        }))
    }

    fn ids(actions: &[Action]) -> Vec<ActionId> {
        actions.iter().map(|a| a.id).collect()
    }

    #[test]
    fn run_titles_agree_in_number() {
        assert_eq!(
            run_title("Delete", 1, "Pod", "Pods").english(),
            "Delete 1 Pod"
        );
        assert_eq!(
            run_title("Delete", 3, "Pod", "Pods").english(),
            "Delete 3 Pods"
        );
        assert_eq!(
            run_title("Restart", 0, "Deployment", "Deployments").english(),
            "Restart 0 Deployments"
        );
    }

    #[test]
    fn common_actions_intersect_and_skip_params_and_interactive() {
        let pod = object(json!({
            "apiVersion": "v1", "kind": "Pod", "metadata": {"name": "p", "namespace": "ns"}
        }));
        assert_eq!(
            ids(&common_actions(&PodPlugin, &[pod])),
            [action::EVICT, action::DELETE]
        );

        let deploy = object(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "d", "namespace": "ns"}, "spec": {"replicas": 2}
        }));
        // Scale needs a replica count and is left out.
        assert_eq!(
            ids(&common_actions(&DeploymentPlugin, &[deploy])),
            [action::RESTART, action::DELETE]
        );

        let both = [cronjob("a", false), cronjob("b", true)];
        let common = ids(&common_actions(&CronJobPlugin, &both));
        assert!(!common.contains(&action::SUSPEND));
        assert!(!common.contains(&action::RESUME));
        assert!(common.contains(&action::DELETE));
        let suspended = [cronjob("a", true), cronjob("b", true)];
        assert!(ids(&common_actions(&CronJobPlugin, &suspended)).contains(&action::RESUME));

        let none: [Arc<DynamicObject>; 0] = [];
        assert!(common_actions(&PodPlugin, &none).is_empty());
    }

    #[test]
    fn delete_command_and_evict_note() {
        let pod = |name: &str, ns: &str| {
            object(json!({"apiVersion": "v1", "kind": "Pod",
                          "metadata": {"name": name, "namespace": ns}}))
        };
        let pods = ApiResource::erase::<k8s_openapi::api::core::v1::Pod>(&());
        assert_eq!(
            delete_command(&pods, &[pod("a", "ns"), pod("b", "ns")]).as_deref(),
            Some("kubectl delete pods a b -n ns")
        );
        assert_eq!(
            delete_command(&pods, &[pod("a", "ns"), pod("b", "x")]),
            None
        );
        let deploys = ApiResource::erase::<k8s_openapi::api::apps::v1::Deployment>(&());
        assert_eq!(
            delete_command(&deploys, &[pod("web", "ns")]).as_deref(),
            Some("kubectl delete deployments.apps web -n ns")
        );
        let nodes = ApiResource::erase::<k8s_openapi::api::core::v1::Node>(&());
        let node = object(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "n1"}}));
        assert_eq!(
            delete_command(&nodes, &[node]).as_deref(),
            Some("kubectl delete nodes n1")
        );
        let none: [Arc<DynamicObject>; 0] = [];
        assert_eq!(delete_command(&pods, &none), None);

        assert_eq!(
            evict_note(&[pod("a", "ns")]).english(),
            "Evict pod ns/a via the Eviction API (respects PodDisruptionBudgets)."
        );
        assert!(
            evict_note(&[pod("a", "ns"), pod("b", "ns")])
                .english()
                .starts_with("Evict 2 pods")
        );
    }

    const RESTART: Action = Action::api(action::RESTART, "Restart", false);
    const CORDON: Action = Action::api(action::CORDON, "Cordon", false);
    const TRIGGER: Action = Action::api(action::TRIGGER, "Trigger now", false).confirmed();
    const DELETE: Action = Action::api(action::DELETE, "Delete", true);
    const EVICT: Action = Action::api(action::EVICT, "Evict", true);

    #[test]
    fn confirmation_depends_on_danger_count_and_strict_mode() {
        // One reversible object runs right away; several are listed.
        assert_eq!(confirmation(&RESTART, 1, false), Confirmation::None);
        assert_eq!(confirmation(&CORDON, 1, false), Confirmation::None);
        assert_eq!(confirmation(&RESTART, 2, false), Confirmation::List);
        assert_eq!(confirmation(&RESTART, 50, false), Confirmation::List);
        assert_eq!(confirmation(&CORDON, 3, false), Confirmation::List);
        assert_eq!(confirmation(&RESTART, 1, true), Confirmation::List);
        assert_eq!(confirmation(&EVICT, 1, false), Confirmation::List);
        assert_eq!(confirmation(&EVICT, 20, false), Confirmation::List);
        assert_eq!(confirmation(&DELETE, 1, false), Confirmation::List);
        assert_eq!(confirmation(&DELETE, 5, false), Confirmation::List);
        assert_eq!(confirmation(&DELETE, 6, false), Confirmation::TypedCount);
        assert_eq!(confirmation(&DELETE, 1, true), Confirmation::TypedCount);
    }

    #[test]
    fn creating_actions_are_confirmed_on_one_object() {
        const { assert!(TRIGGER.confirm_single) };
        assert_eq!(confirmation(&TRIGGER, 1, false), Confirmation::List);
        assert_eq!(confirmation(&TRIGGER, 10, false), Confirmation::List);
        let plugin_trigger = CronJobPlugin
            .actions_for(&cronjob("a", false))
            .into_iter()
            .find(|a| a.id == action::TRIGGER)
            .unwrap();
        assert_eq!(confirmation(&plugin_trigger, 1, false), Confirmation::List);
        // Streams are never run in bulk, so the count does not matter.
        let logs = Action::stream(
            ActionId::new("logs"),
            "Logs",
            crate::plugin::StreamKind::Logs,
        );
        assert_eq!(confirmation(&logs, 3, false), Confirmation::None);
    }

    #[test]
    fn action_commands_match_kubectl() {
        let deploy = |name: &str, ns: &str| {
            object(json!({"apiVersion": "apps/v1", "kind": "Deployment",
                          "metadata": {"name": name, "namespace": ns}}))
        };
        let deploys = ApiResource::erase::<k8s_openapi::api::apps::v1::Deployment>(&());
        assert_eq!(
            action_commands(
                &deploys,
                action::RESTART,
                &[deploy("a", "x"), deploy("b", "y"), deploy("c", "x")]
            ),
            [
                "kubectl rollout restart deployments.apps a c -n x",
                "kubectl rollout restart deployments.apps b -n y"
            ]
        );
        assert_eq!(
            action_commands(&deploys, action::DELETE, &[deploy("a", "x")]),
            ["kubectl delete deployments.apps a -n x"]
        );
        assert!(
            action_commands(
                &deploys,
                action::DELETE,
                &[deploy("a", "x"), deploy("b", "y")]
            )
            .is_empty()
        );
        assert!(action_commands(&deploys, action::SCALE, &[deploy("a", "x")]).is_empty());
        assert_eq!(
            scale_command(&deploys, &deploy("a", "x"), 0),
            "kubectl scale deployments.apps/a --replicas=0 -n x"
        );

        let nodes = ApiResource::erase::<k8s_openapi::api::core::v1::Node>(&());
        let node = |name: &str| {
            object(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": name}}))
        };
        assert_eq!(
            action_commands(&nodes, action::CORDON, &[node("n1"), node("n2")]),
            ["kubectl cordon n1 n2"]
        );
        assert_eq!(
            action_commands(&nodes, action::UNCORDON, &[node("n1")]),
            ["kubectl uncordon n1"]
        );

        let cronjobs = ApiResource::erase::<k8s_openapi::api::batch::v1::CronJob>(&());
        assert_eq!(
            action_commands(&cronjobs, action::TRIGGER, &[cronjob("backup", false)]),
            ["kubectl create job --from=cronjob/backup backup-manual-$(date +%s) -n ns"]
        );
        let many: Vec<_> = (0..10).map(|i| cronjob(&format!("c{i}"), false)).collect();
        let lines = action_commands(&cronjobs, action::TRIGGER, &many);
        assert_eq!(lines.len(), COMMAND_LINES + 1);
        assert_eq!(lines.last().unwrap(), "# … and 2 more");
    }

    #[test]
    fn notes_explain_creating_and_cordoning() {
        assert_eq!(
            action_note(action::TRIGGER, &[cronjob("backup", false)])
                .unwrap()
                .english(),
            "Creates a Job from CronJob ns/backup now, outside its schedule."
        );
        assert!(
            action_note(action::TRIGGER, &[cronjob("a", false), cronjob("b", false)])
                .unwrap()
                .english()
                .starts_with("Creates a Job from each of 2 CronJobs")
        );
        assert_eq!(action_note(action::RESTART, &[cronjob("a", false)]), None);
    }

    fn api_error(code: u16, message: &str) -> PluginError {
        PluginError::Kube(kube::Error::Api(Box::new(
            serde_json::from_value(json!({
                "status": "Failure", "code": code, "message": message,
                "reason": if code == 403 { "Forbidden" } else { "NotFound" }
            }))
            .unwrap(),
        )))
    }

    #[test]
    fn summary_explains_rbac_and_ignores_already_deleted() {
        let results = vec![
            (
                "ns/b".to_owned(),
                Err(api_error(403, "pods \"b\" is forbidden")),
            ),
            ("ns/a".to_owned(), Ok(())),
            (
                "ns/c".to_owned(),
                Err(api_error(404, "pods \"c\" not found")),
            ),
            ("ns/d".to_owned(), Err(PluginError::Stream("logs".into()))),
        ];
        let summary = summarize(action::DELETE, results);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.total(), 4);
        assert!(summary.any_forbidden());
        assert_eq!(summary.failures[0].object, "ns/b");
        assert_eq!(
            summary.failures[0].reason.english(),
            "No permission (RBAC): pods \"b\" is forbidden"
        );
        assert_eq!(summary.failures[1].object, "ns/d");
        assert!(!summary.failures[1].forbidden);

        let restart = summarize(
            action::RESTART,
            vec![("ns/c".to_owned(), Err(api_error(404, "gone")))],
        );
        assert_eq!(restart.failures[0].reason.english(), "Not found: gone");
    }

    #[tokio::test]
    async fn runner_bounds_concurrency_and_reports_every_item() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut done = 0;
        let results = run_bounded(
            (0..30).collect(),
            8,
            |item: usize| {
                let running = running.clone();
                let peak = peak.clone();
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    if item.is_multiple_of(10) {
                        Err(item)
                    } else {
                        Ok(())
                    }
                }
            },
            |_, _| done += 1,
        )
        .await;
        assert_eq!(done, 30);
        assert_eq!(results.len(), 30);
        assert_eq!(results.iter().filter(|(_, r)| r.is_err()).count(), 3);
        assert_eq!(peak.load(Ordering::SeqCst), 8);
    }
}
