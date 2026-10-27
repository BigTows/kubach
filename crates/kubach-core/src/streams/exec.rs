//! Interactive exec/attach sessions and one-shot command execution.
//!
//! A session is driven through [`ExecHandle`] (input, resize) and read as a
//! stream of [`ExecEvent`]s, both plain `futures` types usable from the UI
//! executor. Output is never dropped: a slow reader slows the remote process.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::channel::mpsc;
use futures::future::{Either, join, select};
use futures::{SinkExt, Stream, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::Client;
use kube::api::{Api, AttachParams, AttachedProcess, TerminalSize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::StreamError;
use crate::runtime::{self, BackgroundTask};

/// Shells [`start_shell`] tries, in order: the first one the image has is
/// started (#462). `/busybox/sh` is where distroless `:debug` images keep it.
pub const SHELL_CANDIDATES: &[&str] = &["/bin/bash", "/bin/sh", "/bin/ash", "/busybox/sh"];

/// How long [`start_shell`] waits for the first output of a shell before it
/// takes the shell as started. A missing executable fails much sooner.
const SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Output chunks buffered before the remote process is paused.
const OUTPUT_CHUNKS: usize = 64;

/// What to run in the container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Exec(Vec<String>),
    /// Attach to the container's main process (e.g. a debug container).
    Attach,
}

impl SessionCommand {
    /// Runs the executable at `path` without arguments.
    pub fn program(path: &str) -> Self {
        Self::Exec(vec![path.to_owned()])
    }
}

/// How to connect to the container.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Defaults to the pod's only (or default) container.
    pub container: Option<String>,
    /// Allocate a TTY; stderr is merged into stdout then.
    pub tty: bool,
    pub stdin: bool,
    /// Initial terminal size as `(cols, rows)`.
    pub size: Option<(u16, u16)>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            container: None,
            tty: true,
            stdin: true,
            size: None,
        }
    }
}

impl SessionOptions {
    fn to_params(&self) -> AttachParams {
        AttachParams {
            container: self.container.clone(),
            stdin: self.stdin,
            stdout: true,
            stderr: !self.tty,
            tty: self.tty,
            ..Default::default()
        }
    }
}

/// How the remote process finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    Success,
    Code(i32),
    /// The server reported a failure without an exit code.
    Failed(String),
    /// The connection closed without a status.
    Unknown,
}

impl ExitStatus {
    pub fn from_status(status: &Status) -> Self {
        if status.status.as_deref() == Some("Success") {
            return Self::Success;
        }
        let code = status
            .details
            .as_ref()
            .and_then(|details| details.causes.as_ref())
            .into_iter()
            .flatten()
            .find(|cause| cause.reason.as_deref() == Some("ExitCode"))
            .and_then(|cause| cause.message.as_deref()?.parse().ok());
        match code {
            Some(code) => Self::Code(code),
            None => Self::Failed(status.message.clone().unwrap_or_default()),
        }
    }

    pub fn success(&self) -> bool {
        *self == Self::Success
    }
}

/// Output of an interactive session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecEvent {
    Stdout(Vec<u8>),
    /// Only without a TTY.
    Stderr(Vec<u8>),
    /// Last event of the session.
    Exited(ExitStatus),
}

#[derive(Debug)]
enum Input {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    CloseStdin,
}

/// Sends input to a session. Methods return `false` once the session ended.
#[derive(Clone, Debug)]
pub struct ExecHandle {
    tx: mpsc::UnboundedSender<Input>,
}

impl ExecHandle {
    pub fn write(&self, data: impl Into<Vec<u8>>) -> bool {
        self.tx.unbounded_send(Input::Data(data.into())).is_ok()
    }

    /// Resizes the TTY; ignored without one.
    pub fn resize(&self, cols: u16, rows: u16) -> bool {
        self.tx.unbounded_send(Input::Resize { cols, rows }).is_ok()
    }

