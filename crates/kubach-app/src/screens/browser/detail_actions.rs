//! What the details panel asks for (#363): its buttons emit a
//! [`DetailsEvent`]; the screen showing the panel hands the actions on an
//! object to its [`ObjectActions`], which confirms and runs them with the
//! same toast, confirmations and context check (#344) as bulk actions.

use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use kubach_core::ResourceKind;
use kubach_core::plugin::builtin::action as action_id;
use kubach_core::plugin::bulk::{self, BulkSummary};
use kubach_core::plugin::{ActionId, ActionParams, ResourcePlugin};
use kubach_core::runtime;
use kubach_i18n::{msg, t};
use kubach_ui::ConfirmDialog;
use kube::api::DynamicObject;

use super::actions::{BulkRun, ObjectActions, Subject, Where};
use super::details::DetailsEvent;

impl ObjectActions {
    /// Confirms and runs what `event` asks of the shown object. `subject`
    /// names the kind of that object; `where_` is where it is, for the
    /// messages. Close and Back concern the screen alone.
    pub(in crate::screens) fn on_details_event(
        &mut self,
        event: &DetailsEvent,
        subject: impl FnOnce(&ResourceKind, &Arc<dyn ResourcePlugin>) -> Subject,
        where_: Where,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DetailsEvent::Close | DetailsEvent::Back => {}
            DetailsEvent::Action {
                action,
                object,
                kind,
                plugin,
            } => {
                let subject = subject(kind, plugin);
                self.request_action(
                    action.clone(),
                    vec![object.clone()],
                    subject,
                    where_,
                    window,
                    cx,
                )
            }
            DetailsEvent::Scale {
                object,
                kind,
                plugin,
                delta,
                confirm,
            } => {
                let subject = subject(kind, plugin);
                if *confirm {
                    self.confirm_scale_to_zero(object.clone(), subject, window, cx);
                } else {
                    self.scale_step(object.clone(), subject, *delta, false, cx);
                }
            }
            DetailsEvent::Drain(node) => self.request_drain(node.clone(), window, cx),
            DetailsEvent::NodeShell(node) => self.request_node_shell(node.clone(), window, cx),
        }
    }

    /// Runs `action_id` with `params` on `object`, shown like a bulk run of
    /// one so that failures surface the same way.
    fn run_with_params(
        &mut self,
        title: String,
        object: Arc<DynamicObject>,
        action: ActionId,
        params: ActionParams,
        subject: Subject,
        cx: &mut Context<Self>,
    ) {
        if self.bulk.as_ref().is_some_and(BulkRun::running) {
            return;
        }
        let Some(client) = self
            .cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone())
        else {
            return;
        };
        let plugin = subject.plugin;
        let (tx, mut rx) = mpsc::unbounded::<BulkSummary>();
        // Detached: an action sent to the cluster finishes even if the
        // details close; the result comes back through `rx` (ADR 0011).
        runtime::spawn_detached(async move {
            let result = plugin.run_action(&client, &object, &action, &params).await;
            let summary = match result {
                Ok(()) => BulkSummary {
                    succeeded: 1,
                    failures: Vec::new(),
                },
                Err(error) => {
                    let (reason, forbidden) = bulk::describe_error(&error);
                    BulkSummary {
                        succeeded: 0,
                        failures: vec![bulk::BulkFailure {
                            object: bulk::object_label(&object),
                            reason,
                            forbidden,
                        }],
                    }
                }
            };
            tx.unbounded_send(summary).ok();
        });
        let task = cx.spawn(async move |this, cx| {
            let summary = rx.next().await;
            this.update(cx, |this, cx| {
                if let Some(run) = &mut this.bulk {
                    let summary = summary.unwrap_or_else(|| BulkSummary {
                        succeeded: 0,
                        failures: vec![bulk::BulkFailure {
                            object: "—".into(),
                            reason: msg!("details-action-interrupted"),
                            forbidden: false,
                        }],
                    });
                    run.finish(&summary, cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.bulk = Some(BulkRun::new(title, 1, task, cx));
        cx.notify();
    }

    /// Scales `object` by `delta` from its current count on the cluster.
    fn scale_step(
        &mut self,
        object: Arc<DynamicObject>,
        subject: Subject,
        delta: i32,
        allow_zero: bool,
        cx: &mut Context<Self>,
    ) {
        let name = object.metadata.name.clone().unwrap_or_default();
        let mut params = ActionParams::new();
        params.insert(action_id::DELTA.into(), serde_json::json!(delta));
        if allow_zero {
            params.insert(action_id::ALLOW_ZERO.into(), serde_json::json!(true));
        }
        let title = if delta < 0 {
            t!("details-run-scale-down", name = name)
        } else {
            t!("details-run-scale-up", name = name)
        };
        self.run_with_params(title, object, action_id::SCALE, params, subject, cx);
    }

    /// Asks before a step that turns `object` off, in the context it was
    /// asked in, with the kubectl equivalent.
    fn confirm_scale_to_zero(
        &mut self,
        object: Arc<DynamicObject>,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.cluster.read(cx).connection().cloned() else {
            return;
        };
        let api = &subject.kind.api;
        let name = object.metadata.name.clone().unwrap_or_default();
        let title = t!("action-scale-zero-title", kind = &api.kind, name = name);
        let command = crate::action_context::kubectl_in_context(
            &bulk::scale_command(api, &object, 0),
            &connection.context,
        );
        let message = t!(
            "action-scale-zero-message",
            object = bulk::object_label(&object)
        );
        let actions = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            ConfirmDialog::new(title)
                .context(captured.context.clone())
                .message(message)
                .command(command)
                .confirm_label(t!("action-scale-zero-confirm"))
                .danger()
                .on_confirm(move |_, cx| {
                    let object = object.clone();
                    let captured = captured.clone();
                    let subject = subject.clone();
                    actions
                        .update(cx, |this, cx| {
                            match crate::action_context::check(this.cluster.read(cx), &captured) {
                                Ok(()) => this.scale_step(object, subject, -1, true, cx),
                                Err(changed) => crate::action_context::refuse(&changed, cx),
                            }
                        })
                        .ok();
                })
        });
        self.confirms.track(handle, connection);
    }
}
