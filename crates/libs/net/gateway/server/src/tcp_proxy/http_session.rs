//! Shared request forwarding and task ownership for plaintext and inspected HTTP.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response, StatusCode, Version, header};
use http_body_util::{Either, Empty};
use hyper::body::{Body as HttpBody, Bytes, Incoming};
#[cfg(test)]
use hyper::client::conn::{http1, http2};
use hyper::service::service_fn;
#[cfg(test)]
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::Instrument;

use super::accounting::{Attempt, Outcome};
use super::executor::Executor;
use super::http_host::{self, HostBinding, Served};
use super::routing_headers::SanitizedRequest;
#[cfg(test)]
use super::tls::TlsTransportError;
use super::upstream::{self, Upstream};
use crate::certificates::CertificateLease;
use crate::tls_sni::HttpAlpn;
pub(super) use error::HttpError;
use error::RequestError;
pub(super) use error::http_reason;

mod connect;
mod credentials;
mod error;
mod forward;
mod upgrade;

type Body = Either<ForwardingBody, Empty<Bytes>>;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADERS: usize = 128;
const MAX_HEADER_BYTES: u32 = 32 * 1024;
const MAX_STREAMS: u32 = 32;
// Guest-facing HTTP/2 receive windows. h2 (>= 0.4.16) closes the connection
// with GOAWAY ENHANCE_YOUR_CALM once DATA frames under 256 bytes, buffered but
// not yet read, exceed a budget. Each such frame costs `256 - len`, and the
// default budget is half the connection window. With hyper's defaults (1 MiB
// stream and connection windows, so a 512 KiB budget), one request streaming
// ~50-byte frames exhausts the budget after ~125 KiB sits unread, e.g. git's
// pkt-line bodies while the upstream applies backpressure.
//
// The budget follows only the connection window, while a stream's window caps
// what that stream can buffer. A 128 KiB stream window under a 4 MiB
// connection window (2 MiB budget) lets a few streams fill their whole window
// with 50-byte frames (4.12 budget bytes per payload byte). Floods of tinier
// frames still exhaust it. The guest link is local, so the smaller stream
// window doesn't limit upload throughput.
const GUEST_H2_STREAM_WINDOW: u32 = 128 * 1024;
const GUEST_H2_CONNECTION_WINDOW: u32 = 4 * 1024 * 1024;

struct ForwardingBody {
    inner: Incoming,
    attempt: Option<Attempt>,
}

impl ForwardingBody {
    fn new(
        inner: Incoming,
        protocol: &'static str,
        operation: &'static str,
        inspected: bool,
    ) -> Self {
        let mut attempt = inspected.then(|| Attempt::forwarding(protocol, operation));
        if inner.is_end_stream()
            && let Some(attempt) = &mut attempt
        {
            attempt.finish(Outcome::Success, "body_complete");
        }
        Self { inner, attempt }
    }
}