    /// Sends EOF to the process's stdin.
    pub fn close_stdin(&self) -> bool {
        self.tx.unbounded_send(Input::CloseStdin).is_ok()
    }
}

/// A running exec/attach session: a stream of [`ExecEvent`]s ending with
/// [`ExecEvent::Exited`]. Dropping it closes the connection.
pub struct ExecSession {
    input: ExecHandle,
    /// An event read ahead (by [`start_shell`]), returned first.
    pending: Option<ExecEvent>,
    events: mpsc::Receiver<ExecEvent>,
    _task: BackgroundTask,
}

impl ExecSession {
    pub fn input(&self) -> ExecHandle {
        self.input.clone()
    }
}

impl Stream for ExecSession {
    type Item = ExecEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ExecEvent>> {
        if let Some(event) = self.pending.take() {
            return Poll::Ready(Some(event));
        }
        Pin::new(&mut self.events).poll_next(cx)
    }
}

/// Why exec cannot reach `container` of `pod`: `None` when the container is
/// running (or its state is unknown, so the server gets to decide).
pub fn exec_blocker(pod: &Pod, container: &str) -> Option<StreamError> {
    let spec = pod.spec.as_ref()?;
    let declared = spec
        .containers
        .iter()
        .map(|c| &c.name)
        .chain(spec.init_containers.iter().flatten().map(|c| &c.name))
        .chain(spec.ephemeral_containers.iter().flatten().map(|c| &c.name))
        .any(|name| name == container);
    if !declared {
        return Some(StreamError::Container {
            pod: pod.metadata.name.clone().unwrap_or_default(),
            container: container.to_owned(),
            reason: "no such container".to_owned(),
        });
    }
    let status = pod.status.as_ref()?;
    let found = status
        .container_statuses
        .iter()
        .flatten()
        .chain(status.init_container_statuses.iter().flatten())
        .chain(status.ephemeral_container_statuses.iter().flatten())
        .find(|s| s.name == container);
    let not_running = |state: String| {
        Some(StreamError::ContainerNotRunning {
            container: container.to_owned(),
            state,
        })
    };
    let Some(state) = found.and_then(|s| s.state.as_ref()) else {
        return not_running("not started yet".to_owned());
    };
    if state.running.is_some() {
        return None;
    }
    if let Some(terminated) = &state.terminated {
        let reason = terminated.reason.as_deref().unwrap_or("Terminated");
        return not_running(format!("{reason}, exit code {}", terminated.exit_code));
    }
    if let Some(waiting) = &state.waiting {
        return not_running(waiting.reason.as_deref().unwrap_or("Waiting").to_owned());
    }
    None
}

/// Fails with a readable error when `container` cannot take an exec
/// (stopped, completed init container, still starting).
/// Must run on the [`runtime`](crate::runtime).
pub async fn check_exec_target(
    client: &Client,
    namespace: &str,
    pod: &str,
    container: &str,
) -> Result<(), StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pod = api.get(pod).await?;
    match exec_blocker(&pod, container) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Whether an exec failure means the image has no executable at `path`:
/// the runtime reports `exec: "/bin/sh": stat /bin/sh: no such file or
/// directory` or `executable file not found in $PATH`. Any other failure
/// (permissions, a dead container, the network) is not.
pub fn is_missing_executable(message: &str, path: &str) -> bool {
    let message = message.to_ascii_lowercase();
    let path = path.to_ascii_lowercase();
    let named =
        message.contains(&format!("\"{path}\"")) || message.contains(&format!("stat {path}:"));
    named
        && (message.contains("no such file or directory")
            || message.contains("executable file not found")
            || message.contains("not found in $path"))
}

/// Whether an exec failure means the image has none of the
/// [`SHELL_CANDIDATES`] (distroless, scratch).
pub fn is_missing_shell(message: &str) -> bool {
    SHELL_CANDIDATES
        .iter()
        .any(|shell| is_missing_executable(message, shell))
}

