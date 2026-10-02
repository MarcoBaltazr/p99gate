//! The result of a run, in a stable, serialisable form.
//!
//! [`RunReport`] is the single source for every output: the terminal report,
//! `--output json`, threshold evaluation, and (in later versions) comparisons
//! between runs. Its JSON shape is documented in `docs/json-schema.md` and
//! versioned by [`SCHEMA_VERSION`].
//!
//! Units are part of the field names: `_us` is integer microseconds, `_s` is
//! fractional seconds, `_per_s` is a rate.

use std::collections::BTreeMap;
use std::time::Duration;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

use crate::engine::Measurements;
use crate::outcome::ErrorKind;

/// Version of the JSON schema produced by [`RunReport`].
///
/// Adding fields does not change it; renaming, removing or changing the
/// meaning of a field does.
pub const SCHEMA_VERSION: u32 = 1;

/// p99 send lag above which the run is flagged as not having sustained its
/// target rate. Tokio's timer has millisecond granularity and some platforms
/// schedule more coarsely, so small lags are normal and not reported.
pub const SATURATION_LAG: Duration = Duration::from_millis(50);

/// The full result of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    /// Always [`SCHEMA_VERSION`] for reports produced by this version.
    pub schema_version: u32,
    /// Version of p99gate that produced the report.
    pub p99gate_version: String,
    /// What was tested.
    pub target: Target,
    /// The requested load.
    pub config: ConfigReport,
    /// When and for how long the run happened.
    pub run: RunInfo,
    /// Request counts.
    pub requests: Requests,
    /// Achieved rates.
    pub throughput: Throughput,
    /// Fraction of completed requests that failed (0.0 to 1.0).
    pub error_rate: f64,
    /// Latency from intended send time to completion, for every completed request.
    pub latency: LatencyReport,
    /// How late requests were sent relative to the schedule.
    pub send_lag: Percentiles,
    /// Responses per status code.
    pub status_codes: BTreeMap<u16, u64>,
    /// Transport errors per kind.
    pub errors: BTreeMap<ErrorKind, u64>,
    /// Whether the generator failed to sustain the target rate. See [`SATURATION_LAG`].
    pub saturated: bool,
    /// Results of `--fail-if` thresholds, in the order given.
    pub thresholds: Vec<ThresholdResult>,
}

/// What was tested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Protocol, currently always `"http"`.
    pub protocol: String,
    /// Request method.
    pub method: String,
    /// Request URL.
    pub url: String,
}

/// The requested load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigReport {
    /// Target rate.
    pub rate_per_s: f64,
    /// Configured duration.
    pub duration_s: f64,
    /// Maximum requests in flight.
    pub concurrency: usize,
    /// Per-request timeout.
    pub timeout_s: f64,
}

/// When and for how long the run happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    /// RFC 3339 UTC timestamp of the start.
    pub started_at: String,
    /// From start until sending stopped.
    pub send_window_s: f64,
    /// From start until the last response arrived.
    pub elapsed_s: f64,
    /// Whether the run was stopped early (for example by Ctrl-C).
    pub interrupted: bool,
}

/// Request counts. `completed = succeeded + failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requests {
    /// Requests the schedule called for over the configured duration.
    pub scheduled: u64,
    /// Requests sent.
    pub sent: u64,
    /// Requests that were due but not sent because the generator fell behind.
    pub unsent: u64,
    /// Requests that finished, successfully or not.
    pub completed: u64,
    /// Completed requests with a status below 400.
    pub succeeded: u64,
    /// Completed requests with a transport error or a status of 400 or above.
    pub failed: u64,
}

/// Achieved rates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Throughput {
    /// The configured rate.
    pub target_per_s: f64,
    /// `sent / send_window_s`.
    pub sent_per_s: f64,
    /// `completed / elapsed_s`.
    pub completed_per_s: f64,
}

/// Latency percentiles plus a coarse histogram.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    /// Summary statistics.
    #[serde(flatten)]
    pub percentiles: Percentiles,
    /// Counts per latency range, on a 1-2-5 scale, from the lowest to the
    /// highest non-empty bucket.
    pub histogram: Vec<Bucket>,
}

