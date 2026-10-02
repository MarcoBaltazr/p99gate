//! `--fail-if` thresholds: conditions on a [`RunReport`] that fail the run.
//!
//! A threshold is `<metric> <operator> <value>`, whitespace optional:
//!
//! | Metric | Value | Example |
//! |---|---|---|
//! | `p50` `p75` `p90` `p95` `p99` `p999` `max` `mean` | duration with unit `us`, `ms` or `s` | `p99>300ms` |
//! | `error_rate` | percentage or ratio | `error_rate>1%`, `error_rate>0.01` |
//! | `rps` (completed requests per second) | number | `rps<450` |
//!
//! Operators are `>`, `>=`, `<` and `<=`. The expression states the failure
//! condition: `p99>300ms` fails the run when p99 is above 300 ms.

use std::fmt;
use std::str::FromStr;

use crate::report::{RunReport, ThresholdResult, Unit};

/// A parsed `--fail-if` condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Threshold {
    expression: String,
    metric: Metric,
    op: Op,
    /// In the metric's unit: microseconds, ratio, or per second.
    value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Metric {
    Latency(Stat),
    ErrorRate,
    Rps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stat {
    P50,
    P75,
    P90,
    P95,
    P99,
    P999,
    Max,
    Mean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Gt,
    Ge,
    Lt,
    Le,
}

/// Why a threshold expression could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid threshold `{expression}`: {reason}")]
pub struct ParseThresholdError {
    expression: String,
    reason: String,
}

impl Threshold {
    /// Checks the threshold against a report.
    #[must_use]
    pub fn evaluate(&self, report: &RunReport) -> ThresholdResult {
        let observed = self.observe(report);
        let violated = match self.op {
            Op::Gt => observed > self.value,
            Op::Ge => observed >= self.value,
            Op::Lt => observed < self.value,
            Op::Le => observed <= self.value,
        };
        ThresholdResult {
            expression: self.expression.clone(),
            observed,
            unit: match self.metric {
                Metric::Latency(_) => Unit::Micros,
                Metric::ErrorRate => Unit::Ratio,
                Metric::Rps => Unit::PerSecond,
            },
            violated,
        }
    }

    fn observe(&self, report: &RunReport) -> f64 {
        let p = &report.latency.percentiles;
        let micros = match self.metric {
            Metric::ErrorRate => return report.error_rate,
            Metric::Rps => return report.throughput.completed_per_s,
            Metric::Latency(Stat::P50) => p.p50_us,
            Metric::Latency(Stat::P75) => p.p75_us,
            Metric::Latency(Stat::P90) => p.p90_us,
            Metric::Latency(Stat::P95) => p.p95_us,
            Metric::Latency(Stat::P99) => p.p99_us,
            Metric::Latency(Stat::P999) => p.p999_us,
            Metric::Latency(Stat::Max) => p.max_us,
            Metric::Latency(Stat::Mean) => p.mean_us,
        };
        micros as f64
    }
}

impl fmt::Display for Threshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.expression)
    }
}

impl FromStr for Threshold {
    type Err = ParseThresholdError;

    fn from_str(expression: &str) -> Result<Self, Self::Err> {
        let error = |reason: &str| ParseThresholdError {
            expression: expression.to_owned(),
            reason: reason.to_owned(),
        };

        let compact: String = expression.chars().filter(|c| !c.is_whitespace()).collect();
        let op_start = compact
            .find(['<', '>'])
            .ok_or_else(|| error("expected an operator: >, >=, < or <="))?;
        let (metric, rest) = compact.split_at(op_start);
        let (op, value) = if let Some(v) = rest.strip_prefix(">=") {
            (Op::Ge, v)
        } else if let Some(v) = rest.strip_prefix("<=") {
            (Op::Le, v)
        } else if let Some(v) = rest.strip_prefix('>') {
            (Op::Gt, v)
        } else {
            (Op::Lt, &rest[1..])
        };

        let metric = parse_metric(metric).ok_or_else(|| {
            error("unknown metric; expected p50, p75, p90, p95, p99, p999, max, mean, error_rate or rps")
        })?;
        let value = match metric {
            Metric::Latency(_) => parse_micros(value).ok_or_else(|| {
                error("expected a duration with a unit, such as 300ms, 1.5s or 800us")
            })?,
            Metric::ErrorRate => parse_ratio(value)
                .ok_or_else(|| error("expected a percentage such as 1% or a ratio such as 0.01"))?,
            Metric::Rps => parse_number(value)
                .ok_or_else(|| error("expected a non-negative number of requests per second"))?,
        };

        Ok(Self {
            expression: compact,
            metric,
            op,
            value,
        })
    }
}

