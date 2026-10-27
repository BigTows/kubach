//! Whether the OS screen is locked, so the vault can lock with it (ADR
//! 0008). Polled by the app every few seconds while the vault is unlocked:
//!
//! - macOS: `CGSSessionScreenIsLocked` in the CoreGraphics session
//!   dictionary (set while the lock screen is up).
//! - Windows: the session flags from `WTSQuerySessionInformationW`
//!   (`WTS_SESSIONSTATE_LOCK`), the state behind WTS session notifications.
//! - Linux: the `LockedHint` property of the logind session over D-Bus,
//!   which desktop lock screens set (GNOME, KDE, …).
//!
//! Polling instead of notifications needs no window procedure, run loop
//! observer or signal subscription, and checking once in a few seconds is
//! cheap.

/// Asks the OS whether the screen is locked.
#[derive(Default)]
pub struct ScreenLock {
    #[cfg(target_os = "linux")]
    logind: linux::Logind,
}

impl ScreenLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Some(true)` while the screen is locked, `None` when the OS can't
    /// tell (no GUI session, no logind, another OS). Never blocks for long:
    /// only local calls; run it off the UI thread anyway.
    pub async fn is_locked(&mut self) -> Option<bool> {
        #[cfg(target_os = "macos")]
        {
            macos::is_locked()
        }
        #[cfg(target_os = "windows")]
        {
            windows::is_locked()
        }
        #[cfg(target_os = "linux")]
        {
            self.logind.is_locked().await
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFStringCreateWithCString(
            alloc: CFTypeRef,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
    }

    const UTF8: u32 = 0x0800_0100;

    pub fn is_locked() -> Option<bool> {
        // SAFETY: plain CoreFoundation calls; every object created or
        // copied here is released once, `value` is borrowed from `session`
        // and not used after it is released.
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                // Not in a GUI session.
                return None;
            }
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                c"CGSSessionScreenIsLocked".as_ptr(),
                UTF8,
            );
            let locked = if key.is_null() {
                None
            } else {
                let value = CFDictionaryGetValue(session, key);
                CFRelease(key);
                Some(if value.is_null() {
                    false
                } else if CFGetTypeID(value) == CFBooleanGetTypeID() {
                    CFBooleanGetValue(value) != 0
                } else {
                    // Present at all means locked.
                    true
                })
            };
            CFRelease(session);
            locked
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::c_void;

    /// `WTSINFOEX_LEVEL1_W`.
    #[repr(C)]
    struct InfoExLevel1 {
        session_id: u32,
        session_state: i32,
        session_flags: i32,
        win_station_name: [u16; 33],
        user_name: [u16; 21],
        domain_name: [u16; 18],
        logon_time: i64,
        connect_time: i64,
        disconnect_time: i64,
        last_input_time: i64,
        current_time: i64,
        incoming_bytes: u32,
        outgoing_bytes: u32,
        incoming_frames: u32,
        outgoing_frames: u32,
        incoming_compressed_bytes: u32,
        outgoing_compressed_bytes: u32,
    }

    /// `WTSINFOEXW`.
    #[repr(C)]
    struct InfoEx {
        level: u32,
        data: InfoExLevel1,
    }

    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        fn WTSQuerySessionInformationW(
            server: *mut c_void,
            session_id: u32,
            info_class: i32,
            buffer: *mut *mut u16,
            bytes_returned: *mut u32,
        ) -> i32;
        fn WTSFreeMemory(memory: *mut c_void);
    }

    const WTS_CURRENT_SERVER_HANDLE: *mut c_void = std::ptr::null_mut();
    const WTS_CURRENT_SESSION: u32 = u32::MAX;
    const WTS_SESSION_INFO_EX: i32 = 25;
    const WTS_SESSIONSTATE_LOCK: i32 = 0;
    const WTS_SESSIONSTATE_UNLOCK: i32 = 1;

    pub fn is_locked() -> Option<bool> {
        let mut buffer: *mut u16 = std::ptr::null_mut();
        let mut bytes = 0u32;
        // SAFETY: on success the API hands out a buffer of `bytes` bytes
        // holding a WTSINFOEXW, read only after checking its size and
        // freed once with WTSFreeMemory.
        unsafe {
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTS_SESSION_INFO_EX,
                &mut buffer,
                &mut bytes,
            ) == 0
                || buffer.is_null()
            {
                return None;
            }
            let info = buffer.cast::<InfoEx>();
            let result = if (bytes as usize) < std::mem::size_of::<InfoEx>() || (*info).level != 1 {
                None
            } else {
                match (*info).data.session_flags {
                    WTS_SESSIONSTATE_LOCK => Some(true),
                    WTS_SESSIONSTATE_UNLOCK => Some(false),
                    _ => None,
                }
            };
            WTSFreeMemory(buffer.cast());
            result
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use zbus::proxy::{Builder, CacheProperties};
    use zbus::{Connection, Proxy};

    /// The logind session proxy, connected on first use. Gives up for good
    /// when there is no system bus or logind.
    #[derive(Default)]
    pub struct Logind {
        session: Option<Proxy<'static>>,
        unavailable: bool,
    }

    impl Logind {
        pub async fn is_locked(&mut self) -> Option<bool> {
            if self.unavailable {
                return None;
            }
            if self.session.is_none() {
                match Self::connect().await {
                    Ok(session) => self.session = Some(session),
                    Err(_) => {
                        self.unavailable = true;
                        return None;
                    }
                }
            }
            self.session
                .as_ref()?
                .get_property::<bool>("LockedHint")
                .await
                .ok()
        }

        async fn connect() -> zbus::Result<Proxy<'static>> {
            let connection = Connection::system().await?;
            // `auto`: the caller's session, or the user's display session
            // when Kubach runs outside one.
            let session: Proxy<'static> = Builder::new(&connection)
                .destination("org.freedesktop.login1")?
                .path("/org/freedesktop/login1/session/auto")?
                .interface("org.freedesktop.login1.Session")?
                .cache_properties(CacheProperties::No)
                .build()
                .await?;
            session.get_property::<bool>("LockedHint").await?;
            Ok(session)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_never_panics() {
        // Whatever the machine (CI without a GUI session included), the
        // answer is a plain value, asked as often as needed.
        let mut screen = ScreenLock::new();
        let first = futures::executor::block_on(screen.is_locked());
        let second = futures::executor::block_on(screen.is_locked());
        assert_eq!(first.is_some(), second.is_some());
    }
}
