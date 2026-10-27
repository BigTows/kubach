//! Relative times (`45s`, `3h5m`, `12d`) that stay current.
//!
//! Views keep absolute times (Unix seconds) and format them against the
//! current clock when they render. An [`AgeTicker`] re-renders one entity
//! just often enough for the youngest time it shows: every second while it
//! is under ten minutes old (its label shows seconds), every half minute
//! while it shows minutes, and so on. With no times shown it does not tick.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::{Context, Task};

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const YEAR: u64 = 365 * DAY;
/// Longest pause between re-renders, whatever the ages.
const MAX_DELAY: Duration = Duration::from_secs(HOUR);
/// How long after a whole second the tickers wake.
const TICK_SLACK: Duration = Duration::from_millis(5);

/// Seconds since the Unix epoch.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Formats a duration in seconds the way `kubectl` prints resource ages.
///
/// Duplicated in `kubach_core::plugin::format_age` (text output): `kubach-ui`
/// and `kubach-core` do not depend on each other. The rules must stay the
/// same; both crates check them against the same `AGE_CASES` table.
pub fn format_age(seconds: u64) -> String {
    let minutes = seconds / MINUTE;
    let hours = seconds / HOUR;
    let days = seconds / DAY;
    let years = seconds / YEAR;
    let pair = |major: u64, major_unit: &str, minor: u64, minor_unit: &str| {
        if minor == 0 {
            format!("{major}{major_unit}")
        } else {
            format!("{major}{major_unit}{minor}{minor_unit}")
        }
    };
    if seconds < 2 * MINUTE {
        format!("{seconds}s")
    } else if minutes < 10 {
        pair(minutes, "m", seconds % MINUTE, "s")
    } else if hours < 3 {
        format!("{minutes}m")
    } else if hours < 8 {
        pair(hours, "h", minutes % 60, "m")
    } else if hours < 48 {
        format!("{hours}h")
    } else if days < 8 {
        pair(days, "d", hours % 24, "h")
    } else if years < 2 {
        format!("{days}d")
    } else if years < 8 {
        pair(years, "y", days % 365, "d")
    } else {
        format!("{years}y")
    }
}

/// Age of `since` at `now` (both Unix seconds), formatted; `0s` for a time
/// in the future (clock skew).
pub fn format_since(since: i64, now: i64) -> String {
    format_age(now.saturating_sub(since).max(0) as u64)
}

/// The smallest unit [`format_age`] shows for an age of `seconds`.
fn resolution(seconds: u64) -> u64 {
    if seconds < 10 * MINUTE {
        1
    } else if seconds < 8 * HOUR {
        MINUTE
    } else if seconds < 8 * DAY {
        HOUR
    } else {
        DAY
    }
}

/// How long to wait before re-rendering ages when the youngest one shown is
/// `seconds` old: until its label changes, but no longer than half its unit,
/// so older ages (same unit, other phase) lag by at most that.
pub fn refresh_delay(seconds: i64) -> Duration {
    if seconds < 0 {
        // Shown as `0s` until the clock catches up.
        return Duration::from_secs(seconds.unsigned_abs().saturating_add(1)).min(MAX_DELAY);
    }
    let seconds = seconds as u64;
    let unit = resolution(seconds);
    let next_change = unit - seconds % unit;
    Duration::from_secs(next_change.min((unit / 2).max(1))).min(MAX_DELAY)
}

/// Re-renders an entity while it shows ages; see the module docs.
///
/// Owned by the entity that draws the ages. Call [`track`](Self::track)
/// whenever the shown times change: the timer restarts only when the
/// youngest one does, and nothing is recomputed on a tick but the render.
#[derive(Default)]
pub struct AgeTicker {
    youngest: Option<i64>,
    task: Option<Task<()>>,
}

