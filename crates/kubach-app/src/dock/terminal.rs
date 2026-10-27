//! A dock tab with an interactive shell in a container, or attached to an
//! ephemeral debug container.

use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::settings::Settings;
use kubach_core::streams::{
    DebugOptions, ExecEvent, ExecHandle, ExitStatus, SHELL_CANDIDATES, SessionCommand,
    SessionOptions, StreamError, check_exec_target, create_debug_container, is_missing_shell,
    start_session, start_shell, wait_for_debug_container,
};
use kubach_i18n::{Message, msg, t};
use kubach_ui::{ClipboardPolicy, TerminalEvent, TerminalView};

use super::containers::{OnPick, fill_menu};
use super::target::{ContainerMenu, PodRef, StreamRequest};
use crate::action_context;
use crate::cluster::Cluster;
use crate::settings::AppSettings;

/// How long a debug container may take to start (image pull included).
const DEBUG_START_TIMEOUT: Duration = Duration::from_secs(120);
const BATCH_EVENTS: usize = 32;
/// How long a closed tab keeps its connection so EOF reaches the shell.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// What the tab runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalKind {
    Shell,
    /// A busybox ephemeral container sharing the process namespace of the
    /// target container.
    Debug,
}

/// Steps and exit statuses are messages, so a language switch shows at
/// once; errors are text (mostly from the cluster).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionState {
    Connecting(Message),
    Running,
    Exited(Message),
    Failed(String),
}

impl SessionState {
    pub fn label(&self) -> String {
        match self {
            SessionState::Connecting(step) => step.to_string(),
            SessionState::Running => t!("dock-terminal-running"),
            SessionState::Exited(status) => status.to_string(),
            SessionState::Failed(error) => error.clone(),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, SessionState::Connecting(_) | SessionState::Running)
    }
}

/// "No shell, use Debug" for `container`.
fn no_shell_message(container: Option<&str>) -> Message {
    msg!(
        "dock-terminal-no-shell",
        container = container.unwrap_or("default"),
        shells = SHELL_CANDIDATES.join(", ")
    )
}

/// A shell that could not start because the image has none of the shells
/// reads as "no shell, use Debug" instead of a raw runtime error.
pub fn explain_shell_error(message: &str, container: Option<&str>) -> Option<Message> {
    is_missing_shell(message).then(|| no_shell_message(container))
}

/// The note shown when a shell other than the first candidate started
/// (#462): which one runs and which were not in the image.
pub fn describe_shell_fallback(shell: &str, missing: &[&str]) -> Option<Message> {
    (!missing.is_empty()).then(|| {
        msg!(
            "dock-terminal-shell-fallback",
            shell = shell,
            missing = missing.join(", ")
        )
    })
}

/// Human-readable exit status.
pub fn describe_exit(status: &ExitStatus) -> Message {
    match status {
        ExitStatus::Success => msg!("dock-exit-success"),
        ExitStatus::Code(code) => msg!("dock-exit-code", code = code.to_string()),
        ExitStatus::Failed(message) if message.is_empty() => msg!("dock-exit-failed"),
        ExitStatus::Failed(message) => msg!("dock-exit-failed-message", message = message),
        ExitStatus::Unknown => msg!("dock-exit-disconnected"),
    }
}

/// What programs in the terminal may do with the clipboard (OSC 52), from
/// the settings: writes only when the user allowed them.
fn clipboard_policy(settings: &Settings) -> ClipboardPolicy {
    if settings.general.terminal_clipboard_write {
        ClipboardPolicy::AllowWrite
    } else {
        ClipboardPolicy::Deny
    }
}

pub(super) fn apply_clipboard_policy(
    view: &Entity<TerminalView>,
    settings: &Entity<AppSettings>,
    cx: &mut App,
) {
    let policy = clipboard_policy(settings.read(cx).get());
    view.update(cx, |view, _| view.set_clipboard_policy(policy));
}

/// Where a terminal tab sends what the [`TerminalView`] asks for: an exec
/// session in a container or a local PTY.
pub(super) trait TerminalInput {
    fn send(&self, bytes: Vec<u8>);
    fn resize(&self, cols: u16, rows: u16);
}

impl TerminalInput for ExecHandle {
    fn send(&self, bytes: Vec<u8>) {
        self.write(bytes);
    }

    fn resize(&self, cols: u16, rows: u16) {
        ExecHandle::resize(self, cols, rows);
    }
}

impl TerminalInput for kubach_core::pty::PtyHandle {
    fn send(&self, bytes: Vec<u8>) {
        self.write(bytes);
    }

