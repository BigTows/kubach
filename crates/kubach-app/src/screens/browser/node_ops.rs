//! Drain and Shell for a Node, asked from the details panel (#318). Neither is a
//! plugin action: Drain takes options, reports progress per pod and can be
//! cancelled; Shell opens a dock tab. So bulk runs and the MCP agent never
//! get them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use k8s_openapi::api::core::v1::Pod;
use kubach_core::drain::{self, DrainEvent, DrainOptions, DrainReport};
use kubach_core::plugin::bulk::{BulkFailure, BulkSummary};
use kubach_core::streams::node_shell;
use kubach_core::{Connection, runtime};
use kubach_i18n::{msg, t};
use kubach_ui::{ConfirmDialog, ConfirmLive};

use super::actions::{BulkRun, ObjectActions, bulk_result};
use crate::action_context;
use crate::settings::AppSettings;

/// Options of the drain dialog, in the order of its checkboxes.
fn drain_options(checked: &[bool]) -> DrainOptions {
    let flag = |ix: usize| checked.get(ix).copied().unwrap_or(false);
    DrainOptions {
        ignore_daemonsets: flag(0),
        delete_emptydir_data: flag(1),
        force: flag(2),
        ..DrainOptions::default()
    }
}

/// The parts of the drain dialog that follow its options: the command,
/// what happens to each pod, and what stops the drain.
fn drain_live(node: &str, pods: &[Pod], checked: &[bool]) -> ConfirmLive {
    let options = drain_options(checked);
    let plan = drain::plan(pods, &options);
    let blocked: Vec<SharedString> = plan
        .blocked()
        .map(|(key, block)| {
            msg!(
                "drain-plan-line",
                pod = key.label(),
                what = block.describe()
            )
            .to_string()
            .into()
        })
        .collect();
    let notice = if !blocked.is_empty() {
        Some((t!("drain-blocked-notice").into(), blocked))
    } else if options.delete_emptydir_data {
        Some((t!("drain-emptydir-notice").into(), Vec::new()))
    } else {
        None
    };
    ConfirmLive {
        command: Some(drain::drain_command(node, &options).into()),
        items: plan
            .lines()
            .iter()
            .map(|line| line.to_string().into())
            .collect(),
        notice,
        blocked: plan.is_blocked(),
    }
}

/// The phase of a running drain: how many of its `total` pods are left
/// after `done` went (#382).
fn drain_phase(total: usize, done: usize) -> String {
    match total.saturating_sub(done) {
        0 if total == 0 => t!("node-drain-no-pods"),
        0 => t!("node-drain-finishing"),
        left => t!("node-drain-evicting", count = left),
    }
}

/// What a finished drain shows: failures per pod and what stayed.
fn drain_result(report: &DrainReport) -> kubach_ui::BulkResult {
    let mut result = bulk_result(&report.summary);
    if result.hint.is_none() {
        result.hint = drain::skipped_note(&report.skipped).map(|note| note.to_string().into());
    }
    result
}

