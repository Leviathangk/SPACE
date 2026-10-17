use std::sync::Mutex as StdMutex;

use super::*;
use pplx_space_observability::test_capture::RecordingVisitor;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const KNOWN_HOST: &str = "bucket-a.example.test";

/// Known-egress events with their level, which `CapturedEventsLayer` drops.
#[derive(Clone, Default)]
struct KnownEgressEvents(Arc<StdMutex<Vec<(tracing::Level, CapturedEvent)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for KnownEgressEvents {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "pplx_space_net_gateway::known_egress" {
            return;
        }
        let mut visitor = RecordingVisitor::default();
        event.record(&mut visitor);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), visitor.into_fields()));
    }
}

#[derive(Clone, Copy, Debug)]
enum Peer {
    /// Answers, then closes the keep-alive connection in order.
    ClosesInOrder,
    /// Answers, then the origin resets the connection.
    OriginResets,
    /// The guest resets the connection once it has the response.
    GuestAbandons,
    /// Nothing listens at the destination.
    Refused,
}

/// Relay one pass-through flow to a known-egress destination over loopback,
/// then report it, returning the guest's view and the known-egress events.
async fn relay_to_known_egress(peer: Peer) -> (Vec<u8>, Vec<(tracing::Level, CapturedEvent)>) {
    let host: DomainName = KNOWN_HOST.parse().unwrap();
    let proxy = TcpProxy {
        sandbox_id: "test",
        stats: Arc::new(ProxyStats::new()),
        network_state: GatewayNetworkState::no_identity(),
        tls_interception: Default::default(),
        reserved_dns: crate::reserved_dns::test_support::test_core(),
        known_egress_state: crate::known_egress::KnownEgressState::new(vec![host.clone()]),
        debug_state: GatewayDebugState::default(),
        shutdown_token: CancellationToken::new(),
        refusal_warnings: Arc::default(),
    };
    let guest_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (guest, accepted) = tokio::join!(
        TcpStream::connect(guest_listener.local_addr().unwrap()),
        guest_listener.accept(),
    );
    let (mut guest, (gateway_guest, _)) = (guest.unwrap(), accepted.unwrap());
    let origin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin_listener.local_addr().unwrap();
    let origin_listener = (!matches!(peer, Peer::Refused)).then_some(origin_listener);
    let (delivered, response_delivered) = tokio::sync::oneshot::channel();
    let origin = tokio::spawn(async move {
        let Some(origin_listener) = origin_listener else {
            return;
        };
        let (mut origin, _) = origin_listener.accept().await.unwrap();
        let mut request = [0; 7];
        origin.read_exact(&mut request).await.unwrap();
        origin.write_all(b"response").await.unwrap();
        match peer {
            Peer::OriginResets => {
                // A reset discards unread data, so wait for the guest's copy.
                response_delivered.await.unwrap();
                origin.set_zero_linger().unwrap();
            }
            Peer::GuestAbandons => {
                let mut rest = Vec::new();
                let _ = origin.read_to_end(&mut rest).await;
            }
            Peer::ClosesInOrder | Peer::Refused => {}
        }
    });
    let relay = tokio::spawn(async move {
        let outcome = proxy
            .pass_through(gateway_guest, origin_addr, |dst| async move {
                Ok::<_, transport::Error>(TcpStream::connect(dst).await?)
            })
            .await;
        (proxy, outcome)
    });

    let mut received = Vec::new();
    if !matches!(peer, Peer::Refused) {
        guest.write_all(b"request").await.unwrap();
        let mut response = [0; 8];
        guest.read_exact(&mut response).await.unwrap();
        received.extend_from_slice(&response);
        let _ = delivered.send(());
        if matches!(peer, Peer::GuestAbandons) {
            guest.set_zero_linger().unwrap();
        } else {
            let _ = guest.read_to_end(&mut received).await;
        }
    }
    drop(guest);
    origin.await.unwrap();
    let (proxy, outcome) = relay.await.unwrap();

    let events = KnownEgressEvents::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    tracing::subscriber::with_default(subscriber, || {
        proxy.record(origin_addr, Some(&host), outcome);
    });
    let events = events.0.lock().unwrap().clone();
    (received, events)
}

/// Known egress names connect-phase failures for operators. A connection the
/// destination served and a peer then reset is not a failure to connect, and
/// a guest abandoning its own connection is not the destination's failure.
#[tokio::test]
async fn known_egress_reports_connect_failures_apart_from_peer_resets_after_connect() {
    for (peer, level, message, fault) in [
        (
            Peer::Refused,
            tracing::Level::ERROR,
            "failed to connect to known egress destination",
            None,
        ),
        (
            Peer::OriginResets,
            tracing::Level::INFO,
            "known egress destination failed an established connection",
            Some("origin_read"),
        ),
        (
            Peer::GuestAbandons,
            tracing::Level::DEBUG,
            "guest abandoned an established known egress connection",
            Some("guest_read"),
        ),
    ] {
        let (_, events) = relay_to_known_egress(peer).await;
        let [(logged_level, event)] = events.as_slice() else {
            panic!("{peer:?}: expected one known-egress event, got {events:?}");
        };
        assert_eq!(*logged_level, level, "{peer:?}");
        assert_eq!(field_value(event, "message"), Some(message), "{peer:?}");
        assert_eq!(field_value(event, "fault"), fault, "{peer:?}");
        assert_eq!(field_value(event, "destination"), Some(KNOWN_HOST));
        let reason = field_value(event, "reason").unwrap();
        let phase = if fault.is_some() {
            "transfer: ConnectionReset"
        } else {
            "upstream_connect: TCP connect failed (ConnectionRefused)"
        };
        assert!(reason.starts_with(phase), "{peer:?}: {reason}");
    }
}

/// The relay ends an orderly keep-alive close without a reset: every byte
/// reaches the guest and nothing is reported.
#[tokio::test]
async fn known_egress_reports_nothing_for_an_orderly_close() {
    let (received, events) = relay_to_known_egress(Peer::ClosesInOrder).await;
    assert_eq!(received, b"response");
    assert!(events.is_empty(), "{events:?}");
}