impl AgeTicker {
    /// `youngest` is the most recent time shown (Unix seconds); `None`
    /// stops the ticks.
    pub fn track<T: 'static>(&mut self, youngest: Option<i64>, cx: &mut Context<T>) {
        if youngest == self.youngest && self.task.is_some() == youngest.is_some() {
            return;
        }
        self.youngest = youngest;
        self.task = youngest.map(|since| {
            cx.spawn(async move |this, cx| {
                loop {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let delay = refresh_delay((now.as_secs() as i64).saturating_sub(since));
                    // Wake just after the second turns, when labels change:
                    // every ticker of a window then wakes at once and the
                    // window draws one frame for all of them (#355).
                    let delay = (delay + TICK_SLACK)
                        .saturating_sub(Duration::from_nanos(now.subsec_nanos().into()));
                    cx.background_executor().timer(delay).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
        });
    }

    /// Stops the ticks, e.g. while the view is hidden.
    pub fn stop(&mut self) {
        self.youngest = None;
        self.task = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DAY, HOUR, MAX_DELAY, MINUTE, format_age, format_since, refresh_delay};

    /// Shared with the other copy of `format_age` (`kubach-core/src/plugin/mod.rs`
    /// and `kubach-ui/src/age.rs`): keep both tables equal.
    const AGE_CASES: [(u64, &str); 16] = [
        (0, "0s"),
        (45, "45s"),
        (119, "119s"),
        (125, "2m5s"),
        (180, "3m"),
        (192, "3m12s"),
        (25 * 60, "25m"),
        (3 * 3600 + 5 * 60, "3h5m"),
        (5 * 3600, "5h"),
        (20 * 3600 + 59 * 60, "20h"),
        (3 * 86400 + 4 * 3600, "3d4h"),
        (5 * 86400, "5d"),
        (12 * 86400, "12d"),
        (3 * 365 * 86400 + 10 * 86400, "3y10d"),
        (9 * 365 * 86400, "9y"),
        (2 * 365 * 86400, "2y"),
    ];

    #[test]
    fn formats_ages_like_kubectl() {
        for (seconds, expected) in AGE_CASES {
            assert_eq!(format_age(seconds), expected, "{seconds}s");
        }
    }

    #[test]
    fn formats_since_against_now() {
        assert_eq!(format_since(1_000, 1_045), "45s");
        assert_eq!(format_since(1_000, 1_000 + 3 * 3600), "3h");
        // A time in the future (clock skew) is not negative.
        assert_eq!(format_since(2_000, 1_000), "0s");
    }

    fn secs(delay: Duration) -> u64 {
        delay.as_secs()
    }

    #[test]
    fn ticks_every_second_while_seconds_are_shown() {
        assert_eq!(secs(refresh_delay(0)), 1);
        assert_eq!(secs(refresh_delay(59)), 1);
        // `2m5s` still shows seconds.
        assert_eq!(secs(refresh_delay(125)), 1);
        assert_eq!(secs(refresh_delay(10 * 60 - 1)), 1);
    }

    #[test]
    fn ticks_at_most_every_half_minute_for_minutes() {
        // `10m` until 11m: half a minute, then the rest of it.
        assert_eq!(secs(refresh_delay(10 * 60)), 30);
        assert_eq!(secs(refresh_delay(10 * 60 + 45)), 15);
        assert_eq!(secs(refresh_delay(5 * HOUR as i64 + 59)), 1);
    }

    #[test]
    fn ticks_rarely_for_hours_and_days() {
        assert_eq!(secs(refresh_delay(8 * HOUR as i64)), 30 * MINUTE);
        assert_eq!(
            secs(refresh_delay(8 * HOUR as i64 + 50 * MINUTE as i64)),
            10 * MINUTE
        );
        assert_eq!(refresh_delay(30 * DAY as i64), MAX_DELAY);
        assert_eq!(refresh_delay(i64::MAX), MAX_DELAY);
    }

    #[test]
    fn waits_out_clock_skew() {
        assert_eq!(secs(refresh_delay(-5)), 6);
        assert_eq!(refresh_delay(-10 * DAY as i64), MAX_DELAY);
        assert_eq!(refresh_delay(i64::MIN), MAX_DELAY);
    }
}
