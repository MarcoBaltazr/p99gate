//! The load generator: schedules requests, enforces concurrency and timeouts,
//! and collects samples.
//!
//! # Model
//!
//! One scheduler loop walks the [`Schedule`]. For request `i` it waits until
//! the intended send time, takes a permit from a semaphore sized to
//! `concurrency`, and spawns a task that executes the request while holding
//! the permit. Each task reports a [`Sample`] to a collector task over a
//! channel; the collector owns the histograms, so nothing is locked on the
//! hot path.
//!
//! # Coordinated omission
//!
//! Latency is measured from the *intended* send time. When every permit is
//! taken (the target is slow or `concurrency` is too low) the scheduler
//! falls behind, and requests go out late in a catch-up burst once permits
//! free up. Their waiting time is charged to their latency, so a stall in
//! the target shows up in the tail percentiles in full instead of being
//! hidden by the load generator politely backing off. The separate send-lag
//! histogram shows how far behind schedule the generator ran.
//!
//! Sending stops at the end of the configured duration; requests that were
//! due but never sent are reported as unsent.

use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use tokio::sync::{Semaphore, mpsc};
use tokio::time::{Instant, sleep_until, timeout};

use crate::executor::Executor;
use crate::outcome::{ErrorKind, Outcome, Sample};
use crate::schedule::{Rate, Schedule};
use crate::stats::Recorder;

/// How much load to generate. Protocol-independent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadConfig {
    /// Target request rate.
    pub rate: Rate,
    /// How long to keep sending.
    pub duration: Duration,
    /// Maximum number of requests in flight at once.
    pub concurrency: NonZeroUsize,
    /// Per-request timeout, measured from the actual send time.
    pub timeout: Duration,
}

/// Runs one load test.
#[derive(Debug)]
pub struct Engine<E> {
    config: LoadConfig,
    executor: Arc<E>,
    progress: Progress,
}

impl<E: Executor> Engine<E> {
    /// Creates an engine that will drive `executor` according to `config`.
    pub fn new(config: LoadConfig, executor: E) -> Self {
        Self {
            config,
            executor: Arc::new(executor),
            progress: Progress::default(),
        }
    }

    /// A handle for observing the run while it is in progress.
    #[must_use]
    pub fn progress(&self) -> Progress {
        self.progress.clone()
    }

    /// Runs the test to completion.
    pub async fn run(self) -> Measurements {
        self.run_until(std::future::pending()).await
    }

    /// Runs the test until it completes or `shutdown` resolves, whichever is first.
    ///
    /// After either, in-flight requests are awaited (bounded by the timeout)
    /// so that the measurements cover every request that was sent.
    #[allow(
        clippy::missing_panics_doc,
        reason = "the expects guard internal invariants, not caller input"
    )]
    pub async fn run_until(self, shutdown: impl Future<Output = ()>) -> Measurements {
        let Self {
            config,
            executor,
            progress,
        } = self;
        let schedule = Schedule::new(config.rate, config.duration);
        let semaphore = Arc::new(Semaphore::new(config.concurrency.get()));
        let (samples_tx, mut samples_rx) = mpsc::unbounded_channel::<Sample>();

        let collector = tokio::spawn(async move {
            let mut recorder = Recorder::new();
            while let Some(sample) = samples_rx.recv().await {
                recorder.record(&sample);
            }
            recorder
        });

        let started_at = SystemTime::now();
        let start = Instant::now();
        let deadline = sleep_until(start + config.duration);
        tokio::pin!(shutdown, deadline);

        let mut sent: u64 = 0;
        let interrupted = loop {
            if sent == schedule.len() {
                break false;
            }
            let intended = start + schedule.offset(sent);
            let permit = tokio::select! {
                biased;
                () = &mut shutdown => break true,
                () = &mut deadline => break false,
                permit = async {
                    sleep_until(intended).await;
                    Arc::clone(&semaphore).acquire_owned().await
                } => permit.expect("the semaphore is never closed"),
            };

            let sent_at = Instant::now();
            let executor = Arc::clone(&executor);
            let samples_tx = samples_tx.clone();
            let progress = progress.clone();
            let request_timeout = config.timeout;
            progress.0.sent.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let outcome = timeout(request_timeout, executor.execute())
                    .await
                    .unwrap_or(Outcome::Error(ErrorKind::Timeout));
                let completed_at = Instant::now();
                drop(permit);
                progress.record_completion(outcome);
                // The collector outlives every sender, so this cannot fail.
                let _ = samples_tx.send(Sample {
                    latency: completed_at - intended,
                    send_lag: sent_at - intended,
                    outcome,
                });
            });
            sent += 1;
        };
        // A run that sent its whole schedule covered the full duration, even
        // though the last request went out one period before the end.
        let send_window = if interrupted {
            start.elapsed()
        } else {
            start.elapsed().max(config.duration)
        };

        drop(samples_tx);
        let recorder = collector.await.expect("the collector task does not panic");

        Measurements {
            config,
            scheduled: schedule.len(),
            sent,
            send_window,
            elapsed: start.elapsed(),
            interrupted,
            started_at,
            recorder,
        }
    }
}

