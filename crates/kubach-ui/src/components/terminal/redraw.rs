//! Coalesces the redraws that program output asks for (#166).
//!
//! Output comes in chunks, far more often than a frame while a program
//! prints a lot, and every redraw of the terminal on screen redraws the
//! views it is in. So output redraws at most once per [`REDRAW_INTERVAL`]:
//! the first chunk after a quiet interval redraws at once (the echo of a
//! keystroke does not wait for a timer), and whatever arrives during the
//! interval is shown together when it ends. The emulator itself takes every
//! chunk as it comes; only the drawing waits.

use std::time::Duration;

/// The shortest pause between two redraws for output: one frame at 60 Hz.
pub const REDRAW_INTERVAL: Duration = Duration::from_millis(16);

/// Whether output may redraw now. The owner runs a timer of
/// [`REDRAW_INTERVAL`] from the moment [`output`](Self::output) says to
/// redraw until [`interval_elapsed`](Self::interval_elapsed) says to stop.
#[derive(Debug, Default)]
pub struct RedrawThrottle {
    /// An interval is running: a redraw happened less than an interval ago.
    waiting: bool,
    /// Output arrived during the running interval and is not drawn yet.
    pending: bool,
}

impl RedrawThrottle {
    /// Output arrived. True: redraw now and start the timer. False: the
    /// running timer will redraw.
    pub fn output(&mut self) -> bool {
        if self.waiting {
            self.pending = true;
            false
        } else {
            self.waiting = true;
            true
        }
    }

    /// The timer fired. True: output arrived meanwhile, redraw and wait
    /// another interval. False: nothing to draw, the timer stops and the
    /// next output redraws at once.
    pub fn interval_elapsed(&mut self) -> bool {
        if std::mem::take(&mut self.pending) {
            true
        } else {
            self.waiting = false;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RedrawThrottle;

    #[test]
    fn output_after_a_quiet_interval_redraws_at_once() {
        let mut throttle = RedrawThrottle::default();
        assert!(throttle.output());
        // Nothing more came: the timer stops.
        assert!(!throttle.interval_elapsed());
        // The echo of the next keystroke is not delayed.
        assert!(throttle.output());
    }

    #[test]
    fn a_burst_redraws_once_per_interval() {
        let mut throttle = RedrawThrottle::default();
        let mut redraws = 0;
        // Three intervals with a hundred chunks each.
        for _ in 0..3 {
            for _ in 0..100 {
                redraws += usize::from(throttle.output());
            }
            redraws += usize::from(throttle.interval_elapsed());
        }
        // At once for the first chunk, then once as each interval ends.
        assert_eq!(redraws, 4);
        // The output stopped: the last interval ends with nothing to draw.
        assert!(!throttle.interval_elapsed());
        assert!(throttle.output());
    }

    #[test]
    fn output_during_an_interval_is_drawn_when_it_ends() {
        let mut throttle = RedrawThrottle::default();
        assert!(throttle.output());
        assert!(!throttle.output());
        assert!(throttle.interval_elapsed());
        // That redraw started another interval.
        assert!(!throttle.output());
        assert!(throttle.interval_elapsed());
        assert!(!throttle.interval_elapsed());
    }
}
