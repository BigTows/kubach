//! Environment of the user's login shell.
//!
//! An app started from Finder, the Dock or a Linux `.desktop` entry does not
//! inherit the shell environment, so kubeconfig exec plugins (`aws`,
//! `gke-gcloud-auth-plugin`, `kubelogin`) are missing from `PATH` and
//! `KUBECONFIG` is unset. Like Zed, we run `$SHELL -l -i -c` once at startup
//! and take its environment. Windows keeps environment variables globally, so
//! this is Unix-only.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitStatus;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::spawn::{self, FileActions, SpawnAttr, wait};

/// How long a slow rc file may delay startup.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Separates rc-file output from the `env -0` dump.
const MARKER: &str = "_KUBACH_SHELL_ENV_7f3c1a_";

/// Variables describing the capturing shell itself rather than the user's setup.
const SKIPPED: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

#[derive(Debug, thiserror::Error)]
pub enum ShellEnvError {
    #[error("failed to start {shell}: {source}")]
    Spawn {
        shell: String,
        #[source]
        source: io::Error,
    },
    #[error("{shell} did not finish within {} s", TIMEOUT.as_secs())]
    Timeout { shell: String },
    #[error("failed to read output of {shell}: {source}")]
    Read {
        shell: String,
        #[source]
        source: io::Error,
    },
    #[error("{shell} printed no environment ({status})")]
    NoOutput { shell: String, status: ExitStatus },
}

/// Whether the process was started outside a terminal and so lacks the shell
/// environment. Launches from Finder, the Dock or a `.desktop` entry have
/// neither stdin nor stdout attached to a TTY; `cargo run` and shell launches
/// have both.
pub fn needed() -> bool {
    !io::stdin().is_terminal() && !io::stdout().is_terminal()
}

