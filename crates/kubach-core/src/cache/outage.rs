//! When a failing watch becomes an error the user sees (#316).
//!
//! Watch connections drop all the time on remote clusters (a load
//! balancer's idle timeout, a restarted API server) and come back on the
//! next try. Such a blip is retried quietly; the error is shown only when
//! the watch keeps failing: for [`ERROR_AFTER`], after
//! [`ERROR_AFTER_FAILURES`] failed attempts in a row, or at once when the
//! API server refuses access (HTTP 403).

use std::time::Duration;

use tokio::time::Instant;

use super::WatchError;

/// A watch failing for this long shows its error.
pub const ERROR_AFTER: Duration = Duration::from_secs(12);

/// So many failed attempts in a row show the error sooner.
pub const ERROR_AFTER_FAILURES: u32 = 5;

/// Delay before the first retry; doubled after each failure in a row.
pub const RETRY_MIN: Duration = Duration::from_secs(1);

/// The retry delay grows to at most this many times [`RETRY_MIN`].
const RETRY_MAX_FACTOR: u32 = 30;

/// A watch failing since some time, with its failures in a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outage {
    since: Instant,
    failures: u32,
    last: WatchError,
    /// The error is shown (the watch status is an error).
    shown: bool,
}

impl Outage {
    /// The first failure, at `now`.
    pub fn new(now: Instant, error: WatchError) -> Self {
        Self {
            since: now,
            failures: 1,
            last: error,
            shown: false,
        }
    }

    /// One more failure in a row.
    pub fn fail(&mut self, error: WatchError) {
        self.failures += 1;
        self.last = error;
    }

    /// The last error.
    pub fn error(&self) -> &WatchError {
        &self.last
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Whether the error should be shown at `now` (see the module docs).
    pub fn is_persistent(&self, now: Instant) -> bool {
        self.last.forbidden
            || self.failures >= ERROR_AFTER_FAILURES
            || now >= self.since + ERROR_AFTER
    }

    /// When the error is to be shown if the watch neither recovers nor fails
    /// again; `None` once it is shown.
    pub fn show_at(&self) -> Option<Instant> {
        (!self.shown).then_some(self.since + ERROR_AFTER)
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    pub fn mark_shown(&mut self) {
        self.shown = true;
    }

    /// Delay before the next attempt: `min`, doubled for each earlier
    /// failure in a row, up to 30 times `min`.
    pub fn retry_delay(&self, min: Duration) -> Duration {
        let factor = 1u32
            .checked_shl(self.failures.saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(RETRY_MAX_FACTOR);
        min * factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lost() -> WatchError {
        WatchError {
            forbidden: false,
            connection: true,
            message: "watch stream failed: error reading a body from connection".into(),
        }
    }

    #[test]
    fn a_short_outage_stays_quiet() {
        let start = Instant::now();
        let mut outage = Outage::new(start, lost());
        assert!(!outage.is_persistent(start));
        outage.fail(lost());
        assert!(!outage.is_persistent(start + Duration::from_secs(3)));
        assert_eq!(outage.show_at(), Some(start + ERROR_AFTER));
    }

    #[test]
    fn a_long_outage_is_shown() {
        let start = Instant::now();
        let outage = Outage::new(start, lost());
        assert!(!outage.is_persistent(start + ERROR_AFTER - Duration::from_millis(1)));
        assert!(outage.is_persistent(start + ERROR_AFTER));
    }

    #[test]
    fn failures_in_a_row_are_shown_sooner() {
        let start = Instant::now();
        let mut outage = Outage::new(start, lost());
        for _ in 1..ERROR_AFTER_FAILURES - 1 {
            outage.fail(lost());
        }
        assert!(!outage.is_persistent(start));
        outage.fail(lost());
        assert!(outage.is_persistent(start));
    }

    #[test]
    fn forbidden_is_shown_at_once() {
        let start = Instant::now();
        let outage = Outage::new(
            start,
            WatchError {
                forbidden: true,
                connection: false,
                message: "pods is forbidden".into(),
            },
        );
        assert!(outage.is_persistent(start));
    }

    #[test]
    fn shown_outages_need_no_timer() {
        let mut outage = Outage::new(Instant::now(), lost());
        outage.mark_shown();
        assert!(outage.is_shown());
        assert_eq!(outage.show_at(), None);
    }

    #[test]
    fn retries_back_off_up_to_a_limit() {
        let min = Duration::from_secs(1);
        let mut outage = Outage::new(Instant::now(), lost());
        let mut delays = vec![outage.retry_delay(min)];
        for _ in 0..40 {
            outage.fail(lost());
            delays.push(outage.retry_delay(min));
        }
        assert_eq!(
            delays[..6]
                .iter()
                .map(Duration::as_secs)
                .collect::<Vec<_>>(),
            [1, 2, 4, 8, 16, 30]
        );
        assert_eq!(*delays.last().unwrap(), Duration::from_secs(30));
    }
}
