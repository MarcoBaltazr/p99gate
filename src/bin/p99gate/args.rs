//! Command-line arguments.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use http::{HeaderName, HeaderValue, Method, Uri};
use p99gate::schedule::Rate;
use p99gate::threshold::Threshold;

/// HTTP load testing for CI: catch latency regressions before they ship.
///
/// Requests are sent at a constant rate whether or not earlier ones have
/// answered, and latency is measured from when each request was *meant* to be
/// sent, so server stalls are not hidden by the load generator slowing down.
#[derive(Debug, Parser)]
#[command(version, about, max_term_width = 100)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "parsed once per process; boxing would only add noise"
)]
pub enum Command {
    /// Send load to one HTTP endpoint and report latency.
    Run(RunArgs),
    /// Start a local demo server and load test it. No setup needed.
    Demo(DemoArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// URL to send requests to (http:// or https://).
    #[arg(value_parser = parse_url)]
    pub url: Uri,

    #[command(flatten)]
    pub load: LoadArgs,

    /// HTTP method.
    #[arg(short = 'X', long, default_value = "GET", value_parser = parse_method)]
    pub method: Method,

    /// Request header as "Name: value". Repeatable.
    #[arg(short = 'H', long = "header", value_name = "HEADER", value_parser = parse_header)]
    pub headers: Vec<(HeaderName, HeaderValue)>,

    /// Request body, or @path to read it from a file.
    #[arg(short, long)]
    pub body: Option<String>,

    #[command(flatten)]
    pub protocol: ProtocolArgs,

    /// Send requests through this http:// proxy. By default the proxy is
    /// taken from `HTTP_PROXY` / `HTTPS_PROXY`, honouring `NO_PROXY`.
    #[arg(long, value_name = "URL", value_parser = parse_proxy)]
    pub proxy: Option<Uri>,

    /// Ignore proxy environment variables and connect directly.
    #[arg(long, conflicts_with = "proxy")]
    pub no_proxy: bool,

    #[command(flatten)]
    pub report: ReportArgs,
}

#[derive(Debug, Args)]
pub struct DemoArgs {
    #[command(flatten)]
    pub load: LoadArgs,

    #[command(flatten)]
    pub protocol: ProtocolArgs,

    #[command(flatten)]
    pub report: ReportArgs,
}

/// HTTP protocol options.
#[derive(Debug, Args)]
pub struct ProtocolArgs {
    /// Use HTTP/2 (ALPN for https://, prior knowledge for http://) over a
    /// single multiplexed connection. Default is HTTP/1.1.
    #[arg(long)]
    pub http2: bool,
}

/// Options shared by every command that generates load.
#[derive(Debug, Args)]
pub struct LoadArgs {
    /// Target request rate, in requests per second.
    #[arg(short, long, default_value = "50", value_parser = parse_rate)]
    pub rps: Rate,

    /// How long to send requests, e.g. 30s, 2m.
    #[arg(short, long, default_value = "10s", value_parser = humantime::parse_duration)]
    pub duration: Duration,

    /// Maximum requests in flight. When all are busy, new requests wait and
    /// the wait counts towards their latency.
    #[arg(short, long, default_value = "100")]
    pub concurrency: NonZeroUsize,

    /// Per-request timeout, e.g. 5s, 500ms.
    #[arg(short, long, default_value = "10s", value_parser = humantime::parse_duration)]
    pub timeout: Duration,
}

/// Options controlling output and pass/fail.
#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Output format written to stdout.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,

    /// Also write the JSON report to this file.
    #[arg(long, value_name = "PATH")]
    pub json_out: Option<PathBuf>,

    /// Exit with status 1 if this condition holds, e.g. `p99>300ms`,
    /// `error_rate>1%`, `rps<100`. Repeatable.
    #[arg(long, value_name = "CONDITION")]
    pub fail_if: Vec<Threshold>,

    /// Exit with status 3 if the target rate could not be sustained.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable report.
    Text,
    /// The JSON report described in docs/json-schema.md.
    Json,
}

fn parse_url(s: &str) -> Result<Uri, String> {
    let uri: Uri = s.parse().map_err(|e| format!("{e}"))?;
    match (uri.scheme_str(), uri.host()) {
        (Some("http" | "https"), Some(_)) => Ok(uri),
        _ => Err("expected an absolute http:// or https:// URL".to_owned()),
    }
}

fn parse_proxy(s: &str) -> Result<Uri, String> {
    let uri: Uri = s.parse().map_err(|e| format!("{e}"))?;
    match (uri.scheme_str(), uri.host()) {
        (Some("http"), Some(_)) => Ok(uri),
        _ => Err("expected an http:// proxy URL".to_owned()),
    }
}

fn parse_method(s: &str) -> Result<Method, String> {
    Method::from_bytes(s.to_ascii_uppercase().as_bytes()).map_err(|e| e.to_string())
}

fn parse_header(s: &str) -> Result<(HeaderName, HeaderValue), String> {
    let (name, value) = s
        .split_once(':')
        .ok_or_else(|| "expected \"Name: value\"".to_owned())?;
    let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|e| e.to_string())?;
    let value = HeaderValue::from_str(value.trim()).map_err(|e| e.to_string())?;
    Ok((name, value))
}

fn parse_rate(s: &str) -> Result<Rate, String> {
    s.parse::<f64>()
        .ok()
        .and_then(Rate::per_second)
        .ok_or_else(|| "expected a positive number".to_owned())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_a_full_run_command() {
        let cli = Cli::try_parse_from([
            "p99gate",
            "run",
            "https://example.com/api",
            "--rps",
            "250",
            "-d",
            "1m",
            "-X",
            "post",
            "-H",
            "Content-Type: application/json",
            "-H",
            "X-Trace:abc",
            "--body",
            "{}",
            "--fail-if",
            "p99>300ms",
            "--fail-if",
            "error_rate>1%",
            "-o",
            "json",
        ])
        .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(args.load.rps.get(), 250.0);
        assert_eq!(args.load.duration, Duration::from_secs(60));
        assert_eq!(args.method, Method::POST);
        assert_eq!(args.headers.len(), 2);
        assert_eq!(args.headers[1].1, "abc");
        assert_eq!(args.report.fail_if.len(), 2);
        assert_eq!(args.report.output, OutputFormat::Json);
    }

    #[test]
    fn rejects_invalid_values() {
        for argv in [
            &["p99gate", "run", "example.com"][..],
            &["p99gate", "run", "http://x/", "--rps", "0"],
            &["p99gate", "run", "http://x/", "-H", "no-colon"],
            &["p99gate", "run", "http://x/", "--fail-if", "p99>fast"],
            &["p99gate", "run", "http://x/", "-c", "0"],
            &["p99gate", "run", "http://x/", "--proxy", "socks5://p:1080"],
            &[
                "p99gate",
                "run",
                "http://x/",
                "--proxy",
                "http://p/",
                "--no-proxy",
            ],
        ] {
            assert!(Cli::try_parse_from(argv).is_err(), "{argv:?} should fail");
        }
    }
}
