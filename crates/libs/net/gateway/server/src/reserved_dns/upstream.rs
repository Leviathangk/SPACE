use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use futures_util::future::BoxFuture;
use futures_util::stream::{self, BoxStream};
use futures_util::{StreamExt, TryStreamExt};
use hickory_resolver::config::ConnectionConfig;
use hickory_resolver::net::NetError;
use hickory_resolver::net::xfer::{DnsExchange, DnsHandle};
use hickory_resolver::proto::op::{DnsRequest, DnsResponse};
use hickory_resolver::{ConnectionProvider, PoolContext, TlsConfig};
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::{GatewayRuntimeProvider, MAX_CONCURRENT_LOOKUPS};

/// Log before Hickory's server pool can replace I/O or Busy with NoConnections,
/// and reconnect connections the upstream closed while they sat idle.
#[derive(Clone)]
pub(super) struct LoggingConnectionProvider(pub(super) GatewayRuntimeProvider);

impl ConnectionProvider for LoggingConnectionProvider {
    type Conn = LoggingDnsHandle;
    type FutureConn = BoxFuture<'static, Result<Self::Conn, NetError>>;
    type RuntimeProvider = GatewayRuntimeProvider;

    fn new_connection(
        &self,
        ip: IpAddr,
        config: &ConnectionConfig,
        cx: &PoolContext,
    ) -> Result<Self::FutureConn, NetError> {
        // Hickory lends its pool context only for this call. Opportunistic
        // encryption is off, so options and TLS are all a reconnect needs.
        let connector = Connector {
            runtime: self.0.clone(),
            upstream_addr: SocketAddr::new(ip, config.port),
            config: config.clone(),
            cx: PoolContext::new(
                cx.options.clone(),
                TlsConfig {
                    config: cx.tls.clone(),
                },
            ),
        };
        let connection = connector.connect()?;
        Ok(Box::pin(async move {
            let exchange = connection.await?;
            Ok(LoggingDnsHandle(Arc::new(Upstream {
                exchange: Mutex::new(Arc::new(exchange)),
                connector,
            })))
        }))
    }

    fn runtime_provider(&self) -> &Self::RuntimeProvider {
        &self.0
    }
}

struct Connector {
    runtime: GatewayRuntimeProvider,
    upstream_addr: SocketAddr,
    config: ConnectionConfig,
    cx: PoolContext,
}

impl Connector {
    fn connect(
        &self,
    ) -> Result<BoxFuture<'static, Result<DnsExchange<GatewayRuntimeProvider>, NetError>>, NetError>
    {
        let upstream_addr = self.upstream_addr;
        let log_error = move |error: &NetError| {
            tracing::warn!(%upstream_addr, ?error, "reserved DNS upstream connection failed");
        };
        let connection = self
            .runtime
            .new_connection(upstream_addr.ip(), &self.config, &self.cx)
            .inspect_err(log_error)?;
        Ok(Box::pin(
            async move { connection.await.inspect_err(log_error) },
        ))
    }
}

/// One pooled Hickory connection whose exchange can be replaced in place, so
/// every clone Hickory holds sees the replacement.
#[derive(Clone)]
pub(super) struct LoggingDnsHandle(Arc<Upstream>);

struct Upstream {
    exchange: Mutex<Arc<DnsExchange<GatewayRuntimeProvider>>>,
    connector: Connector,
}

impl Upstream {
    async fn reconnect(
        &self,
        closed: &Arc<DnsExchange<GatewayRuntimeProvider>>,
    ) -> Result<Arc<DnsExchange<GatewayRuntimeProvider>>, NetError> {
        let exchange = Arc::new(self.connector.connect()?.await?);
        let mut current = self.exchange.lock();
        // A concurrent send may have replaced it already; this request still
        // uses its own exchange, which closes once the response is dropped.
        if Arc::ptr_eq(&current, closed) {
            *current = exchange.clone();
        }
        Ok(exchange)
    }
}

impl DnsHandle for LoggingDnsHandle {
    type Response = BoxStream<'static, Result<DnsResponse, NetError>>;
    type Runtime = GatewayRuntimeProvider;

    fn send(&self, request: DnsRequest) -> Self::Response {
        let upstream_addr = self.0.connector.upstream_addr;
        let query_type = request.queries.first().map(|query| query.query_type);
        let lookup_slots = self.0.connector.runtime.lookup_slots.clone();
        let exchange = self.0.exchange.lock().clone();
        let responses = match send_unless_closed(&exchange, request.clone()) {
            Some(responses) => responses,
            None => {
                // Upstreams close idle DNS-over-TLS connections routinely.
                // Hickory would treat the Busy as backpressure, back off, and
                // report NoConnections after five closed pooled connections.
                tracing::debug!(
                    %upstream_addr,
                    ?query_type,
                    "reserved DNS upstream connection closed; reconnecting"
                );
                let upstream = self.0.clone();
                stream::once(async move {
                    match upstream.reconnect(&exchange).await {
                        Ok(exchange) => exchange.send(request).boxed(),
                        Err(error) => stream::iter([Err(error)]).boxed(),
                    }
                })
                .flatten()
                .boxed()
            }
        };
        responses
            .inspect_err(move |error| {
                if matches!(error, NetError::Busy) {
                    let space_lookups_in_flight = record_busy(&lookup_slots);
                    tracing::warn!(
                        %upstream_addr,
                        ?query_type,
                        ?error,
                        space_lookups_in_flight,
                        "reserved DNS upstream request failed"
                    );
                    return;
                }
                tracing::warn!(
                    %upstream_addr,
                    ?query_type,
                    ?error,
                    "reserved DNS upstream request failed"
                );
            })
            .boxed()
    }
}

/// Returns `None` when the exchange's background task has exited.
///
/// Each send enqueues through a fresh sender clone, which a bounded futures
/// channel never reports as full, so a Busy that is ready before the background
/// task runs means only that the queue's receiver is gone. Busy from the
/// multiplexer's request limit arrives later and still reaches Hickory.
fn send_unless_closed(
    exchange: &DnsExchange<GatewayRuntimeProvider>,
    request: DnsRequest,
) -> Option<BoxStream<'static, Result<DnsResponse, NetError>>> {
    let mut responses = exchange.send(request).peekable();
    let closed = matches!(
        Pin::new(&mut responses).poll_peek(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Some(Err(NetError::Busy)))
    );
    (!closed).then(|| responses.boxed())
}

fn record_busy(lookup_slots: &Semaphore) -> usize {
    let active = MAX_CONCURRENT_LOOKUPS - lookup_slots.available_permits();
    pplx_space_metrics::gateway::reserved_dns_upstream_busy_lookup_in_flight(active);
    active
}
