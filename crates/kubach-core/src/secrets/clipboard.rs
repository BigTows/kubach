//! When a copied Secret value leaves the clipboard.
//!
//! A copied value is cleared [`CLIPBOARD_TTL`] after Copy (independently of
//! how long it stays shown), when the vault locks and when the app quits —
//! each time only if the clipboard still holds that value: the user may have
//! copied something else since, and that is left alone.
//!
//! [`ClipboardGuard`] decides; the UI reads and writes the clipboard. The
//! guard keeps only a keyed hash of the copied text (random key per guard),
//! never the text itself.

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, Instant};

/// How long a copied value may stay in the clipboard.
pub const CLIPBOARD_TTL: Duration = Duration::from_secs(30);

/// Why the clipboard is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearTrigger {
    /// The timer started by Copy fired.
    Expired,
    /// The vault was locked.
    VaultLocked,
    /// The app is quitting.
    Quit,
}

#[derive(Clone, Copy)]
struct Copied {
    digest: u64,
    deadline: Instant,
}

/// Remembers the last copied Secret value (as a hash) and its deadline.
pub struct ClipboardGuard {
    hasher: RandomState,
    copied: Option<Copied>,
}

impl Default for ClipboardGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardGuard {
    pub fn new() -> Self {
        Self {
            hasher: RandomState::new(),
            copied: None,
        }
    }

    fn digest(&self, text: &str) -> u64 {
        self.hasher.hash_one(text)
    }

    /// `text` was put into the clipboard at `now`; returns when it expires.
    /// A newer copy replaces the older one.
    pub fn copied(&mut self, text: &str, now: Instant) -> Instant {
        let deadline = now + CLIPBOARD_TTL;
        self.copied = Some(Copied {
            digest: self.digest(text),
            deadline,
        });
        deadline
    }

    /// Whether a copied value may still be in the clipboard.
    pub fn is_pending(&self) -> bool {
        self.copied.is_some()
    }

    /// When the pending value expires.
    pub fn deadline(&self) -> Option<Instant> {
        self.copied.map(|copied| copied.deadline)
    }

    /// Whether to empty the clipboard on `trigger`, given its `current`
    /// text. Once decided (cleared, or the clipboard holds something else)
    /// the copy is forgotten; an early [`ClearTrigger::Expired`] (a timer of
    /// a replaced copy) keeps it.
    pub fn should_clear(
        &mut self,
        trigger: ClearTrigger,
        now: Instant,
        current: Option<&str>,
    ) -> bool {
        let Some(copied) = self.copied else {
            return false;
        };
        if trigger == ClearTrigger::Expired && now < copied.deadline {
            return false;
        }
        self.copied = None;
        current.is_some_and(|text| self.digest(text) == copied.digest)
    }
}

impl fmt::Debug for ClipboardGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardGuard")
            .field("pending", &self.is_pending())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATER: Duration = Duration::from_secs(1);

    #[test]
    fn nothing_copied_nothing_cleared() {
        let mut guard = ClipboardGuard::new();
        let now = Instant::now();
        for trigger in [
            ClearTrigger::Expired,
            ClearTrigger::VaultLocked,
            ClearTrigger::Quit,
        ] {
            assert!(!guard.should_clear(trigger, now, Some("hunter2")));
        }
    }

    #[test]
    fn expires_after_ttl_not_before() {
        let mut guard = ClipboardGuard::new();
        let start = Instant::now();
        let deadline = guard.copied("hunter2", start);
        assert_eq!(deadline, start + CLIPBOARD_TTL);
        assert!(!guard.should_clear(ClearTrigger::Expired, start + LATER, Some("hunter2")));
        assert!(guard.is_pending());
        assert!(guard.should_clear(ClearTrigger::Expired, deadline, Some("hunter2")));
        assert!(!guard.is_pending());
        // Cleared once; a later check does nothing.
        assert!(!guard.should_clear(ClearTrigger::Quit, deadline, Some("hunter2")));
    }

    #[test]
    fn lock_and_quit_clear_before_the_deadline() {
        let now = Instant::now();
        for trigger in [ClearTrigger::VaultLocked, ClearTrigger::Quit] {
            let mut guard = ClipboardGuard::new();
            guard.copied("hunter2", now);
            assert!(guard.should_clear(trigger, now + LATER, Some("hunter2")));
            assert!(!guard.is_pending());
        }
    }

    #[test]
    fn something_else_in_the_clipboard_is_kept() {
        let now = Instant::now();
        let mut guard = ClipboardGuard::new();
        guard.copied("hunter2", now);
        assert!(!guard.should_clear(ClearTrigger::Quit, now, Some("user's own text")));
        // The copy is forgotten: copying the same text by hand later is
        // not cleared.
        assert!(!guard.is_pending());
        assert!(!guard.should_clear(ClearTrigger::Quit, now, Some("hunter2")));

        let mut guard = ClipboardGuard::new();
        guard.copied("hunter2", now);
        assert!(!guard.should_clear(ClearTrigger::VaultLocked, now, None));
    }

    #[test]
    fn newer_copy_restarts_the_timer() {
        let start = Instant::now();
        let mut guard = ClipboardGuard::new();
        let first = guard.copied("one", start);
        let second = guard.copied("two", start + LATER);
        assert_eq!(guard.deadline(), Some(second));
        // The first copy's timer fires: too early for the second one.
        assert!(!guard.should_clear(ClearTrigger::Expired, first, Some("two")));
        assert!(guard.should_clear(ClearTrigger::Expired, second, Some("two")));
    }

    #[test]
    fn keeps_no_text() {
        let mut guard = ClipboardGuard::new();
        guard.copied("hunter2", Instant::now());
        let printed = format!("{guard:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("pending: true"));
    }
}
