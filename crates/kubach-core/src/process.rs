//! Running external programs (`helm`, `kustomize`, `kubectl`): one place for
//! the timeout, `kill_on_drop` and the stdout/stderr handling (ADR 0006, 0007).
//!
//! The caller builds a [`Command`] (program, arguments, environment); [`run`]
//! adds the login shell's variables the caller did not set
//! ([`crate::env::apply`]), sets up the pipes, waits for the output and stops the process when the
//! timeout expires or the future is dropped — for example when the
//! [`BackgroundTask`](crate::runtime::BackgroundTask) running it is dropped
//! (ADR 0011).

use std::ffi::OsString;
use std::fmt;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;
use zeroize::Zeroizing;

/// What a finished process wrote.
#[derive(Clone, Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// stdout as text (invalid UTF-8 is replaced).
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr of a successful run as warnings: trimmed text, empty when the
    /// program was silent (ADR 0007).
    pub fn warnings(&self) -> String {
        stderr_warnings(&self.stderr)
    }
}

/// Trimmed stderr text; what the program complained about.
pub fn stderr_warnings(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_owned()
}

/// Data for the program's stdin (values, a registry password). Never shown in
/// `Debug` and wiped from memory on drop; keep it out of the arguments, which
/// show up in the process list.
#[derive(Clone)]
pub struct Stdin(Zeroizing<Vec<u8>>);

impl Stdin {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(data.into()))
    }
}

impl fmt::Debug for Stdin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Stdin(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{} not found", .program.to_string_lossy())]
    NotFound { program: OsString },
    #[error("can't run {}: {source}", .program.to_string_lossy())]
    Spawn {
        program: OsString,
        #[source]
        source: std::io::Error,
    },
    #[error("did not finish in {} s and was stopped", .0.as_secs())]
    Timeout(Duration),
    #[error("was stopped")]
    Cancelled {
        /// Did not exit in [`CANCEL_GRACE`] after being asked and was killed.
        killed: bool,
        /// What it wrote to stderr before it exited.
        stderr: Vec<u8>,
    },
    #[error("exited with {status}")]
    Failed {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
}

/// Runs `cmd` without stdin and returns its output if it exits with zero.
pub async fn run(cmd: Command, timeout: Duration) -> Result<Output, ProcessError> {
    run_with_stdin(cmd, None, timeout).await
}

/// Like [`run`], feeding `stdin` to the program (closed afterwards).
/// The process is killed on timeout or when the future is dropped.
pub async fn run_with_stdin(
    cmd: Command,
    stdin: Option<Stdin>,
    timeout: Duration,
) -> Result<Output, ProcessError> {
    run_watched(cmd, stdin, timeout, Watch::default()).await
}

/// Which pipe a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Called with every line a program writes, as it writes it (without the
/// line break).
pub type LineHandler = Arc<dyn Fn(Stream, &str) + Send + Sync>;

/// How long a cancelled program may take to exit after it was asked to
/// (SIGTERM) before it is killed.
pub const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Stops a running program on request: it is asked to exit (SIGTERM on
/// Unix, so `helm` records the release as failed) and killed if it still
/// runs after [`CANCEL_GRACE`]. Clones share the request.
#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    requested: AtomicBool,
    notify: Notify,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the program to stop; does nothing after it finished.
    pub fn cancel(&self) {
        self.0.requested.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.requested.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Cancel").field(&self.is_cancelled()).finish()
    }
}

/// Live output and cancellation of a run; the default watches nothing.
#[derive(Clone, Default)]
pub struct Watch {
    pub on_line: Option<LineHandler>,
    pub cancel: Option<Cancel>,
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("on_line", &self.on_line.is_some())
            .field("cancel", &self.cancel)
            .finish()
    }
}