    fn resize(&self, cols: u16, rows: u16) {
        kubach_core::pty::PtyHandle::resize(self, cols, rows);
    }
}

/// Passes keys and size changes of the view to `input` and remembers the
/// size for the next session. Returns whether the tab should re-render (the
/// title changed).
pub(super) fn forward_terminal_event(
    input: Option<&dyn TerminalInput>,
    size: &mut (u16, u16),
    event: &TerminalEvent,
) -> bool {
    match event {
        TerminalEvent::Input(bytes) => {
            if let Some(input) = input {
                input.send(bytes.clone());
            }
            false
        }
        TerminalEvent::Resize { cols, rows } => {
            *size = (*cols, *rows);
            if let Some(input) = input {
                input.resize(*cols, *rows);
            }
            false
        }
        TerminalEvent::Title(_) => true,
    }
}

pub struct TerminalTab {
    cluster: Entity<Cluster>,
    /// Context the tab was opened in: reconnects, container switches and
    /// new debug containers stay in it, whatever is active now (#344).
    context: Option<String>,
    pod: PodRef,
    container: Option<String>,
    kind: TerminalKind,
    /// Debug container created for this tab, once known.
    debug_container: Option<String>,
    view: Entity<TerminalView>,
    input: Option<ExecHandle>,
    size: (u16, u16),
    state: SessionState,
    /// The shell failed because the image has no shell: the bar offers
    /// Debug.
    no_shell: bool,
    /// Bumped by every (re)connect; a session's task stops touching the tab
    /// once a newer one started (e.g. after switching containers).
    generation: u64,
    _session: Option<Task<()>>,
    _subscription: Subscription,
    /// Follows the OSC 52 clipboard setting.
    _settings: Option<Subscription>,
}

