//! End-to-end tests: the engine and HTTP executor against real local servers.
//!
//! These use wall-clock time and real sockets, so the assertions leave room
//! for scheduler noise on busy CI machines. The exact coordinated-omission
//! arithmetic is covered by the paused-clock tests in `src/engine.rs`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use http::StatusCode;
use p99gate::demo::{self, DemoServer, Reply};
use p99gate::engine::{Engine, LoadConfig};
use p99gate::http::{HttpExecutor, RequestSpec};
use p99gate::outcome::ErrorKind;
use p99gate::report::RunReport;
use p99gate::schedule::Rate;

fn config(rps: f64, duration: Duration, concurrency: usize) -> LoadConfig {
    LoadConfig {
        rate: Rate::per_second(rps).unwrap(),
        duration,
        concurrency: NonZeroUsize::new(concurrency).unwrap(),
        timeout: Duration::from_secs(5),
    }
}

async fn run(server: &DemoServer, config: LoadConfig) -> RunReport {
    let spec = RequestSpec::get(server.url().parse().unwrap());
    let target = spec.target();
    let executor = HttpExecutor::new(spec, config.concurrency.get()).unwrap();
    let measurements = Engine::new(config, executor).run().await;
    RunReport::new(&measurements, target)
}

fn ms(us: u64) -> f64 {
    us as f64 / 1_000.0
}

#[tokio::test(flavor = "multi_thread")]
async fn demo_server_run_produces_a_plausible_report() {
    let server = DemoServer::start().await.unwrap();
    let report = run(&server, config(200.0, Duration::from_secs(2), 100)).await;

    assert_eq!(report.requests.scheduled, 400);
    assert_eq!(report.requests.sent, 400);
    assert_eq!(report.requests.completed, 400);
    assert!(!report.saturated, "send lag: {:?}", report.send_lag);
    assert!(report.status_codes[&200] > 380);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

    // The demo's median is 20 ms; allow for timer granularity and CI noise.
    let p50 = ms(report.latency.percentiles.p50_us);
    assert!((15.0..80.0).contains(&p50), "p50 was {p50} ms");
    assert_eq!(
        report
            .latency
            .histogram
            .iter()
            .map(|b| b.count)
            .sum::<u64>(),
        400
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_stall_shows_up_in_the_tail_latency() {
    // 300 requests over 3 s; the server freezes for the second second.
    // The ~100 requests sent during the freeze wait 1 s down to 0 s, so the
    // top 10% of latencies are in the first ~third of that range or above.
    let server = DemoServer::start_with(demo::stalling(
        Duration::from_millis(2),
        Duration::from_secs(1),
        Duration::from_secs(2),
    ))
    .await
    .unwrap();
    let report = run(&server, config(100.0, Duration::from_secs(3), 1_000)).await;
    let latency = report.latency.percentiles;

    assert_eq!(report.requests.completed, 300);
    assert!(
        ms(latency.p50_us) < 100.0,
        "p50 was {} ms",
        ms(latency.p50_us)
    );
    assert!(
        ms(latency.p90_us) > 600.0,
        "p90 was {} ms",
        ms(latency.p90_us)
    );
    assert!(
        ms(latency.p99_us) > 900.0,
        "p99 was {} ms",
        ms(latency.p99_us)
    );
    assert!(
        ms(latency.max_us) < 1_500.0,
        "max was {} ms",
        ms(latency.max_us)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stall_that_blocks_the_generator_is_still_charged_to_latency() {
    // With one connection, the stalled request blocks every request due after
    // it. Measuring from the actual send time would report one slow request;
    // measuring from the intended time reports all ~100 that were held back.
    let server = DemoServer::start_with(demo::stalling(
        Duration::from_millis(2),
        Duration::from_secs(1),
        Duration::from_secs(2),
    ))
    .await
    .unwrap();
    let report = run(&server, config(100.0, Duration::from_secs(3), 1)).await;
    let latency = report.latency.percentiles;

    assert!(
        ms(latency.p95_us) > 300.0,
        "p95 was {} ms",
        ms(latency.p95_us)
    );
    assert!(
        ms(latency.p99_us) > 800.0,
        "p99 was {} ms",
        ms(latency.p99_us)
    );
    assert!(report.saturated, "falling a second behind must be flagged");
    assert!(ms(report.send_lag.p99_us) > 800.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn error_statuses_count_towards_the_error_rate() {
    let server = DemoServer::start_with(Arc::new(|_| Reply {
        delay: Duration::ZERO,
        status: StatusCode::INTERNAL_SERVER_ERROR,
    }))
    .await
    .unwrap();
    let report = run(&server, config(50.0, Duration::from_millis(500), 10)).await;

    assert_eq!(report.requests.failed, report.requests.completed);
    assert!((report.error_rate - 1.0).abs() < f64::EPSILON);
    assert_eq!(report.status_codes[&500], report.requests.completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_responses_time_out() {
    let server = DemoServer::start_with(Arc::new(|_| Reply {
        delay: Duration::from_secs(10),
        status: StatusCode::OK,
    }))
    .await
    .unwrap();
    let mut cfg = config(20.0, Duration::from_millis(500), 100);
    cfg.timeout = Duration::from_millis(200);
    let report = run(&server, cfg).await;

    assert_eq!(report.errors[&ErrorKind::Timeout], 10);
    let max = ms(report.latency.percentiles.max_us);
    assert!((200.0..400.0).contains(&max), "max was {max} ms");
}
