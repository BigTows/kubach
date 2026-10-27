//! Starting child processes with `posix_spawn`, without `fork` (Unix).
//!
//! `std::process::Command` with a `pre_exec` hook (and crates built on it,
//! such as `portable-pty`) forks the whole multi-threaded process and runs
//! Rust code in the child. Only async-signal-safe code may run there; an
//! allocation or a lock taken by another thread at the moment of the fork
//! can deadlock or crash the child, and on macOS it sometimes aborts in
//! libSystem's atfork handlers (`os_once_t is corrupt`, #375).
//!
//! `posix_spawn` runs no code of ours in the child (macOS: a single syscall;
//! glibc and musl: `clone(CLONE_VM | CLONE_VFORK)` without atfork handlers),
//! and it can still create a new session (`POSIX_SPAWN_SETSID`), reset
//! signals and set up the descriptors. Used by the login-shell environment
//! capture ([`crate::shell_env`]) and the local terminal ([`crate::pty`]).

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;

/// `POSIX_SPAWN_SETSID` from `<spawn.h>` (macOS 10.15+); the `libc` crate
/// does not export it for Apple targets.
#[cfg(target_vendor = "apple")]
const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
#[cfg(not(target_vendor = "apple"))]
use libc::POSIX_SPAWN_SETSID;

/// `program` and `args` as a C `argv`.
pub(crate) fn c_args<'a>(
    program: &'a OsStr,
    args: impl IntoIterator<Item = &'a OsStr>,
) -> io::Result<Vec<CString>> {
    std::iter::once(program)
        .chain(args)
        .map(|arg| CString::new(arg.as_bytes()).map_err(|_| nul_error()))
        .collect()
}

/// `KEY=value` entries for `envp`; entries that cannot be passed to exec
/// (with a NUL byte) are skipped.
pub(crate) fn c_env(env: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<CString> {
    env.into_iter()
        .filter_map(|(key, value)| {
            let mut entry = key.into_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).ok()
        })
        .collect()
}

/// A path as a C string.
pub(crate) fn c_path(path: &OsStr) -> io::Result<CString> {
    CString::new(path.as_bytes()).map_err(|_| nul_error())
}

fn nul_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "nul byte in argument")
}

/// Pointers to `strings` followed by null, for `argv` and `envp`.
pub(crate) fn null_terminated(strings: &[CString]) -> Vec<*mut libc::c_char> {
    strings
        .iter()
        .map(|string| string.as_ptr().cast_mut())
        .chain([ptr::null_mut()])
        .collect()
}

/// A close-on-exec pipe whose ends are above the standard descriptors, so
/// `dup2` onto stdout always clears close-on-exec in the child, even when
/// the app was started with stdio closed.
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `pipe` just returned these descriptors and nothing owns them.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((above_stdio(&read)?, above_stdio(&write)?))
}

