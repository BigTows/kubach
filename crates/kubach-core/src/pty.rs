//! Local processes in a pseudo-terminal: the user's shell in a local
//! terminal tab (#319). On Unix our own PTY and `posix_spawn`, without a
//! `fork` of the multi-threaded app (#385); ConPTY through `portable-pty` on
//! Windows.
//!
//! [`spawn`] starts the process and returns a [`PtyHandle`] for input and
//! resizing plus a stream of [`PtyEvent`]s with its output and exit. The
//! output is read and the input written on threads of their own, so a busy
//! or stuck program never blocks the caller. Dropping the handle ends the
//! process: `SIGHUP` to its session, `SIGKILL` if it is still there a moment
//! later (the process is terminated on Windows).

use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use futures::channel::mpsc as fmpsc;

use crate::streams::ExitStatus;

/// How long the exit waits for output still in the PTY to be read.
const DRAIN: Duration = Duration::from_millis(300);
/// How long a closed process has after `SIGHUP` before it is killed.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(1);
const READ_BUFFER: usize = 16 * 1024;

/// What to run.
#[derive(Clone, Debug, Default)]
pub struct PtyCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Working directory; the home folder when `None`.
    pub cwd: Option<PathBuf>,
    /// Variables set on top of this process's environment.
    pub env: Vec<(OsString, OsString)>,
}

impl PtyCommand {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// The user's interactive login shell in the home folder: `$SHELL -l`
    /// on Unix (`/bin/sh` without `$SHELL`); `pwsh`, Windows PowerShell or
    /// `cmd` on Windows, whichever is found first. `TERM` and `COLORTERM`
    /// describe the built-in terminal.
    pub fn login_shell() -> Self {
        #[cfg(unix)]
        let command = Self::new(crate::shell_env::user_shell()).arg("-l");
        #[cfg(not(unix))]
        let command = Self::new(windows_shell(&crate::env::path()));
        let command = match crate::env::home_dir() {
            Some(home) => command.cwd(home),
            None => command,
        };
        command
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "Kubach")
    }
}

/// `pwsh`, then Windows PowerShell, then `%COMSPEC%` (`cmd.exe`).
#[cfg_attr(unix, allow(dead_code))]
fn windows_shell(path: &OsStr) -> OsString {
    ["pwsh", "powershell"]
        .into_iter()
        .find_map(|name| crate::which::which(name, path))
        .map(PathBuf::into_os_string)
        .or_else(|| crate::env::var_os("COMSPEC"))
        .unwrap_or_else(|| "cmd.exe".into())
}

/// Output and the end of a process in a PTY.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The process ended; the last event.
    Exited(ExitStatus),
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("can't open a pseudo-terminal: {0}")]
    Open(String),
    #[error("can't start {}: {message}", .program.to_string_lossy())]
    Spawn { program: OsString, message: String },
}

/// Input and size of a running PTY process; dropping it ends the process.
pub struct PtyHandle {
    input: mpsc::Sender<Vec<u8>>,
    control: sys::Control,
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
}

impl PtyHandle {
    /// Queues bytes for the process's input; `false` once it has ended.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) -> bool {
        self.input.send(bytes.into()).is_ok()
    }

    /// Sets the terminal size the process sees (`SIGWINCH` on Unix).
    pub fn resize(&self, cols: u16, rows: u16) {
        self.control.resize(cols.max(1), rows.max(1));
    }

    /// Process id of the process, while known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        if self.exited.load(Ordering::Acquire) {
            return;
        }
        self.control.hang_up();
        #[cfg(unix)]
        {
            let (pid, exited) = (self.pid, self.exited.clone());
            thread::spawn(move || {
                thread::sleep(KILL_GRACE);
                if exited.load(Ordering::Acquire) {
                    return;
                }
                if let Some(pid) = pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
                    // SAFETY: plain syscall; the waiter thread has not reaped
                    // the leader, so the group id is still ours.
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                }
            });
        }
    }
}

/// A process just started in a PTY.
struct Spawned {
    control: sys::Control,
    writer: Box<dyn io::Write + Send>,
    pid: Option<u32>,
    /// Blocks until the process exits.
    wait: Box<dyn FnOnce() -> ExitStatus + Send>,
}