impl ObjectActions {
    /// Lists the node's pods, then asks with the options and the plan.
    /// The drain runs in the connection it was asked in, or not at all
    /// (#344).
    pub(super) fn request_drain(
        &mut self,
        node: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.cluster.read(cx).connection().cloned() else {
            return;
        };
        let client = connection.client.clone();
        let target = node.clone();
        let listing = runtime::spawn(async move { drain::node_pods(&client, &target, None).await });
        cx.spawn_in(window, async move |this, cx| {
            let (pods, error) = match listing.await {
                Ok(Ok(pods)) => (pods, None),
                Ok(Err(error)) => (Vec::new(), Some(error.to_string())),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
            this.update_in(cx, |this, window, cx| {
                // The context changed while listing: no dialog for it.
                if action_context::still_active(&this.cluster, &connection, cx) {
                    this.confirm_drain(node, pods, error, connection, window, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_drain(
        &mut self,
        node: String,
        pods: Vec<Pod>,
        error: Option<String>,
        connection: Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let timeout = drain::DEFAULT_TIMEOUT.as_secs() / 60;
        let mut message = t!("drain-confirm-message", node = &node, minutes = timeout);
        if let Some(error) = error {
            message.push(' ');
            message.push_str(&t!("drain-list-failed", error = error));
        }
        let actions = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            let live_node = node.clone();
            let context = captured.context.clone();
            let connection = captured.clone();
            ConfirmDialog::new(t!("drain-confirm-title", node = &node))
                .context(context.clone())
                .message(message)
                .confirm_label(t!("drain-confirm"))
                .danger()
                .option(t!("drain-option-ignore-daemonsets"), true)
                .option(t!("drain-option-delete-emptydir"), false)
                .option(t!("drain-option-force"), false)
                .live(move |checked| {
                    let mut live = drain_live(&live_node, &pods, checked);
                    live.command = live.command.map(|command| {
                        action_context::kubectl_in_context(&command, &context).into()
                    });
                    live
                })
                .on_confirm_with_options(move |checked, _, cx| {
                    let options = drain_options(checked);
                    let node = node.clone();
                    let connection = connection.clone();
                    actions
                        .update(cx, |this, cx| this.run_drain(node, options, connection, cx))
                        .ok();
                })
        });
        self.confirms.track(handle, connection);
    }

    /// Drains `node` on the I/O runtime with progress (and Cancel) in a
    /// toast.
    /// Refused unless `connection`, where the drain was asked, is still
    /// the active one (#344).
    fn run_drain(
        &mut self,
        node: String,
        options: DrainOptions,
        connection: Connection,
        cx: &mut Context<Self>,
    ) {
        if self.bulk.as_ref().is_some_and(BulkRun::running) {
            return;
        }
        if !action_context::still_active(&self.cluster, &connection, cx) {
            return;
        }
        let client = connection.client;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = mpsc::unbounded();
        // Detached: a drain goes on (or stops at Cancel), not at a random
        // pod when the screen goes away (ADR 0011).
        let handle = runtime::spawn_detached(drain::drain(
            client,
            node.clone(),
            options,
            cancel.clone(),
            move |event| {
                tx.unbounded_send(event).ok();
            },
        ));
        let task = cx.spawn(async move |this, cx| {
            let (mut total, mut done) = (0, 0);
            while let Some(event) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    if let Some(run) = &mut this.bulk {
                        match event {
                            DrainEvent::Planned { total: planned } => {
                                total = planned;
                                run.toast.set_total(total, cx);
                            }
                            DrainEvent::PodDone => {
                                done += 1;
                                run.progress(cx);
                            }
                        }
                        run.toast.set_phase(drain_phase(total, done), cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            let report = handle.await.unwrap_or_else(|error| DrainReport {
                summary: BulkSummary {
                    succeeded: 0,
                    failures: vec![BulkFailure {
                        object: "—".into(),
                        reason: msg!("node-drain-aborted", error = error.to_string()),
                        forbidden: false,
                    }],
                },
                skipped: Vec::new(),
            });
            this.update(cx, |this, cx| {
                if let Some(run) = &mut this.bulk {
                    run.finished = true;
                    run.toast.finish(drain_result(&report), cx);
                    cx.notify();
                }
            })
            .ok();
        });
        let run = BulkRun::new(t!("node-drain-run", node = node.as_str()), 0, task, cx);
        run.toast.set_phase(t!("node-drain-cordoning"), cx);
        run.toast
            .set_cancel(move |_| cancel.store(true, Ordering::Relaxed), cx);
        self.bulk = Some(run);
        cx.notify();
    }

    /// Says what a node shell creates, then opens it in the dock.
    pub(super) fn request_node_shell(
        &mut self,
        node: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The pod is created in the context the shell was asked in (#344).
        let Some(connection) = self.cluster.read(cx).connection().cloned() else {
            return;
        };
        let (namespace, image) = AppSettings::try_global(cx)
            .map(|settings| {
                let general = &settings.read(cx).get().general;
                (
                    general.node_shell_namespace().to_owned(),
                    general.node_shell_image().to_owned(),
                )
            })
            .unwrap_or_else(|| {
                (
                    node_shell::DEFAULT_NAMESPACE.to_owned(),
                    node_shell::DEFAULT_IMAGE.to_owned(),
                )
            });
        let actions = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            let target = node.clone();
            let (actions, connection) = (actions.clone(), captured.clone());
            ConfirmDialog::new(t!("node-shell-confirm-title", node = &node))
                .context(connection.context.clone())
                .message(t!(
                    "node-shell-confirm-message",
                    node = &node,
                    namespace = &namespace
                ))
                .items([t!("node-shell-image", image = &image)])
                .notice(t!("node-shell-root-notice"), Vec::<SharedString>::new())
                .confirm_label(t!("node-shell-confirm"))
                .on_confirm(move |window, cx| {
                    let active = actions
                        .update(cx, |this, cx| {
                            action_context::still_active(&this.cluster, &connection, cx)
                        })
                        .unwrap_or(false);
                    if active {
                        crate::dock::open_node_shell(target.clone(), window, cx)
                    }
                })
        });
        self.confirms.track(handle, connection);
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::Pod;
    use serde_json::json;

    use super::{drain_live, drain_options, drain_phase};

    #[test]
    fn drain_phase_counts_pods_left() {
        assert_eq!(drain_phase(6, 2), "Evicting: 4 pods left");
        assert_eq!(drain_phase(2, 1), "Evicting: 1 pod left");
        assert_eq!(drain_phase(2, 2), "Finishing…");
        assert_eq!(drain_phase(0, 0), "No pods to evict");
    }

    fn pod(name: &str, owner: Option<&str>, empty_dir: bool) -> Pod {
        let mut value = json!({
            "metadata": {"name": name, "namespace": "ns"},
            "spec": {"containers": []},
            "status": {"phase": "Running"}
        });
        if let Some(kind) = owner {
            value["metadata"]["ownerReferences"] = json!([{
                "apiVersion": "apps/v1", "kind": kind, "name": "o", "uid": "u",
                "controller": true
            }]);
        }
        if empty_dir {
            value["spec"]["volumes"] = json!([{"name": "v", "emptyDir": {}}]);
        }
        serde_json::from_value(value).unwrap()
    }

    /// The core's drain texts are messages, nested ones translated in the
    /// language of the whole.
    #[test]
    fn drain_notes_translate_as_a_whole() {
        use kubach_core::drain::{Skip, skipped_note};
        use kubach_i18n::Locale;

        let skipped = [
            ("a".to_owned(), Skip::Mirror),
            ("b".to_owned(), Skip::DaemonSet),
            ("c".to_owned(), Skip::DaemonSet),
        ];
        let note = skipped_note(&skipped).unwrap();
        assert_eq!(
            note.english(),
            "Left on the node: 2 DaemonSet pods, 1 static (mirror) pod."
        );
        assert_eq!(
            note.translate(Locale::Ru),
            "Осталось на ноде: Pod DaemonSet: 2, 1 статический (mirror) Pod."
        );
    }

    #[test]
    fn options_follow_checkboxes() {
        let options = drain_options(&[true, false, true]);
        assert!(options.ignore_daemonsets && !options.delete_emptydir_data && options.force);
        assert!(!drain_options(&[]).ignore_daemonsets);
    }

    #[test]
    fn dialog_follows_options() {
        let pods = [
            pod("web", Some("ReplicaSet"), false),
            pod("agent", Some("DaemonSet"), false),
            pod("cache", Some("ReplicaSet"), true),
        ];
        let live = drain_live("n1", &pods, &[true, false, false]);
        assert!(live.blocked);
        assert_eq!(
            live.command.as_deref(),
            Some("kubectl drain n1 --ignore-daemonsets --timeout=5m")
        );
        assert_eq!(live.items.len(), 3);
        assert_eq!(live.notice.as_ref().map(|(_, items)| items.len()), Some(1));

        let live = drain_live("n1", &pods, &[true, true, false]);
        assert!(!live.blocked);
        assert!(live.command.unwrap().contains("--delete-emptydir-data"));
        // The emptyDir warning replaces the blocked list.
        assert!(live.notice.unwrap().0.contains("lost"));

        let live = drain_live("n1", &pods, &[false, true, false]);
        assert!(live.blocked);
    }
}