/// Outcome of trying one shell in [`pick_shell`].
#[derive(Debug)]
pub enum ShellAttempt<S> {
    Started(S),
    /// The image has no such executable: the next candidate is tried.
    Missing,
}

/// Tries `candidates` in order with `attempt` and returns the first that
/// started with its path, or `None` when the image has none of them. Only
/// [`ShellAttempt::Missing`] moves on to the next candidate; an error stops
/// the search, since another shell would fail the same way.
pub async fn pick_shell<'a, S, F, Fut>(
    candidates: &[&'a str],
    mut attempt: F,
) -> Result<Option<(S, &'a str)>, StreamError>
where
    F: FnMut(&'a str) -> Fut,
    Fut: Future<Output = Result<ShellAttempt<S>, StreamError>>,
{
    for &shell in candidates {
        match attempt(shell).await? {
            ShellAttempt::Started(session) => return Ok(Some((session, shell))),
            ShellAttempt::Missing => continue,
        }
    }
    Ok(None)
}

/// A shell started by [`start_shell`].
pub struct StartedShell {
    pub session: ExecSession,
    /// Path of the shell that runs.
    pub shell: &'static str,
    /// Candidates before it that the image does not have; empty when the
    /// first one started.
    pub missing: &'static [&'static str],
}

/// Starts the first of [`SHELL_CANDIDATES`] the container has (#462). A
/// missing executable shows up either as the exec error or as the first
/// event of the session; the session that started is returned with its
/// first event still unread. Fails with [`StreamError::NoShell`] when the
/// image has none of them. Must run on the [`runtime`](crate::runtime).
pub async fn start_shell(
    client: &Client,
    namespace: &str,
    pod: &str,
    options: &SessionOptions,
) -> Result<StartedShell, StreamError> {
    let picked = pick_shell(SHELL_CANDIDATES, |shell| {
        try_shell(client, namespace, pod, shell, options)
    })
    .await?;
    match picked {
        Some((session, shell)) => {
            let index = SHELL_CANDIDATES
                .iter()
                .position(|candidate| *candidate == shell)
                .unwrap_or(0);
            Ok(StartedShell {
                session,
                shell,
                missing: &SHELL_CANDIDATES[..index],
            })
        }
        None => Err(StreamError::NoShell {
            container: options.container.clone().unwrap_or_default(),
        }),
    }
}

async fn try_shell(
    client: &Client,
    namespace: &str,
    pod: &str,
    shell: &str,
    options: &SessionOptions,
) -> Result<ShellAttempt<ExecSession>, StreamError> {
    let command = SessionCommand::program(shell);
    let mut session = match start_session(client, namespace, pod, command, options).await {
        Ok(session) => session,
        Err(error) if is_missing_executable(&error.to_string(), shell) => {
            return Ok(ShellAttempt::Missing);
        }
        Err(error) => return Err(error),
    };
    match tokio::time::timeout(SHELL_PROBE_TIMEOUT, session.next()).await {
        Ok(Some(ExecEvent::Exited(ExitStatus::Failed(message))))
            if is_missing_executable(&message, shell) =>
        {
            Ok(ShellAttempt::Missing)
        }
        Ok(event) => {
            session.pending = event;
            Ok(ShellAttempt::Started(session))
        }
        // Quiet so far: the shell runs and waits for input.
        Err(_) => Ok(ShellAttempt::Started(session)),
    }
}

/// Starts an exec or attach session in `pod`.
/// Must run on the [`runtime`](crate::runtime).
pub async fn start_session(
    client: &Client,
    namespace: &str,
    pod: &str,
    command: SessionCommand,
    options: &SessionOptions,
) -> Result<ExecSession, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = options.to_params();
    let process = match command {
        SessionCommand::Exec(command) => api.exec(pod, command, &params).await?,
        SessionCommand::Attach => api.attach(pod, &params).await?,
    };
    let (input_tx, input_rx) = mpsc::unbounded();
    let (events_tx, events) = mpsc::channel(OUTPUT_CHUNKS);
    let input = ExecHandle { tx: input_tx };
    if let Some((cols, rows)) = options.size {
        input.resize(cols, rows);
    }
    Ok(ExecSession {
        input,
        pending: None,
        events,
        _task: runtime::spawn(drive(process, input_rx, events_tx)),
    })
}

