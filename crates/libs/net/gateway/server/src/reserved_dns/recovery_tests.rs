use futures_util::future::join_all;
use hickory_resolver::config::{ConnectionConfig, NameServerConfig};
use hickory_resolver::net::xfer::{DnsHandle, FirstAnswer};
use hickory_resolver::proto::op::{DnsRequest, DnsRequestOptions};
use hickory_resolver::proto::rr::rdata::A;
use hickory_resolver::{ConnectionProvider, PoolContext, TlsConfig};
use pplx_space_observability::test_capture::{CapturedEvents, CapturedEventsLayer, field_value};
use rustls::pki_types::PrivateKeyDer;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tracing_subscriber::prelude::*;

use super::upstream::LoggingConnectionProvider;
use super::*;

// TCP and TLS share Hickory's exchange queue and multiplexer. Five closed
// pooled connections exhaust Hickory's Busy backoff unless each reconnects.
#[tokio::test]
async fn idle_closed_connections_reconnect_without_resolver_replacement() {
    let layer = CapturedEventsLayer::new();
    let events = layer.events();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut server = NameServerConfig::tcp(listener.local_addr().unwrap().ip());
    server.connections[0].port = listener.local_addr().unwrap().port();
    let mut options = ResolverOpts::default();
    options.attempts = 1;
    options.max_active_requests = MAX_CONCURRENT_LOOKUPS;
    options.use_hosts_file = ResolveHosts::Never;
    let resolver = RecoveringResolver::new(
        ResolverConfig::from_parts(None, Vec::new(), vec![server]),
        options,
        GatewayRuntimeProvider {
            handle: TokioHandle::default(),
            outbound_mark: None,
            lookup_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_LOOKUPS)),
        },
    )
    .unwrap();
    let close_idle = CancellationToken::new();
    let peer = tokio::spawn({
        let close_idle = close_idle.clone();
        async move {
            let mut replies = JoinSet::new();
            for _ in 0..5 {
                replies.spawn(answer(listener.accept().await.unwrap().0));
            }
            let mut idle = Vec::new();
            while let Some(reply) = replies.join_next().await {
                idle.push(reply.unwrap());
            }
            close_idle.cancelled().await;
            drop(idle);
            answer(listener.accept().await.unwrap().0).await;
        }
    });

    tokio::time::timeout(Duration::from_secs(3), async {
        let shared = &resolver;
        let warm = join_all((0..5).map(|i| async move {
            shared
                .lookup(
                    ReservedResolver::Google,
                    &question(&format!("warm-{i}.example.")),
                )
                .await
        }))
        .await;
        assert!(warm.iter().all(Result::is_ok));
        close_idle.cancel();
        idle_connections_closed(&events, 5).await;
        resolver
            .lookup(ReservedResolver::Google, &question("after.example."))
            .await
            .unwrap();
        assert_eq!(count(&events, "reserved DNS upstream request failed"), 0);
        assert_eq!(count(&events, RESOLVER_REPLACED), 0);

        // Hickory reports the refused reconnects as NoConnections.
        peer.await.unwrap();
        for name in ["outage-1.example.", "outage-2.example."] {
            assert!(matches!(
                resolver
                    .lookup(ReservedResolver::Google, &question(name))
                    .await,
                Err(NetError::NoConnections)
            ));
        }
    })
    .await
    .unwrap();

    assert!(count(&events, "reserved DNS upstream connection failed") > 0);
    assert_eq!(
        count(&events, RESOLVER_REPLACED),
        1,
        "the outage must not reset again during cooldown"
    );
}

// The reconnect must keep the pool's TLS trust, which plain TCP cannot show.
#[tokio::test]
async fn idle_closed_tls_connection_reconnects_with_pool_tls_config() {
    let layer = CapturedEventsLayer::new();
    let events = layer.events();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
    let certificate = rcgen::generate_simple_self_signed(vec!["dns.example.test".into()]).unwrap();
    let crypto = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(crypto.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let acceptor = TlsAcceptor::from(Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
            )
            .unwrap(),
    ));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        for _ in 0..2 {
            let stream = acceptor
                .accept(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            // Close like an upstream idle timeout, after the answer.
            answer(stream).await.shutdown().await.unwrap();
        }
    });
    let mut config = ConnectionConfig::tls(Arc::from("dns.example.test"));
    config.port = upstream_addr.port();
    let mut options = ResolverOpts::default();
    options.timeout = Duration::from_secs(2);
    let handle = LoggingConnectionProvider(GatewayRuntimeProvider {
        handle: TokioHandle::default(),
        outbound_mark: None,
        lookup_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_LOOKUPS)),
    })
    .new_connection(
        upstream_addr.ip(),
        &config,
        &PoolContext::new(options, TlsConfig { config: client }),
    )
    .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        let handle = handle.await.unwrap();
        handle
            .send(request("first.example."))
            .first_answer()
            .await
            .unwrap();
        idle_connections_closed(&events, 1).await;
        handle
            .send(request("after.example."))
            .first_answer()
            .await
            .unwrap();
        peer.await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(count(&events, "reserved DNS upstream request failed"), 0);
}

const RESOLVER_REPLACED: &str = "reserved DNS resolver replaced after NoConnections";

fn count(events: &CapturedEvents, message: &str) -> usize {
    events
        .events()
        .iter()
        .filter(|event| field_value(event, "message") == Some(message))
        .count()
}

// Await Hickory's observed EOF, not a sleep that races its I/O task.
async fn idle_connections_closed(events: &CapturedEvents, connections: usize) {
    while count(events, "io_stream is done, shutting down") < connections {
        tokio::task::yield_now().await;
    }
}

fn request(name: &str) -> DnsRequest {
    let mut message = Message::query();
    message.add_query(question(name));
    DnsRequest::new(message, DnsRequestOptions::default())
}

fn question(name: &str) -> Query {
    Query::query(Name::from_ascii(name).unwrap(), RecordType::A)
}

async fn answer<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> S {
    let length = stream.read_u16().await.unwrap();
    let mut wire = vec![0; usize::from(length)];
    stream.read_exact(&mut wire).await.unwrap();
    let mut response = Message::from_vec(&wire).unwrap().into_response();
    response.answers.push(Record::from_rdata(
        response.queries[0].name.clone(),
        60,
        RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
    ));
    let wire = response.to_vec().unwrap();
    stream
        .write_u16(u16::try_from(wire.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&wire).await.unwrap();
    stream
}
