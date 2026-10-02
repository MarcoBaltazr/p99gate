//! The protocol seam: anything that can perform one request.

use std::future::Future;

use crate::outcome::Outcome;

/// Performs a single request and reports what happened.
///
/// The engine owns timing, scheduling, concurrency and timeouts; an executor
/// only talks to the target. That keeps protocol support (HTTP today, gRPC or
/// WebSocket later) independent of the measurement logic.
///
/// Implementations are shared across all in-flight requests, so `execute`
/// takes `&self` and must be safe to call concurrently.
pub trait Executor: Send + Sync + 'static {
    /// Sends one request and waits for its outcome.
    ///
    /// The engine cancels the returned future when the request times out, so
    /// it must be cancel-safe (dropping it must not leave shared state broken).
    fn execute(&self) -> impl Future<Output = Outcome> + Send;
}
