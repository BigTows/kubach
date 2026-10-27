//! Actions on objects, asked from a list's bulk bar or a details panel
//! (#363, #394): confirmation with the kubectl equivalent, the context
//! check (#344), a run on the I/O runtime and its toast. A view of its own
//! so that every screen that shows [`ResourceDetails`] (the browser, the
//! topology) acts on objects the same way.

use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use kubach_core::ResourceKind;
use kubach_core::plugin::builtin::action as action_id;
use kubach_core::plugin::builtin::delete_used_by_note as pvc_note;
use kubach_core::plugin::bulk::{self, BulkSummary, Confirmation};
use kubach_core::plugin::{Action, ResourcePlugin};
use kubach_core::runtime;
use kubach_i18n::{msg, t};
use kubach_ui::{ActionToast, BulkResult, ConfirmDialog};
use kube::api::DynamicObject;

use super::details::ResourceDetails;
use crate::action_context::{self, OpenConfirms};
use crate::cluster::Cluster;
use crate::screens::plugin_text;

/// What an action runs on: the kind, its plugin and its plural title.
/// The listed kind, or another one the details show in place (a pod of
/// the selected Deployment, #393).
#[derive(Clone)]
pub(in crate::screens) struct Subject {
    pub(super) kind: ResourceKind,
    pub(super) plugin: Arc<dyn ResourcePlugin>,
    /// `Deployments`.
    pub(super) title: String,
}

impl Subject {
    pub(super) fn new(kind: ResourceKind, plugin: Arc<dyn ResourcePlugin>, title: String) -> Self {
        Self {
            kind,
            plugin,
            title,
        }
    }

    /// An object of `kind` shown outside a list of its kind (in place in
    /// the details, on the topology): titled by its kind.
    pub(in crate::screens) fn of(kind: &ResourceKind, plugin: &Arc<dyn ResourcePlugin>) -> Self {
        Self::new(kind.clone(), plugin.clone(), format!("{}s", kind.api.kind))
    }
}

/// What an action asked for on an object of `kind` runs on: the listed
/// kind (`listed`, titled as the list is) when the object is of it, else
/// the object's own kind (a pod shown in place over a Deployment, any
/// object on the topology).
pub(super) fn subject_for(
    listed: Option<Subject>,
    kind: &ResourceKind,
    plugin: &Arc<dyn ResourcePlugin>,
) -> Subject {
    match listed {
        Some(listed) if listed.kind.api == kind.api => listed,
        _ => Subject::of(kind, plugin),
    }
}

/// Where the objects of an action are, for the messages that name it
/// (`$scope`, `$namespace`, `$namespaces` of `action-confirm-many`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::screens) struct Where {
    scope: &'static str,
    namespace: String,
    namespaces: usize,
}

impl Where {
    /// Cluster-scoped objects: messages name no namespace.
    pub(in crate::screens) fn cluster() -> Self {
        Self {
            scope: "cluster",
            namespace: String::new(),
            namespaces: 0,
        }
    }

    /// Objects of every namespace.
    pub(in crate::screens) fn any() -> Self {
        Self {
            scope: "any",
            ..Self::cluster()
        }
    }

    /// Objects of `namespace`.
    pub(in crate::screens) fn one(namespace: impl Into<String>) -> Self {
        Self {
            scope: "one",
            namespace: namespace.into(),
            namespaces: 1,
        }
    }

    /// Objects of `count` namespaces.
    pub(in crate::screens) fn many(count: usize) -> Self {
        Self {
            scope: "many",
            namespaces: count,
            ..Self::cluster()
        }
    }

    /// The namespace of an object, if it has one.
    pub(in crate::screens) fn namespace(namespace: Option<&str>) -> Self {
        namespace.map_or_else(Self::cluster, Self::one)
    }
}

/// Names of `objects` when the view lists namespaces.
fn namespace_names(subject: &Subject, objects: &[Arc<DynamicObject>]) -> Option<Vec<String>> {
    let api = &subject.kind.api;
    kubach_core::namespace::is_namespace_kind(&api.group, &api.kind).then(|| {
        objects
            .iter()
            .filter_map(|object| object.metadata.name.clone())
            .collect()
    })
}

/// A bulk action running (or finished) over checked objects.
/// An action running over objects, shown as a toast.
pub(super) struct BulkRun {
    pub(super) toast: ActionToast,
    done: usize,
    pub(super) finished: bool,
    _task: Task<()>,
}