/// A close-on-exec duplicate of `fd` numbered 3 or above.
pub(crate) fn above_stdio(fd: &impl AsRawFd) -> io::Result<OwnedFd> {
    // SAFETY: plain syscall on a live descriptor.
    let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if dup == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fcntl` just returned this descriptor and nothing owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// Maps a `posix_spawn*` return code to a result.
pub(crate) fn check(code: libc::c_int) -> io::Result<()> {
    match code {
        0 => Ok(()),
        code => Err(io::Error::from_raw_os_error(code)),
    }
}

/// `posix_spawnattr_t`, destroyed on drop. Boxed so that it never moves
/// after `init`, which POSIX does not promise to be safe.
pub(crate) struct SpawnAttr(Box<MaybeUninit<libc::posix_spawnattr_t>>);

impl SpawnAttr {
    pub(crate) fn new() -> io::Result<Self> {
        let mut attr = Box::new(MaybeUninit::uninit());
        // SAFETY: `init` initialises the object in place.
        check(unsafe { libc::posix_spawnattr_init(attr.as_mut_ptr()) })?;
        Ok(Self(attr))
    }

    /// Attributes for a child in a new session of its own (`setsid`), with
    /// an empty signal mask and the default action for `default_signals`
    /// (exec keeps ignored signals ignored; handlers are reset anyway). On
    /// Apple platforms only the descriptors named in the file actions reach
    /// the child, not whatever other threads opened without close-on-exec.
    pub(crate) fn new_session(default_signals: &[libc::c_int]) -> io::Result<Self> {
        let attr = Self::new()?;
        // SAFETY: the attribute object is initialised; the signal set is
        // initialised by `sigemptyset` before use.
        unsafe {
            let mut signals = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(signals.as_mut_ptr());
            check(libc::posix_spawnattr_setsigmask(
                attr.ptr(),
                signals.as_ptr(),
            ))?;
            for &signal in default_signals {
                libc::sigaddset(signals.as_mut_ptr(), signal);
            }
            check(libc::posix_spawnattr_setsigdefault(
                attr.ptr(),
                signals.as_ptr(),
            ))?;
            let flags = POSIX_SPAWN_SETSID
                | libc::POSIX_SPAWN_SETSIGMASK as libc::c_short
                | libc::POSIX_SPAWN_SETSIGDEF as libc::c_short;
            #[cfg(target_vendor = "apple")]
            let flags = flags | libc::POSIX_SPAWN_CLOEXEC_DEFAULT as libc::c_short;
            check(libc::posix_spawnattr_setflags(attr.ptr(), flags))?;
        }
        Ok(attr)
    }

    pub(crate) fn ptr(&self) -> *mut libc::posix_spawnattr_t {
        self.0.as_ptr().cast_mut()
    }
}

impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: initialised in `new`, destroyed once.
        unsafe { libc::posix_spawnattr_destroy(self.0.as_mut_ptr()) };
    }
}

/// `posix_spawn_file_actions_t`, destroyed on drop; boxed like [`SpawnAttr`].
pub(crate) struct FileActions(Box<MaybeUninit<libc::posix_spawn_file_actions_t>>);

impl FileActions {
    pub(crate) fn new() -> io::Result<Self> {
        let mut actions = Box::new(MaybeUninit::uninit());
        // SAFETY: `init` initialises the object in place.
        check(unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) })?;
        Ok(Self(actions))
    }

    pub(crate) fn ptr(&self) -> *mut libc::posix_spawn_file_actions_t {
        self.0.as_ptr().cast_mut()
    }

    /// Opens `path` as `fd` in the child. The path is copied.
    pub(crate) fn open(
        &self,
        fd: libc::c_int,
        path: &CString,
        flags: libc::c_int,
    ) -> io::Result<()> {
        // SAFETY: initialised object, live NUL-terminated path.
        check(unsafe {
            libc::posix_spawn_file_actions_addopen(self.ptr(), fd, path.as_ptr(), flags, 0)
        })
    }

    /// Duplicates `from` onto `to` in the child.
    pub(crate) fn dup2(&self, from: libc::c_int, to: libc::c_int) -> io::Result<()> {
        // SAFETY: initialised object; descriptors are plain numbers here.
        check(unsafe { libc::posix_spawn_file_actions_adddup2(self.ptr(), from, to) })
    }

    /// Changes the child's working directory (after the actions before it).
    pub(crate) fn chdir(&self, dir: &CString) -> io::Result<()> {
        // SAFETY: initialised object, live NUL-terminated path.
        check(unsafe { libc::posix_spawn_file_actions_addchdir_np(self.ptr(), dir.as_ptr()) })
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: initialised in `new`, destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(self.0.as_mut_ptr()) };
    }
}

/// `posix_spawn` (`posix_spawnp` when `search_path`) of `argv[0]`.
pub(crate) fn spawn(
    argv: &[CString],
    envp: &[CString],
    actions: &FileActions,
    attr: &SpawnAttr,
    search_path: bool,
) -> io::Result<libc::pid_t> {
    let program = argv.first().ok_or_else(nul_error)?;
    let argv_ptrs = null_terminated(argv);
    let envp_ptrs = null_terminated(envp);
    let mut pid = 0;
    // SAFETY: the attribute and file-action objects are initialised and
    // outlive the call; every pointer refers to a live NUL-terminated string
    // and both pointer arrays end with null.
    check(unsafe {
        if search_path {
            libc::posix_spawnp(
                &mut pid,
                program.as_ptr(),
                actions.ptr(),
                attr.ptr(),
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            )
        } else {
            libc::posix_spawn(
                &mut pid,
                program.as_ptr(),
                actions.ptr(),
                attr.ptr(),
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            )
        }
    })?;
    Ok(pid)
}

/// Reaps `pid` and returns its exit status.
pub(crate) fn wait(pid: libc::pid_t) -> io::Result<ExitStatus> {
    loop {
        let mut status = 0;
        // SAFETY: plain syscall; `pid` is our unreaped child.
        if unsafe { libc::waitpid(pid, &mut status, 0) } != -1 {
            return Ok(ExitStatus::from_raw(status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
