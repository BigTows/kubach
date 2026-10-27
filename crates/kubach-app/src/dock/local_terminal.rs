//! A dock tab with the user's local shell (#319). `KUBECONFIG` points at a
//! temporary kubeconfig with only the context open in Kubach and its
//! namespace, so `kubectl` and `helm` in the shell work on what the window
//! shows, without touching `~/.kube/config` (ADR 0015).

use futures::StreamExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::kubeconfig::temp::{self, TempKubeconfig};
use kubach_core::pty::{self, PtyCommand, PtyEvent, PtyHandle};
use kubach_i18n::{msg, t};
use kubach_ui::{TerminalEvent, TerminalView};

use super::terminal::{
    SessionState, TerminalInput, apply_clipboard_policy, describe_exit, forward_terminal_event,
};
use crate::cluster::Cluster;
use crate::settings::AppSettings;
use crate::shell_env;

const BATCH_EVENTS: usize = 32;

/// Removes temporary kubeconfigs left by Kubach processes that are gone,
/// and the ones of this process when it quits.
pub fn init(cx: &mut App) {
    if let Some(dir) = temp::dir() {
        let orphans = dir.clone();
        cx.background_spawn(async move {
            temp::remove_orphans(&orphans);
        })
        .detach();
        cx.on_app_quit(move |_| {
            temp::remove_own(&dir);
            async {}
        })
        .detach();
    }
}

/// Tab title: `Terminal — <context>/<namespace>`.
pub fn title(context: Option<&str>, namespace: Option<&str>) -> String {
    match context {
        Some(context) => t!(
            "dock-local-title",
            context = context,
            namespace = namespace.unwrap_or("default")
        ),
        None => t!("dock-local-title-plain"),
    }
}

pub struct LocalTerminalTab {
    cluster: Entity<Cluster>,
    /// Kubach context the shell's `KUBECONFIG` has; `None` leaves the
    /// shell's own `KUBECONFIG`.
    context: Option<String>,
    namespace: Option<String>,
    view: Entity<TerminalView>,
    pty: Option<PtyHandle>,
    /// Removed when dropped: on exit, restart and close.
    kubeconfig: Option<TempKubeconfig>,
    size: (u16, u16),
    state: SessionState,
    generation: u64,
    _session: Option<Task<()>>,
    _subscription: Subscription,
    _settings: Option<Subscription>,
}

impl LocalTerminalTab {
    pub fn new(
        cluster: Entity<Cluster>,
        context: Option<String>,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| TerminalView::new(window, cx));
        let subscription = cx.subscribe(&view, |this, _, event: &TerminalEvent, cx| {
            let input = this.pty.as_ref().map(|pty| pty as &dyn TerminalInput);
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
        let mut this = Self {
            cluster,
            context,
            namespace,
            view,
            pty: None,
            kubeconfig: None,
            size,
            state: SessionState::Connecting(msg!("dock-local-starting")),
            generation: 0,
            _session: None,
            _subscription: subscription,
            _settings: settings,
        };
        this.start(cx);
        this
    }

    /// Context the tab was opened in; its hotbar color marks the tab (#461).
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub fn title(&self) -> String {
        title(self.context.as_deref(), self.namespace.as_deref())
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

    fn set_state(&mut self, state: SessionState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    /// Ends the shell (hang-up, then kill) and removes the kubeconfig.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self._session = None;
        self.pty = None;
        self.kubeconfig = None;
        cx.notify();
    }

    /// Starts the shell (again) with a fresh kubeconfig.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.shutdown(cx);
        let generation = self.generation;
        let config = match &self.context {
            Some(context) => {
                let config = self.cluster.read(cx).contexts().ok().and_then(|contexts| {
                    contexts.single_context(context, self.namespace.as_deref())
                });
                match config {
                    Some(config) => Some(config),
                    None => {
                        let error = t!("dock-local-context-unavailable", context = context);
                        self.note(&error, cx);
                        self.set_state(SessionState::Failed(error), cx);
                        return;
                    }
                }
            }
            None => None,
        };
        if self.context.is_none() {
            self.note(&t!("dock-local-no-cluster"), cx);
        }
        self.set_state(SessionState::Connecting(msg!("dock-local-starting")), cx);
        let env_loaded = shell_env::loaded(cx);
        let size = self.size;
        self._session = Some(cx.spawn(async move |this, cx| {
            // The shell inherits Kubach's environment: wait for the login
            // shell's PATH (ADR 0013).
            env_loaded.await;
            let started = cx
                .background_spawn(async move {
                    let kubeconfig = match config {
                        Some(config) => {
                            let dir = temp::dir().ok_or_else(|| t!("dock-local-no-temp-dir"))?;
                            let file = TempKubeconfig::create(&dir, &config).map_err(|error| {
                                t!("dock-local-temp-write-failed", error = error.to_string())
                            })?;
                            Some(file)
                        }
                        None => None,
                    };
                    let mut command = PtyCommand::login_shell();
                    if let Some(file) = &kubeconfig {
                        command = command.env("KUBECONFIG", file.path());
                    }
                    let (handle, events) =
                        pty::spawn(&command, size).map_err(|error| error.to_string())?;
                    Ok::<_, String>((kubeconfig, handle, events))
                })
                .await;
            let mut events = match started {
                Ok((kubeconfig, handle, events)) => {
                    let ready = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        // The panel may have been resized while starting.
                        handle.resize(this.size.0, this.size.1);
                        this.pty = Some(handle);
                        this.kubeconfig = kubeconfig;
                        this.set_state(SessionState::Running, cx);
                        true
                    });
                    if !matches!(ready, Ok(true)) {
                        return;
                    }
                    events.ready_chunks(BATCH_EVENTS)
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.note(&error, cx);
                            this.set_state(SessionState::Failed(error), cx);
                        }
                    })
                    .ok();
                    return;
                }
            };
            let mut exit = None;
            while let Some(batch) = events.next().await {
                let mut output = Vec::new();
                for event in batch {
                    match event {
                        PtyEvent::Output(bytes) => output.extend_from_slice(&bytes),
                        PtyEvent::Exited(status) => exit = Some(status),
                    }
                }
                let alive = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    if !output.is_empty() {
                        this.view.update(cx, |view, cx| view.feed(&output, cx));
                    }
                    true
                });
                if !matches!(alive, Ok(true)) || exit.is_some() {
                    break;
                }
            }
            let status = describe_exit(&exit.unwrap_or(kubach_core::streams::ExitStatus::Unknown));
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.pty = None;
                this.kubeconfig = None;
                this.note(&format!("[{status}]"), cx);
                this.set_state(SessionState::Exited(status), cx);
            })
            .ok();
        }));
    }
}

impl Render for LocalTerminalTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = match &self.state {
            SessionState::Running => None,
            state => Some(state.clone()),
        };
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
                                Button::new("local-terminal-restart")
                                    .ghost()
                                    .xsmall()
                                    .label(t!("dock-local-restart"))
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
    fn title_names_context_and_namespace() {
        assert_eq!(
            title(Some("minikube"), Some("web")),
            "Terminal — minikube/web"
        );
        assert_eq!(title(Some("minikube"), None), "Terminal — minikube/default");
        assert_eq!(title(None, Some("web")), "Terminal");
    }
}
