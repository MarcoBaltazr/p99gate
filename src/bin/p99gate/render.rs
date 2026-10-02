//! The human-readable report.
//!
//! Rendering produces a `String` with ANSI styling; the caller writes it
//! through `anstream`, which strips the styling when stdout is not a
//! terminal or `NO_COLOR` is set.

use std::fmt::Write as _;

use anstyle::{AnsiColor, Style};
use p99gate::report::{Bucket, RunReport, Unit};

const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();
const GOOD: Style = AnsiColor::Green.on_default();
const WARN: Style = AnsiColor::Yellow.on_default().bold();
const BAD: Style = AnsiColor::Red.on_default().bold();
const BAR: Style = AnsiColor::Cyan.on_default();

/// Width of the longest histogram bar, in terminal cells.
const BAR_WIDTH: usize = 32;

/// Renders the full report.
pub fn report(r: &RunReport) -> String {
    let mut out = String::new();
    // Writing to a String cannot fail.
    let _ = write_report(&mut out, r);
    out
}

fn write_report(out: &mut String, r: &RunReport) -> std::fmt::Result {
    write_latency(out, r)?;
    write_summary(out, r)?;
    write_warnings(out, r)?;
    write_thresholds(out, r)
}

fn write_latency(out: &mut String, r: &RunReport) -> std::fmt::Result {
    writeln!(
        out,
        "\n{BOLD}{} {}{BOLD:#}  {DIM}{} req/s for {}, concurrency {}{DIM:#}\n",
        r.target.method,
        r.target.url,
        number(r.config.rate_per_s),
        seconds(r.config.duration_s),
        r.config.concurrency,
    )?;

    writeln!(
        out,
        "{BOLD}Latency{BOLD:#}  {DIM}measured from intended send time{DIM:#}"
    )?;
    let p = &r.latency.percentiles;
    for (label, us) in [
        ("p50", p.p50_us),
        ("p90", p.p90_us),
        ("p95", p.p95_us),
        ("p99", p.p99_us),
        ("max", p.max_us),
    ] {
        writeln!(out, "  {label:<5}{:>10}", micros(us))?;
    }
    writeln!(out)?;
    write_histogram(out, &r.latency.histogram)?;
    writeln!(out)
}

fn write_summary(out: &mut String, r: &RunReport) -> std::fmt::Result {
    let t = &r.throughput;
    writeln!(
        out,
        "{BOLD}Throughput{BOLD:#}  {} req/s  {DIM}(target {}, sent {}){DIM:#}",
        number(t.completed_per_s),
        number(t.target_per_s),
        number(t.sent_per_s),
    )?;

    let q = &r.requests;
    let failed_style = if q.failed > 0 { BAD } else { GOOD };
    writeln!(
        out,
        "{BOLD}Requests{BOLD:#}    {} completed, {GOOD}{} ok{GOOD:#}, {failed_style}{} failed{failed_style:#} {DIM}({}){DIM:#}",
        q.completed,
        q.succeeded,
        q.failed,
        percent(r.error_rate),
    )?;

    let statuses: Vec<String> = r
        .status_codes
        .iter()
        .map(|(code, n)| {
            let style = if *code >= 400 { BAD } else { Style::new() };
            format!("{style}{code}{style:#} {DIM}×{DIM:#}{n}")
        })
        .collect();
    if !statuses.is_empty() {
        writeln!(out, "{BOLD}Status{BOLD:#}      {}", statuses.join("   "))?;
    }
    if !r.errors.is_empty() {
        let errors: Vec<String> = r
            .errors
            .iter()
            .map(|(kind, n)| format!("{BAD}{kind}{BAD:#} {DIM}×{DIM:#}{n}"))
            .collect();
        writeln!(out, "{BOLD}Errors{BOLD:#}      {}", errors.join("   "))?;
    }
    Ok(())
}

fn write_warnings(out: &mut String, r: &RunReport) -> std::fmt::Result {
    if r.saturated {
        writeln!(
            out,
            "\n{WARN}! Target rate not sustained.{WARN:#} {} of {} scheduled requests were not sent; \
             p99 send lag was {}.\n  The latencies above include time spent waiting to be sent. \
             If the target is not the bottleneck,\n  raise --concurrency or lower --rps.",
            r.requests.unsent,
            r.requests.scheduled,
            micros(r.send_lag.p99_us),
        )?;
    }
    if r.run.interrupted {
        writeln!(
            out,
            "\n{WARN}! Interrupted{WARN:#} after {}; results cover requests sent until then.",
            seconds(r.run.send_window_s),
        )?;
    }
    Ok(())
}