/// Everything measured during one run.
#[derive(Debug, Clone)]
pub struct Measurements {
    /// The configuration the run used.
    pub config: LoadConfig,
    /// Requests the schedule called for over the full duration.
    pub scheduled: u64,
    /// Requests actually sent.
    pub sent: u64,
    /// Time from the start of the run until sending stopped.
    pub send_window: Duration,
    /// Time from the start of the run until the last response arrived.
    pub elapsed: Duration,
    /// Whether the run was stopped early by the shutdown signal.
    pub interrupted: bool,
    /// Wall-clock time at which the run started.
    pub started_at: SystemTime,
    pub(crate) recorder: Recorder,
}

impl Measurements {
    /// Requests that were due within the send window but never sent because
    /// the generator could not keep up.
    #[must_use]
    pub fn unsent(&self) -> u64 {
        if self.interrupted {
            // Requests after the interruption were never due, so only count
            // the ones the schedule expected before it.
            let due =
                Schedule::new(self.config.rate, self.config.duration).due_by(self.send_window);
            due.saturating_sub(self.sent)
        } else {
            self.scheduled - self.sent
        }
    }
}

/// A cheap, cloneable view of a run's live counters.
#[derive(Debug, Clone, Default)]
pub struct Progress(Arc<Counters>);

#[derive(Debug, Default)]
struct Counters {
    sent: AtomicU64,
    completed: AtomicU64,
    failures: AtomicU64,
}

impl Progress {
    fn record_completion(&self, outcome: Outcome) {
        self.0.completed.fetch_add(1, Ordering::Relaxed);
        if outcome.is_failure() {
            self.0.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Reads the current counters.
    #[must_use]
    pub fn snapshot(&self) -> ProgressSnapshot {
        // Load completions first: a racing request can then only make
        // `sent` look larger, never `completed > sent`.
        let completed = self.0.completed.load(Ordering::Relaxed);
        let failures = self.0.failures.load(Ordering::Relaxed);
        let sent = self.0.sent.load(Ordering::Relaxed);
        ProgressSnapshot {
            sent,
            completed,
            failures,
        }
    }
}

/// Point-in-time counters from a running test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// Requests sent so far.
    pub sent: u64,
    /// Requests completed so far, successfully or not.
    pub completed: u64,
    /// Completed requests that count as failures.
    pub failures: u64,
}

impl ProgressSnapshot {
    /// Requests currently awaiting a response.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.sent.saturating_sub(self.completed)
    }
}

#[cfg(test)]
mod tests {
    //! These tests run on Tokio's paused clock: time only advances when every
    //! task is idle, so timings are exact and the tests are deterministic and
    //! instant regardless of machine load.

    use super::*;

    const OK: Outcome = Outcome::Response { status: 200 };

    fn config(rps: f64, secs: u64, concurrency: usize) -> LoadConfig {
        LoadConfig {
            rate: Rate::per_second(rps).unwrap(),
            duration: Duration::from_secs(secs),
            concurrency: NonZeroUsize::new(concurrency).unwrap(),
            timeout: Duration::from_secs(30),
        }
    }

    fn ms(us: u64) -> f64 {
        us as f64 / 1_000.0
    }

    /// A fake target that answers after `base`, except that it freezes
    /// between `stall_from` and `stall_until` (offsets from creation).
    /// Requests arriving during the freeze complete when it ends.
    struct FreezingTarget {
        created: Instant,
        base: Duration,
        stall_from: Duration,
        stall_until: Duration,
    }

    impl FreezingTarget {
        fn new(base: Duration, stall_from: Duration, stall_until: Duration) -> Self {
            Self {
                created: Instant::now(),
                base,
                stall_from,
                stall_until,
            }
        }
    }

    impl Executor for FreezingTarget {
        async fn execute(&self) -> Outcome {
            let now = self.created.elapsed();
            if (self.stall_from..self.stall_until).contains(&now) {
                sleep_until(self.created + self.stall_until).await;
            }
            tokio::time::sleep(self.base).await;
            OK
        }
    }

    /// A fake target with a fixed response time.
    struct Fixed(Duration);

