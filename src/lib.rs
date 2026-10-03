//! p99gate: HTTP load testing focused on catching latency regressions in CI.
//!
//! This crate is the engine behind the `p99gate` command-line tool. It can be
//! used on its own to generate load and measure latency from Rust code.
//!
//! # Measurement model
//!
//! - **Open loop.** Requests are sent on a fixed schedule ([`schedule`])
//!   regardless of how quickly responses arrive.
//! - **Coordinated omission correction.** Latency is measured from the time a
//!   request was *meant* to be sent. If the generator falls behind, the delay
//!   counts towards latency and the run is flagged as saturated.
//! - **HDR histograms** keep percentiles accurate to three significant digits.
//!
//! # Example
//!
//! ```
//! use std::num::NonZeroUsize;
//! use std::time::Duration;
//!
//! use p99gate::demo::DemoServer;
//! use p99gate::engine::{Engine, LoadConfig};
//! use p99gate::http::{HttpExecutor, HttpOptions, RequestSpec};
//! use p99gate::report::RunReport;
//! use p99gate::schedule::Rate;
//! use p99gate::threshold::Threshold;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let server = DemoServer::start().await?;
//! let spec = RequestSpec::get(server.url().parse()?);
//!
//! let config = LoadConfig {
//!     rate: Rate::per_second(100.0).expect("positive rate"),
//!     duration: Duration::from_millis(500),
//!     concurrency: NonZeroUsize::new(50).expect("non-zero"),
//!     timeout: Duration::from_secs(5),
//! };
//! let executor = HttpExecutor::new(spec, &HttpOptions::new(config.concurrency.get()))?;
//! let target = executor.target();
//! let measurements = Engine::new(config, executor).run().await;
//! let report = RunReport::new(&measurements, target);
//!
//! assert_eq!(report.requests.sent, 50);
//! let gate: Threshold = "error_rate>50%".parse()?;
//! assert!(!gate.evaluate(&report).violated);
//! # Ok(())
//! # }
//! ```
//!
//! # Structure
//!
//! - [`engine`] schedules requests, enforces concurrency and timeouts, and
//!   collects samples. It is protocol-independent.
//! - [`executor::Executor`] is the protocol seam; [`http::HttpExecutor`] is
//!   the HTTP implementation.
//! - [`report::RunReport`] is the serialisable result, documented in
//!   `docs/json-schema.md`.
//! - [`threshold`] parses and evaluates `--fail-if` conditions.
//! - [`demo`] is a local server with controllable latency.

pub mod demo;
pub mod engine;
pub mod executor;
pub mod http;
pub mod outcome;
pub mod report;
pub mod schedule;
mod stats;
pub mod threshold;