/// Starts reading the PTY output; called before the process starts.
type StartReader<'a> = &'a mut dyn FnMut(Box<dyn io::Read + Send>) -> Result<(), PtyError>;

/// Starts `command` in a new PTY of `cols` × `rows`.
pub fn spawn(
    command: &PtyCommand,
    (cols, rows): (u16, u16),
) -> Result<(PtyHandle, fmpsc::UnboundedReceiver<PtyEvent>), PtyError> {
    let exited = Arc::new(AtomicBool::new(false));
    let (events, receiver) = fmpsc::unbounded();

    // Output, until the PTY closes. The reader runs before the process
    // starts: macOS discards output still queued when the last terminal
    // side closes and nobody reads it within a moment (`ptsclose`), so a
    // short-lived process that exits before a late reader thread is
    // scheduled would lose all its output.
    let (read_done, read_finished) = mpsc::channel::<()>();
    let mut read_done = Some(read_done);
    let output = events.clone();
    let mut start_reader = |mut reader: Box<dyn io::Read + Send>| {
        let read_done = read_done.take();
        let output = output.clone();
        thread::Builder::new()
            .name("pty-read".into())
            .spawn(move || {
                let mut buffer = vec![0u8; READ_BUFFER];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if output
                                .unbounded_send(PtyEvent::Output(buffer[..n].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        // Linux reports `EIO` once the last terminal side closed.
                        Err(_) => break,
                    }
                }
                drop(read_done);
            })
            .map(drop)
            .map_err(|error| PtyError::Open(error.to_string()))
    };
    let Spawned {
        control,
        mut writer,
        pid,
        wait,
    } = sys::spawn(command, (cols.max(1), rows.max(1)), &mut start_reader)?;

    // Input, so a program that doesn't read never blocks the caller.
    let (input, queued) = mpsc::channel::<Vec<u8>>();
    thread::Builder::new()
        .name("pty-write".into())
        .spawn(move || {
            for bytes in queued {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|error| PtyError::Open(error.to_string()))?;

    // The exit, after the output still in the PTY was read.
    let done = exited.clone();
    thread::Builder::new()
        .name("pty-wait".into())
        .spawn(move || {
            let status = wait();
            done.store(true, Ordering::Release);
            let _ = read_finished.recv_timeout(DRAIN);
            let _ = events.unbounded_send(PtyEvent::Exited(status));
        })
        .map_err(|error| PtyError::Open(error.to_string()))?;

    Ok((
        PtyHandle {
            input,
            control,
            pid,
            exited,
        },
        receiver,
    ))
}

/// Unix: our own PTY and `posix_spawn`, without `fork` (#385).
///
/// `portable-pty` (like `std::process::Command` with `pre_exec`) forks the
/// multi-threaded app and runs Rust code in the child — `setsid`,
/// `TIOCSCTTY`, closing stray descriptors through `read_dir` — which may
/// deadlock or crash on locks and allocations held by other threads at the
/// moment of the fork (#375, see [`crate::spawn`]). Here `posix_spawn`
/// creates the session (`POSIX_SPAWN_SETSID`) and then opens the terminal
/// side by path as stdin, without `O_NOCTTY`: the first terminal a session
/// leader without one opens becomes its controlling terminal (Linux; XNU
/// too, checked by the tests), so job control, `Ctrl-C` and `SIGWINCH` work
/// without `TIOCSCTTY`.
#[cfg(unix)]
mod sys {
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::PathBuf;
    use std::ptr;

    use super::{PtyCommand, PtyError, Spawned, StartReader};
    use crate::spawn::{self, FileActions, SpawnAttr};
    use crate::streams::ExitStatus;

    /// Signals the shell gets with their default action even if Kubach
    /// ignores them (Rust ignores `SIGPIPE`); handlers are reset by exec.
    const DEFAULT_SIGNALS: &[libc::c_int] = &[
        libc::SIGPIPE,
        libc::SIGCHLD,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGALRM,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ];

    /// The PTY's controlling side and the process's session.
    pub(super) struct Control {
        master: OwnedFd,
        pid: libc::pid_t,
    }

    impl Control {
        pub(super) fn resize(&self, cols: u16, rows: u16) {
            let size = winsize(cols, rows);
            // SAFETY: plain ioctl on our live descriptor with a valid
            // `winsize`; the kernel sends `SIGWINCH` to the foreground group.
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        }

        /// `SIGHUP` to the process's session, as closing a terminal window
        /// does: the process leads its own session and process group.
        pub(super) fn hang_up(&mut self) {
            // SAFETY: plain syscalls; the child is not reaped before
            // `exited` is set, which the caller checked.
            unsafe {
                libc::killpg(self.pid, libc::SIGHUP);
                libc::kill(self.pid, libc::SIGHUP);
            }
        }
    }

    fn winsize(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    pub(super) fn spawn(
        command: &PtyCommand,
        (cols, rows): (u16, u16),
        start_reader: StartReader<'_>,
    ) -> Result<Spawned, PtyError> {
        let open_error = |error: io::Error| PtyError::Open(error.to_string());
        let spawn_error = |error: io::Error| PtyError::Spawn {
            program: command.program.clone(),
            message: error.to_string(),
        };

        // This process's environment, the login shell's on top of it
        // (ADR 0013), the command's on top of both.
        let mut env: Vec<(OsString, OsString)> = crate::env::vars_os();
        for (key, value) in &command.env {
            env.retain(|(existing, _)| existing != key);
            env.push((key.clone(), value.clone()));
        }
        let var = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let program = resolve(&command.program, &var("PATH").unwrap_or_default())
            .ok_or_else(|| spawn_error(io::ErrorKind::NotFound.into()))?;
        // The requested folder, else home, as with `portable-pty`.
        let dir = command
            .cwd
            .clone()
            .filter(|dir| dir.is_dir())
            .or_else(|| var("HOME").map(PathBuf::from).or_else(dirs::home_dir))
            .filter(|dir| dir.is_dir())
            .map(|dir| spawn::c_path(dir.as_os_str()))
            .transpose()
            .map_err(spawn_error)?;
        let argv = spawn::c_args(
            program.as_os_str(),
            command.args.iter().map(OsString::as_os_str),
        )
        .map_err(spawn_error)?;
        let envp = spawn::c_env(env);

        let (master, slave) = open_pty(cols, rows).map_err(open_error)?;
        let slave_path = tty_name(&master, &slave).map_err(open_error)?;
        start_reader(Box::new(File::from(
            master.try_clone().map_err(open_error)?,
        )))?;

        let actions = FileActions::new().map_err(spawn_error)?;
        // Opened after `setsid` and without `O_NOCTTY`, so it becomes the
        // controlling terminal of the new session.
        actions
            .open(libc::STDIN_FILENO, &slave_path, libc::O_RDWR)
            .and_then(|()| actions.dup2(libc::STDIN_FILENO, libc::STDOUT_FILENO))
            .and_then(|()| actions.dup2(libc::STDIN_FILENO, libc::STDERR_FILENO))
            .map_err(spawn_error)?;
        if let Some(dir) = &dir {
            actions.chdir(dir).map_err(spawn_error)?;
        }
        let attr = SpawnAttr::new_session(DEFAULT_SIGNALS).map_err(spawn_error)?;
        let pid = spawn::spawn(&argv, &envp, &actions, &attr, false).map_err(spawn_error)?;
        // The parent's terminal side would keep the output open after the
        // process exits; the child has its own by now (`posix_spawn`
        // returns after the exec).
        drop(slave);

        let writer = File::from(master.try_clone().map_err(open_error)?);
        Ok(Spawned {
            control: Control { master, pid },
            writer: Box::new(writer),
            pid: u32::try_from(pid).ok(),
            wait: Box::new(move || match spawn::wait(pid) {
                Ok(status) => match status.code() {
                    Some(0) => ExitStatus::Success,
                    Some(code) => ExitStatus::Code(code),
                    // "signal: 1 (SIGHUP)"
                    None => ExitStatus::Failed(status.to_string()),
                },
                Err(error) => ExitStatus::Failed(error.to_string()),
            }),
        })
    }

    /// `program` as is when it names a path, else found in `path`.
    fn resolve(program: &OsStr, path: &OsStr) -> Option<PathBuf> {
        if program.as_bytes().contains(&b'/') {
            return Some(PathBuf::from(program));
        }
        crate::which::which(program.to_str()?, path)
    }

    /// A new PTY of `cols` × `rows`: its controlling and terminal sides,
    /// both close-on-exec and above the standard descriptors.
    fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
        let (mut master, mut slave) = (-1, -1);
        let mut size = winsize(cols, rows);
        // SAFETY: valid out-pointers; no name buffer (its size is not
        // checked) and default terminal settings.
        let code = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                ptr::null_mut(),
                ptr::null_mut::<libc::termios>(),
                &raw mut size,
            )
        };
        if code != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openpty` just returned these descriptors and nothing owns them.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Close-on-exec right away: other threads may start processes too.
        Ok((spawn::above_stdio(&master)?, spawn::above_stdio(&slave)?))
    }

    /// Path of the terminal side, such as `/dev/pts/3` or `/dev/ttys003`,
    /// asked from the kernel through the controlling side. (`ttyname_r` on
    /// the terminal side looks the device up in `/dev`, and on macOS that
    /// sometimes fails with `ERANGE` right after the PTY was created.)
    fn tty_name(master: &OwnedFd, slave: &OwnedFd) -> io::Result<CString> {
        let mut buffer = [0 as libc::c_char; 256];
        // SAFETY: the buffer is larger than the 128 bytes `TIOCPTYGNAME`
        // writes and than the length passed to `ptsname_r`; `ttyname_r`
        // gets the matching length.
        let code = unsafe {
            #[cfg(target_vendor = "apple")]
            {
                let _ = slave;
                /// `_IOR('t', 83, char[128])` from `<sys/ttycom.h>`.
                const TIOCPTYGNAME: libc::c_ulong = 0x4080_7453;
                match libc::ioctl(master.as_raw_fd(), TIOCPTYGNAME, buffer.as_mut_ptr()) {
                    0 => 0,
                    _ => io::Error::last_os_error()
                        .raw_os_error()
                        .unwrap_or(libc::EIO),
                }
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                let _ = slave;
                libc::ptsname_r(master.as_raw_fd(), buffer.as_mut_ptr(), buffer.len() - 1)
            }
            #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
            {
                libc::ttyname_r(slave.as_raw_fd(), buffer.as_mut_ptr(), buffer.len() - 1)
            }
        };
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        buffer[buffer.len() - 1] = 0;
        // SAFETY: NUL-terminated: the call wrote a C string and the last
        // byte is NUL regardless.
        Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_owned())
    }
}

/// Windows: ConPTY through `portable-pty` (no `fork` there).
#[cfg(windows)]
mod sys {
    use std::sync::Mutex;

    use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

    use super::{PtyCommand, PtyError, Spawned, StartReader};
    use crate::streams::ExitStatus;

    pub(super) struct Control {
        master: Mutex<Box<dyn MasterPty + Send>>,
        killer: Box<dyn ChildKiller + Send + Sync>,
    }

    impl Control {
        pub(super) fn resize(&self, cols: u16, rows: u16) {
            if let Ok(master) = self.master.lock() {
                let _ = master.resize(size(cols, rows));
            }
        }

        /// Terminates the process.
        pub(super) fn hang_up(&mut self) {
            let _ = self.killer.kill();
        }
    }

    fn size(cols: u16, rows: u16) -> PtySize {
        PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    pub(super) fn spawn(
        command: &PtyCommand,
        (cols, rows): (u16, u16),
        start_reader: StartReader<'_>,
    ) -> Result<Spawned, PtyError> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .map_err(|error| PtyError::Open(error.to_string()))?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        for (key, value) in crate::env::captured().iter().chain(&command.env) {
            builder.env(key, value);
        }
        if let Some(dir) = &command.cwd {
            builder.cwd(dir);
        }
        let spawn_error = |error: &dyn std::fmt::Display| PtyError::Spawn {
            program: command.program.clone(),
            message: error.to_string(),
        };
        let master = pair.master;
        start_reader(master.try_clone_reader().map_err(|e| spawn_error(&e))?)?;
        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| spawn_error(&e))?;
        // The parent's copy of the terminal side would keep the output open
        // after the process exits.
        drop(pair.slave);
        let writer = master.take_writer().map_err(|e| spawn_error(&e))?;
        let killer = child.clone_killer();
        let pid = child.process_id();
        Ok(Spawned {
            control: Control {
                master: Mutex::new(master),
                killer,
            },
            writer,
            pid,
            wait: Box::new(move || match child.wait() {
                Ok(status) if status.success() => ExitStatus::Success,
                Ok(status) => match status.signal() {
                    Some(signal) => ExitStatus::Failed(signal.to_owned()),
                    None => ExitStatus::Code(i32::try_from(status.exit_code()).unwrap_or(-1)),
                },
                Err(error) => ExitStatus::Failed(error.to_string()),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Instant;

    #[cfg(unix)]
    use futures::StreamExt as _;

    use super::*;

    /// Collects output until the process exits or `timeout` passes.
    #[cfg(unix)]
    fn run(
        command: &PtyCommand,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> (String, Option<ExitStatus>) {
        let (handle, mut events) = spawn(command, (80, 24)).unwrap();
        if let Some(input) = input {
            handle.write(input);
        }
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut exit = None;
        futures::executor::block_on(async {
            while Instant::now() < deadline {
                match futures::future::select(
                    events.next(),
                    futures_timer(Duration::from_millis(50)),
                )
                .await
                {
                    futures::future::Either::Left((Some(PtyEvent::Output(bytes)), _)) => {
                        output.extend(bytes)
                    }
                    futures::future::Either::Left((Some(PtyEvent::Exited(status)), _)) => {
                        exit = Some(status);
                        break;
                    }
                    futures::future::Either::Left((None, _)) => break,
                    futures::future::Either::Right(_) => {}
                }
            }
        });
        (String::from_utf8_lossy(&output).into_owned(), exit)
    }

    /// A timer future without a runtime: a thread that sleeps.
    #[cfg(unix)]
    fn futures_timer(duration: Duration) -> futures::channel::oneshot::Receiver<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        thread::spawn(move || {
            thread::sleep(duration);
            let _ = tx.send(());
        });
        rx
    }

    #[cfg(unix)]
    #[test]
    fn passes_environment_and_reports_exit() {
        let command = PtyCommand::new("/bin/sh")
            .arg("-c")
            .arg("echo \"kc=$KUBECONFIG\"; exit 3")
            .env("KUBECONFIG", "/kubach-test-319/config.yaml");
        let (output, exit) = run(&command, None, Duration::from_secs(10));
        assert!(
            output.contains("kc=/kubach-test-319/config.yaml"),
            "output: {output:?}"
        );
        assert_eq!(exit, Some(ExitStatus::Code(3)));
    }

    #[cfg(unix)]
    #[test]
    fn runs_in_a_terminal_and_takes_input() {
        let command = PtyCommand::new("/bin/sh")
            .arg("-c")
            .arg("[ -t 0 ] && echo tty; read line; echo \"got $line\"")
            .cwd("/");
        let (output, exit) = run(&command, Some(b"hello\r"), Duration::from_secs(10));
        assert!(output.contains("tty"), "output: {output:?}");
        assert!(output.contains("got hello"), "output: {output:?}");
        assert_eq!(exit, Some(ExitStatus::Success));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_handle_ends_the_process() {
        let command = PtyCommand::new("/bin/sh").arg("-c").arg("sleep 30");
        let (handle, mut events) = spawn(&command, (80, 24)).unwrap();
        let pid = libc::pid_t::try_from(handle.pid().unwrap()).unwrap();
        drop(handle);
        let exit = futures::executor::block_on(async {
            while let Some(event) = events.next().await {
                if let PtyEvent::Exited(status) = event {
                    return Some(status);
                }
            }
            None
        });
        assert!(matches!(exit, Some(ExitStatus::Failed(_))), "{exit:?}");
        // SAFETY: signal 0 only checks that the process exists.
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    }

    #[test]
    fn missing_program_fails_to_spawn() {
        let command = PtyCommand::new("/nonexistent/kubach-test-319/shell");
        assert!(matches!(
            spawn(&command, (80, 24)),
            Err(PtyError::Spawn { .. })
        ));
    }

    /// Reads output until it contains `needle`; panics after `timeout`.
    #[cfg(unix)]
    fn wait_for(
        events: &mut fmpsc::UnboundedReceiver<PtyEvent>,
        output: &mut String,
        needle: &str,
        timeout: Duration,
    ) {
        assert!(
            read_until(events, output, needle, timeout),
            "no {needle:?} in output: {output:?}"
        );
    }

    /// Reads output until it contains `needle`; `false` after `timeout`.
    #[cfg(unix)]
    fn read_until(
        events: &mut fmpsc::UnboundedReceiver<PtyEvent>,
        output: &mut String,
        needle: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        futures::executor::block_on(async {
            while !output.contains(needle) {
                if Instant::now() >= deadline {
                    return false;
                }
                match futures::future::select(
                    events.next(),
                    futures_timer(Duration::from_millis(50)),
                )
                .await
                {
                    futures::future::Either::Left((Some(PtyEvent::Output(bytes)), _)) => {
                        output.push_str(&String::from_utf8_lossy(&bytes))
                    }
                    futures::future::Either::Left((event, _)) => {
                        panic!("ended ({event:?}) without {needle:?}: {output:?}")
                    }
                    futures::future::Either::Right(_) => {}
                }
            }
            true
        })
    }

    /// Waits for the exit, collecting output; `None` after `timeout`.
    #[cfg(unix)]
    fn wait_exit(
        events: &mut fmpsc::UnboundedReceiver<PtyEvent>,
        timeout: Duration,
    ) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        futures::executor::block_on(async {
            while Instant::now() < deadline {
                match futures::future::select(
                    events.next(),
                    futures_timer(Duration::from_millis(50)),
                )
                .await
                {
                    futures::future::Either::Left((Some(PtyEvent::Exited(status)), _)) => {
                        return Some(status);
                    }
                    futures::future::Either::Left((None, _)) => return None,
                    _ => {}
                }
            }
            None
        })
    }

    /// The shell leads a new session whose controlling terminal is the PTY,
    /// with its process group in the foreground (#385).
    #[cfg(unix)]
    #[test]
    fn leads_session_with_controlling_terminal() {
        let command = PtyCommand::new("/bin/sh").arg("-c").arg(
            "tty; exec 3</dev/tty && echo ctty-ok; \
             echo \"ids=$$ $(ps -o pgid= -p $$) $(ps -o tpgid= -p $$)\"",
        );
        let (handle, mut events) = spawn(&command, (80, 24)).unwrap();
        let pid = handle.pid().unwrap().to_string();
        let mut output = String::new();
        wait_for(&mut events, &mut output, "\n", Duration::from_secs(10));
        wait_exit_with(&mut events, &mut output);
        assert!(output.starts_with("/dev/"), "output: {output:?}");
        assert!(output.contains("ctty-ok"), "output: {output:?}");
        let ids: Vec<&str> = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("ids="))
            .unwrap_or_else(|| panic!("output: {output:?}"))
            .split_whitespace()
            .collect();
        // pid == pgid (group leader), tpgid == pgid (foreground group of
        // its controlling terminal).
        assert_eq!(ids, [pid.as_str(); 3], "output: {output:?}");
    }

    /// Collects the rest of the output until the exit.
    #[cfg(unix)]
    fn wait_exit_with(events: &mut fmpsc::UnboundedReceiver<PtyEvent>, output: &mut String) {
        futures::executor::block_on(async {
            while let Some(event) = events.next().await {
                match event {
                    PtyEvent::Output(bytes) => output.push_str(&String::from_utf8_lossy(&bytes)),
                    PtyEvent::Exited(status) => {
                        assert_eq!(status, ExitStatus::Success, "output: {output:?}");
                        return;
                    }
                }
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn resize_is_seen_by_the_process() {
        let command = PtyCommand::new("/bin/sh")
            .arg("-c")
            .arg("echo \"size=$(stty size)\"; read line; echo \"size=$(stty size)\"");
        let (handle, mut events) = spawn(&command, (80, 24)).unwrap();
        let mut output = String::new();
        wait_for(
            &mut events,
            &mut output,
            "size=24 80",
            Duration::from_secs(10),
        );
        handle.resize(100, 30);
        handle.write(b"\r".to_vec());
        wait_for(
            &mut events,
            &mut output,
            "size=30 100",
            Duration::from_secs(10),
        );
    }

    /// `Ctrl-C` reaches the foreground group through the terminal.
    #[cfg(unix)]
    #[test]
    fn ctrl_c_interrupts_the_foreground_group() {
        let command = PtyCommand::new("/bin/sh")
            .arg("-c")
            .arg("echo ready; exec sleep 30");
        let (handle, mut events) = spawn(&command, (80, 24)).unwrap();
        let mut output = String::new();
        wait_for(&mut events, &mut output, "ready", Duration::from_secs(10));
        // Again until it works: the shell may defer a `SIGINT` caught just
        // before the `exec` and then lose it.
        let exit = (0..20).find_map(|_| {
            handle.write(b"\x03".to_vec());
            wait_exit(&mut events, Duration::from_millis(500))
        });
        assert!(
            matches!(&exit, Some(ExitStatus::Failed(_) | ExitStatus::Code(130))),
            "{exit:?}"
        );
    }

    /// An interactive shell has job control: `Ctrl-Z` stops the job.
    #[cfg(unix)]
    #[test]
    fn interactive_shell_has_job_control() {
        let command = PtyCommand::new("/bin/sh")
            .arg("-i")
            .env("ENV", "")
            .env("PS1", "$ ");
        let (handle, mut events) = spawn(&command, (80, 24)).unwrap();
        let mut output = String::new();
        wait_for(&mut events, &mut output, "$ ", Duration::from_secs(10));
        // `Ctrl-Z` flushes unread input, so only once the shell ran the
        // line. Not its echo: the terminal echoes it before a shell reading
        // whole lines (dash) has read it. The quotes keep "started" out of
        // the echo.
        handle.write(b"echo st''arted; sleep 30\r".to_vec());
        wait_for(
            &mut events,
            &mut output,
            "started\r\n",
            Duration::from_secs(10),
        );
        // Again until it works: a `SIGTSTP` that arrives before the child
        // has reset its handlers and exec'd `sleep` is lost.
        let stopped = (0..20).any(|_| {
            handle.write(b"\x1a".to_vec());
            read_until(
                &mut events,
                &mut output,
                "Stopped",
                Duration::from_millis(500),
            )
        });
        assert!(stopped, "output: {output:?}");
        // Hangs up the session, the stopped job included.
        drop(handle);
        assert!(wait_exit(&mut events, Duration::from_secs(10)).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn starts_in_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let command = PtyCommand::new("sh")
            .arg("-c")
            .arg("echo \"cwd=$(pwd -P)\"")
            .cwd(dir.path());
        let (output, exit) = run(&command, None, Duration::from_secs(10));
        let expected = format!("cwd={}", dir.path().canonicalize().unwrap().display());
        assert!(output.contains(&expected), "output: {output:?}");
        assert_eq!(exit, Some(ExitStatus::Success));
    }

    #[cfg(unix)]
    #[test]
    fn missing_program_on_path_fails_to_spawn() {
        let command = PtyCommand::new("kubach-test-385-no-such-shell");
        assert!(matches!(
            spawn(&command, (80, 24)),
            Err(PtyError::Spawn { .. })
        ));
    }

    /// Starts shells in PTYs in a loop while other threads churn the
    /// allocator and spawn threads, as in the running app (#385, like
    /// `shell_env::captures_under_thread_churn`). Run with `--ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore = "stress test, ~200 PTY launches"]
    fn spawns_under_thread_churn() {
        let stop = Arc::new(AtomicBool::new(false));
        let churners: Vec<_> = (0..8)
            .map(|_| {
                let stop = stop.clone();
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let blocks: Vec<Vec<u8>> =
                            (0..64).map(|n| vec![n as u8; 64 << (n % 8)]).collect();
                        thread::spawn(move || drop(blocks)).join().unwrap();
                    }
                })
            })
            .collect();
        let command = PtyCommand::new("/bin/sh")
            .arg("-c")
            .arg("echo kubach-ok-385");
        let failures: Vec<String> = (0..200)
            .filter_map(|_| match run(&command, None, Duration::from_secs(10)) {
                (output, Some(ExitStatus::Success)) if output.contains("kubach-ok-385") => None,
                (output, exit) => Some(format!("{exit:?}: {output:?}")),
            })
            .collect();
        stop.store(true, Ordering::Relaxed);
        for churner in churners {
            churner.join().unwrap();
        }
        assert!(
            failures.is_empty(),
            "{} failures: {failures:?}",
            failures.len()
        );
    }

    #[test]
    fn windows_shell_falls_back_to_comspec() {
        let shell = windows_shell(OsStr::new(""));
        assert!(!shell.is_empty());
    }
}