impl BulkRun {
    pub(super) fn new(title: String, total: usize, task: Task<()>, cx: &mut App) -> Self {
        Self {
            toast: ActionToast::show_in_active_window(title, total, cx),
            done: 0,
            finished: false,
            _task: task,
        }
    }

    pub(super) fn running(&self) -> bool {
        !self.finished
    }

    pub(super) fn progress(&mut self, cx: &mut App) {
        self.done += 1;
        self.toast.set_done(self.done, cx);
    }

    pub(super) fn finish(&mut self, summary: &BulkSummary, cx: &mut App) {
        self.finished = true;
        self.toast.finish(bulk_result(summary), cx);
    }
}

/// What a finished run shows: which objects failed and why.
pub(super) fn bulk_result(summary: &BulkSummary) -> BulkResult {
    let hint = summary
        .any_forbidden()
        .then(|| t!("action-rbac-hint").into());
    BulkResult {
        succeeded: summary.succeeded,
        failures: summary
            .failures
            .iter()
            .map(|f| (f.object.clone().into(), f.reason.to_string().into()))
            .collect(),
        hint,
    }
}

/// Runs actions on objects for one screen; its details panel names the
/// pods that still mount a claim, and what a namespace holds, in the
/// confirmations. Notifies when a run starts or ends.
pub(in crate::screens) struct ObjectActions {
    pub(super) cluster: Entity<Cluster>,
    details: WeakEntity<ResourceDetails>,
    /// The last run; a new one waits until it ends.
    pub(super) bulk: Option<BulkRun>,
    /// Confirmations open, closed when their context is no longer active.
    pub(super) confirms: OpenConfirms,
    _cluster: Subscription,
}