/// The user's shell: `$SHELL`, or `/bin/sh` when unset.
pub fn user_shell() -> OsString {
    crate::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Runs `shell` as a login interactive shell and returns its environment,
/// without variables that only describe that shell process.
pub fn capture(shell: &OsStr) -> Result<Vec<(OsString, OsString)>, ShellEnvError> {
    capture_with_env(shell, std::env::vars_os())
}

/// [`capture`] with the given environment for the shell.
fn capture_with_env(
    shell: &OsStr,
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Vec<(OsString, OsString)>, ShellEnvError> {
    let name = shell.to_string_lossy().into_owned();
    // The same command line is valid in sh, bash, zsh and fish.
    let script = format!("printf '%s' '{MARKER}'; /usr/bin/env -0");
    let (pid, mut stdout) =
        spawn_session(shell, &["-l", "-i", "-c", &script], env).map_err(|source| {
            ShellEnvError::Spawn {
                shell: name.clone(),
                source,
            }
        })?;

    let (tx, rx) = mpsc::channel();
    // A daemon started by an rc file may keep the pipe open forever, so the
    // reader is detached rather than joined.
    thread::spawn(move || {
        let mut output = Vec::new();
        let _ = tx.send(stdout.read_to_end(&mut output).map(|_| output));
    });

    let output = match rx.recv_timeout(TIMEOUT) {
        Ok(output) => output,
        Err(_) => {
            // The shell leads its own session and process group, so this
            // also kills whatever the rc files started.
            // SAFETY: plain syscall; the group still exists as the shell has
            // not been reaped.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            let _ = wait(pid);
            return Err(ShellEnvError::Timeout { shell: name });
        }
    };
    let output = output.map_err(|source| ShellEnvError::Read {
        shell: name.clone(),
        source,
    })?;
    let status = wait(pid).map_err(|source| ShellEnvError::Read {
        shell: name.clone(),
        source,
    })?;

    parse(&output)
        .map(|vars| {
            vars.into_iter()
                .filter(|(key, _)| !SKIPPED.iter().any(|skip| key == OsStr::new(skip)))
                .collect()
        })
        .ok_or(ShellEnvError::NoOutput {
            shell: name,
            status,
        })
}

/// Starts `program` with `args` and `env` in a new session, with stdin and
/// stderr on `/dev/null`; returns its pid and the read end of its stdout.
///
/// A new session has no controlling terminal, so an interactive shell cannot
/// grab or stop on our TTY. The session is created by `posix_spawn` itself
/// rather than by `setsid` in a `pre_exec` hook, which would fork the whole
/// multi-threaded process (#375, see [`crate::spawn`]).
fn spawn_session(
    program: &OsStr,
    args: &[&str],
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> io::Result<(libc::pid_t, File)> {
    let argv = spawn::c_args(program, args.iter().map(OsStr::new))?;
    let envp = spawn::c_env(env);
    let (read, write) = spawn::pipe()?;
    let actions = FileActions::new()?;
    let dev_null = c"/dev/null".to_owned();
    actions.open(libc::STDIN_FILENO, &dev_null, libc::O_RDONLY)?;
    actions.dup2(write.as_raw_fd(), libc::STDOUT_FILENO)?;
    actions.open(libc::STDERR_FILENO, &dev_null, libc::O_WRONLY)?;
    // Rust ignores SIGPIPE; the child gets the default action back, as
    // with `std::process::Command`.
    let attr = SpawnAttr::new_session(&[libc::SIGPIPE])?;
    let pid = spawn::spawn(&argv, &envp, &actions, &attr, true)?;
    Ok((pid, File::from(read)))
}

/// Parses `env -0` output that follows [`MARKER`]; anything before the marker
/// is rc-file noise. Returns `None` if the marker is missing.
fn parse(output: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let start = output
        .windows(MARKER.len())
        .position(|window| window == MARKER.as_bytes())?
        + MARKER.len();
    let vars = output[start..]
        .split(|&byte| byte == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|&byte| byte == b'=')?;
            if eq == 0 {
                return None;
            }
            let (key, value) = (&entry[..eq], &entry[eq + 1..]);
            Some((
                OsStr::from_bytes(key).to_owned(),
                OsString::from_vec(value.to_vec()),
            ))
        })
        .collect();
    Some(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(vars: &[(OsString, OsString)]) -> Vec<(&str, &str)> {
        vars.iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect()
    }

    #[test]
    fn parses_vars_after_marker() {
        let output = format!("{MARKER}PATH=/usr/bin:/bin\0HOME=/home/me\0");
        let vars = parse(output.as_bytes()).unwrap();
        assert_eq!(
            pairs(&vars),
            [("PATH", "/usr/bin:/bin"), ("HOME", "/home/me")]
        );
    }

    #[test]
    fn skips_noise_before_marker() {
        let output = format!("Welcome!\nFOO=bar\0\n{MARKER}A=1\0");
        assert_eq!(pairs(&parse(output.as_bytes()).unwrap()), [("A", "1")]);
    }

    #[test]
    fn keeps_empty_values_newlines_and_equals() {
        let output = format!("{MARKER}EMPTY=\0MULTI=a\nb\0OPTS=--x=1=2\0");
        assert_eq!(
            pairs(&parse(output.as_bytes()).unwrap()),
            [("EMPTY", ""), ("MULTI", "a\nb"), ("OPTS", "--x=1=2")]
        );
    }

    #[test]
    fn missing_marker_is_none() {
        assert!(parse(b"PATH=/bin\0").is_none());
    }

    #[test]
    fn ignores_entries_without_equals() {
        let output = format!("{MARKER}garbage\0A=1\0");
        assert_eq!(pairs(&parse(output.as_bytes()).unwrap()), [("A", "1")]);
    }

    #[test]
    fn captures_from_sh() {
        let vars = capture(OsStr::new("/bin/sh")).unwrap();
        assert!(vars.iter().any(|(key, _)| key == "PATH"));
        assert!(!vars.iter().any(|(key, _)| key == "PWD" || key == "SHLVL"));
    }

    #[test]
    fn reports_missing_shell() {
        let error = capture(OsStr::new("/nonexistent/kubach-test-55/shell")).unwrap_err();
        assert!(
            matches!(&error, ShellEnvError::Spawn { source, .. } if source.kind() == io::ErrorKind::NotFound),
            "expected Spawn(NotFound), got {error:?}"
        );
    }

    #[test]
    fn reports_missing_shell_on_path() {
        let error = capture(OsStr::new("kubach-test-375-no-such-shell")).unwrap_err();
        assert!(
            matches!(&error, ShellEnvError::Spawn { source, .. } if source.kind() == io::ErrorKind::NotFound),
            "expected Spawn(NotFound), got {error:?}"
        );
    }

    #[test]
    fn passes_environment_to_shell() {
        // No HOME: the shell reads no user rc files.
        let env = [
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("KUBACH_TEST_375".into(), "a b=c\nd".into()),
        ];
        let vars = capture_with_env(OsStr::new("/bin/sh"), env).unwrap();
        let value = vars
            .iter()
            .find(|(key, _)| key == "KUBACH_TEST_375")
            .map(|(_, value)| value.to_str().unwrap());
        assert_eq!(value, Some("a b=c\nd"));
    }

    #[test]
    fn shell_leads_its_own_session() {
        let (pid, mut stdout) = spawn_session(
            OsStr::new("/bin/sh"),
            &["-c", "ps -o pgid= -p $$; ps -o sess= -p $$"],
            std::env::vars_os(),
        )
        .unwrap();
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert!(wait(pid).unwrap().success());
        let mut lines = output.lines().map(str::trim);
        // A session leader also leads its process group.
        assert_eq!(lines.next(), Some(pid.to_string().as_str()), "{output}");
        // macOS `ps` prints 0 for the session id; Linux prints the leader's pid.
        let sess = lines.next().unwrap_or_default();
        assert!(sess == "0" || sess == pid.to_string(), "{output}");
    }

    /// Captures in a loop while other threads churn the allocator and spawn
    /// threads, as at app startup (#375). Run with `--ignored`.
    #[test]
    #[ignore = "stress test, ~200 shell launches"]
    fn captures_under_thread_churn() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

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
        let failures: Vec<String> = (0..200)
            .filter_map(|_| match capture(OsStr::new("/bin/sh")) {
                Ok(vars) if vars.iter().any(|(key, _)| key == "PATH") => None,
                Ok(_) => Some("no PATH".into()),
                Err(error) => Some(error.to_string()),
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
}
