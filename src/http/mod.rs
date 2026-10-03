//! HTTP/1.1 support, over plain TCP or TLS, directly or through a proxy.

mod connector;

use std::time::Duration;

use bytes::Bytes;
use http::header::PROXY_AUTHORIZATION;
use http::{HeaderMap, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::TokioExecutor;

use self::connector::{Connector, Route};
use crate::executor::Executor;
use crate::outcome::{ErrorKind, Outcome};
use crate::report::Target;

/// The request to send, repeatedly.
#[derive(Debug, Clone)]
pub struct RequestSpec {
    /// HTTP method.
    pub method: Method,
    /// Absolute `http` or `https` URL.
    pub uri: Uri,
    /// Extra headers. `Host` is derived from `uri` unless set here.
    pub headers: HeaderMap,
    /// Request body, empty for none.
    pub body: Bytes,
}

impl RequestSpec {
    /// A `GET` request with no extra headers or body.
    #[must_use]
    pub fn get(uri: Uri) -> Self {
        Self {
            method: Method::GET,
            uri,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    fn build(&self) -> Request<Full<Bytes>> {
        let mut request = Request::new(Full::new(self.body.clone()));
        *request.method_mut() = self.method.clone();
        *request.uri_mut() = self.uri.clone();
        *request.headers_mut() = self.headers.clone();
        request
    }
}

/// Whether and how to use an HTTP proxy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// Connect to the target directly.
    #[default]
    None,
    /// Use the proxy named by `HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY`
    /// (or their lowercase forms) unless `NO_PROXY` excludes the target,
    /// the same way curl does.
    FromEnv,
    /// Send every request through this `http://` proxy. Credentials in the
    /// URL (`http://user:pass@proxy:3128`) are sent as basic auth.
    Url(Uri),
}

/// Options for [`HttpExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOptions {
    /// Idle connections to keep open; should match the engine's concurrency.
    pub max_connections: usize,
    /// Proxy configuration.
    pub proxy: Proxy,
}

impl HttpOptions {
    /// Direct connections, keeping up to `max_connections` open.
    #[must_use]
    pub fn new(max_connections: usize) -> Self {
        Self {
            max_connections,
            proxy: Proxy::None,
        }
    }
}

/// Errors creating an [`HttpExecutor`].
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL is not an absolute `http` or `https` URL.
    #[error("unsupported URL `{0}`: expected an absolute http:// or https:// URL")]
    UnsupportedUrl(Uri),
    /// The proxy URL is not one that can be used.
    #[error("unsupported proxy `{0}`: expected an http:// proxy URL")]
    UnsupportedProxy(String),
    /// The platform's trusted root certificates could not be loaded.
    #[error("could not load the system's root certificates for TLS")]
    RootCertificates(#[source] std::io::Error),
}

/// Sends a [`RequestSpec`] over pooled keep-alive connections.
///
/// Every response body is read to the end, so measured latency covers the
/// full response and connections are returned to the pool for reuse.
#[derive(Debug, Clone)]
pub struct HttpExecutor {
    client: Client<HttpsConnector<Connector>, Full<Bytes>>,
    spec: RequestSpec,
    /// The proxy in use, without credentials, for reporting.
    proxy: Option<String>,
}

impl HttpExecutor {
    /// Creates an executor for `spec`.
    pub fn new(mut spec: RequestSpec, options: &HttpOptions) -> Result<Self, HttpError> {
        if !matches!(spec.uri.scheme_str(), Some("http" | "https")) || spec.uri.host().is_none() {
            return Err(HttpError::UnsupportedUrl(spec.uri));
        }

        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        // Without this, Nagle's algorithm can add tens of milliseconds to
        // small requests, which would be measured as server latency.
        tcp.set_nodelay(true);

        let matcher = match &options.proxy {
            Proxy::None => None,
            Proxy::FromEnv => Some(Matcher::from_env()),
            Proxy::Url(url) => Some(Matcher::builder().all(url.to_string()).build()),
        };
        let (route, proxy) = match matcher.and_then(|m| m.intercept(&spec.uri)) {
            None => (Route::Direct, None),
            Some(intercept) => {
                let proxy = intercept.uri().clone();
                let description = describe_proxy(&proxy)?;
                let auth = intercept.basic_auth().cloned();
                let route = if spec.uri.scheme_str() == Some("https") {
                    let tunnel = Tunnel::new(proxy, tcp.clone());
                    Route::Tunnel(match auth {
                        Some(auth) => tunnel.with_auth(auth),
                        None => tunnel,
                    })
                } else {
                    if let Some(auth) = auth {
                        spec.headers.entry(PROXY_AUTHORIZATION).or_insert(auth);
                    }
                    Route::Forward { proxy }
                };
                (route, Some(description))
            }
        };

        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(HttpError::RootCertificates)?
            .https_or_http()
            .enable_http1()
            .wrap_connector(Connector::new(tcp, route));

        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(options.max_connections)
            .pool_idle_timeout(Duration::from_secs(90))
            .build(connector);
        Ok(Self {
            client,
            spec,
            proxy,
        })
    }

    /// Describes what this executor sends to, for a
    /// [`RunReport`](crate::report::RunReport).
    #[must_use]
    pub fn target(&self) -> Target {
        Target {
            protocol: "http".to_owned(),
            method: self.spec.method.to_string(),
            url: self.spec.uri.to_string(),
            proxy: self.proxy.clone(),
        }
    }
}

/// Validates a proxy URL and renders it without credentials.
fn describe_proxy(proxy: &Uri) -> Result<String, HttpError> {
    match (proxy.scheme_str(), proxy.host()) {
        (Some("http"), Some(host)) => Ok(match proxy.port_u16() {
            Some(port) => format!("http://{host}:{port}"),
            None => format!("http://{host}"),
        }),
        // Never echo the URL back as given: it may contain credentials.
        _ => Err(HttpError::UnsupportedProxy(format!(
            "{}://{}",
            proxy.scheme_str().unwrap_or("?"),
            proxy.host().unwrap_or("?"),
        ))),
    }
}

impl Executor for HttpExecutor {
    async fn execute(&self) -> Outcome {
        let response = match self.client.request(self.spec.build()).await {
            Ok(response) => response,
            Err(e) if e.is_connect() => return Outcome::Error(ErrorKind::Connect),
            Err(_) => return Outcome::Error(ErrorKind::Io),
        };
        let status = response.status().as_u16();
        match response.into_body().collect().await {
            Ok(_) => Outcome::Response { status },
            Err(_) => Outcome::Error(ErrorKind::Io),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_urls_it_cannot_send_to() {
        for url in ["/relative", "ftp://example.com/", "example.com"] {
            let spec = RequestSpec::get(url.parse().unwrap());
            assert!(
                matches!(
                    HttpExecutor::new(spec, &HttpOptions::new(1)),
                    Err(HttpError::UnsupportedUrl(_))
                ),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn proxies_must_be_http_and_are_reported_without_credentials() {
        let describe = |s: &str| describe_proxy(&s.parse().unwrap());
        assert_eq!(describe("http://proxy:3128").unwrap(), "http://proxy:3128");
        assert_eq!(
            describe("http://user:secret@proxy").unwrap(),
            "http://proxy"
        );
        let err = describe("socks5://user:secret@proxy:1080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("socks5://proxy"), "{err}");
        assert!(!err.contains("secret"), "{err}");
    }

    #[tokio::test]
    async fn connection_refused_is_a_connect_error() {
        // Bind then drop a listener to get a port that is very likely closed.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let uri = format!("http://127.0.0.1:{port}/").parse().unwrap();
        let executor = HttpExecutor::new(RequestSpec::get(uri), &HttpOptions::new(1)).unwrap();
        assert_eq!(executor.execute().await, Outcome::Error(ErrorKind::Connect));
    }
}
