//! A dock tab with a shell on a node (#318): a temporary privileged pod on
//! the node and an exec running `nsenter` into the host's namespaces
//! (`kubach_core::streams::node_shell`). The pod goes when the tab closes
//! or the app quits; pods left by a crash are swept on the next connect.
//! While the tab has a pod it refreshes the pod's heartbeat, so other
//! Kubach instances never sweep it (#351).

use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::streams::node_shell::{self, START_TIMEOUT};
use kubach_core::streams::{ExecEvent, ExecHandle, ExitStatus, SessionCommand, SessionOptions};
use kubach_i18n::{msg, t};
use kubach_ui::{TerminalEvent, TerminalView};
use kube::Client;

use super::terminal::{
    SessionState, TerminalInput, apply_clipboard_policy, describe_exit, forward_terminal_event,
};
use crate::action_context;
use crate::cluster::Cluster;
use crate::settings::AppSettings;

const BATCH_EVENTS: usize = 32;
/// How long a deletion may take (quitting waits for it).
const DELETE_TIMEOUT: Duration = Duration::from_secs(3);

/// Tab title: `Node shell — <node>`.
pub fn title(node: &str) -> String {
    t!("dock-node-title", node = node)
}

/// Deletes node shell pods left by a crashed Kubach in the context of
/// `client`: orphans of this installation and long-stale pods of others
/// (`node_shell::owner::decide`), never the ones this process still uses
/// or a live pod of another Kubach. `fallback` is searched when listing
/// every namespace is not allowed.
pub fn sweep(client: Client, fallback: String) {
    runtime::spawn_detached(async move {
        node_shell::sweep_orphans(&client, &fallback).await.ok();
    });
}

/// The pod of a tab: where it lives and the client that created it.
#[derive(Clone)]
struct ShellPod {
    client: Client,
    namespace: String,
    name: String,
    uid: Option<String>,
}

impl ShellPod {
    /// Refreshes the pod's heartbeat until the returned task is dropped.
    fn keep_alive(&self) -> runtime::BackgroundTask<()> {
        runtime::spawn(node_shell::keep_alive(
            self.client.clone(),
            self.namespace.clone(),
            self.name.clone(),
            self.uid.clone(),
        ))
    }

    /// Deletes the pod on the I/O runtime, detached: it goes even if the
    /// tab or the app does not wait. The returned future ends with the
    /// deletion, or after [`DELETE_TIMEOUT`] with a cluster that does not
    /// answer (the next connect sweeps the pod then).
    fn delete(self) -> impl std::future::Future<Output = ()> {
        let handle = runtime::spawn_detached(async move {
            let delete = node_shell::delete_shell_pod(
                &self.client,
                &self.namespace,
                &self.name,
                self.uid.as_deref(),
            );
            let _ = tokio::time::timeout(DELETE_TIMEOUT, delete).await;
        });
        async move {
            let _ = handle.await;
        }
    }
}

pub struct NodeShellTab {
    cluster: Entity<Cluster>,
    /// Context the tab was opened in: the pod is created, reconnected to
    /// and deleted there, whatever is active now (#344).
    context: Option<String>,
    node: String,
    view: Entity<TerminalView>,
    /// Created for this tab; kept across reconnects.
    pod: Option<ShellPod>,
    /// Refreshes the heartbeat of `pod` while there is one.
    _heartbeat: Option<runtime::BackgroundTask<()>>,
    input: Option<ExecHandle>,
    size: (u16, u16),
    state: SessionState,
    generation: u64,
    _session: Option<Task<()>>,
    _subscription: Subscription,
    _settings: Option<Subscription>,
    /// Deletes the pod when the app quits.
    _quit: Subscription,
}

