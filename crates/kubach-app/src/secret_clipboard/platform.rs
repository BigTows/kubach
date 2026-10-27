//! Writes a Secret value to the system clipboard marked so that clipboard
//! history managers and cloud clipboards leave it alone (#203). GPUI writes
//! plain text only, so this talks to the OS directly.
//!
//! - macOS: `org.nspasteboard.ConcealedType` and `TransientType`
//!   (nspasteboard.org; Raycast, Paste, Maccy, Alfred), and the contents stay
//!   on this Mac (`NSPasteboardContentsCurrentHostOnly`: no Universal
//!   Clipboard).
//! - Windows: `ExcludeClipboardContentFromMonitorProcessing`, and
//!   `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` set to 0
//!   (Win+V history and cloud sync).
//! - Linux: `x-kde-passwordManagerHint: secret` next to the text (Klipper,
//!   CopyQ, KeePassXC and others), written by `arboard` on X11 and on Wayland
//!   (`wlr`/`ext-data-control`; without it, through XWayland). GPUI's own
//!   selection owner offers fixed text types only. On X11 the value is served
//!   by the process that owns the selection, so the `arboard` handle lives in
//!   [`Concealer`] for the life of the app; on Wayland a background thread of
//!   `wl-clipboard-rs` serves it until the clipboard changes.
//! - Elsewhere, or when the clipboard cannot be opened that way (no display,
//!   no protocol), the caller writes the value the usual way through GPUI.

/// Pasteboard types that tell macOS clipboard managers not to record the
/// value (nspasteboard.org). Their contents do not matter.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MACOS_MARKER_TYPES: [&str; 2] = [
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.TransientType",
];

/// Registered clipboard formats with the DWORD each carries: excluded from
/// clipboard monitors (the value does not matter), from Win+V history and
/// from cloud clipboard sync.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WINDOWS_MARKER_FORMATS: [(&str, u32); 3] = [
    ("ExcludeClipboardContentFromMonitorProcessing", 1),
    ("CanIncludeInClipboardHistory", 0),
    ("CanUploadToCloudClipboard", 0),
];

/// `text` as `CF_UNICODETEXT`: UTF-16 ending in NUL, with embedded NULs
/// replaced by spaces (as GPUI does) so the value is not cut short.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn utf16_nul(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .map(|unit| if unit == 0 { u16::from(b' ') } else { unit })
        .chain(Some(0))
        .collect()
}

/// Replaces the clipboard with `text`, marked as above. `false` when the
/// clipboard could not be written; nothing was written then.
#[cfg(target_os = "macos")]
pub fn write_concealed(text: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardContentsOptions, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    let pasteboard = NSPasteboard::generalPasteboard();
    // Clears the pasteboard; what is written next stays on this host.
    pasteboard.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    // SAFETY: an immutable AppKit constant, valid for the life of the process.
    let string_type = unsafe { NSPasteboardTypeString };
    // Markers first: a manager reacting to the change sees them with the text.
    for marker in MACOS_MARKER_TYPES {
        pasteboard.setString_forType(&NSString::new(), &NSString::from_str(marker));
    }
    pasteboard.setString_forType(&NSString::from_str(text), string_type)
}

#[cfg(windows)]
pub fn write_concealed(text: &str) -> bool {
    use std::ptr;

    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
    use zeroize::Zeroize as _;

    /// Hands a copy of `bytes` to the clipboard as `format`.
    ///
    /// # Safety
    /// The clipboard must be open by this thread.
    unsafe fn set(format: u32, bytes: &[u8]) -> bool {
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if memory.is_null() {
                return false;
            }
            let target = GlobalLock(memory).cast::<u8>();
            if target.is_null() {
                GlobalFree(memory);
                return false;
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            GlobalUnlock(memory);
            // On success the clipboard owns the memory.
            if SetClipboardData(format, memory).is_null() {
                GlobalFree(memory);
                return false;
            }
            true
        }
    }

    let mut wide = utf16_nul(text);
    // SAFETY: open, write and close on this thread; every handle passed to
    // the clipboard was allocated just above.
    let written = unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            wide.zeroize();
            return false;
        }
        let mut written = EmptyClipboard() != 0;
        if written {
            // Markers first: a monitor reacting to the change sees them.
            for (name, value) in WINDOWS_MARKER_FORMATS {
                let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                let format = RegisterClipboardFormatW(name.as_ptr());
                if format != 0 {
                    set(format, &value.to_ne_bytes());
                }
            }
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            written = set(u32::from(CF_UNICODETEXT), bytes);
        }
        CloseClipboard();
        written
    };
    wide.zeroize();
    written
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn write_concealed(_text: &str) -> bool {
    false
}

