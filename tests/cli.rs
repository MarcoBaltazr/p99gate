//! Tests of the `p99gate` binary: arguments in, output and exit status out.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::mpsc;

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

/// A forward proxy on a background thread that answers every request with
/// 200 and reports each request line it receives.
fn forward_proxy() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (seen_tx, seen_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let seen_tx = seen_tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                // One iteration per request on a keep-alive connection.
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let _ = seen_tx.send(line.trim_end().to_owned());
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                    }
                    let _ = writer.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                    line.clear();
                }
            });
        }
    });
    (url, seen_rx)
}

fn p99gate_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_p99gate"));
    // Start from a clean proxy environment whatever the test runner has.
    for var in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(var);
    }
    command
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("the binary runs")
}

#[test]
fn run_uses_the_proxy_from_the_environment() {
    let (proxy, seen) = forward_proxy();
    let out = p99gate_with_env(
        &[
            "run",
            "http://target.invalid/health",
            "--rps",
            "5",
            "--duration",
            "1s",
            "-o",
            "json",
        ],
        &[("HTTP_PROXY", &proxy)],
    );
    assert_eq!(out.status.code(), Some(0));
    let report: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.status_codes[&200], 5);
    assert_eq!(report.target.proxy.as_deref(), Some(proxy.as_str()));
    assert_eq!(
        seen.recv().unwrap(),
        "GET http://target.invalid/health HTTP/1.1"
    );
}

#[test]
fn no_proxy_and_the_demo_ignore_the_proxy_environment() {
    let (proxy, seen) = forward_proxy();
    let env = [("HTTP_PROXY", proxy.as_str())];

    let out = p99gate_with_env(
        &[
            "run",
            "http://127.0.0.1:1/",
            "--no-proxy",
            "--rps",
            "2",
            "--duration",
            "1s",
            "-o",
            "json",
        ],
        &env,
    );
    let report: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.target.proxy, None);
    assert_eq!(
        report.requests.failed, 2,
        "port 1 is closed: direct connections fail"
    );

    let out = p99gate_with_env(&["demo", "--duration", "1s", "-o", "json"], &env);
    let report: RunReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.target.proxy, None);
    assert!(report.status_codes[&200] > 40);

    assert!(seen.try_recv().is_err(), "nothing may reach the proxy");
}