async fn drive(
    mut process: AttachedProcess,
    input: mpsc::UnboundedReceiver<Input>,
    events: mpsc::Sender<ExecEvent>,
) {
    let stdout = pump_output(process.stdout(), events.clone(), ExecEvent::Stdout);
    let stderr = pump_output(process.stderr(), events.clone(), ExecEvent::Stderr);
    let input = pump_input(process.stdin(), process.terminal_size(), input);
    let status = process.take_status();

    // Output ends when the process exits; input may end earlier (EOF).
    let outputs = Box::pin(join(stdout, stderr));
    if let Either::Right((_, outputs)) = select(outputs, Box::pin(input)).await {
        outputs.await;
    }
    let status = match status {
        Some(status) => status.await,
        None => None,
    };
    let joined = process.join().await;
    let exit = match (status, joined) {
        (Some(status), _) => ExitStatus::from_status(&status),
        (None, Err(error)) => ExitStatus::Failed(error.to_string()),
        (None, Ok(())) => ExitStatus::Unknown,
    };
    let mut events = events;
    let _ = events.send(ExecEvent::Exited(exit)).await;
}

async fn pump_output(
    reader: Option<impl AsyncRead + Unpin>,
    mut events: mpsc::Sender<ExecEvent>,
    wrap: fn(Vec<u8>) -> ExecEvent,
) {
    let Some(mut reader) = reader else {
        return;
    };
    let mut buf = vec![0u8; 8 * 1024];
    while let Ok(n @ 1..) = reader.read(&mut buf).await {
        if events.send(wrap(buf[..n].to_vec())).await.is_err() {
            return;
        }
    }
}

async fn pump_input(
    mut stdin: Option<impl AsyncWrite + Unpin>,
    mut resize: Option<mpsc::Sender<TerminalSize>>,
    mut input: mpsc::UnboundedReceiver<Input>,
) {
    while let Some(message) = input.next().await {
        match message {
            Input::Data(data) => {
                if let Some(writer) = &mut stdin
                    && (writer.write_all(&data).await.is_err() || writer.flush().await.is_err())
                {
                    stdin = None;
                }
            }
            Input::Resize { cols, rows } => {
                if let Some(sender) = &mut resize {
                    let size = TerminalSize {
                        width: cols,
                        height: rows,
                    };
                    if sender.send(size).await.is_err() {
                        resize = None;
                    }
                }
            }
            Input::CloseStdin => {
                if let Some(mut writer) = stdin.take() {
                    let _ = writer.shutdown().await;
                }
            }
        }
    }
}

/// Collected output of [`exec_capture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecCapture {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
    /// stdout or stderr went past the limit: the output was cut there and
    /// the connection closed, so the status is [`ExitStatus::Unknown`].
    pub truncated: bool,
}

