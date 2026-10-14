//! HTTP/1.1 relay for TCP flows admitted by a DNS-domain allow.
//!
//! A CDN address is not a hostname identity: one edge IP can route requests for
//! many unrelated tenants. Hyper owns framing here so every request on a
//! keep-alive connection is checked before it reaches the already-admitted IP.
//! The upstream socket is created only after the first request passes that
//! check, so a rejected authority cannot produce network egress.

use std::convert::Infallible;
use std::future::Future;
#[cfg(test)]
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use http::{Request, Response, StatusCode, header, uri::Authority};
use http_body_util::{Either, Empty};
use hyper::body::Incoming;
#[cfg(test)]
use hyper::client::conn::http1 as client;
use hyper::server::conn::http1 as server;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use pplx_space_net_common::DomainName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Mutex;

use super::routing_headers::SanitizedRequest;
use super::{
    tls_http::{HttpProtocol, RequestError},
    upstream::{self, Upstream},
};
use crate::dns::DnsAllowCache;
use crate::egress::{HostBindingError, authorize_host};
use crate::state::{GatewayNetworkState, TrafficPolicy};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct HostBinding {
    sandbox_id: &'static str,
    destination: SocketAddr,
    network_state: GatewayNetworkState,
    dns_allow_cache: DnsAllowCache,
}

impl HostBinding {
    pub(super) fn new(
        sandbox_id: &'static str,
        destination: SocketAddr,
        network_state: GatewayNetworkState,
        dns_allow_cache: DnsAllowCache,
    ) -> Self {
        Self {
            sandbox_id,
            destination,
            network_state,
            dns_allow_cache,
        }
    }