/// The system clipboard as far as Secret values go. Kept in the app state:
/// on Linux it holds the `arboard` handle that owns the X11 selection.
#[derive(Default)]
pub struct Concealer {
    #[cfg(target_os = "linux")]
    linux: SelectionWriter<arboard::Clipboard>,
}

impl Concealer {
    /// Replaces the clipboard with `text`, marked as above. `false` when the
    /// platform has no such marks or the clipboard could not be written;
    /// nothing was written then.
    pub fn write(&mut self, text: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.linux.write(text)
        }
        #[cfg(not(target_os = "linux"))]
        {
            write_concealed(text)
        }
    }

    /// Empties the clipboard after a [`Self::write`]. `false` when it was
    /// not emptied; the caller then writes an empty string through GPUI.
    pub fn clear(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.linux.clear()
        }
        #[cfg(not(target_os = "linux"))]
        {
            write_concealed("")
        }
    }
}

/// A clipboard that can take a value marked secret: `arboard` on Linux, a
/// fake in tests.
#[cfg(any(target_os = "linux", test))]
pub trait SecretSelection: Sized {
    /// Connects to the clipboard; `None` without a display.
    fn open() -> Option<Self>;
    /// Replaces the clipboard with `text` marked secret.
    fn set_secret(&mut self, text: &str) -> bool;
    /// Empties the clipboard (on X11 gives up the selection).
    fn clear(&mut self) -> bool;
}

#[cfg(target_os = "linux")]
impl SecretSelection for arboard::Clipboard {
    fn open() -> Option<Self> {
        arboard::Clipboard::new().ok()
    }

    fn set_secret(&mut self, text: &str) -> bool {
        use arboard::SetExtLinux as _;
        // No wait: on X11 the selection is served by arboard's own thread as
        // long as `self` lives; on Wayland by a wl-clipboard-rs thread.
        self.set().exclude_from_history().text(text).is_ok()
    }

    fn clear(&mut self) -> bool {
        arboard::Clipboard::clear(self).is_ok()
    }
}

/// Writes Secret values through a [`SecretSelection`] opened on first use,
/// and tells the caller when to fall back to GPUI.
#[cfg(any(target_os = "linux", test))]
pub struct SelectionWriter<S> {
    selection: Option<S>,
    /// Opening failed once: no display or protocol for it, not retried.
    unavailable: bool,
    /// The clipboard was last written through `selection`.
    written: bool,
}

#[cfg(any(target_os = "linux", test))]
impl<S> Default for SelectionWriter<S> {
    fn default() -> Self {
        Self {
            selection: None,
            unavailable: false,
            written: false,
        }
    }
}

#[cfg(any(target_os = "linux", test))]
impl<S: SecretSelection> SelectionWriter<S> {
    /// `false` when the value was not written and GPUI should write it.
    pub fn write(&mut self, text: &str) -> bool {
        self.written = false;
        if self.unavailable {
            return false;
        }
        if self.selection.is_none() {
            self.selection = S::open();
            self.unavailable = self.selection.is_none();
        }
        let Some(selection) = self.selection.as_mut() else {
            return false;
        };
        self.written = selection.set_secret(text);
        if !self.written {
            // Reconnects next time (the X11 server thread may have stopped).
            self.selection = None;
        }
        self.written
    }

    /// Empties the clipboard written by [`Self::write`]; `false` when it was
    /// written some other way or could not be emptied.
    pub fn clear(&mut self) -> bool {
        if !std::mem::take(&mut self.written) {
            return false;
        }
        let cleared = self.selection.as_mut().is_some_and(S::clear);
        if !cleared {
            self.selection = None;
        }
        cleared
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        MACOS_MARKER_TYPES, SecretSelection, SelectionWriter, WINDOWS_MARKER_FORMATS, utf16_nul,
    };

