//! What happened to a single request.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The result of executing one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A response arrived. The request may still have failed at the
    /// application level; see [`Outcome::is_failure`].
    Response {
        /// Protocol status code, e.g. the HTTP status.
        status: u16,
    },
    /// No usable response arrived.
    Error(ErrorKind),
}

impl Outcome {
    /// Whether this outcome counts towards the error rate.
    ///
    /// Transport errors always do. HTTP responses with status 400 or above do
    /// too: a load test against an endpoint that answers 404 or 503 is not
    /// measuring what it claims to.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        match self {
            Self::Response { status } => *status >= 400,
            Self::Error(_) => true,
        }
    }
}

/// Why a request produced no response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// No response within the configured timeout.
    Timeout,
    /// The connection could not be established.
    Connect,
    /// The connection failed after it was established, or the response was malformed.
    Io,
    /// Any other failure.
    Other,
}

impl ErrorKind {
    /// A short, stable, machine-friendly name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Timing and outcome of one completed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Time from the *intended* send time to completion.
    ///
    /// This is the coordinated-omission-corrected latency: if the request went
    /// out late, the delay is charged to the request.
    pub latency: Duration,
    /// Time from the intended send time to the actual send time.
    pub send_lag: Duration,
    /// What happened.
    pub outcome: Outcome,
}
