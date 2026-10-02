//! The `p99gate` command-line interface.
//!
//! A thin layer over the library: it turns arguments into a [`LoadConfig`]
//! and an executor, shows progress, renders the [`RunReport`] and maps the
//! result to an exit code.

mod args;
mod live;
mod render;

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use bytes::Bytes;
use clap::Parser as _;
use http::HeaderMap;
use p99gate::demo::DemoServer;
use p99gate::engine::{Engine, LoadConfig};
use p99gate::http::{HttpExecutor, RequestSpec};
use p99gate::report::RunReport;

use crate::args::{Cli, Command, LoadArgs, OutputFormat, ReportArgs};
use crate::live::Live;

/// Exit statuses. Usage errors exit with 2 via clap.
mod exit {
    pub const THRESHOLD_VIOLATED: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const SATURATED: u8 = 3;
    pub const INTERRUPTED: u8 = 130;
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(exit::ERROR)
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Run(args) => {
            let mut headers = HeaderMap::new();
            for (name, value) in args.headers {
                headers.append(name, value);
            }
            let spec = RequestSpec {
                method: args.method,
                uri: args.url,
                headers,
                body: load_body(args.body.as_deref())?,
            };
            load_test(spec, &args.load, &args.report).await
        }
        Command::Demo(args) => {
            let server = DemoServer::start()
                .await
                .context("could not start the demo server")?;
            eprintln!(
                "Demo server listening on {}\n  \
                 latency ~20 ms median, 1% slow responses (250-600 ms), 0.3% 503 errors",
                server.url()
            );
            let spec = RequestSpec::get(server.url().parse()?);
            load_test(spec, &args.load, &args.report).await
        }
    }
}

async fn load_test(
    spec: RequestSpec,
    load: &LoadArgs,
    output: &ReportArgs,
) -> anyhow::Result<ExitCode> {
    let config = LoadConfig {
        rate: load.rps,
        duration: load.duration,
        concurrency: load.concurrency,
        timeout: load.timeout,
    };
    let target = spec.target();
    let executor = HttpExecutor::new(spec, config.concurrency.get())?;
    let engine = Engine::new(config, executor);

    eprintln!(
        "Sending {} req/s to {} {} for {}",
        config.rate.get(),
        target.method,
        target.url,
        humantime::format_duration(config.duration),
    );
    let live = Live::start(engine.progress(), config.duration);
    let measurements = engine.run_until(ctrl_c()).await;
    live.finish();

    let mut report = RunReport::new(&measurements, target);
    report.thresholds = output.fail_if.iter().map(|t| t.evaluate(&report)).collect();

    // Write the file first: it is the CI artifact, and must not depend on
    // stdout still being open.
    if let Some(path) = &output.json_out {
        write_json(path, &report).with_context(|| format!("could not write {}", path.display()))?;
    }
    match output.output {
        OutputFormat::Text => {
            anstream::stdout().write_all(render::report(&report).as_bytes())?;
        }
        OutputFormat::Json => {
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer_pretty(&mut stdout, &report)?;
            writeln!(stdout)?;
        }
    }

    Ok(ExitCode::from(if report.run.interrupted {
        exit::INTERRUPTED
    } else if report.any_threshold_violated() {
        exit::THRESHOLD_VIOLATED
    } else if output.strict && report.saturated {
        exit::SATURATED
    } else {
        0
    }))
}

/// Reads `--body`: a literal, or `@path` for a file's contents.
fn load_body(body: Option<&str>) -> anyhow::Result<Bytes> {
    Ok(match body {
        None => Bytes::new(),
        Some(body) => match body.strip_prefix('@') {
            Some(path) => std::fs::read(path)
                .with_context(|| format!("could not read request body from {path}"))?
                .into(),
            None => Bytes::copy_from_slice(body.as_bytes()),
        },
    })
}

fn write_json(path: &Path, report: &RunReport) -> anyhow::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut file, report)?;
    writeln!(file)?;
    file.flush()?;
    Ok(())
}

/// Resolves on the first Ctrl-C, after which a second one exits immediately.
async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        // No signal handling available: never interrupt.
        std::future::pending::<()>().await;
    }
    eprintln!("\nStopping: waiting for in-flight requests. Press Ctrl-C again to quit now.");
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(exit::INTERRUPTED.into());
    });
}