/// Like [`run_with_stdin`], passing each line of output to `watch.on_line`
/// as it comes and stopping the program when `watch.cancel` is triggered
/// ([`ProcessError::Cancelled`]).
pub async fn run_watched(
    mut cmd: Command,
    stdin: Option<Stdin>,
    timeout: Duration,
    watch: Watch,
) -> Result<Output, ProcessError> {
    let program = cmd.as_std().get_program().to_owned();
    // The login shell's `PATH`, `KUBECONFIG` and the rest (ADR 0013).
    crate::env::apply(cmd.as_std_mut());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ProcessError::NotFound {
                program: program.clone(),
            }
        } else {
            ProcessError::Spawn {
                program: program.clone(),
                source,
            }
        }
    })?;
    let pid = child.id();
    let pipe = child.stdin.take();
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    // Written while the output is read, so a large input can't deadlock;
    // dropping the pipe closes the program's stdin, so `--values -` ends
    // right after the data.
    let write = async move {
        if let (Some(mut pipe), Some(input)) = (pipe, stdin) {
            // The program may exit without reading (bad flags); its stderr says why.
            pipe.write_all(&input.0).await.ok();
        }
    };
    let on_line = watch.on_line;
    let finish = async move {
        let (_, stdout, stderr, status) = tokio::join!(
            write,
            read_lines(stdout, Stream::Stdout, on_line.as_ref()),
            read_lines(stderr, Stream::Stderr, on_line.as_ref()),
            child.wait(),
        );
        Ok::<_, std::io::Error>(std::process::Output {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    tokio::pin!(finish);
    let cancel = watch.cancel;
    let cancelled = async {
        match &cancel {
            Some(cancel) => cancel.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let output = tokio::select! {
        output = &mut finish => output,
        // Returning drops `finish`, which kills the program.
        () = tokio::time::sleep(timeout) => return Err(ProcessError::Timeout(timeout)),
        () = cancelled => {
            terminate(pid);
            return Err(match tokio::time::timeout(CANCEL_GRACE, &mut finish).await {
                Ok(output) => ProcessError::Cancelled {
                    killed: false,
                    stderr: output.map(|output| output.stderr).unwrap_or_default(),
                },
                Err(_) => ProcessError::Cancelled {
                    killed: true,
                    stderr: Vec::new(),
                },
            });
        }
    };
    let output = output.map_err(|source| ProcessError::Spawn { program, source })?;
    if output.status.success() {
        Ok(Output {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    } else {
        Err(ProcessError::Failed {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// Reads `pipe` to the end, passing each line to `on_line`.
async fn read_lines(
    pipe: Option<impl AsyncRead + Unpin>,
    stream: Stream,
    on_line: Option<&LineHandler>,
) -> std::io::Result<Vec<u8>> {
    let Some(pipe) = pipe else {
        return Ok(Vec::new());
    };
    let mut reader = BufReader::new(pipe);
    let mut all = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(all);
        }
        all.extend_from_slice(&line);
        if let Some(on_line) = on_line {
            let text = String::from_utf8_lossy(&line);
            on_line(stream, text.trim_end_matches(['\n', '\r']));
        }
    }
}

/// Asks the program to exit: SIGTERM on Unix. Elsewhere nothing; the grace
/// period then ends with a kill.
fn terminate(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: a plain syscall on our own child, not reaped yet: its
        // `Child` is still owned by the pending future.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::{ProcessError, Stdin, run, run_with_stdin};
    use std::time::Duration;
    use tokio::process::Command;

    const LONG: Duration = Duration::from_secs(30);

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[cfg(windows)]
    fn shell(script: &str) -> Command {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", script]);
        cmd
    }

    #[cfg(unix)]
    const ECHO_STDIN: &str = "cat";
    #[cfg(windows)]
    const ECHO_STDIN: &str = "findstr \"^\"";

    #[cfg(unix)]
    const SLEEP: &str = "sleep 5";
    #[cfg(windows)]
    const SLEEP: &str = "ping -n 6 127.0.0.1 >nul";

    #[cfg(unix)]
    const FAIL: &str = "echo out; echo err >&2; exit 3";
    #[cfg(windows)]
    const FAIL: &str = "echo out& echo err 1>&2& exit /b 3";

    #[cfg(unix)]
    const WARN: &str = "echo ok; echo ' careful ' >&2";
    #[cfg(windows)]
    const WARN: &str = "echo ok& echo careful 1>&2";

    #[tokio::test]
    async fn returns_stdout_and_warnings() {
        let output = run(shell(WARN), LONG).await.unwrap();
        assert_eq!(output.stdout_text().trim(), "ok");
        assert_eq!(output.warnings(), "careful");
    }

    #[tokio::test]
    async fn non_zero_exit_keeps_both_streams() {
        let error = run(shell(FAIL), LONG).await.unwrap_err();
        let ProcessError::Failed {
            status,
            stdout,
            stderr,
        } = error
        else {
            panic!("{error}");
        };
        assert_eq!(status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&stderr).trim(), "err");
    }

    #[tokio::test]
    async fn slow_process_is_stopped_on_timeout() {
        let error = run(shell(SLEEP), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(error, ProcessError::Timeout(_)), "{error}");
    }

    #[tokio::test]
    async fn missing_program_is_not_found() {
        let error = run(Command::new("/nonexistent/kubach-tool"), LONG)
            .await
            .unwrap_err();
        assert!(matches!(error, ProcessError::NotFound { .. }), "{error}");
    }

    #[tokio::test]
    async fn stdin_reaches_the_program() {
        let input = Stdin::new("password-123\n");
        let output = run_with_stdin(shell(ECHO_STDIN), Some(input), LONG)
            .await
            .unwrap();
        assert_eq!(output.stdout_text().trim(), "password-123");
    }

    /// Whether `pid` runs; a killed child not yet reaped (zombie) is not.
    #[cfg(unix)]
    fn running(pid: &str) -> bool {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&output.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    /// Dropping the task that runs a program (a closed screen, Cancel)
    /// kills the program instead of leaving it until the timeout (#206).
    #[cfg(unix)]
    #[test]
    fn dropping_the_task_kills_the_process() {
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // `exec` keeps the pid the shell wrote.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo $$ > \"$0\"; exec sleep 30"])
            .arg(&pid_file);
        let task = crate::runtime::spawn(run(cmd, LONG));

        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
            if pid.ends_with('\n') {
                break pid.trim().to_owned();
            }
            assert!(Instant::now() < deadline, "the program did not start");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(running(&pid));

        drop(task);
        while running(&pid) {
            assert!(Instant::now() < deadline, "process {pid} still runs");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Lines reach the handler while the program still runs, and a large
    /// stdin with a large output neither deadlocks nor waits for more
    /// input: stdin is closed after the data (#382).
    #[cfg(unix)]
    #[tokio::test]
    async fn lines_stream_while_the_program_runs() {
        use super::{Stream, Watch};
        use std::sync::{Arc, Mutex};

        let seen: Arc<Mutex<Vec<(Stream, String)>>> = Arc::default();
        let sink = seen.clone();
        let watch = Watch {
            on_line: Some(Arc::new(move |stream, line| {
                sink.lock().unwrap().push((stream, line.to_owned()))
            })),
            cancel: None,
        };
        // `wc -c` only prints once stdin is closed; 1 MiB overflows every pipe.
        let script = "echo waiting >&2; wc -c | tr -d ' '; yes x | head -c 1048576";
        let input = Stdin::new(vec![b'v'; 1 << 20]);
        let output = super::run_watched(shell(script), Some(input), LONG, watch)
            .await
            .unwrap();
        assert_eq!(output.stdout.len(), (1 << 20) + "1048576\n".len());
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], (Stream::Stderr, "waiting".to_owned()));
        assert!(seen.contains(&(Stream::Stdout, "1048576".to_owned())));
    }

    /// Cancel asks the program to exit (SIGTERM) and returns what it wrote.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_terminates_the_program() {
        use super::{Cancel, Watch};
        use std::sync::Arc;

        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let watch = Watch {
            on_line: Some(Arc::new(move |_, line| {
                if line == "started" {
                    trigger.cancel();
                }
            })),
            cancel: Some(cancel),
        };
        let script = "trap 'echo stopping >&2; exit 1' TERM; echo started; \
                      while :; do sleep 0.1; done";
        let started = std::time::Instant::now();
        let error = super::run_watched(shell(script), None, LONG, watch)
            .await
            .unwrap_err();
        let ProcessError::Cancelled { killed, stderr } = error else {
            panic!("{error}");
        };
        assert!(!killed);
        assert_eq!(String::from_utf8_lossy(&stderr).trim(), "stopping");
        assert!(started.elapsed() < super::CANCEL_GRACE);
    }

    #[test]
    fn stdin_is_redacted_in_debug() {
        let input = Stdin::new("password-123");
        assert!(!format!("{input:?}").contains("password"));
    }
}
