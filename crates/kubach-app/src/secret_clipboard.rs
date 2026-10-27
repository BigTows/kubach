//! The clipboard side of copied Secret values: Copy starts a
//! [`CLIPBOARD_TTL`] timer, and the clipboard is also checked when the vault
//! locks and when the app quits. What to clear is decided by
//! [`ClipboardGuard`] (kubach-core); this module only reads and writes the
//! system clipboard. Where the platform allows, the value is marked so that
//! clipboard history managers and cloud clipboards skip it ([`platform`]).

mod platform;

use std::time::Instant;

use gpui_kit::*;
use kubach_core::secrets::{CLIPBOARD_TTL, ClearTrigger, ClipboardGuard};
use zeroize::Zeroize as _;

struct SecretClipboard {
    guard: ClipboardGuard,
    /// Writes values marked for clipboard managers; on Linux owns the X11
    /// selection while it holds a value.
    concealer: platform::Concealer,
    /// Clears the clipboard when the last copy expires.
    _timer: Option<Task<()>>,
}

impl Global for SecretClipboard {}

/// Sets up the guard and clears the clipboard on quit.
pub fn init(cx: &mut App) {
    cx.set_global(SecretClipboard {
        guard: ClipboardGuard::new(),
        concealer: platform::Concealer::default(),
        _timer: None,
    });
    cx.on_app_quit(|cx| {
        clear(ClearTrigger::Quit, cx);
        async {}
    })
    .detach();
}

/// Puts a Secret value (or any other credential, such as the MCP token) into
/// the clipboard for at most [`CLIPBOARD_TTL`].
pub fn copy(text: &str, cx: &mut App) {
    write(text, cx);
    if !cx.has_global::<SecretClipboard>() {
        return;
    }
    let timer = cx.spawn(async move |cx| {
        cx.background_executor().timer(CLIPBOARD_TTL).await;
        cx.update(|cx| clear(ClearTrigger::Expired, cx));
    });
    let state = cx.global_mut::<SecretClipboard>();
    state.guard.copied(text, Instant::now());
    state._timer = Some(timer);
}

/// Empties the clipboard if `trigger` calls for it and the clipboard still
/// holds the copied value.
pub fn clear(trigger: ClearTrigger, cx: &mut App) {
    if !cx
        .try_global::<SecretClipboard>()
        .is_some_and(|state| state.guard.is_pending())
    {
        return;
    }
    // The clipboard may hold the value: compared, then wiped.
    let mut current = cx.read_from_clipboard().and_then(|item| item.text());
    let clear = cx.global_mut::<SecretClipboard>().guard.should_clear(
        trigger,
        Instant::now(),
        current.as_deref(),
    );
    current.zeroize();
    if clear && !cx.global_mut::<SecretClipboard>().concealer.clear() {
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    }
}

/// Replaces the clipboard with `text`, marked concealed where supported.
fn write(text: &str, cx: &mut App) {
    let concealed = cx.has_global::<SecretClipboard>()
        && cx.global_mut::<SecretClipboard>().concealer.write(text);
    if !concealed {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    }
}
