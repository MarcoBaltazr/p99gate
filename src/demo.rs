//! A local HTTP server with controllable latency, used by `p99gate demo` and
//! by the integration tests.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Response, StatusCode};
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

/// How the server should answer one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// How long to wait before answering.
    pub delay: Duration,
    /// Status code of the answer.
    pub status: StatusCode,
}

/// Decides the [`Reply`] for each request, given the time since the server started.
pub type Responder = Arc<dyn Fn(Duration) -> Reply + Send + Sync>;

/// A running local server. It stops when dropped.
#[derive(Debug)]
pub struct DemoServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl DemoServer {
    /// Starts a server on an ephemeral localhost port with the latency
    /// profile of [`realistic`].
    pub async fn start() -> io::Result<Self> {
        Self::start_with(realistic()).await
    }

    /// Starts a server on an ephemeral localhost port that answers according
    /// to `responder`.
    pub async fn start_with(responder: Responder) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let started = Instant::now();
        let task = tokio::spawn(async move {
            // Connections live in this set so that aborting the accept loop
            // also closes every open connection.
            let mut connections = JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let responder = Arc::clone(&responder);
                let service = service_fn(move |_request| {
                    let reply = responder(started.elapsed());
                    async move {
                        tokio::time::sleep(reply.delay).await;
                        let mut response = Response::new(Full::new(Bytes::from_static(
                            b"{\"message\":\"hello from p99gate demo\"}\n",
                        )));
                        *response.status_mut() = reply.status;
                        Ok::<_, Infallible>(response)
                    }
                });
                connections.spawn(async move {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
                // Reap finished connections so the set does not grow forever.
                while connections.try_join_next().is_some() {}
            }
        });
        Ok(Self { addr, task })
    }

    /// The address the server listens on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The server's root URL, e.g. `http://127.0.0.1:41234/`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }
}

impl Drop for DemoServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A latency profile resembling a typical API endpoint:
///
/// - most responses are log-normally distributed around 20 ms;
/// - 1% are slow, between 250 and 600 ms (a GC pause, a cold cache);
/// - 0.3% fail fast with `503 Service Unavailable`.
#[must_use]
pub fn realistic() -> Responder {
    Arc::new(|_| {
        let roll = fastrand::f64();
        if roll < 0.003 {
            Reply {
                delay: Duration::from_millis(2),
                status: StatusCode::SERVICE_UNAVAILABLE,
            }
        } else if roll < 0.013 {
            Reply {
                delay: Duration::from_millis(fastrand::u64(250..=600)),
                status: StatusCode::OK,
            }
        } else {
            Reply {
                delay: Duration::from_secs_f64(log_normal(0.020, 0.5)),
                status: StatusCode::OK,
            }
        }
    })
}

/// A server that answers `200 OK` after `base`, except that it freezes from
/// `stall_from` to `stall_until` after starting: requests arriving in that
/// window are answered when it ends. Models a stop-the-world pause.
#[must_use]
pub fn stalling(base: Duration, stall_from: Duration, stall_until: Duration) -> Responder {
    Arc::new(move |since_start| {
        let wait = if (stall_from..stall_until).contains(&since_start) {
            stall_until.saturating_sub(since_start)
        } else {
            Duration::ZERO
        };
        Reply {
            delay: base + wait,
            status: StatusCode::OK,
        }
    })
}

/// Samples a log-normal distribution with the given median and shape `sigma`.
fn log_normal(median: f64, sigma: f64) -> f64 {
    median * (sigma * standard_normal()).exp()
}

/// Samples a standard normal distribution (Box-Muller transform).
fn standard_normal() -> f64 {
    // 1 - u keeps the argument of ln in (0, 1], avoiding ln(0).
    let u1 = 1.0 - fastrand::f64();
    let u2 = fastrand::f64();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_normal_has_the_requested_median() {
        fastrand::seed(7);
        let mut samples: Vec<f64> = (0..20_001).map(|_| log_normal(0.020, 0.5)).collect();
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        assert!((0.019..0.021).contains(&median), "median was {median}");
    }

    #[test]
    fn stalling_holds_requests_until_the_stall_ends() {
        let r = stalling(
            Duration::from_millis(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
        );
        assert_eq!(r(Duration::from_secs(1)).delay, Duration::from_millis(1));
        assert_eq!(
            r(Duration::from_millis(2_250)).delay,
            Duration::from_millis(751)
        );
        assert_eq!(r(Duration::from_secs(3)).delay, Duration::from_millis(1));
    }
}