    /// What the fake clipboard is told to do and what it did.
    #[derive(Default)]
    struct Script {
        open_fails: bool,
        set_fails: bool,
        clear_fails: bool,
        opened: usize,
        contents: Option<String>,
    }

    thread_local! {
        static SCRIPT: RefCell<Script> = RefCell::default();
    }

    fn script<T>(f: impl FnOnce(&mut Script) -> T) -> T {
        SCRIPT.with(|script| f(&mut script.borrow_mut()))
    }

    struct FakeSelection;

    impl SecretSelection for FakeSelection {
        fn open() -> Option<Self> {
            script(|s| {
                s.opened += 1;
                (!s.open_fails).then_some(FakeSelection)
            })
        }

        fn set_secret(&mut self, text: &str) -> bool {
            script(|s| {
                if !s.set_fails {
                    s.contents = Some(text.to_owned());
                }
                !s.set_fails
            })
        }

        fn clear(&mut self) -> bool {
            script(|s| {
                if !s.clear_fails {
                    s.contents = None;
                }
                !s.clear_fails
            })
        }
    }

    fn writer() -> SelectionWriter<FakeSelection> {
        SCRIPT.set(Script::default());
        SelectionWriter::default()
    }

    #[test]
    fn secret_goes_through_one_selection_kept_open() {
        let mut writer = writer();
        assert!(writer.write("one"));
        assert!(writer.write("two"));
        // The handle that owns the selection is kept, not reopened per copy.
        assert_eq!(script(|s| s.opened), 1);
        assert_eq!(script(|s| s.contents.clone()).as_deref(), Some("two"));
        assert!(writer.clear());
        assert_eq!(script(|s| s.contents.clone()), None);
    }

    #[test]
    fn no_display_falls_back_without_retrying() {
        let mut writer = writer();
        script(|s| s.open_fails = true);
        assert!(!writer.write("secret"));
        assert!(!writer.write("secret"));
        assert_eq!(script(|s| s.opened), 1);
        // Written by GPUI, so emptied by GPUI too.
        assert!(!writer.clear());
    }

    #[test]
    fn failed_write_falls_back_and_reconnects() {
        let mut writer = writer();
        script(|s| s.set_fails = true);
        assert!(!writer.write("secret"));
        assert!(!writer.clear());
        script(|s| s.set_fails = false);
        assert!(writer.write("secret"));
        assert_eq!(script(|s| s.opened), 2);
    }

    #[test]
    fn clear_only_after_own_write() {
        let mut writer = writer();
        assert!(!writer.clear());
        assert!(writer.write("secret"));
        assert!(writer.clear());
        // Already emptied: a second trigger leaves it to GPUI's check.
        assert!(!writer.clear());
    }

    #[test]
    fn failed_clear_falls_back_to_gpui() {
        let mut writer = writer();
        assert!(writer.write("secret"));
        script(|s| s.clear_fails = true);
        assert!(!writer.clear());
        script(|s| s.clear_fails = false);
        assert!(writer.write("secret"));
        assert_eq!(script(|s| s.opened), 2);
    }

    #[test]
    fn windows_text_is_nul_terminated_utf16() {
        assert_eq!(utf16_nul(""), vec![0]);
        assert_eq!(utf16_nul("ab"), vec![97, 98, 0]);
        // Outside the BMP: a surrogate pair.
        assert_eq!(utf16_nul("🔑").len(), 3);
    }

    #[test]
    fn embedded_nul_does_not_cut_the_value() {
        assert_eq!(utf16_nul("a\0b"), vec![97, 32, 98, 0]);
    }

    #[test]
    fn markers_keep_history_and_sync_off() {
        assert!(MACOS_MARKER_TYPES.contains(&"org.nspasteboard.ConcealedType"));
        assert!(MACOS_MARKER_TYPES.contains(&"org.nspasteboard.TransientType"));
        let value = |name: &str| {
            WINDOWS_MARKER_FORMATS
                .iter()
                .find(|(format, _)| *format == name)
                .map(|(_, value)| *value)
        };
        assert!(value("ExcludeClipboardContentFromMonitorProcessing").is_some());
        assert_eq!(value("CanIncludeInClipboardHistory"), Some(0));
        assert_eq!(value("CanUploadToCloudClipboard"), Some(0));
    }
}