fn parse_metric(s: &str) -> Option<Metric> {
    Some(match s.to_ascii_lowercase().as_str() {
        "p50" | "median" => Metric::Latency(Stat::P50),
        "p75" => Metric::Latency(Stat::P75),
        "p90" => Metric::Latency(Stat::P90),
        "p95" => Metric::Latency(Stat::P95),
        "p99" => Metric::Latency(Stat::P99),
        "p999" | "p99.9" => Metric::Latency(Stat::P999),
        "max" => Metric::Latency(Stat::Max),
        "mean" => Metric::Latency(Stat::Mean),
        "error_rate" => Metric::ErrorRate,
        "rps" => Metric::Rps,
        _ => return None,
    })
}

fn parse_number(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

fn parse_micros(s: &str) -> Option<f64> {
    // Longer suffixes first: `us` and `ms` also end in `s`.
    const UNITS: [(&str, f64); 4] = [
        ("us", 1.0),
        ("µs", 1.0),
        ("ms", 1_000.0),
        ("s", 1_000_000.0),
    ];
    let (number, scale) = UNITS
        .iter()
        .find_map(|&(unit, scale)| s.strip_suffix(unit).map(|n| (n, scale)))?;
    parse_number(number).map(|v| v * scale)
}

fn parse_ratio(s: &str) -> Option<f64> {
    match s.strip_suffix('%') {
        Some(percent) => parse_number(percent).map(|v| v / 100.0),
        None => parse_number(s),
    }
    .filter(|v| *v <= 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Threshold {
        s.parse().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_latency_thresholds_in_every_unit() {
        assert_eq!(parse("p99>300ms").value, 300_000.0);
        assert_eq!(parse("p99 > 1.5s").value, 1_500_000.0);
        assert_eq!(parse("max>=800us").value, 800.0);
        assert_eq!(parse("p50<=250µs").value, 250.0);
        assert_eq!(parse("P99.9>2s").metric, Metric::Latency(Stat::P999));
    }

    #[test]
    fn parses_operators() {
        assert_eq!(parse("p99>1ms").op, Op::Gt);
        assert_eq!(parse("p99>=1ms").op, Op::Ge);
        assert_eq!(parse("rps<10").op, Op::Lt);
        assert_eq!(parse("rps<=10").op, Op::Le);
    }

    #[test]
    fn parses_error_rate_as_percentage_or_ratio() {
        assert!((parse("error_rate>1%").value - 0.01).abs() < 1e-12);
        assert!((parse("error_rate>0.01").value - 0.01).abs() < 1e-12);
    }

    #[test]
    fn normalises_the_expression() {
        assert_eq!(parse(" p99 >  300ms ").to_string(), "p99>300ms");
    }

    #[tokio::test(start_paused = true)]
    async fn evaluates_against_a_report() {
        use std::num::NonZeroUsize;
        use std::time::Duration;

        use crate::engine::{Engine, LoadConfig};
        use crate::executor::Executor;
        use crate::outcome::Outcome;
        use crate::report::Target;
        use crate::schedule::Rate;

        struct TenMillis;
        impl Executor for TenMillis {
            async fn execute(&self) -> Outcome {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Outcome::Response { status: 200 }
            }
        }

        let config = LoadConfig {
            rate: Rate::per_second(100.0).unwrap(),
            duration: Duration::from_secs(1),
            concurrency: NonZeroUsize::new(10).unwrap(),
            timeout: Duration::from_secs(1),
        };
        let measurements = Engine::new(config, TenMillis).run().await;
        let target = Target {
            protocol: "http".into(),
            method: "GET".into(),
            url: "http://test/".into(),
        };
        let report = RunReport::new(&measurements, target);

        let check = |s: &str| parse(s).evaluate(&report);
        assert!(check("p99>5ms").violated);
        assert!(!check("p99>20ms").violated);
        assert!((check("p99>20ms").observed - 10_000.0).abs() <= 10.0);
        assert_eq!(check("p99>20ms").unit, Unit::Micros);
        assert_eq!(check("error_rate>0%").unit, Unit::Ratio);
        assert!(!check("error_rate>0%").violated);
        assert!(check("error_rate<=0%").violated);
        assert!(!check("rps<90").violated);
        assert!(check("rps<120").violated);
    }

    #[test]
    fn rejects_malformed_thresholds_with_a_reason() {
        for (input, reason) in [
            ("p99 300ms", "operator"),
            ("p42>300ms", "unknown metric"),
            ("p99>300", "duration with a unit"),
            ("p99>-1ms", "duration with a unit"),
            ("p99>", "duration with a unit"),
            ("error_rate>150%", "percentage"),
            ("rps<lots", "number"),
        ] {
            let err = input.parse::<Threshold>().unwrap_err().to_string();
            assert!(err.contains(reason), "{input:?} gave {err:?}");
            assert!(err.contains(input), "{err:?} should quote the input");
        }
    }
}