impl TerminalTab {
    pub fn new(
        cluster: Entity<Cluster>,
        pod: PodRef,
        container: Option<String>,
        kind: TerminalKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| TerminalView::new(window, cx));
        let subscription = cx.subscribe(&view, |this, _, event: &TerminalEvent, cx| {
            this.on_terminal_event(event, cx)
        });
        let container = container.or_else(|| pod.default_container().map(str::to_owned));
        let size = view.read(cx).size();
        let settings = AppSettings::try_global(cx).map(|settings| {
            apply_clipboard_policy(&view, &settings, cx);
            cx.observe(&settings, |this: &mut Self, settings, cx| {
                apply_clipboard_policy(&this.view, &settings, cx)
            })
        });
        let context = cluster.read(cx).active_name().map(str::to_owned);
        let mut this = Self {
            cluster,
            context,
            pod,
            container,
            kind,
            debug_container: None,
            view,
            input: None,
            size,
            state: SessionState::Connecting(msg!("dock-connecting")),
            no_shell: false,
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
        let prefix = match (&self.kind, &self.debug_container) {
            (TerminalKind::Shell, _) => String::new(),
            (TerminalKind::Debug, Some(name)) => format!("{name} → "),
            (TerminalKind::Debug, None) => "debug → ".into(),
        };
        let title = match &self.container {
            Some(container) if self.pod.containers.len() > 1 => {
                format!("{prefix}{}/{container}", self.pod.name)
            }
            _ => format!("{prefix}{}", self.pod.name),
        };
        action_context::title_in_context(&title, self.context.as_deref())
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn on_terminal_event(&mut self, event: &TerminalEvent, cx: &mut Context<Self>) {
        let input = self.input.as_ref().map(|input| input as &dyn TerminalInput);
        if forward_terminal_event(input, &mut self.size, event) {
            cx.notify();
        }
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

    /// Fails with "no shell", offering Debug in the bar.
    fn fail_no_shell(&mut self, error: String, cx: &mut Context<Self>) {
        self.no_shell = true;
        self.note(&error, cx);
        self.set_state(SessionState::Failed(error), cx);
    }

    /// Opens a Debug tab on this tab's container, asking first like the
    /// Debug action does.
    fn open_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = StreamRequest::Debug {
            pod: self.pod.clone(),
            container: self.container.clone(),
        };
        super::open_request(request, window, cx);
    }

    /// Ends the session when its tab closes. Closing the stream does not stop
    /// an exec'd process in Kubernetes, so the shell first gets Ctrl-D (EOF at
    /// its prompt) and the connection is dropped a moment later.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = self.input.take() {
            input.write(vec![0x04]);
            input.close_stdin();
        }
        if let Some(session) = self._session.take() {
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(SHUTDOWN_GRACE).await;
                drop(session);
            })
            .detach();
        }
    }

    /// The containers the tab can switch to, when there is a choice.
    fn container_menu(&self) -> Option<ContainerMenu> {
        (self.pod.containers.len() > 1)
            .then(|| ContainerMenu::for_terminal(&self.pod, self.kind == TerminalKind::Shell))
    }

    /// Reconnects to `container`: the running shell gets EOF, a debug tab
    /// starts a new debug container targeting it.
    fn set_container(&mut self, container: String, cx: &mut Context<Self>) {
        if self.container.as_deref() == Some(container.as_str()) && self.state.is_live() {
            return;
        }
        self.shutdown(cx);
        self.container = Some(container);
        self.debug_container = None;
        // Reset the screen so output of the old container does not mix in.
        self.view.update(cx, |view, cx| view.feed(b"\x1bc", cx));
        self.start(cx);
    }

    /// Connects (again): a new exec session, or a new debug container.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self._session = None;
        self.input = None;
        self.no_shell = false;
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
        let namespace = self.pod.namespace.clone();
        let pod = self.pod.name.clone();
        let container = self.container.clone();
        let size = self.size;
        let kind = self.kind;
        self.set_state(
            SessionState::Connecting(match kind {
                TerminalKind::Shell => msg!("dock-connecting"),
                TerminalKind::Debug => msg!("dock-terminal-starting-debug"),
            }),
            cx,
        );

        self._session = Some(cx.spawn(async move |this, cx| {
            let shell_container = container.clone();
            let started = runtime::spawn(async move {
                match kind {
                    TerminalKind::Shell => {
                        if let Some(container) = &container {
                            check_exec_target(&client, &namespace, &pod, container).await?;
                        }
                        let options = SessionOptions {
                            container: container.clone(),
                            size: Some(size),
                            ..SessionOptions::default()
                        };
                        let shell = start_shell(&client, &namespace, &pod, &options).await?;
                        let fallback =
                            (!shell.missing.is_empty()).then_some((shell.shell, shell.missing));
                        Ok::<_, StreamError>((shell.session, container, fallback))
                    }
                    TerminalKind::Debug => {
                        let options = DebugOptions {
                            target: container,
                            ..DebugOptions::default()
                        };
                        let debug =
                            create_debug_container(&client, &namespace, &pod, &options).await?;
                        wait_for_debug_container(&client, &debug, DEBUG_START_TIMEOUT).await?;
                        let options = SessionOptions {
                            container: Some(debug.name.clone()),
                            size: Some(size),
                            ..SessionOptions::default()
                        };
                        let session = start_session(
                            &client,
                            &namespace,
                            &pod,
                            SessionCommand::Attach,
                            &options,
                        )
                        .await?;
                        Ok((session, Some(debug.name), None))
                    }
                }
            })
            .await;
            let session = match started {
                Ok(Ok((session, container, fallback))) => {
                    let ready = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        let input = session.input();
                        // The panel may have been resized while connecting.
                        input.resize(this.size.0, this.size.1);
                        this.input = Some(input);
                        if this.kind == TerminalKind::Debug {
                            this.debug_container = container.clone();
                            this.note(
                                &t!(
                                    "dock-terminal-attached",
                                    container = container.unwrap_or_default()
                                ),
                                cx,
                            );
                        }
                        if let Some(note) = fallback
                            .and_then(|(shell, missing)| describe_shell_fallback(shell, missing))
                        {
                            this.note(&note.to_string(), cx);
                        }
                        this.set_state(SessionState::Running, cx);
                        true
                    });
                    if !matches!(ready, Ok(true)) {
                        return;
                    }
                    session
                }
                Ok(Err(error)) => {
                    let no_shell =
                        kind == TerminalKind::Shell && matches!(error, StreamError::NoShell { .. });
                    let error = if no_shell {
                        no_shell_message(shell_container.as_deref()).to_string()
                    } else {
                        error.to_string()
                    };
                    this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        if no_shell {
                            this.fail_no_shell(error, cx);
                        } else {
                            this.note(&error, cx);
                            this.set_state(SessionState::Failed(error), cx);
                        }
                    })
                    .ok();
                    return;
                }
                Err(error) => {
                    let error = error.to_string();
                    this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.set_state(SessionState::Failed(error), cx)
                        }
                    })
                    .ok();
                    return;
                }
            };

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
            let exit = exit.unwrap_or(ExitStatus::Unknown);
            let no_shell = match (&kind, &exit) {
                (TerminalKind::Shell, ExitStatus::Failed(message)) => {
                    explain_shell_error(message, shell_container.as_deref())
                        .map(|explained| explained.to_string())
                }
                _ => None,
            };
            let status = describe_exit(&exit);
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.input = None;
                match no_shell {
                    Some(error) => this.fail_no_shell(error, cx),
                    None => {
                        this.note(&format!("[{status}]"), cx);
                        this.set_state(SessionState::Exited(status), cx);
                    }
                }
            })
            .ok();
        }));
    }
}

