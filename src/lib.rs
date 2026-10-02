//! p99gate: HTTP load testing focused on catching latency regressions in CI.

pub mod engine;
pub mod executor;
pub mod outcome;
pub mod report;
pub mod schedule;
mod stats;