fn write_thresholds(out: &mut String, r: &RunReport) -> std::fmt::Result {
    if !r.thresholds.is_empty() {
        writeln!(out, "\n{BOLD}Thresholds{BOLD:#}")?;
        let width = r
            .thresholds
            .iter()
            .map(|t| t.expression.len())
            .max()
            .unwrap_or(0);
        for t in &r.thresholds {
            let (mark, verdict) = if t.violated {
                (format!("{BAD}✗{BAD:#}"), format!("{BAD}FAIL{BAD:#}"))
            } else {
                (format!("{GOOD}✓{GOOD:#}"), format!("{GOOD}pass{GOOD:#}"))
            };
            writeln!(
                out,
                "  {mark} {:<width$}  {verdict}  {DIM}observed {}{DIM:#}",
                t.expression,
                observed(t.observed, t.unit),
            )?;
        }
    }
    Ok(())
}

fn write_histogram(out: &mut String, buckets: &[Bucket]) -> std::fmt::Result {
    let Some(peak) = buckets.iter().map(|b| b.count).max().filter(|&n| n > 0) else {
        return Ok(());
    };
    let count_width = peak.to_string().len();
    for b in buckets {
        writeln!(
            out,
            "  {DIM}≤{DIM:#}{:>8}  {BAR}{:<BAR_WIDTH$}{BAR:#} {:>count_width$}",
            bound(b.le_us),
            bar(b.count, peak),
            b.count,
        )?;
    }
    Ok(())
}

/// A horizontal bar with eighth-cell resolution; any non-zero count is visible.
fn bar(count: u64, peak: u64) -> String {
    const PARTIAL: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    if count == 0 {
        return String::new();
    }
    let eighths = (u128::from(count) * (BAR_WIDTH as u128 * 8) / u128::from(peak)).max(1);
    let eighths = usize::try_from(eighths).unwrap_or(BAR_WIDTH * 8);
    let mut s = "█".repeat(eighths / 8);
    if eighths % 8 > 0 {
        s.push(PARTIAL[eighths % 8]);
    }
    s
}

/// Formats microseconds with a unit and three significant digits.
pub fn micros(us: u64) -> String {
    let us_f = us as f64;
    if us < 1_000 {
        format!("{us} µs")
    } else if us < 1_000_000 {
        format!("{} ms", sig3(us_f / 1_000.0))
    } else {
        format!("{} s", sig3(us_f / 1_000_000.0))
    }
}

/// Formats a 1-2-5 bucket bound, which is always a whole number of its unit.
fn bound(us: u64) -> String {
    match us {
        0..1_000 => format!("{us} µs"),
        1_000..1_000_000 => format!("{} ms", us / 1_000),
        _ => format!("{} s", us / 1_000_000),
    }
}

fn sig3(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn seconds(s: f64) -> String {
    if s.fract() == 0.0 {
        format!("{s}s")
    } else {
        format!("{s:.1}s")
    }
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v}")
    } else {
        format!("{v:.1}")
    }
}

fn percent(ratio: f64) -> String {
    format!("{:.2}%", ratio * 100.0)
}

/// Formats a threshold's observed value in its unit.
fn observed(value: f64, unit: Unit) -> String {
    match unit {
        Unit::Ratio => percent(value),
        Unit::PerSecond => format!("{} req/s", number(value)),
        // Latencies are whole microseconds.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Unit::Micros => micros(value as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_with_three_significant_digits() {
        assert_eq!(micros(850), "850 µs");
        assert_eq!(micros(1_234), "1.23 ms");
        assert_eq!(micros(21_340), "21.3 ms");
        assert_eq!(micros(312_400), "312 ms");
        assert_eq!(micros(1_500_000), "1.50 s");
    }

    #[test]
    fn formats_bucket_bounds_without_decimals() {
        assert_eq!(bound(500), "500 µs");
        assert_eq!(bound(5_000), "5 ms");
        assert_eq!(bound(200_000), "200 ms");
        assert_eq!(bound(2_000_000), "2 s");
    }

    #[test]
    fn bars_scale_to_the_peak_and_never_vanish() {
        assert_eq!(bar(0, 10), "");
        assert_eq!(bar(10, 10).chars().count(), BAR_WIDTH);
        assert_eq!(bar(1, 1_000_000), "▏");
        assert_eq!(bar(5, 10), "█".repeat(BAR_WIDTH / 2));
    }

    #[test]
    fn observed_values_use_the_metric_unit() {
        assert_eq!(observed(312_400.0, Unit::Micros), "312 ms");
        assert_eq!(observed(0.003, Unit::Ratio), "0.30%");
        assert_eq!(observed(99.5, Unit::PerSecond), "99.5 req/s");
    }
}