impl TerminalTab {
    /// Switches the container: reconnects the shell, or starts a new debug
    /// container targeting the chosen one.
    fn render_container_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let menu = self.container_menu()?;
        let current = self.container.clone();
        let label: SharedString = current.clone().unwrap_or_default().into();
        let tab = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |value, window, cx| {
            if let Some(container) = value {
                tab.update(cx, |tab, cx| {
                    tab.set_container(container, cx);
                    tab.focus_handle(cx).focus(window, cx);
                })
                .ok();
            }
        });
        Some(
            Button::new("terminal-container")
                .outline()
                .xsmall()
                .label(label)
                .tooltip(match self.kind {
                    TerminalKind::Shell => t!("dock-terminal-container-tooltip"),
                    TerminalKind::Debug => t!("dock-terminal-target-tooltip"),
                })
                .dropdown_menu(move |popup: PopupMenu, _, _| {
                    fill_menu(popup, &menu, Some(current.as_deref()), on_pick.clone())
                }),
        )
    }
}

impl Render for TerminalTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = self.render_container_picker(cx);
        let theme = cx.theme();
        let status = match &self.state {
            SessionState::Running => None,
            state => Some(state.clone()),
        };
        let show_bar = picker.is_some() || status.is_some();
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(show_bar, |tab| {
                let color = match &status {
                    Some(SessionState::Failed(_)) => theme.danger,
                    _ => theme.muted_foreground,
                };
                let live = status.as_ref().is_none_or(SessionState::is_live);
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
                        .children(picker)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(color)
                                .children(status.as_ref().map(SessionState::label)),
                        )
                        .when(!live && self.no_shell, |bar| {
                            bar.child(
                                Button::new("terminal-debug")
                                    .outline()
                                    .xsmall()
                                    .label(t!("action-debug"))
                                    .tooltip(t!("dock-terminal-debug-tooltip"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_debug(window, cx)
                                    })),
                            )
                        })
                        .when(!live, |bar| {
                            bar.child(
                                Button::new("terminal-restart")
                                    .ghost()
                                    .xsmall()
                                    .label(match self.kind {
                                        TerminalKind::Shell => t!("dock-terminal-reconnect"),
                                        TerminalKind::Debug => t!("dock-terminal-new-debug"),
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
    use kubach_core::settings::Settings;
    use kubach_core::streams::ExitStatus;
    use kubach_ui::ClipboardPolicy;

    use super::{
        SessionState, clipboard_policy, describe_exit, describe_shell_fallback, explain_shell_error,
    };

    #[test]
    fn clipboard_writes_follow_the_setting() {
        let mut settings = Settings::default();
        assert_eq!(clipboard_policy(&settings), ClipboardPolicy::Deny);
        settings.general.terminal_clipboard_write = true;
        assert_eq!(clipboard_policy(&settings), ClipboardPolicy::AllowWrite);
    }

    #[test]
    fn describes_exit_statuses() {
        assert_eq!(describe_exit(&ExitStatus::Success).english(), "Exited");
        assert_eq!(
            describe_exit(&ExitStatus::Code(127)).english(),
            "Exited with code 127"
        );
        assert_eq!(
            describe_exit(&ExitStatus::Unknown).english(),
            "Disconnected"
        );
        assert!(!SessionState::Exited(describe_exit(&ExitStatus::Success)).is_live());
        assert!(SessionState::Running.is_live());
    }

    #[test]
    fn explains_missing_shell() {
        let error = explain_shell_error(
            "exec: \"/bin/sh\": stat /bin/sh: no such file or directory",
            Some("pause"),
        )
        .unwrap()
        .english();
        assert!(error.starts_with("Container pause has no shell"));
        assert!(error.contains("/bin/bash, /bin/sh, /bin/ash, /busybox/sh"));
        assert!(error.contains("Debug"));
        assert_eq!(explain_shell_error("connection reset", Some("app")), None);
    }

    #[test]
    fn notes_a_fallback_shell() {
        assert_eq!(describe_shell_fallback("/bin/bash", &[]), None);
        let note = describe_shell_fallback("/bin/sh", &["/bin/bash"])
            .unwrap()
            .english();
        assert_eq!(note, "Started /bin/sh: /bin/bash not found in the image");
    }
}
