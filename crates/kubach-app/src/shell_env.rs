//! Loading the login shell environment at startup and reporting a failure in
//! the UI: a launch from the Dock shows no stderr, and a missing `PATH` only
//! surfaces later as an obscure exec-plugin error.
//!
//! The shell runs while the window opens (#299): an rc file with `nvm` or
//! `conda` can take a second, and the window does not wait for it. What needs
//! the environment (reading the kubeconfig, exec plugins) waits for
//! [`loaded`]; see ADR 0013.

use std::future::Future;

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::Shared;
use gpui_kit::{App, Global, SharedString};
use kubach_core::tools::cli::{self, CliTool};
use kubach_i18n::t;

/// Outcome of loading the shell environment; set once at startup.
#[derive(Clone, Debug, Default)]
pub struct ShellEnvStatus {
    /// Why the environment was not loaded; `None` if it was loaded or not
    /// needed.
    pub error: Option<SharedString>,
}

impl Global for ShellEnvStatus {}

/// Resolves once the environment is stored (or not needed).
struct Loaded(Shared<oneshot::Receiver<()>>);

impl Global for Loaded {}

type Captured = Result<Vec<(std::ffi::OsString, std::ffi::OsString)>, String>;

/// The shell started by [`start`], still running.
pub struct ShellEnvLoad(Option<oneshot::Receiver<Captured>>);

/// Starts the login shell when launched outside a terminal; [`init`] stores
/// its `PATH`, `KUBECONFIG` and the rest in [`kubach_core::env`] once it
/// finishes.
///
/// Call first thing in `main`, before any other thread exists.
pub fn start() -> ShellEnvLoad {
    #[cfg(unix)]
    {
        use kubach_core::shell_env;

        if !shell_env::needed() {
            return ShellEnvLoad(None);
        }
        let shell = shell_env::user_shell();
        let (tx, rx) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("shell-env".into())
            .spawn(move || {
                let captured = shell_env::capture(&shell).map_err(|error| error.to_string());
                tx.send(captured).ok();
            });
        match spawned {
            Ok(_) => ShellEnvLoad(Some(rx)),
            Err(_) => ShellEnvLoad(None),
        }
    }
    #[cfg(not(unix))]
    ShellEnvLoad(None)
}

/// Makes the status global and stores the environment of [`start`] once
/// the shell is done; [`loaded`] resolves then.
pub fn init(load: ShellEnvLoad, cx: &mut App) {
    cx.set_global(ShellEnvStatus::default());
    let (done, loaded) = oneshot::channel();
    cx.set_global(Loaded(loaded.shared()));
    let Some(rx) = load.0 else {
        done.send(()).ok();
        return;
    };
    cx.spawn(async move |cx| {
        let captured = rx
            .await
            .unwrap_or_else(|_| Err("the shell reader stopped".into()));
        cx.update(|cx| {
            match captured {
                Ok(vars) => {
                    // Kept aside, not written with `set_var`: other threads
                    // run already, and C code reads the environment without
                    // std's lock (#307). Child processes, exec plugins and
                    // lookups take it from `kubach_core::env` (ADR 0013).
                    kubach_core::env::set_captured(vars);
                }
                Err(error) => {
                    cx.set_global(ShellEnvStatus {
                        error: Some(error.into()),
                    });
                    cx.refresh_windows();
                }
            }
            done.send(()).ok();
        });
    })
    .detach();
}

/// Resolves once the shell environment is stored, failed or not needed;
/// at once without [`init`] (tests).
pub fn loaded(cx: &App) -> impl Future<Output = ()> + 'static {
    let loaded = cx.try_global::<Loaded>().map(|loaded| loaded.0.clone());
    async move {
        if let Some(loaded) = loaded {
            loaded.await.ok();
        }
    }
}

/// Why the shell environment was not loaded, if it failed.
pub fn error(cx: &App) -> Option<SharedString> {
    cx.try_global::<ShellEnvStatus>()?.error.clone()
}

/// Banner text for a failed load.
pub fn banner_message(error: &str) -> String {
    kubach_i18n::t!("workspace-shell-env-banner", error = error)
}

/// Connection error text with a `PATH` hint for an exec plugin that was not
/// found.
pub fn exec_not_found_message(error: &str, command: Option<&str>, cx: &App) -> String {
    // The status view offers to install it (#38); PATH is beside the point.
    if command.is_some_and(cli::is_gke_plugin) && self::error(cx).is_none() {
        let hint = if cli::locate(CliTool::Gcloud).is_some() {
            t!("workspace-gke-plugin-missing")
        } else {
            t!("workspace-gke-gcloud-missing")
        };
        return format!("{error}\n\n{hint}");
    }
    let plugin = command.map_or_else(|| "The exec auth plugin".to_owned(), |c| format!("`{c}`"));
    let hint = match self::error(cx) {
        Some(shell_error) => format!(
            "{plugin} was not found in PATH. The shell environment was not loaded \
             ({shell_error}), so PATH from your shell profile is missing."
        ),
        None => {
            let path = kubach_core::env::path();
            let path = path.to_string_lossy();
            format!(
                "{plugin} was not found in PATH. Install it or add its directory to PATH \
                 in your shell profile. Current PATH: {path}"
            )
        }
    };
    format!("{error}\n\n{hint}")
}
