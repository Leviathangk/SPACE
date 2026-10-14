//! Shared request forwarding and task ownership for plaintext and inspected HTTP.
//!
//! `forward` is the one pipeline both entries run: target, policy, mode
//! admission, origin readiness, final re-check, submission. `mode` owns every
//! difference between plaintext and inspected HTTP; `binding` owns the
//! hostname-to-destination policy question both modes ask.

use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{Request, Response, header};
use http_body_util::{Either, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::executor::Executor;
use super::http_protocol::HttpProtocol;
use super::routing_headers::SanitizedRequest;
use super::upstream::{self, Upstream};
use crate::certificates::CertificateLease;
pub(super) use binding::{BindingOutcome, HostBinding};
use body::ForwardingBody;
pub(super) use error::{HttpError, http_reason};
use mode::Mode;

/// Events moved into child modules keep this module's target.
const LOG_TARGET: &str = module_path!();

mod binding;
mod body;
mod connect;
mod credentials;
mod error;
mod forward;
mod mode;
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

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("HTTP origin setup failed: {0}")]
    Setup(#[source] Arc<upstream::Error>),
    #[error("guest HTTP failed")]
    GuestHttp(#[source] hyper::Error),
}

impl Failure {
    pub(super) fn reason(&self) -> &'static str {
        match self {
            Self::Setup(error) if matches!(error.as_ref(), upstream::Error::Connect(_)) => {
                "connect"
            }
            Self::Setup(_) => "plaintext_upstream_http",
            Self::GuestHttp(_) => "plaintext_guest_http",
        }
    }
}

/// Per-flow request tally for plaintext HTTP.
#[derive(Default)]
pub(super) struct Served {
    pub requests: u32,
    pub forwarded: u32,
    pub refused: u32,
    pub upstream_failed: u32,
    pub wire_tx: u64,
    pub wire_rx: u64,
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
                error = self.mode.invalidated() => Err(error),
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
            error = self.mode.invalidated() => { result = Err(error); },
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
) -> Result<Served, Failure>
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
        return Err(Failure::Setup(error));
    }
    if let Err(HttpError::Protocol(error)) = result
        && error.is_parse()
    {
        return Err(Failure::GuestHttp(error));
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
            Err(_) => Err(BindingOutcome::InvalidHeaders.into()),
        };
        self.mode.tally(&self.served, &result);
        Ok(match result {
            Ok(response) => response,
            Err(error) => {
                self.mode.log_failure(&self.binding, &error);
                let reason = self.mode.reason(&error);
                let status = error.status();
                self.mode.record(protocol, reason, status);
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
        let attempt = self
            .mode
            .attempt(|| super::accounting::Attempt::forwarding(protocol.as_str(), operation));
        ForwardingBody::new(body, attempt)
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