/// Summary statistics of a distribution, in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs, reason = "field names are self-describing")]
pub struct Percentiles {
    pub min_us: u64,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p75_us: u64,
    pub p90_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}

/// Number of samples with a value in `(previous bucket's le_us, le_us]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bucket {
    /// Inclusive upper bound.
    pub le_us: u64,
    /// Samples in the bucket.
    pub count: u64,
}

/// The outcome of one `--fail-if` threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThresholdResult {
    /// The threshold as written, e.g. `"p99>300ms"`.
    pub expression: String,
    /// The measured value, in the metric's unit (microseconds, ratio or per second).
    pub observed: f64,
    /// Whether the threshold was violated, i.e. the run should fail.
    pub violated: bool,
}

impl RunReport {
    /// Builds the report for a finished run.
    #[must_use]
    pub fn new(measurements: &Measurements, target: Target) -> Self {
        let m = measurements;
        let r = &m.recorder;
        let completed = r.completed();
        let failed = r.failures();
        let send_lag = Percentiles::from_histogram(r.send_lag());
        let unsent = m.unsent();

        Self {
            schema_version: SCHEMA_VERSION,
            p99gate_version: env!("CARGO_PKG_VERSION").to_owned(),
            target,
            config: ConfigReport {
                rate_per_s: m.config.rate.get(),
                duration_s: m.config.duration.as_secs_f64(),
                concurrency: m.config.concurrency.get(),
                timeout_s: m.config.timeout.as_secs_f64(),
            },
            run: RunInfo {
                started_at: humantime::format_rfc3339_millis(m.started_at).to_string(),
                send_window_s: m.send_window.as_secs_f64(),
                elapsed_s: m.elapsed.as_secs_f64(),
                interrupted: m.interrupted,
            },
            requests: Requests {
                scheduled: m.scheduled,
                sent: m.sent,
                unsent,
                completed,
                succeeded: completed - failed,
                failed,
            },
            throughput: Throughput {
                target_per_s: m.config.rate.get(),
                sent_per_s: per_second(m.sent, m.send_window),
                completed_per_s: per_second(completed, m.elapsed),
            },
            error_rate: if completed == 0 {
                0.0
            } else {
                failed as f64 / completed as f64
            },
            latency: LatencyReport {
                percentiles: Percentiles::from_histogram(r.latency()),
                histogram: buckets(r.latency()),
            },
            send_lag,
            status_codes: r.statuses().clone(),
            errors: r.errors().clone(),
            saturated: unsent > 0 || u128::from(send_lag.p99_us) > SATURATION_LAG.as_micros(),
            thresholds: Vec::new(),
        }
    }

    /// Whether any threshold was violated.
    #[must_use]
    pub fn any_threshold_violated(&self) -> bool {
        self.thresholds.iter().any(|t| t.violated)
    }
}

impl Percentiles {
    fn from_histogram(h: &Histogram<u64>) -> Self {
        if h.is_empty() {
            return Self::default();
        }
        // `mean` is a float over integer microseconds; rounding to the
        // nearest microsecond loses nothing meaningful.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mean_us = h.mean().round() as u64;
        Self {
            min_us: h.min(),
            mean_us,
            p50_us: h.value_at_quantile(0.50),
            p75_us: h.value_at_quantile(0.75),
            p90_us: h.value_at_quantile(0.90),
            p95_us: h.value_at_quantile(0.95),
            p99_us: h.value_at_quantile(0.99),
            p999_us: h.value_at_quantile(0.999),
            max_us: h.max(),
        }
    }
}

fn per_second(count: u64, over: Duration) -> f64 {
    let secs = over.as_secs_f64();
    if secs > 0.0 { count as f64 / secs } else { 0.0 }
}

/// Upper bounds of the histogram buckets: 1, 2, 5, 10, 20, 50, ... µs.
fn bucket_bounds() -> impl Iterator<Item = u64> {
    std::iter::successors(Some(1_u64), |&b| b.checked_mul(10))
        .flat_map(|decade| [decade, 2 * decade, 5 * decade])
}

