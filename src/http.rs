//! HTTP/1.1 support, over plain TCP or TLS.

use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

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

    /// Describes this request for a [`RunReport`](crate::report::RunReport).
    #[must_use]
    pub fn target(&self) -> Target {
        Target {
            protocol: "http".to_owned(),
            method: self.method.to_string(),
            url: self.uri.to_string(),
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

/// Errors creating an [`HttpExecutor`].
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL is not an absolute `http` or `https` URL.
    #[error("unsupported URL `{0}`: expected an absolute http:// or https:// URL")]
    UnsupportedUrl(Uri),
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
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    spec: RequestSpec,
}

impl HttpExecutor {
    /// Creates an executor keeping up to `max_connections` idle connections,
    /// which should match the engine's concurrency.
    pub fn new(spec: RequestSpec, max_connections: usize) -> Result<Self, HttpError> {
        if !matches!(spec.uri.scheme_str(), Some("http" | "https")) || spec.uri.host().is_none() {
            return Err(HttpError::UnsupportedUrl(spec.uri));
        }

        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        // Without this, Nagle's algorithm can add tens of milliseconds to
        // small requests, which would be measured as server latency.
        tcp.set_nodelay(true);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(HttpError::RootCertificates)?
            .https_or_http()
            .enable_http1()
            .wrap_connector(tcp);

        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(max_connections)
            .pool_idle_timeout(Duration::from_secs(90))
            .build(connector);
        Ok(Self { client, spec })
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
                    HttpExecutor::new(spec, 1),
                    Err(HttpError::UnsupportedUrl(_))
                ),
                "{url} should be rejected"
            );
        }
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
        let executor = HttpExecutor::new(RequestSpec::get(uri), 1).unwrap();
        assert_eq!(executor.execute().await, Outcome::Error(ErrorKind::Connect));
    }
}