/// Runs `command` without stdin or TTY and collects its output, at most
/// `limit` bytes of stdout and of stderr each (#195). When either goes past
/// it the connection is closed and the capture is marked
/// [`truncated`](ExecCapture::truncated). Kubernetes does not kill the
/// process then: a command that keeps writing stays blocked on its output
/// (checked on minikube), it does not keep running. Must run on the
/// [`runtime`](crate::runtime).
pub async fn exec_capture<I, T>(
    client: &Client,
    namespace: &str,
    pod: &str,
    container: Option<&str>,
    command: I,
    limit: usize,
) -> Result<ExecCapture, StreamError>
where
    I: IntoIterator<Item = T> + std::fmt::Debug,
    T: Into<String>,
{
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = AttachParams {
        container: container.map(str::to_owned),
        stdin: false,
        stdout: true,
        stderr: true,
        tty: false,
        ..Default::default()
    };
    let mut process = api.exec(pod, command, &params).await?;
    let status = process.take_status();
    let (stdout, stderr) = (process.stdout(), process.stderr());
    let truncated = std::sync::atomic::AtomicBool::new(false);
    let overflow = || {
        truncated.store(true, std::sync::atomic::Ordering::Relaxed);
        process.abort();
    };
    let (stdout, stderr) = join(
        read_limited(stdout, limit, &overflow),
        read_limited(stderr, limit, &overflow),
    )
    .await;
    let truncated = truncated.into_inner();
    let status = match status {
        Some(status) => status.await,
        None => None,
    };
    let joined = process.join().await;
    let status = match (status, joined) {
        (Some(status), _) => ExitStatus::from_status(&status),
        (None, _) if truncated => ExitStatus::Unknown,
        (None, Err(error)) => return Err(StreamError::Remote(error.to_string())),
        (None, Ok(())) => ExitStatus::Unknown,
    };
    // Closing the connection may break the other stream midway.
    let (stdout, stderr) = if truncated {
        (
            stdout.unwrap_or_else(|e| e.read),
            stderr.unwrap_or_else(|e| e.read),
        )
    } else {
        (stdout.map_err(|e| e.error)?, stderr.map_err(|e| e.error)?)
    };
    Ok(ExecCapture {
        stdout,
        stderr,
        status,
        truncated,
    })
}

/// A read that failed after `read` bytes.
#[derive(Debug)]
struct ReadError {
    read: Vec<u8>,
    error: std::io::Error,
}