    pub(super) async fn authorize(&self, host: &DomainName) -> Result<(), BindingOutcome> {
        // Established requests retain the bounded PendingPolicy grace.
        let policy = match self.network_state.wait_for_tcp_policy().await {
            TrafficPolicy::Ready(policy) => policy,
            TrafficPolicy::NotReady(_) => return Err(BindingOutcome::PolicyUnavailable),
        };
        authorize_host(&policy, self.destination, host, &self.dns_allow_cache).map_err(|error| {
            match error {
                HostBindingError::HostNotAllowed => BindingOutcome::HostNotAllowed,
                HostBindingError::HostUnattributed => BindingOutcome::HostUnattributed,
                HostBindingError::PolicyDenied(_) => BindingOutcome::PolicyDenied,
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BindingOutcome {
    Allowed,
    InvalidHost,
    InvalidHeaders,
    HostNotAllowed,
    HostUnattributed,
    PolicyDenied,
    PolicyUnavailable,
    UpstreamFailed,
    UpgradeUnsupported,
}

impl BindingOutcome {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::InvalidHost => "invalid_host",
            Self::InvalidHeaders => "invalid_headers",
            Self::HostNotAllowed => "host_not_allowed",
            Self::HostUnattributed => "host_unattributed",
            Self::PolicyDenied => "policy_denied",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::UpstreamFailed => "upstream_failed",
            Self::UpgradeUnsupported => "upgrade_unsupported",
        }
    }

    pub(super) const fn refusal_status(self) -> StatusCode {
        match self {
            Self::InvalidHeaders => StatusCode::BAD_REQUEST,
            Self::PolicyUnavailable | Self::UpstreamFailed => StatusCode::BAD_GATEWAY,
            Self::Allowed
            | Self::InvalidHost
            | Self::HostNotAllowed
            | Self::HostUnattributed
            | Self::PolicyDenied
            | Self::UpgradeUnsupported => StatusCode::FORBIDDEN,
        }
    }
}

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

#[derive(Default)]
pub(super) struct Served {
    pub requests: u32,
    pub forwarded: u32,
    pub refused: u32,
    pub upstream_failed: u32,
    pub wire_tx: u64,
    pub wire_rx: u64,
    setup_failure: Option<Failure>,
}

type ForwardBody = Either<Incoming, Empty<hyper::body::Bytes>>;

pub(super) async fn serve<C, F, Fut, U>(
    guest: C,
    binding: HostBinding,
    connect: F,
) -> Result<Served, Failure>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnOnce(SocketAddr) -> Fut + Send + 'static,
    Fut: Future<Output = Result<U, super::transport::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let wire_tx = Arc::new(AtomicU64::new(0));
    let wire_rx = Arc::new(AtomicU64::new(0));
    let shutdown = CancellationToken::new();
    let _cancel = shutdown.clone().drop_guard();
    let destination = binding.destination;
    let upstream = Arc::new(Upstream::new(
        {
            let read = wire_rx.clone();
            let written = wire_tx.clone();
            move |_| async move {
                let inner = connect(destination).await?;
                Ok((
                    MeteredIo {
                        inner,
                        read,
                        written,
                    },
                    HttpProtocol::Http1,
                ))
            }
        },
        shutdown,
        CancellationToken::new(),
    ));
    let tally = Arc::new(Mutex::new(Served::default()));

    let serving = server::Builder::new().serve_connection(
        TokioIo::new(guest),
        service_fn(|request: Request<Incoming>| {
            let request = SanitizedRequest::try_from(request.map(Either::Left));
            let upstream = upstream.clone();
            let tally = tally.clone();
            let binding = binding.clone();
            async move {
                let result = match request {
                    Ok(request) => forward(request, upstream, &binding).await,
                    Err(_) => Err(BindingOutcome::InvalidHeaders.into()),
                };
                let (response, outcome, setup_failure) = match result {
                    Ok(response) => (response, BindingOutcome::Allowed, None),
                    Err(error) => {
                        let outcome = error.binding_outcome();
                        let response = error.response(HttpProtocol::Http1).map(Either::Right);
                        let setup_failure = if let RequestError::Setup(error) = error {
                            let (failure, error_kind) = match error.as_ref() {
                                upstream::Error::Connect(error) => ("connect", error.io_kind()),
                                upstream::Error::Http(_) => ("http_handshake", None),
                                upstream::Error::Tls(_) | upstream::Error::Cancelled => {
                                    ("setup", None)
                                }
                            };
                            tracing::debug!(
                                sandbox_id = binding.sandbox_id,
                                dst_port = binding.destination.port(),
                                address_family = if binding.destination.is_ipv6() {
                                    "ipv6"
                                } else {
                                    "ipv4"
                                },
                                failure,
                                ?error_kind,
                                "plaintext HTTP upstream connection failed"
                            );
                            Some(Failure::Setup(error))
                        } else {
                            None
                        };
                        (response, outcome, setup_failure)
                    }
                };
                pplx_space_metrics::gateway::domain_binding(
                    "tcp",
                    "http_host",
                    outcome.as_str(),
                    None,
                );
                let mut tally = tally.lock().await;
                if tally.setup_failure.is_none() {
                    tally.setup_failure = setup_failure;
                }
                tally.requests += 1;
                if outcome == BindingOutcome::Allowed {
                    tally.forwarded += 1;
                } else if outcome == BindingOutcome::UpstreamFailed {
                    tally.upstream_failed += 1;
                } else {
                    tally.refused += 1;
                }
                Ok::<_, Infallible>(response)
            }
        }),
    );
    let ended = serving.await;
    upstream.shutdown().await;
    let mut served = std::mem::take(&mut *tally.lock().await);
    served.wire_tx = wire_tx.load(Ordering::Relaxed);
    served.wire_rx = wire_rx.load(Ordering::Relaxed);
    // Setup can fail only on the first in-flight HTTP/1 request, whose refusal
    // carries Connection: close so Hyper flushes the 502 before this terminal
    // conversion discards its tally. Retrying setup would make that loss material.
    if let Some(failure) = served.setup_failure.take() {
        return Err(failure);
    }
    match ended {
        Ok(()) => Ok(served),
        Err(error) if error.is_parse() => Err(Failure::GuestHttp(error)),
        Err(_) => Ok(served),
    }
}

async fn forward<F, Fut, U>(
    mut request: SanitizedRequest<ForwardBody>,
    upstream: Arc<Upstream<F, ForwardBody>>,
    binding: &HostBinding,
) -> Result<Response<ForwardBody>, RequestError>
where
    F: FnOnce(HttpProtocol) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(U, HttpProtocol), upstream::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (host, authority) =
        validated_request_authority(&request).map_err(|()| BindingOutcome::InvalidHost)?;
    binding.authorize(&host).await?;
    // This relay cannot leave HTTP framing. Refuse before hop-header stripping
    // can turn an upgrade into an ordinary request to the origin.
    if request.headers().contains_key(header::UPGRADE) {
        return Err(RequestError::UnsupportedUpgrade);
    }
    normalize_upstream_routing(&mut request, &authority)
        .map_err(|()| BindingOutcome::InvalidHost)?;

    let sender = upstream.sender(HttpProtocol::Http1).await?;
    let sender = sender.ready().await?;
    let response = sender.send(request).await?;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        return Err(RequestError::UnexpectedUpgrade);
    }
    Ok(response.map(Either::Left))
}

