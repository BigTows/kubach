//! How long the whole OS session has gone without keyboard or mouse input,
//! so the vault's idle lock follows the person at the computer and not the
//! Kubach window (ADR 0008). Polled by the app while the vault is unlocked:
//!
//! - macOS: `CGEventSourceSecondsSinceLastEventType` for any input event
//!   of the combined session state.
//! - Windows: `GetLastInputInfo` against `GetTickCount`.
//! - Linux: `GetIdletime` of Mutter's idle monitor on the session bus
//!   (GNOME); otherwise the `IdleHint` / `IdleSinceHintMonotonic`
//!   properties of the logind session, for desktops that set them.
//!
//! The answer is only a hint that there was input and how long ago: the
//! app keeps its own clocks for the idle time itself, so a counter that
//! stands still or restarts over a sleep does no harm.

use std::time::Duration;

/// Asks the OS for the time since the last input in the session.
#[derive(Default)]
pub struct SystemIdle {
    #[cfg(target_os = "linux")]
    session: linux::Session,
}

impl SystemIdle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Time since the last keyboard or mouse input anywhere in the OS
    /// session; `None` when the OS can't tell (no GUI session, a desktop
    /// that doesn't report it, another OS). Never blocks for long: only
    /// local calls; run it off the UI thread anyway.
    pub async fn idle(&mut self) -> Option<Duration> {
        #[cfg(target_os = "macos")]
        {
            macos::idle()
        }
        #[cfg(target_os = "windows")]
        {
            windows::idle()
        }
        #[cfg(target_os = "linux")]
        {
            self.session.idle().await
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::Duration;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state_id: i32, event_type: u32) -> f64;
    }

    /// `kCGEventSourceStateCombinedSessionState`.
    const COMBINED_SESSION_STATE: i32 = 0;
    /// `kCGAnyInputEventType`.
    const ANY_INPUT_EVENT: u32 = !0;

    pub fn idle() -> Option<Duration> {
        // SAFETY: a plain call with two constants, no pointers.
        let seconds = unsafe {
            CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT)
        };
        // Outside a GUI session the counter is not a time.
        Duration::try_from_secs_f64(seconds).ok()
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::time::Duration;

    /// `LASTINPUTINFO`.
    #[repr(C)]
    struct LastInputInfo {
        size: u32,
        /// Tick count of the last input.
        time: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTickCount() -> u32;
    }

    pub fn idle() -> Option<Duration> {
        let mut info = LastInputInfo {
            size: std::mem::size_of::<LastInputInfo>() as u32,
            time: 0,
        };
        // SAFETY: `info` is a LASTINPUTINFO with its size filled in, alive
        // for the call.
        let now = unsafe {
            if GetLastInputInfo(&mut info) == 0 {
                return None;
            }
            GetTickCount()
        };
        // Both are the 32-bit tick count, which wraps every 49.7 days.
        Some(Duration::from_millis(u64::from(
            now.wrapping_sub(info.time),
        )))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use zbus::proxy::{Builder, CacheProperties};
    use zbus::{Connection, Proxy};

    /// Where the idle time comes from, found on first use.
    #[derive(Default)]
    pub struct Session {
        source: Option<Source>,
    }

    enum Source {
        /// `org.gnome.Mutter.IdleMonitor`: the exact time since the last
        /// input.
        Mutter(Proxy<'static>),
        /// The logind session, whose idle hint the desktop sets.
        Logind(Proxy<'static>),
        /// Neither answers; given up for good.
        Unavailable,
    }

    impl Session {
        pub async fn idle(&mut self) -> Option<Duration> {
            if self.source.is_none() {
                self.source = Some(Source::find().await);
            }
            match self.source.as_ref()? {
                Source::Mutter(monitor) => mutter_idle(monitor).await,
                Source::Logind(session) => logind_idle(session).await,
                Source::Unavailable => None,
            }
        }
    }

    impl Source {
        async fn find() -> Self {
            if let Ok(monitor) = mutter().await {
                return Self::Mutter(monitor);
            }
            match logind().await {
                Ok(session) => Self::Logind(session),
                Err(_) => Self::Unavailable,
            }
        }
    }

    async fn mutter() -> zbus::Result<Proxy<'static>> {
        let connection = Connection::session().await?;
        let monitor: Proxy<'static> = Builder::new(&connection)
            .destination("org.gnome.Mutter.IdleMonitor")?
            .path("/org/gnome/Mutter/IdleMonitor/Core")?
            .interface("org.gnome.Mutter.IdleMonitor")?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        monitor.call::<_, _, u64>("GetIdletime", &()).await?;
        Ok(monitor)
    }

    async fn mutter_idle(monitor: &Proxy<'static>) -> Option<Duration> {
        let millis = monitor.call::<_, _, u64>("GetIdletime", &()).await.ok()?;
        Some(Duration::from_millis(millis))
    }

    async fn logind() -> zbus::Result<Proxy<'static>> {
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
        session.get_property::<bool>("IdleHint").await?;
        Ok(session)
    }

    async fn logind_idle(session: &Proxy<'static>) -> Option<Duration> {
        let idle = session.get_property::<bool>("IdleHint").await.ok()?;
        let since = session
            .get_property::<u64>("IdleSinceHintMonotonic")
            .await
            .ok()?;
        hint_idle(idle, since, monotonic_micros()?)
    }

    /// Reads the logind idle hint. `since` is when the hint last changed
    /// (`CLOCK_MONOTONIC`, microseconds), zero while it never has: then
    /// the desktop doesn't report idleness at all, and "not idle" would
    /// keep the vault open forever. A desktop that does report it is
    /// trusted: not idle is input just now, idle is idle since the hint
    /// was set (the desktop's own delay before it is not counted).
    pub(super) fn hint_idle(idle: bool, since: u64, now: u64) -> Option<Duration> {
        if since == 0 {
            return None;
        }
        Some(if idle {
            Duration::from_micros(now.saturating_sub(since))
        } else {
            Duration::ZERO
        })
    }

    fn monotonic_micros() -> Option<u64> {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a timespec alive for the call.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
            return None;
        }
        let seconds = u64::try_from(time.tv_sec).ok()?;
        let nanos = u64::try_from(time.tv_nsec).ok()?;
        Some(
            seconds
                .saturating_mul(1_000_000)
                .saturating_add(nanos / 1_000),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_never_panics() {
        // Whatever the machine (CI without a GUI session included), the
        // answer is a plain value, asked as often as needed.
        let mut system = SystemIdle::new();
        let first = futures::executor::block_on(system.idle());
        let second = futures::executor::block_on(system.idle());
        assert_eq!(first.is_some(), second.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_logind_hint_counts_only_once_the_desktop_reports_it() {
        use super::linux::hint_idle;

        // Never changed: the desktop doesn't set the hint.
        assert_eq!(hint_idle(false, 0, 5_000_000), None);
        // Reported and not idle: input just now.
        assert_eq!(hint_idle(false, 1_000_000, 5_000_000), Some(Duration::ZERO));
        // Idle since the hint was set.
        assert_eq!(
            hint_idle(true, 1_000_000, 5_000_000),
            Some(Duration::from_secs(4))
        );
        // A hint from the future is no idle time.
        assert_eq!(hint_idle(true, 9_000_000, 5_000_000), Some(Duration::ZERO));
    }
}
