//! Opens TCP connections to the target, directly or through an HTTP proxy.
//!
//! A run has a single target, so the route is decided once, when the
//! executor is created:
//!
//! - **Direct**: connect to the target.
//! - **Forward**: plain `http://` targets through a proxy. The connection goes
//!   to the proxy and is marked as proxied, so hyper sends absolute-form
//!   request URIs (`GET http://host/path`) as the proxy expects.
//! - **Tunnel**: `https://` targets through a proxy. An HTTP `CONNECT` tunnel
//!   is opened through the proxy, and TLS runs end to end inside it.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::Uri;
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::legacy::connect::{Connected, Connection, HttpConnector};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tower_service::Service;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// How connections reach the target.
#[derive(Debug, Clone)]
pub(super) enum Route {
    Direct,
    Forward { proxy: Uri },
    Tunnel(Tunnel<HttpConnector>),
}

/// A connector that follows a fixed [`Route`].
#[derive(Debug, Clone)]
pub(super) struct Connector {
    tcp: HttpConnector,
    route: Route,
}

impl Connector {
    pub(super) fn new(tcp: HttpConnector, route: Route) -> Self {
        Self { tcp, route }
    }
}

impl Service<Uri> for Connector {
    type Response = Stream;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Stream, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &mut self.route {
            Route::Direct | Route::Forward { .. } => self.tcp.poll_ready(cx).map_err(Into::into),
            Route::Tunnel(tunnel) => tunnel.poll_ready(cx).map_err(Into::into),
        }
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        match &mut self.route {
            Route::Direct => {
                let connecting = self.tcp.call(dst);
                Box::pin(async move { Ok(Stream::new(connecting.await?, false)) })
            }
            Route::Forward { proxy } => {
                let connecting = self.tcp.call(proxy.clone());
                Box::pin(async move { Ok(Stream::new(connecting.await?, true)) })
            }
            Route::Tunnel(tunnel) => {
                let connecting = tunnel.call(dst);
                // Inside a tunnel the connection talks to the target itself,
                // so requests use the normal origin form.
                Box::pin(async move { Ok(Stream::new(connecting.await?, false)) })
            }
        }
    }
}

/// A TCP connection that remembers whether it leads to a forward proxy.
#[derive(Debug)]
pub(super) struct Stream {
    io: TokioIo<TcpStream>,
    proxied: bool,
}

impl Stream {
    fn new(io: TokioIo<TcpStream>, proxied: bool) -> Self {
        Self { io, proxied }
    }
}

impl Connection for Stream {
    fn connected(&self) -> Connected {
        self.io.connected().proxy(self.proxied)
    }
}

impl Read for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl Write for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }
}