/// Groups samples into 1-2-5 buckets, trimmed to the non-empty range.
fn buckets(h: &Histogram<u64>) -> Vec<Bucket> {
    if h.is_empty() {
        return Vec::new();
    }
    let max = h.max();
    let mut out = Vec::new();
    let mut low = 0;
    for le_us in bucket_bounds() {
        // `count_between` is inclusive at both ends; start one above the
        // previous bound so no sample is counted twice.
        let count = h.count_between(low, le_us);
        out.push(Bucket { le_us, count });
        if le_us >= max {
            break;
        }
        low = le_us + 1;
    }
    let first = out.iter().position(|b| b.count > 0).unwrap_or(0);
    out.drain(..first);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn histogram(values_us: &[u64]) -> Histogram<u64> {
        let mut h = Histogram::new_with_bounds(1, 3_600_000_000, 3).unwrap();
        for &v in values_us {
            h.record(v).unwrap();
        }
        h
    }

    #[test]
    fn bucket_bounds_follow_a_1_2_5_scale() {
        let bounds: Vec<_> = bucket_bounds().take(7).collect();
        assert_eq!(bounds, [1, 2, 5, 10, 20, 50, 100]);
    }

    #[test]
    fn buckets_cover_every_sample_once_and_trim_empty_leading_buckets() {
        let h = histogram(&[150, 180, 200, 201, 4_000, 9_000]);
        let b = buckets(&h);
        assert_eq!(b.first().unwrap().le_us, 200);
        assert_eq!(b.last().unwrap().le_us, 10_000);
        assert_eq!(b.iter().map(|b| b.count).sum::<u64>(), 6);
        let counts: Vec<_> = b.iter().map(|b| (b.le_us, b.count)).collect();
        assert_eq!(
            counts,
            [
                (200, 3),
                (500, 1),
                (1_000, 0),
                (2_000, 0),
                (5_000, 1),
                (10_000, 1)
            ]
        );
    }

    #[test]
    fn empty_histograms_produce_zeroes() {
        let h = histogram(&[]);
        assert_eq!(Percentiles::from_histogram(&h), Percentiles::default());
        assert_eq!(buckets(&h), Vec::<Bucket>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn report_from_a_run_round_trips_through_json() {
        use std::num::NonZeroUsize;

        use crate::engine::{Engine, LoadConfig};
        use crate::executor::Executor;
        use crate::outcome::Outcome;
        use crate::schedule::Rate;

        struct Alternating(std::sync::atomic::AtomicU64);
        impl Executor for Alternating {
            async fn execute(&self) -> Outcome {
                tokio::time::sleep(Duration::from_millis(3)).await;
                let n = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Outcome::Response {
                    status: if n % 4 == 0 { 500 } else { 200 },
                }
            }
        }

        let config = LoadConfig {
            rate: Rate::per_second(40.0).unwrap(),
            duration: Duration::from_secs(1),
            concurrency: NonZeroUsize::new(8).unwrap(),
            timeout: Duration::from_secs(1),
        };
        let measurements = Engine::new(config, Alternating(0.into())).run().await;
        let report = RunReport::new(
            &measurements,
            Target {
                protocol: "http".into(),
                method: "GET".into(),
                url: "http://test/".into(),
            },
        );

        assert_eq!(report.requests.completed, 40);
        assert_eq!(report.requests.failed, 10);
        assert!((report.error_rate - 0.25).abs() < f64::EPSILON);
        assert_eq!(report.status_codes[&200], 30);
        assert!(!report.saturated);
        assert!((3_000..=3_010).contains(&report.latency.percentiles.p99_us));

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""status_codes":{"200":30,"500":10}"#));
        assert!(json.contains(r#""p99_us":"#), "percentiles are flattened");
        let parsed: RunReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, report);
    }

    #[test]
    fn per_second_handles_zero_duration() {
        assert!((per_second(10, Duration::from_secs(2)) - 5.0).abs() < f64::EPSILON);
        assert!(per_second(10, Duration::ZERO).abs() < f64::EPSILON);
    }
}