/// Make the bytes forwarded upstream express the same authority that passed
/// policy. For absolute-form targets RFC 7230 requires a proxy to replace a
/// conflicting `Host`; leaving the guest's original value in place would let a
/// CDN route on unchecked metadata. Known provider-routing headers have already
/// been removed by the sanitized request boundary.
fn normalize_upstream_routing<B>(
    request: &mut SanitizedRequest<B>,
    authority: &Authority,
) -> Result<(), ()> {
    let value = authority.as_str().parse().map_err(|_| ())?;
    request.set_host(value);
    let origin_form = request
        .uri()
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or("/")
        .parse()
        .map_err(|_| ())?;
    *request.uri_mut() = origin_form;
    Ok(())
}

fn canonical_authority(host: &DomainName, port: Option<u16>) -> Result<Authority, ()> {
    match port {
        Some(port) => format!("{}:{port}", host.as_str()),
        None => host.as_str().to_string(),
    }
    .parse()
    .map_err(|_| ())
}

fn validated_request_authority<B>(
    request: &SanitizedRequest<B>,
) -> Result<(DomainName, Authority), ()> {
    let authority = request_authority(request)?;
    let host: DomainName = authority.host().parse().map_err(|_| ())?;
    let authority = canonical_authority(&host, authority.port_u16())?;
    Ok((host, authority))
}

/// Parse the effective HTTP request authority. Absolute-form request-target
/// authority outranks `Host`; accepting the header first would permit a
/// proxy-form request to name an allowed host in the header while targeting a
/// different service. Origin-form requests must carry exactly one `Host`, so
/// the value authorized here cannot differ from one a CDN chooses upstream.
fn request_authority<B>(request: &SanitizedRequest<B>) -> Result<Authority, ()> {
    if let Some(authority) = request.uri().authority() {
        return Ok(authority.clone());
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host = hosts.next().ok_or(())?;
    if hosts.next().is_some() {
        return Err(());
    }
    host.to_str()
        .ok()
        .and_then(|value| value.parse::<Authority>().ok())
        .ok_or(())
}

pub(super) struct MeteredIo<T> {
    pub(super) inner: T,
    pub(super) read: Arc<AtomicU64>,
    pub(super) written: Arc<AtomicU64>,
}

impl<T: AsyncRead + Unpin> AsyncRead for MeteredIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            let read = buffer.filled().len().saturating_sub(before) as u64;
            self.read.fetch_add(read, Ordering::Relaxed);
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for MeteredIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let result = Pin::new(&mut self.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(written)) = result {
            self.written.fetch_add(written as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests;