/// Reads `reader` to its end, but at most `limit` bytes: on more it calls
/// `overflow` (which closes the stream) and returns the first `limit`.
async fn read_limited(
    reader: Option<impl AsyncRead + Unpin>,
    limit: usize,
    overflow: &(dyn Fn() + Sync),
) -> Result<Vec<u8>, ReadError> {
    let mut out = Vec::new();
    let Some(reader) = reader else {
        return Ok(out);
    };
    let mut reader = reader.take(limit as u64 + 1);
    if let Err(error) = reader.read_to_end(&mut out).await {
        out.truncate(limit);
        return Err(ReadError { read: out, error });
    }
    if out.len() > limit {
        out.truncate(limit);
        overflow();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{StatusCause, StatusDetails};

    use super::*;

    #[tokio::test]
    async fn output_is_read_up_to_the_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let overflowed = AtomicUsize::new(0);
        let overflow = || {
            overflowed.fetch_add(1, Ordering::Relaxed);
        };
        let out = read_limited(Some(&b"hello"[..]), 5, &overflow)
            .await
            .unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(overflowed.load(Ordering::Relaxed), 0);
        let out = read_limited(Some(&b"hello!"[..]), 5, &overflow)
            .await
            .unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(overflowed.load(Ordering::Relaxed), 1);
        let none: Option<&[u8]> = None;
        assert!(read_limited(none, 5, &overflow).await.unwrap().is_empty());
        // An endless stream (`yes`) ends at the limit.
        let endless = tokio::io::repeat(b'y');
        let out = read_limited(Some(endless), 1024, &overflow).await.unwrap();
        assert_eq!(out.len(), 1024);
        assert_eq!(overflowed.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn maps_exit_status() {
        let success = Status {
            status: Some("Success".into()),
            ..Default::default()
        };
        assert_eq!(ExitStatus::from_status(&success), ExitStatus::Success);

        let code = Status {
            status: Some("Failure".into()),
            reason: Some("NonZeroExitCode".into()),
            message: Some("command terminated with non-zero exit code".into()),
            details: Some(StatusDetails {
                causes: Some(vec![StatusCause {
                    reason: Some("ExitCode".into()),
                    message: Some("42".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(ExitStatus::from_status(&code), ExitStatus::Code(42));

        let failure = Status {
            status: Some("Failure".into()),
            message: Some("exec: not found".into()),
            ..Default::default()
        };
        assert_eq!(
            ExitStatus::from_status(&failure),
            ExitStatus::Failed("exec: not found".into())
        );
    }

    #[test]
    fn tty_merges_stderr() {
        let params = SessionOptions::default().to_params();
        assert!(params.tty && params.stdin && params.stdout && !params.stderr);
        let params = SessionOptions {
            tty: false,
            stdin: false,
            container: Some("app".into()),
            size: None,
        }
        .to_params();
        assert!(!params.tty && !params.stdin && params.stderr);
        assert_eq!(params.container.as_deref(), Some("app"));
    }

    #[test]
    fn shells_start_with_bash_and_end_with_busybox() {
        assert_eq!(SHELL_CANDIDATES.first(), Some(&"/bin/bash"));
        assert!(SHELL_CANDIDATES.contains(&"/bin/sh"));
        assert!(SHELL_CANDIDATES.contains(&"/bin/ash"));
        assert_eq!(SHELL_CANDIDATES.last(), Some(&"/busybox/sh"));
        assert_eq!(
            SessionCommand::program("/bin/sh"),
            SessionCommand::Exec(vec!["/bin/sh".into()])
        );
    }

    /// Runs [`pick_shell`] over [`SHELL_CANDIDATES`] where the image has
    /// `present` and every attempt in `broken` fails with an error; returns
    /// the pick and the shells tried.
    fn pick(
        present: &[&str],
        broken: &[&str],
    ) -> (Result<Option<&'static str>, String>, Vec<&'static str>) {
        let mut tried = Vec::new();
        let picked = futures::executor::block_on(pick_shell(SHELL_CANDIDATES, |shell| {
            tried.push(shell);
            let outcome = if broken.contains(&shell) {
                Err(StreamError::Remote("connection reset".into()))
            } else if present.contains(&shell) {
                Ok(ShellAttempt::Started(()))
            } else {
                Ok(ShellAttempt::Missing)
            };
            async move { outcome }
        }));
        let picked = picked
            .map(|picked| picked.map(|((), shell)| shell))
            .map_err(|error| error.to_string());
        (picked, tried)
    }

    #[test]
    fn picks_the_first_shell_the_image_has() {
        // Debian: bash first, nothing else tried.
        let (picked, tried) = pick(&["/bin/bash", "/bin/sh"], &[]);
        assert_eq!(picked, Ok(Some("/bin/bash")));
        assert_eq!(tried, ["/bin/bash"]);
        // Alpine: no bash, sh is next.
        let (picked, tried) = pick(&["/bin/sh", "/bin/ash"], &[]);
        assert_eq!(picked, Ok(Some("/bin/sh")));
        assert_eq!(tried, ["/bin/bash", "/bin/sh"]);
        // distroless :debug keeps busybox in /busybox.
        let (picked, tried) = pick(&["/busybox/sh"], &[]);
        assert_eq!(picked, Ok(Some("/busybox/sh")));
        assert_eq!(tried, SHELL_CANDIDATES);
    }

    #[test]
    fn no_shell_after_trying_every_candidate() {
        let (picked, tried) = pick(&[], &[]);
        assert_eq!(picked, Ok(None));
        assert_eq!(tried, SHELL_CANDIDATES);
    }

    #[test]
    fn other_errors_stop_the_search() {
        let (picked, tried) = pick(&["/bin/sh"], &["/bin/bash"]);
        assert_eq!(picked, Err("connection reset".into()));
        assert_eq!(tried, ["/bin/bash"]);
    }

    #[test]
    fn missing_executable_names_the_path() {
        let containerd = "OCI runtime exec failed: exec failed: unable to start container \
             process: exec: \"/bin/bash\": stat /bin/bash: no such file or directory: unknown";
        assert!(is_missing_executable(containerd, "/bin/bash"));
        assert!(!is_missing_executable(containerd, "/bin/sh"));
        assert!(is_missing_executable(
            "exec: \"/busybox/sh\": executable file not found in $PATH",
            "/busybox/sh"
        ));
        // A shell that exists but cannot run is not "missing".
        assert!(!is_missing_executable(
            "exec: \"/bin/bash\": permission denied",
            "/bin/bash"
        ));
        assert!(!is_missing_executable(
            "command terminated with exit code 127",
            "/bin/bash"
        ));
        // A shell that started and then could not find a file is running.
        assert!(!is_missing_executable(
            "/bin/bash: line 1: /app/start: No such file or directory",
            "/bin/bash"
        ));
    }

    #[tokio::test]
    async fn pending_event_comes_first() {
        let (input_tx, _input_rx) = mpsc::unbounded();
        let (mut events_tx, events) = mpsc::channel(4);
        let mut session = ExecSession {
            input: ExecHandle { tx: input_tx },
            pending: Some(ExecEvent::Stdout(b"$ ".to_vec())),
            events,
            _task: runtime::spawn(async {}),
        };
        events_tx
            .send(ExecEvent::Exited(ExitStatus::Success))
            .await
            .unwrap();
        drop(events_tx);
        assert_eq!(
            session.next().await,
            Some(ExecEvent::Stdout(b"$ ".to_vec()))
        );
        assert_eq!(
            session.next().await,
            Some(ExecEvent::Exited(ExitStatus::Success))
        );
        assert_eq!(session.next().await, None);
    }

    fn pod_with_statuses() -> Pod {
        serde_json::from_value(serde_json::json!({
            "metadata": {"name": "web-1"},
            "spec": {
                "initContainers": [{"name": "migrate"}],
                "containers": [{"name": "app"}, {"name": "sidecar"}, {"name": "new"}]
            },
            "status": {
                "initContainerStatuses": [{
                    "name": "migrate", "image": "x", "imageID": "", "ready": false,
                    "restartCount": 0,
                    "state": {"terminated": {"reason": "Completed", "exitCode": 0}}
                }],
                "containerStatuses": [
                    {"name": "app", "image": "x", "imageID": "", "ready": true,
                     "restartCount": 0, "state": {"running": {}}},
                    {"name": "sidecar", "image": "x", "imageID": "", "ready": false,
                     "restartCount": 4,
                     "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
                ]
            }
        }))
        .unwrap()
    }

    #[test]
    fn explains_unreachable_containers() {
        let pod = pod_with_statuses();
        assert!(exec_blocker(&pod, "app").is_none());
        assert_eq!(
            exec_blocker(&pod, "migrate").unwrap().to_string(),
            "container migrate is not running (Completed, exit code 0)"
        );
        assert_eq!(
            exec_blocker(&pod, "sidecar").unwrap().to_string(),
            "container sidecar is not running (CrashLoopBackOff)"
        );
        assert_eq!(
            exec_blocker(&pod, "new").unwrap().to_string(),
            "container new is not running (not started yet)"
        );
        assert_eq!(
            exec_blocker(&pod, "ghost").unwrap().to_string(),
            "container ghost in pod web-1: no such container"
        );
    }

    #[test]
    fn detects_missing_shell() {
        assert!(is_missing_shell(
            "OCI runtime exec failed: exec failed: unable to start container process: \
             exec: \"/bin/sh\": stat /bin/sh: no such file or directory: unknown"
        ));
        assert!(is_missing_shell(
            "exec: \"/bin/sh\": executable file not found in $PATH"
        ));
        assert!(is_missing_shell(
            "exec: \"/busybox/sh\": stat /busybox/sh: no such file or directory"
        ));
        assert!(!is_missing_shell("command terminated with exit code 1"));
    }
}
