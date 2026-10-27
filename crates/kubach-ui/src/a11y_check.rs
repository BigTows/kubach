//! Accessibility-tree self-check for debug runs (#354).
//!
//! GPUI builds the accessibility tree only while an assistive client is
//! connected, and a debug build panics on a duplicate node id ("Duplicate
//! a11y node id") only then. With `KUBACH_A11Y_CHECK=1` the process connects
//! to its own windows as such a client, lets a few frames render with the
//! tree on, and quits: exit code 0 means no duplicates, a duplicate panics.
//! [`run`] drives such a run from an ignored test: `kubach-ui`'s
//! `tests/a11y_ids.rs` for the gallery, `kubach-app`'s `tests/a11y_check.rs`
//! for the main window.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui_kit::App;

/// The variable that turns the check on.
pub const ENV: &str = "KUBACH_A11Y_CHECK";

/// Time for the windows to open and settle before the tree is turned on.
const SETTLE: Duration = Duration::from_millis(1500);
/// Frames rendered with the tree on before quitting.
const RUN: Duration = Duration::from_millis(2000);

/// Starts the check if [`ENV`] is set; call after opening the windows.
pub fn start(cx: &mut App) {
    if std::env::var_os(ENV).is_none() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SETTLE).await;
        activate();
        cx.background_executor().timer(RUN).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Runs `command` (the gallery or the app binary) with the check on and
/// waits for it to quit; `Err` carries its stderr if it failed. The window
/// shows on screen for a few seconds.
pub fn run(command: &mut Command) -> Result<(), String> {
    let mut child = command
        .env(ENV, "1")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("can't start {command:?}: {err}"))?;
    let deadline = Instant::now() + SETTLE + RUN + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            result => {
                child.kill().ok();
                child.wait().ok();
                return Err(format!("{command:?} did not quit: {result:?}"));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        std::io::Read::read_to_string(&mut pipe, &mut stderr).ok();
    }
    Err(format!("{command:?} failed ({status}):\n{stderr}"))
}

/// Asks every window for its accessibility children, which is what an
/// assistive client does first; GPUI then builds the tree on every frame.
#[cfg(target_os = "macos")]
fn activate() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: plain AppKit getters on the main thread (the foreground
    // executor), on objects AppKit owns for the life of the app.
    unsafe {
        let app: *mut AnyObject = msg_send![class, sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        for ix in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: ix];
            let view: *mut AnyObject = msg_send![window, contentView];
            if !view.is_null() {
                let _: *mut AnyObject = msg_send![view, accessibilityChildren];
            }
        }
    }
}

/// Only the macOS activation is wired up; elsewhere the run just quits.
#[cfg(not(target_os = "macos"))]
fn activate() {}
