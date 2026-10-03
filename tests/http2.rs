//! HTTP version selection, checked against a server that records the
//! version of every request it receives.

use std::convert::Infallible;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::{Response, Version};
use http_body_util::Full;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use p99gate::engine::{Engine, LoadConfig};
use p99gate::http::{HttpExecutor, HttpOptions, HttpVersion, RequestSpec};
use p99gate::report::RunReport;
use p99gate::schedule::Rate;
use tokio::net::TcpListener;

/// Starts a server speaking HTTP/1.1 and h2c, returning its URL and the
/// versions of the requests it has served.
async fn recording_server() -> (String, Arc<Mutex<Vec<Version>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let versions = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&versions);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let service = service_fn(move |request: http::Request<_>| {
                    log.lock().unwrap().push(request.version());
                    async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok")))) }
                });
                let _ = auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (url, versions)
}

async fn run(url: &str, version: HttpVersion) -> RunReport {
    let config = LoadConfig {
        rate: Rate::per_second(100.0).unwrap(),
        duration: Duration::from_millis(500),
        concurrency: NonZeroUsize::new(20).unwrap(),
        timeout: Duration::from_secs(5),
    };
    let mut options = HttpOptions::new(config.concurrency.get());
    options.version = version;
    let executor = HttpExecutor::new(RequestSpec::get(url.parse().unwrap()), &options).unwrap();
    let target = executor.target();
    RunReport::new(&Engine::new(config, executor).run().await, target)
}

#[tokio::test(flavor = "multi_thread")]
async fn http1_is_the_default() {
    let (url, versions) = recording_server().await;
    let report = run(&url, HttpVersion::default()).await;

    assert_eq!(report.target.http_version, "1.1");
    assert_eq!(report.requests.succeeded, 50);
    assert!(
        versions
            .lock()
            .unwrap()
            .iter()
            .all(|v| *v == Version::HTTP_11)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn http2_uses_prior_knowledge_for_plain_http() {
    let (url, versions) = recording_server().await;
    let report = run(&url, HttpVersion::Http2).await;

    assert_eq!(report.target.http_version, "2");
    assert_eq!(report.requests.succeeded, 50, "errors: {:?}", report.errors);
    let versions = versions.lock().unwrap();
    assert_eq!(versions.len(), 50);
    assert!(versions.iter().all(|v| *v == Version::HTTP_2));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_demo_server_speaks_http2() {
    let server = p99gate::demo::DemoServer::start().await.unwrap();
    let report = run(&server.url(), HttpVersion::Http2).await;
    assert_eq!(report.requests.completed, 50);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
}