impl NodeShellTab {
    pub fn new(
        cluster: Entity<Cluster>,
        node: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| TerminalView::new(window, cx));
        let subscription = cx.subscribe(&view, |this, _, event: &TerminalEvent, cx| {
            let input = this.input.as_ref().map(|input| input as &dyn TerminalInput);
            if forward_terminal_event(input, &mut this.size, event) {
                cx.notify();
            }
        });
        let size = view.read(cx).size();
        let settings = AppSettings::try_global(cx).map(|settings| {
            apply_clipboard_policy(&view, &settings, cx);
            cx.observe(&settings, |this: &mut Self, settings, cx| {
                apply_clipboard_policy(&this.view, &settings, cx)
            })
        });
        let quit = cx.on_app_quit(|this, _| {
            let pod = this.take_pod();
            async move {
                if let Some(pod) = pod {
                    pod.delete().await;
                }
            }
        });
        let context = cluster.read(cx).active_name().map(str::to_owned);
        let mut this = Self {
            cluster,
            context,
            node,
            view,
            pod: None,
            _heartbeat: None,
            input: None,
            size,
            state: SessionState::Connecting(msg!("dock-node-creating")),
            generation: 0,
            _session: None,
            _subscription: subscription,
            _settings: settings,
            _quit: quit,
        };
        this.start(cx);
        this
    }

    /// Context the tab was opened in; its hotbar color marks the tab (#461).
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub fn title(&self) -> String {
        action_context::title_in_context(&title(&self.node), self.context.as_deref())
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn note(&self, text: &str, cx: &mut Context<Self>) {
        let line = format!("\r\n\x1b[2m{text}\x1b[0m\r\n");
        self.view
            .update(cx, |view, cx| view.feed(line.as_bytes(), cx));
    }

    /// Takes the pod away from the tab, stopping its heartbeat.
    fn take_pod(&mut self) -> Option<ShellPod> {
        self._heartbeat = None;
        self.pod.take()
    }

    fn set_state(&mut self, state: SessionState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    /// Ends the session and deletes the pod (grace 0) when the tab closes;
    /// deleting the pod ends the shell too.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        if let Some(input) = self.input.take() {
            input.close_stdin();
        }
        self._session = None;
        if let Some(pod) = self.take_pod() {
            // Started now; nobody needs to wait for it.
            drop(pod.delete());
        }
        cx.notify();
    }

    /// Connects: creates the pod when the tab has none (or it failed),
    /// waits for it and runs the shell.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self._session = None;
        self.input = None;
        self.generation += 1;
        let generation = self.generation;
        let client =
            match action_context::tab_connection(self.cluster.read(cx), self.context.as_deref()) {
                Ok(connection) => connection.client.clone(),
                Err(error) => {
                    self.note(&error, cx);
                    self.set_state(SessionState::Failed(error), cx);
                    return;
                }
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
        let existing = self.pod.clone();
        let node = self.node.clone();
        let size = self.size;
        self.set_state(
            SessionState::Connecting(match &existing {
                Some(_) => msg!("dock-connecting"),
                None => msg!("dock-node-creating-in", namespace = &namespace),
            }),
            cx,
        );

        self._session = Some(cx.spawn(async move |this, cx| {
            let pod = match existing {
                Some(pod) => pod,
                None => {
                    let created = runtime::spawn({
                        let client = client.clone();
                        let namespace = namespace.clone();
                        async move {
                            node_shell::create_shell_pod(&client, &node, &namespace, &image).await
                        }
                    })
                    .await;
                    let node_shell::CreatedPod { name, uid } = match created {
                        Ok(Ok(created)) => created,
                        Ok(Err(error)) => return fail(&this, generation, error.to_string(), cx),
                        Err(error) => return fail(&this, generation, error.to_string(), cx),
                    };
                    let pod = ShellPod {
                        client: client.clone(),
                        namespace: namespace.clone(),
                        name: name.clone(),
                        uid,
                    };
                    let kept = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        this._heartbeat = Some(pod.keep_alive());
                        this.pod = Some(pod.clone());
                        this.note(
                            &t!("dock-node-created", namespace = &namespace, pod = &name),
                            cx,
                        );
                        this.set_state(SessionState::Connecting(msg!("dock-node-waiting")), cx);
                        true
                    });
                    if !matches!(kept, Ok(true)) {
                        // The tab closed meanwhile.
                        pod.delete().await;
                        return;
                    }
                    pod
                }
            };
            let started = runtime::spawn({
                let pod = pod.clone();
                async move {
                    node_shell::wait_running(&pod.client, &pod.namespace, &pod.name, START_TIMEOUT)
                        .await?;
                    let options = SessionOptions {
                        container: Some("shell".into()),
                        size: Some(size),
                        ..SessionOptions::default()
                    };
                    let command = SessionCommand::Exec(node_shell::shell_command());
                    kubach_core::streams::start_session(
                        &pod.client,
                        &pod.namespace,
                        &pod.name,
                        command,
                        &options,
                    )
                    .await
                }
            })
            .await;
            let session = match started {
                Ok(Ok(session)) => session,
                failed => {
                    let error = match failed {
                        Ok(Err(error)) => error.to_string(),
                        Err(error) => error.to_string(),
                        Ok(Ok(_)) => unreachable!(),
                    };
                    // A pod that does not run is of no use: delete it, and
                    // Restart creates a new one.
                    let pod = this
                        .update(cx, |this, _| {
                            (this.generation == generation)
                                .then(|| this.take_pod())
                                .flatten()
                        })
                        .ok()
                        .flatten();
                    if let Some(pod) = pod {
                        pod.delete().await;
                    }
                    return fail(&this, generation, error, cx);
                }
            };
            let ready = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return false;
                }
                let input = session.input();
                input.resize(this.size.0, this.size.1);
                this.input = Some(input);
                this.set_state(SessionState::Running, cx);
                true
            });
            if !matches!(ready, Ok(true)) {
                return;
            }
            let mut batches = session.ready_chunks(BATCH_EVENTS);
            let mut exit = None;
            while let Some(batch) = batches.next().await {
                let mut output = Vec::new();
                for event in batch {
                    match event {
                        ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes) => {
                            output.extend_from_slice(&bytes)
                        }
                        ExecEvent::Exited(status) => exit = Some(status),
                    }
                }
                let alive = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.view.update(cx, |view, cx| view.feed(&output, cx));
                    true
                });
                if !matches!(alive, Ok(true)) {
                    return;
                }
            }
            let status = describe_exit(&exit.unwrap_or(ExitStatus::Unknown));
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.input = None;
                this.note(&format!("[{status}]"), cx);
                this.set_state(SessionState::Exited(status), cx);
            })
            .ok();
        }));
    }
}

