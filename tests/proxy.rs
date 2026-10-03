//! Requests through HTTP proxies: forward proxying for http:// targets and
//! CONNECT tunnels for https:// targets.
//!
//! The targets use the reserved `.invalid` TLD, so a request can only
//! succeed by going through the test proxy.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::Response;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use p99gate::executor::Executor;
use p99gate::http::{HttpExecutor, HttpOptions, Proxy, RequestSpec};
use p99gate::outcome::Outcome;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// "user:secret" in base64.
const BASIC_AUTH: &str = "Basic dXNlcjpzZWNyZXQ=";

/// What a test proxy saw: the request target line and Proxy-Authorization.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A forward proxy that answers every request itself with 200.
async fn forward_proxy() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let service = service_fn(move |request: http::Request<_>| {
                    let auth = request
                        .headers()
                        .get(http::header::PROXY_AUTHORIZATION)
                        .map(|v| v.to_str().unwrap().to_owned());
                    log.lock().unwrap().push((request.uri().to_string(), auth));
                    async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok")))) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (format!("http://user:secret@{addr}"), seen)
}

/// A proxy that accepts one CONNECT request, records it, then closes.
async fn connect_proxy() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let line = head.lines().next().unwrap().to_owned();
        let auth = head.lines().find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("proxy-authorization")
                .then(|| value.trim().to_owned())
        });
        log.lock().unwrap().push((line, auth));
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        // Closing here makes the TLS handshake inside the tunnel fail, which
        // is fine: the test only checks that the tunnel was requested.
    });
    (format!("http://user:secret@{addr}"), seen)
}

fn executor(target: &str, proxy: &str) -> HttpExecutor {
    let mut options = HttpOptions::new(4);
    options.proxy = Proxy::Url(proxy.parse().unwrap());
    HttpExecutor::new(RequestSpec::get(target.parse().unwrap()), &options).unwrap()
}

#[tokio::test]
async fn http_targets_are_sent_to_the_proxy_in_absolute_form_with_credentials() {
    let (proxy, seen) = forward_proxy().await;
    let executor = executor("http://target.invalid/items?page=2", &proxy);

    assert_eq!(executor.execute().await, Outcome::Response { status: 200 });
    assert_eq!(executor.execute().await, Outcome::Response { status: 200 });

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "http://target.invalid/items?page=2");
    assert_eq!(seen[0].1.as_deref(), Some(BASIC_AUTH));
}

#[tokio::test]
async fn https_targets_are_tunnelled_through_connect_with_credentials() {
    let (proxy, seen) = connect_proxy().await;
    let executor = executor("https://target.invalid/", &proxy);

    let outcome = executor.execute().await;
    assert!(matches!(outcome, Outcome::Error(_)), "got {outcome:?}");

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "CONNECT target.invalid:443 HTTP/1.1");
    assert_eq!(seen[0].1.as_deref(), Some(BASIC_AUTH));
}

#[tokio::test]
async fn the_report_names_the_proxy_without_credentials() {
    let (proxy, _) = forward_proxy().await;
    let target = executor("http://target.invalid/", &proxy).target();
    let proxy = target.proxy.unwrap();
    assert!(proxy.starts_with("http://127.0.0.1:"), "{proxy}");
    assert!(!proxy.contains("secret"), "{proxy}");
}
