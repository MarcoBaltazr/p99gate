//! Open-loop request schedule.
//!
//! A [`Schedule`] answers one question: *when should request `i` be sent?*
//! Send times are absolute offsets from the start of the run, computed from
//! the target rate alone. They never depend on how quickly earlier responses
//! arrived, which is what makes the load model open-loop.

use std::fmt;
use std::time::Duration;

/// A constant request rate in requests per second.
///
/// Guaranteed finite and strictly positive.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Rate(f64);

impl Rate {
    /// Creates a rate, returning `None` unless `per_second` is finite and positive.
    #[must_use]
    pub fn per_second(per_second: f64) -> Option<Self> {
        (per_second.is_finite() && per_second > 0.0).then_some(Self(per_second))
    }

    /// The rate in requests per second.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// The time between two consecutive intended sends.
    #[must_use]
    pub fn period(self) -> Duration {
        Duration::from_secs_f64(1.0 / self.0)
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/s", self.0)
    }
}

/// The intended send times for a run at a constant [`Rate`] over a fixed duration.
///
/// Offsets are computed independently for each index (`i / rate`) rather than
/// by repeatedly adding a period, so rounding error never accumulates.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    rate: Rate,
    duration: Duration,
    len: u64,
}

impl Schedule {
    /// Creates the schedule for sending at `rate` for `duration`.
    ///
    /// The first request is due at offset zero and the last one strictly
    /// before `duration`.
    #[must_use]
    pub fn new(rate: Rate, duration: Duration) -> Self {
        // Number of offsets i / rate that lie in [0, duration).
        // Saturating float-to-int casts are fine: no real run comes close to u64::MAX.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let estimate = (duration.as_secs_f64() * rate.get()).ceil() as u64;
        let mut schedule = Self {
            rate,
            duration,
            len: estimate,
        };
        // The float product can round up past an exact boundary
        // (0.3 s * 10/s = 3.0000000000000004), so trim against the real offsets.
        while schedule.len > 0 && schedule.offset(schedule.len - 1) >= duration {
            schedule.len -= 1;
        }
        schedule
    }

    /// The target rate.
    #[must_use]
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// The length of the sending window.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// The total number of requests in the schedule.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the schedule contains no requests (only for a zero duration).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The offset from the start of the run at which request `index` is due.
    #[must_use]
    pub fn offset(&self, index: u64) -> Duration {
        Duration::from_secs_f64(index as f64 / self.rate.get())
    }

    /// How many requests are due at or before `elapsed`.
    #[must_use]
    pub fn due_by(&self, elapsed: Duration) -> u64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let due = (elapsed.as_secs_f64() * self.rate.get()).floor() as u64 + 1;
        due.min(self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(rps: f64, secs: f64) -> Schedule {
        Schedule::new(
            Rate::per_second(rps).unwrap(),
            Duration::from_secs_f64(secs),
        )
    }

    #[test]
    fn rate_rejects_non_positive_and_non_finite() {
        assert!(Rate::per_second(0.0).is_none());
        assert!(Rate::per_second(-1.0).is_none());
        assert!(Rate::per_second(f64::NAN).is_none());
        assert!(Rate::per_second(f64::INFINITY).is_none());
        assert_eq!(Rate::per_second(0.5).unwrap().get(), 0.5);
    }

    #[test]
    fn length_covers_the_half_open_window() {
        assert_eq!(schedule(100.0, 1.0).len(), 100);
        assert_eq!(schedule(100.0, 0.995).len(), 100);
        assert_eq!(schedule(3.0, 1.0).len(), 3);
        assert_eq!(schedule(0.5, 3.0).len(), 2); // t = 0s, 2s
        assert!(schedule(10.0, 0.0).is_empty());
        // 0.3 * 10 rounds to 3.0000000000000004 in f64; the window still holds exactly 3.
        assert_eq!(schedule(10.0, 0.3).len(), 3);
    }

    #[test]
    fn every_offset_lies_inside_the_window() {
        let s = schedule(7.0, 2.3);
        let last = s.offset(s.len() - 1);
        assert!(last < s.duration());
        assert!(s.offset(s.len()) >= s.duration());
    }

    #[test]
    fn offsets_are_evenly_spaced_without_drift() {
        let s = schedule(1000.0, 3600.0);
        assert_eq!(s.offset(0), Duration::ZERO);
        assert_eq!(s.offset(1), Duration::from_millis(1));
        // After an hour of millisecond spacing there is still no accumulated error.
        assert_eq!(s.offset(3_599_999), Duration::from_millis(3_599_999));
    }

    #[test]
    fn due_by_counts_requests_whose_time_has_come() {
        let s = schedule(10.0, 1.0);
        assert_eq!(s.due_by(Duration::ZERO), 1);
        assert_eq!(s.due_by(Duration::from_millis(99)), 1);
        assert_eq!(s.due_by(Duration::from_millis(100)), 2);
        assert_eq!(s.due_by(Duration::from_secs(5)), 10);
    }
}
