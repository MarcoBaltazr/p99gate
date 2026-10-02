//! Tests of the `p99gate` binary: arguments in, output and exit status out.

use std::process::{Command, Output};

use p99gate::report::RunReport;

fn p99gate(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_p99gate"))
        .args(args)
        .output()
        .expect("the binary runs")
}

#[test]
fn demo_prints_a_plain_text_report_when_not_on_a_terminal() {
    let out = p99gate(&["demo", "--duration", "1s"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    for section in ["Latency", "p99", "Throughput", "Requests", "Status"] {
        assert!(stdout.contains(section), "missing {section}:\n{stdout}");
    }
    assert!(
        !stdout.contains('\x1b'),
        "styling must be stripped off-terminal"
    );
}

#[test]
fn violated_threshold_exits_1_and_is_recorded_in_json() {
    let out = p99gate(&[
        "demo",
        "--duration",
        "1s",
        "--output",
        "json",
        "--fail-if",
        "p99>1us",
        "--fail-if",
        "error_rate>100%",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let report: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.thresholds.len(), 2);
    assert!(report.thresholds[0].violated);
    assert!(!report.thresholds[1].violated);
}

#[test]
fn json_out_writes_the_same_report_to_a_file() {
    let path = std::env::temp_dir().join(format!("p99gate-test-{}.json", std::process::id()));
    let out = p99gate(&[
        "demo",
        "--duration",
        "1s",
        "--output",
        "json",
        "--json-out",
        path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
    let from_file: RunReport =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let from_stdout: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(from_file, from_stdout);
}

#[test]
fn invalid_arguments_exit_2_with_a_helpful_message() {
    let out = p99gate(&["run", "http://localhost/", "--fail-if", "p99>fast"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("duration with a unit"), "{stderr}");
}

#[test]
fn unreachable_target_reports_connect_errors() {
    // Port 1 (tcpmux) is essentially never listening.
    let out = p99gate(&[
        "run",
        "http://127.0.0.1:1/",
        "--rps",
        "5",
        "--duration",
        "1s",
        "--output",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "no threshold was set");
    let report: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.requests.failed, 5);
    assert!((report.error_rate - 1.0).abs() < f64::EPSILON);
}
