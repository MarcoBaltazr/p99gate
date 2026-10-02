//! Aggregation of [`Sample`]s into histograms and counters.

use std::collections::BTreeMap;
use std::time::Duration;

use hdrhistogram::Histogram;

use crate::outcome::{ErrorKind, Outcome, Sample};

/// Largest trackable value: one hour, in microseconds. Larger values saturate.
const MAX_TRACKABLE_US: u64 = 3_600_000_000;
/// Significant decimal digits kept by the histograms (0.1% relative error).
const SIGNIFICANT_DIGITS: u8 = 3;

/// Accumulates samples from a run. Values are stored in microseconds.
#[derive(Debug, Clone)]
pub(crate) struct Recorder {
    latency: Histogram<u64>,
    send_lag: Histogram<u64>,
    statuses: BTreeMap<u16, u64>,
    errors: BTreeMap<ErrorKind, u64>,
    failures: u64,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the report module, added next")
)]
impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            latency: new_histogram(),
            send_lag: new_histogram(),
            statuses: BTreeMap::new(),
            errors: BTreeMap::new(),
            failures: 0,
        }
    }

    /// Records one sample. Every sample, failed or not, contributes to the
    /// latency histogram, so a slow failure can never make the tail look better.
    pub(crate) fn record(&mut self, sample: &Sample) {
        self.latency.saturating_record(micros(sample.latency));
        self.send_lag.saturating_record(micros(sample.send_lag));
        match sample.outcome {
            Outcome::Response { status } => *self.statuses.entry(status).or_default() += 1,
            Outcome::Error(kind) => *self.errors.entry(kind).or_default() += 1,
        }
        if sample.outcome.is_failure() {
            self.failures += 1;
        }
    }

    /// Latency from intended send time to completion, in microseconds.
    pub(crate) fn latency(&self) -> &Histogram<u64> {
        &self.latency
    }

    /// Delay between intended and actual send time, in microseconds.
    pub(crate) fn send_lag(&self) -> &Histogram<u64> {
        &self.send_lag
    }

    /// Count of responses per status code.
    pub(crate) fn statuses(&self) -> &BTreeMap<u16, u64> {
        &self.statuses
    }

    /// Count of transport errors per kind.
    pub(crate) fn errors(&self) -> &BTreeMap<ErrorKind, u64> {
        &self.errors
    }

    /// Number of completed requests.
    pub(crate) fn completed(&self) -> u64 {
        self.latency.len()
    }

    /// Number of completed requests that count as failures.
    pub(crate) fn failures(&self) -> u64 {
        self.failures
    }
}

fn new_histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(1, MAX_TRACKABLE_US, SIGNIFICANT_DIGITS)
        .expect("histogram bounds are valid constants")
}

/// Converts to whole microseconds, saturating, with a floor of 1 so that
/// every sample is recordable.
fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(latency_ms: u64, outcome: Outcome) -> Sample {
        Sample {
            latency: Duration::from_millis(latency_ms),
            send_lag: Duration::ZERO,
            outcome,
        }
    }

    const OK: Outcome = Outcome::Response { status: 200 };

    #[test]
    fn counts_statuses_errors_and_failures() {
        let mut r = Recorder::new();
        r.record(&sample(1, OK));
        r.record(&sample(1, OK));
        r.record(&sample(1, Outcome::Response { status: 503 }));
        r.record(&sample(1, Outcome::Error(ErrorKind::Timeout)));

        assert_eq!(r.completed(), 4);
        assert_eq!(r.failures(), 2);
        assert_eq!(r.statuses()[&200], 2);
        assert_eq!(r.statuses()[&503], 1);
        assert_eq!(r.errors()[&ErrorKind::Timeout], 1);
    }

    #[test]
    fn failed_requests_contribute_to_latency() {
        let mut r = Recorder::new();
        for _ in 0..99 {
            r.record(&sample(1, OK));
        }
        r.record(&sample(5_000, Outcome::Error(ErrorKind::Timeout)));
        let max_ms = r.latency().max() / 1_000;
        assert!((4_995..=5_005).contains(&max_ms), "max was {max_ms} ms");
    }

    #[test]
    fn percentiles_are_accurate_to_three_significant_digits() {
        let mut r = Recorder::new();
        for ms in 1..=1_000 {
            r.record(&sample(ms, OK));
        }
        let p99 = r.latency().value_at_quantile(0.99);
        assert!(p99.abs_diff(990_000) <= 990, "p99 was {p99} us");
    }

    #[test]
    fn extreme_values_saturate_instead_of_panicking() {
        let mut r = Recorder::new();
        r.record(&sample(0, OK));
        r.record(&Sample {
            latency: Duration::MAX,
            send_lag: Duration::MAX,
            outcome: OK,
        });
        assert_eq!(r.completed(), 2);
        assert_eq!(r.latency().min(), 1);
    }
}