    impl Executor for Fixed {
        async fn execute(&self) -> Outcome {
            tokio::time::sleep(self.0).await;
            OK
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sends_the_scheduled_load_regardless_of_response_time() {
        // Responses take 10x the send interval; an open-loop generator keeps
        // sending on schedule anyway.
        let engine = Engine::new(config(100.0, 2, 1_000), Fixed(Duration::from_millis(100)));
        let m = engine.run().await;

        assert_eq!(m.scheduled, 200);
        assert_eq!(m.sent, 200);
        assert_eq!(m.unsent(), 0);
        assert_eq!(m.send_window, Duration::from_secs(2));
        assert_eq!(m.recorder.completed(), 200);
        assert_eq!(
            m.recorder.send_lag().max(),
            1,
            "nothing should be sent late"
        );
        let p50 = ms(m.recorder.latency().value_at_quantile(0.5));
        assert!((99.0..=101.0).contains(&p50), "p50 was {p50} ms");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stall_is_reflected_in_the_tail_when_concurrency_is_unbounded() {
        // 1,000 requests over 10 s. The target freezes from 2 s to 4 s, so the
        // 200 requests sent in that window wait between 2 s and 0 s for it to
        // thaw: their latencies are spread evenly over (0, 2 s].
        let target = FreezingTarget::new(
            Duration::from_millis(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
        );
        let m = Engine::new(config(100.0, 10, 10_000), target).run().await;
        let latency = m.recorder.latency();

        assert_eq!(m.recorder.completed(), 1_000);
        // Top 10% (100 samples) are the first half of the stall: >= 1 s.
        let p90 = ms(latency.value_at_quantile(0.90));
        assert!((990.0..=1_020.0).contains(&p90), "p90 was {p90} ms");
        // Top 1% (10 samples) are the first 100 ms of the stall: >= 1.9 s.
        let p99 = ms(latency.value_at_quantile(0.99));
        assert!((1_890.0..=1_920.0).contains(&p99), "p99 was {p99} ms");
        let max = ms(latency.max());
        assert!((2_000.0..=2_010.0).contains(&max), "max was {max} ms");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stall_is_reflected_in_the_tail_when_concurrency_is_exhausted() {
        // With concurrency 1, a frozen target also freezes the generator: no
        // permit is free, so the requests due during the freeze cannot be sent.
        // A closed-loop tool would record one slow sample and ~99 fast ones
        // sent after the freeze. Measuring from the intended send time charges
        // each of those ~100 requests for its wait.
        let target = FreezingTarget::new(
            Duration::from_millis(1),
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let m = Engine::new(config(100.0, 10, 1), target).run().await;
        let latency = m.recorder.latency();

        assert_eq!(m.sent, 1_000, "the backlog is caught up after the stall");
        // About 10% of requests were affected, with waits spread over 0..1 s.
        let p95 = ms(latency.value_at_quantile(0.95));
        assert!(p95 > 400.0, "p95 was {p95} ms");
        let p99 = ms(latency.value_at_quantile(0.99));
        assert!(p99 > 850.0, "p99 was {p99} ms");

        // The send lag shows the generator fell behind, which is how the
        // report tells "slow target" apart from "slow responses".
        let lag_p99 = ms(m.recorder.send_lag().value_at_quantile(0.99));
        assert!(lag_p99 > 850.0, "send lag p99 was {lag_p99} ms");

        // Sanity check of the claim above: time spent on the wire alone
        // (latency minus send lag) is tiny for every request but the stalled one.
        let wire_p99 = p99 - lag_p99;
        assert!(wire_p99 < 50.0, "wire time p99 was {wire_p99} ms");
    }

    #[tokio::test(start_paused = true)]
    async fn requests_that_could_not_be_sent_are_reported_as_unsent() {
        // Each response takes 1 s with only 2 in flight: at most ~2/s can be
        // sent against a target of 10/s.
        let m = Engine::new(config(10.0, 5, 2), Fixed(Duration::from_secs(1)))
            .run()
            .await;

        assert_eq!(m.scheduled, 50);
        assert_eq!(m.sent, 10);
        assert_eq!(m.unsent(), 40);
        assert_eq!(m.send_window, Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn timeouts_are_errors_with_latency_from_the_intended_time() {
        let mut cfg = config(10.0, 1, 100);
        cfg.timeout = Duration::from_millis(250);
        let m = Engine::new(cfg, Fixed(Duration::from_secs(10))).run().await;

        assert_eq!(m.recorder.errors()[&ErrorKind::Timeout], 10);
        assert_eq!(m.recorder.failures(), 10);
        let max = ms(m.recorder.latency().max());
        assert!((250.0..=251.0).contains(&max), "max was {max} ms");
        // In-flight requests are awaited, bounded by the timeout.
        assert!(m.elapsed <= Duration::from_millis(900 + 251));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_sending_and_drains_in_flight_requests() {
        let engine = Engine::new(config(100.0, 60, 1_000), Fixed(Duration::from_millis(50)));
        let progress = engine.progress();
        let m = engine
            .run_until(tokio::time::sleep(Duration::from_secs(1)))
            .await;

        assert!(m.interrupted);
        assert_eq!(m.sent, 100);
        assert_eq!(m.unsent(), 1, "the request due exactly at 1 s");
        assert_eq!(m.recorder.completed(), 100);
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.sent, 100);
        assert_eq!(snapshot.completed, 100);
        assert_eq!(snapshot.in_flight(), 0);
    }
}