impl HttpBody for ForwardingBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(context);
        let finished = match &result {
            Poll::Ready(Some(Err(_))) => Some((Outcome::Error, "body_error")),
            Poll::Ready(None) => Some((Outcome::Success, "body_complete")),
            Poll::Ready(Some(Ok(_))) if self.inner.is_end_stream() => {
                Some((Outcome::Success, "body_complete"))
            }
            _ => None,
        };
        if let (Some(attempt), Some((outcome, reason))) = (&mut self.attempt, finished) {
            attempt.finish(outcome, reason);
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

enum Mode {
    Plaintext,
    Inspected(Arc<CertificateLease>),
}
impl Mode {
    async fn invalidated(&self) {
        match self {
            Self::Plaintext => std::future::pending().await,
            Self::Inspected(lease) => lease.invalidated().await,
        }
    }
    fn is_inspected(&self) -> bool {
        matches!(self, Self::Inspected(_))
    }
}

struct Session<F> {
    binding: HostBinding,
    mode: Mode,
    upstream: Upstream<F, Body>,
    executor: Executor,
    upgrades: TaskTracker,
    shutdown: CancellationToken,
    draining: CancellationToken,
    served: Mutex<Served>,
}

impl<F> Session<F> {
    fn new(binding: HostBinding, mode: Mode, connect: F, shutdown: CancellationToken) -> Arc<Self> {
        let draining = CancellationToken::new();
        Arc::new(Self {
            binding,
            mode,
            upstream: Upstream::new(connect, shutdown.clone(), draining.clone()),
            executor: Executor::new(shutdown.child_token()),
            upgrades: TaskTracker::new(),
            shutdown,
            draining,
            served: Mutex::default(),
        })
    }

    async fn run<G>(&self, serving: G) -> Result<(), HttpError>
    where
        G: Future<Output = Result<(), HttpError>>,
    {
        let mut result = {
            // Drop the protocol driver and its service before closing task trackers.
            let serving = serving;
            tokio::pin!(serving);
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => Err(HttpError::Cancelled),
                () = self.mode.invalidated() => Err(super::tls::TlsTransportError::Invalidated.into()),
                result = &mut serving => result,
            }
        };
        if result.is_err() {
            self.shutdown.cancel();
        }
        self.executor.shutdown().await;
        self.upgrades.close();
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => {},
            () = self.mode.invalidated() => { result = Err(super::tls::TlsTransportError::Invalidated.into()); },
            () = self.upgrades.wait() => {},
        }
        self.shutdown.cancel();
        self.upgrades.wait().await;
        self.upstream.shutdown().await;
        result
    }
}

/// HTTP/1 plaintext entry. The caller's connector targets the admitted address.
pub(super) async fn serve_plaintext<C, F, Fut, U>(
    guest: C,
    binding: HostBinding,
    connect: F,
    shutdown: CancellationToken,
) -> Result<Served, http_host::Failure>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnOnce(HttpProtocol) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(U, HttpProtocol), upstream::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let shutdown = shutdown.child_token();
    let _cancel = shutdown.clone().drop_guard();
    let session = Session::new(binding, Mode::Plaintext, connect, shutdown);
    let service = service_fn(|request| session.clone().request(request));
    let serving = async {
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(guest), service);
        tokio::pin!(connection);
        tokio::select! {
            result = &mut connection => result,
            () = session.draining.cancelled() => { connection.as_mut().graceful_shutdown(); connection.await }
        }.map_err(HttpError::from)
    };
    let result = session.run(serving).await;
    if let Some(error) = session.upstream.failure() {
        return Err(http_host::Failure::Setup(error));
    }
    if let Err(HttpError::Protocol(error)) = result
        && error.is_parse()
    {
        return Err(http_host::Failure::GuestHttp(error));
    }
    Ok(std::mem::take(
        &mut *session.served.lock().expect("HTTP tally lock poisoned"),
    ))
}

/// Detect HTTP framing inside an inspected TLS connection, including H1 CONNECT
/// from clients that offered H2 in ALPN. Authorization precedes origin setup.
pub(super) async fn serve_inspected<C, F, Fut, U>(
    guest: C,
    binding: HostBinding,
    lease: Arc<CertificateLease>,
    connect: F,
    shutdown: CancellationToken,
) -> Result<(), HttpError>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnOnce(HttpProtocol) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(U, HttpProtocol), upstream::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let shutdown = shutdown.child_token();
    let _cancel = shutdown.clone().drop_guard();
    let session = Session::new(binding, Mode::Inspected(lease), connect, shutdown);
    let service = service_fn(|request| session.clone().request(request));
    let builder = guest_http_builder(session.executor.clone());
    let serving = async {
        let connection = builder.serve_connection_with_upgrades(TokioIo::new(guest), service);
        tokio::pin!(connection);
        tokio::select! {
            result = &mut connection => result,
            () = session.draining.cancelled() => { connection.as_mut().graceful_shutdown(); connection.await }
        }.map_err(HttpError::from)
    };
    session.run(serving).await
}