/// Shows `error` in the tab of `generation`.
fn fail(this: &WeakEntity<NodeShellTab>, generation: u64, error: String, cx: &mut AsyncApp) {
    this.update(cx, |this, cx| {
        if this.generation == generation {
            this.note(&error, cx);
            this.set_state(SessionState::Failed(error), cx);
        }
    })
    .ok();
}

impl Render for NodeShellTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = match &self.state {
            SessionState::Running => None,
            state => Some(state.clone()),
        };
        let has_pod = self.pod.is_some();
        div()
            .size_full()
            .flex()
            .flex_col()
            .when_some(status, |tab, status| {
                let color = match &status {
                    SessionState::Failed(_) => theme.danger,
                    _ => theme.muted_foreground,
                };
                let live = status.is_live();
                tab.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(color)
                                .child(status.label()),
                        )
                        .when(!live, |bar| {
                            bar.child(
                                Button::new("node-shell-restart")
                                    .ghost()
                                    .xsmall()
                                    .label(if has_pod {
                                        t!("dock-terminal-reconnect")
                                    } else {
                                        t!("dock-node-new-pod")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                            )
                        }),
                )
            })
            .child(div().flex_1().min_h_0().child(self.view.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::title;

    #[test]
    fn title_names_the_node() {
        assert_eq!(title("minikube"), "Node shell — minikube");
    }
}