impl ObjectActions {
    pub(in crate::screens) fn new(
        cluster: Entity<Cluster>,
        details: &Entity<ResourceDetails>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&cluster, |this: &mut Self, _, cx| {
            this.confirms.close_stale(&this.cluster, cx)
        });
        Self {
            cluster,
            details: details.downgrade(),
            bulk: None,
            confirms: OpenConfirms::default(),
            _cluster: subscription,
        }
    }

    /// An action runs: buttons that start another are off.
    pub(in crate::screens) fn running(&self) -> bool {
        self.bulk.as_ref().is_some_and(BulkRun::running)
    }

    /// Asks for confirmation if `action` needs it, then runs it on
    /// `objects` (the checked ones, or the one in the details).
    pub(in crate::screens) fn request_action(
        &mut self,
        action: Action,
        objects: Vec<Arc<DynamicObject>>,
        subject: Subject,
        where_: Where,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if objects.is_empty() {
            return;
        }
        // The action runs in the connection it was asked in, or not at all.
        let Some(connection) = self.cluster.read(cx).connection().cloned() else {
            return;
        };
        let count = objects.len();
        let noun = subject.title.clone();
        // Context security settings (#39) cover only the MCP agent so far;
        // pass their strict flag here once one applies to the user.
        let strict = false;
        let confirmation = bulk::confirmation(&action, count, strict);
        if confirmation == Confirmation::None {
            self.run_bulk(action, objects, connection, subject, cx);
            return;
        }
        let labels: Vec<String> = objects.iter().map(|o| bulk::object_label(o)).collect();
        let action_title = plugin_text::action_title(&action);
        let message = if action.id == action_id::DELETE {
            let mut message = match labels.as_slice() {
                [one] => t!("action-delete-one", object = one),
                _ => t!(
                    "action-delete-many",
                    count = count,
                    title = &noun,
                    scope = where_.scope,
                    namespace = &where_.namespace,
                    namespaces = where_.namespaces
                ),
            };
            if let Some(note) = self.claims_in_use_note(&subject, &objects, cx) {
                message.push(' ');
                message.push_str(&note);
            }
            if let Some(names) = namespace_names(&subject, &objects)
                && let Some(details) = self.details.upgrade()
            {
                message.push(' ');
                message.push_str(&details.read(cx).namespaces_delete_note(&names));
            }
            message
        } else if action.id == action_id::EVICT {
            bulk::evict_note(&objects).to_string()
        } else if let Some(note) = bulk::action_note(action.id, &objects) {
            note.to_string()
        } else if let [one] = labels.as_slice() {
            t!("action-confirm-one", action = &action_title, object = one)
        } else {
            t!(
                "action-confirm-many",
                action = &action_title,
                count = count,
                title = &noun,
                scope = where_.scope,
                namespace = &where_.namespace,
                namespaces = where_.namespaces
            )
        };
        // The kubectl equivalent (#346), in the context it runs in.
        let lines = bulk::action_commands(&subject.kind.api, action.id, &objects);
        let command = (!lines.is_empty()).then(|| {
            lines
                .iter()
                .map(|line| action_context::kubectl_in_context(line, &connection.context))
                .collect::<Vec<_>>()
                .join("\n")
        });
        // One object (the details panel) is named in the title.
        let title = match objects.as_slice() {
            [one] => t!(
                "action-confirm-title-one",
                action = &action_title,
                kind = &subject.kind.api.kind,
                name = one.metadata.name.as_deref().unwrap_or_default()
            ),
            _ => t!(
                "action-confirm-title-many",
                action = &action_title,
                count = count,
                title = &noun
            ),
        };
        let confirm_label = if count == 1 {
            action_title.clone()
        } else {
            t!(
                "action-confirm-label-many",
                action = &action_title,
                count = count
            )
        };
        // A system namespace (kube-node-lease) is deleted only after typing
        // its name, or the count when several go.
        let typed = match namespace_names(&subject, &objects) {
            Some(names) if action.id == action_id::DELETE => {
                let system: Vec<&String> = names
                    .iter()
                    .filter(|name| kubach_core::namespace::is_system(name))
                    .collect();
                match (system.as_slice(), names.len()) {
                    ([], _) => None,
                    ([name], 1) => Some((*name).clone()),
                    _ => Some(count.to_string()),
                }
            }
            _ => None,
        };
        let actions = cx.entity().downgrade();
        let confirm_connection = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |window, cx| {
            let confirm_action = action.clone();
            let dialog = ConfirmDialog::new(title)
                .context(confirm_connection.context.clone())
                .message(message)
                .items(if count > 1 { labels } else { Vec::new() })
                .confirm_label(confirm_label);
            let dialog = match command {
                Some(command) => dialog.command(command),
                None => dialog,
            };
            let dialog = if action.dangerous {
                dialog.danger()
            } else {
                dialog
            };
            let dialog = dialog.on_confirm(move |_, cx| {
                let action = confirm_action.clone();
                let objects = objects.clone();
                let connection = confirm_connection.clone();
                let subject = subject.clone();
                actions
                    .update(cx, |this, cx| {
                        this.run_bulk(action, objects, connection, subject, cx)
                    })
                    .ok();
            });
            match (typed, confirmation) {
                (Some(text), _) => dialog.require_typed(text, window, cx),
                (None, Confirmation::TypedCount) => {
                    dialog.require_typed(count.to_string(), window, cx)
                }
                (None, _) => dialog,
            }
        });
        self.confirms.track(handle, connection);
    }

    /// For a delete of PersistentVolumeClaims: the pods that still mount
    /// them (from the details), or that such claims wait for their pods.
    fn claims_in_use_note(
        &self,
        subject: &Subject,
        objects: &[Arc<DynamicObject>],
        cx: &App,
    ) -> Option<String> {
        let api = &subject.kind.api;
        if !api.group.is_empty() || api.kind != "PersistentVolumeClaim" {
            return None;
        }
        let mut notes = Vec::new();
        let mut unknown = false;
        let details = self.details.upgrade();
        for object in objects {
            match details
                .as_ref()
                .and_then(|d| d.read(cx).claim_users(object))
            {
                Some(users) if !users.is_empty() => {
                    let note = pvc_note(&users).to_string();
                    notes.push(if objects.len() == 1 {
                        note
                    } else {
                        t!(
                            "action-claim-of",
                            object = bulk::object_label(object),
                            note = note
                        )
                    });
                }
                Some(_) => {}
                None => unknown = true,
            }
        }
        if notes.is_empty() && unknown {
            notes.push(t!(
                "action-claim-maybe-used",
                finalizer = kubach_core::plugin::builtin::PVC_PROTECTION
            ));
        }
        (!notes.is_empty()).then(|| notes.join(" "))
    }

    /// Runs `action` on `objects` with bounded concurrency on the I/O
    /// runtime, tracking progress in [`BulkRun`]. Deleted objects leave the
    /// table through the cache. Refused unless `connection`, where the
    /// action was asked, is still the active one (#344).
    fn run_bulk(
        &mut self,
        action: Action,
        objects: Vec<Arc<DynamicObject>>,
        connection: kubach_core::Connection,
        subject: Subject,
        cx: &mut Context<Self>,
    ) {
        if self.bulk.as_ref().is_some_and(BulkRun::running) {
            return;
        }
        if !action_context::still_active(&self.cluster, &connection, cx) {
            return;
        }
        let client = connection.client;
        let plugin = subject.plugin;
        let total = objects.len();
        let title = bulk::run_title(
            plugin_text::action_title(&action),
            total,
            &subject.kind.api.kind,
            &subject.title,
        )
        .to_string();
        let (tx, mut rx) = mpsc::unbounded();
        // Detached: a bulk change goes through every object even if the
        // screen goes away, rather than stopping at a random one (ADR 0011).
        let handle = runtime::spawn_detached(bulk::run_bulk(
            plugin,
            client,
            objects,
            action.id,
            move |_, _| {
                tx.unbounded_send(()).ok();
            },
        ));
        let task = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                let progressed = this.update(cx, |this, cx| {
                    if let Some(run) = &mut this.bulk {
                        run.progress(cx);
                    }
                });
                if progressed.is_err() {
                    return;
                }
            }
            let summary = handle.await.unwrap_or_else(|error| BulkSummary {
                succeeded: 0,
                failures: vec![bulk::BulkFailure {
                    object: "—".into(),
                    reason: msg!("action-run-aborted", error = error.to_string()),
                    forbidden: false,
                }],
            });
            this.update(cx, |this, cx| {
                if let Some(run) = &mut this.bulk {
                    run.finish(&summary, cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.bulk = Some(BulkRun::new(title, total, task, cx));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::ResourceKind;
    use kubach_core::plugin::{PluginRegistry, ResourcePlugin};
    use kube::api::{ApiResource, GroupVersionKind};

    use kubach_i18n::{Locale, msg};

    use super::{Subject, Where, subject_for};

    /// Confirmations name where the objects are, and run titles take the
    /// kind for one object only (21 in Russian is not "one").
    #[test]
    fn messages_name_the_scope_and_agree_in_number() {
        let confirm = |w: Where| {
            msg!(
                "action-confirm-many",
                action = "Delete",
                count = 3,
                title = "Pods",
                scope = w.scope,
                namespace = w.namespace,
                namespaces = w.namespaces
            )
        };
        assert_eq!(confirm(Where::cluster()).english(), "Delete 3 Pods?");
        assert_eq!(
            confirm(Where::namespace(Some("web"))).english(),
            "Delete 3 Pods in namespace web?"
        );
        assert_eq!(
            confirm(Where::many(2)).english(),
            "Delete 3 Pods in 2 namespaces?"
        );
        assert_eq!(
            confirm(Where::any()).translate(Locale::Ru),
            "Delete 3 Pods во всех namespace?"
        );
        let run = |count: usize| {
            msg!(
                "action-run-title",
                count = count,
                action = "Удалить",
                kind = "Pod",
                title = "Pods"
            )
            .translate(Locale::Ru)
        };
        assert_eq!(run(1), "Удалить 1 Pod");
        assert_eq!(run(21), "Удалить 21 Pods");
    }

    fn kind(group: &str, kind: &str) -> (ResourceKind, Arc<dyn ResourcePlugin>) {
        let api = ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", kind));
        let plugin = PluginRegistry::with_builtins().plugin_for_resource(&api);
        (
            ResourceKind {
                api,
                namespaced: true,
                verbs: Vec::new(),
            },
            plugin,
        )
    }

    fn same_plugin(a: &Arc<dyn ResourcePlugin>, b: &Arc<dyn ResourcePlugin>) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
    }

    /// An action of the details runs on the shown object's kind: the
    /// listed one as listed, another (a pod in place, #393) or any object
    /// on the topology (#394) as its own kind with its own plugin.
    #[test]
    fn actions_run_on_the_kind_of_the_shown_object() {
        let (deployments, deployment_plugin) = kind("apps", "Deployment");
        let (pods, pod_plugin) = kind("", "Pod");
        let listed = Subject::new(
            deployments.clone(),
            deployment_plugin.clone(),
            "Deployments (apps)".into(),
        );

        let subject = subject_for(Some(listed.clone()), &deployments, &deployment_plugin);
        assert_eq!(subject.title, "Deployments (apps)");
        assert!(same_plugin(&subject.plugin, &deployment_plugin));

        let subject = subject_for(Some(listed), &pods, &pod_plugin);
        assert_eq!(subject.kind.api.kind, "Pod");
        assert_eq!(subject.title, "Pods");
        assert!(same_plugin(&subject.plugin, &pod_plugin));

        // The topology lists nothing: every object is its own subject.
        let subject = subject_for(None, &deployments, &deployment_plugin);
        assert_eq!(subject.kind.api.kind, "Deployment");
        assert_eq!(subject.title, "Deployments");
        assert!(same_plugin(&subject.plugin, &deployment_plugin));
    }
}