impl<F, Fut, U> Session<F>
where
    F: FnOnce(HttpProtocol) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(U, HttpProtocol), upstream::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn request(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<Body>, Infallible> {
        let protocol = HttpProtocol::of(request.version());
        tracing::Span::current().record("protocol", protocol.as_str());
        let result = match SanitizedRequest::try_from(request) {
            Ok(request) => self.forward(request).await,
            Err(_) => Err(http_host::BindingOutcome::InvalidHeaders.into()),
        };
        if !self.mode.is_inspected() {
            let mut served = self.served.lock().expect("HTTP tally lock poisoned");
            served.requests += 1;
            match &result {
                Ok(_) => served.forwarded += 1,
                Err(error) if error.is_upstream() => served.upstream_failed += 1,
                Err(_) => served.refused += 1,
            }
        }
        Ok(match result {
            Ok(response) => response,
            Err(error) => {
                match &error {
                    RequestError::Setup(error) if !self.mode.is_inspected() => {
                        self.binding.record_setup_failure(error)
                    }
                    RequestError::Setup(error) => error.record("upstream_setup"),
                    RequestError::Upstream(error) => tracing::info!(
                        http_reason = http_reason(error),
                        "HTTP request forwarding failed"
                    ),
                    _ => {}
                }
                let reason = error.reason(self.mode.is_inspected());
                let status = error.status();
                self.record(protocol, reason, status);
                let mut response = Response::new(Either::Right(Empty::new()));
                *response.status_mut() = status;
                if protocol == HttpProtocol::Http1 {
                    response
                        .headers_mut()
                        .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
                }
                response
            }
        })
    }

    fn forwarding_body(
        &self,
        protocol: HttpProtocol,
        body: Incoming,
        operation: &'static str,
    ) -> ForwardingBody {
        ForwardingBody::new(body, protocol.as_str(), operation, self.mode.is_inspected())
    }

    fn record(&self, protocol: HttpProtocol, outcome: &'static str, status: StatusCode) {
        let binding = if self.mode.is_inspected() {
            "tls_http"
        } else {
            "http_host"
        };
        pplx_space_metrics::gateway::domain_binding("tcp", binding, outcome, None);
        tracing::debug!(
            protocol = protocol.as_str(),
            outcome,
            status = status.as_u16(),
            "HTTP request forwarding result"
        );
        if self.mode.is_inspected()
            && !matches!(outcome, "allowed" | "websocket" | "connect_forwarded")
        {
            tracing::info!(
                protocol = protocol.as_str(),
                outcome,
                status = status.as_u16(),
                "inspected HTTP request refused"
            );
        }
    }
}

#[cfg(test)]
mod tests;

fn guest_http_builder(executor: Executor) -> auto::Builder<Executor> {
    let mut builder = auto::Builder::new(executor);
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .max_headers(MAX_HEADERS)
        .max_buf_size(MAX_HEADER_BYTES as usize);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(MAX_STREAMS)
        .max_header_list_size(MAX_HEADER_BYTES)
        .max_send_buf_size(64 * 1024)
        .initial_stream_window_size(GUEST_H2_STREAM_WINDOW)
        .initial_connection_window_size(GUEST_H2_CONNECTION_WINDOW)
        .keep_alive_interval(Some(H2_KEEP_ALIVE_INTERVAL))
        .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT);
    builder
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HttpProtocol {
    Http1,
    Http2,
}

impl HttpProtocol {
    /// The protocol hyper actually parsed a request with. This, not the ALPN
    /// result, decides how the request is forwarded.
    pub(super) fn of(version: http::Version) -> Self {
        if version == http::Version::HTTP_2 {
            Self::Http2
        } else {
            Self::Http1
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Http1 => "http1",
            Self::Http2 => "http2",
        }
    }
}

impl From<HttpAlpn> for HttpProtocol {
    fn from(protocol: HttpAlpn) -> Self {
        match protocol {
            HttpAlpn::Http1 => Self::Http1,
            HttpAlpn::Http2 => Self::Http2,
        }
    }
}
