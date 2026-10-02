use super::*;
use futures_util::FutureExt as _;
use tokio::io::AsyncWriteExt as _;
use tracing_subscriber::Layer as _;

#[derive(Clone, Copy)]
enum SigningResult {
    Unavailable,
    Pending,
}

#[tokio::test]
async fn interception_wiring_preserves_admission_and_never_falls_back_to_relay() {
    use crate::tls_sni::test_fixtures::client_hello;

    // 0.0.0.0 reaches this local listener without using the policy's forbidden
    // 127/8 range. No request in this test reaches an external destination.
    let upstream = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let dst = upstream.local_addr().unwrap();
    let tls = client_hello(Some("good.com"));
    let no_sni = client_hello(None);
    let malformed = client_hello(Some("not a domain"));
    // name, policy allows, bytes, signing result, requests
    for (name, allowed, bytes, signing_result, requests) in [
        ("signing error", true, &tls, SigningResult::Unavailable, 1),
        ("signing timeout", true, &tls, SigningResult::Pending, 1),
        ("policy denial", false, &tls, SigningResult::Unavailable, 0),
        ("no SNI", true, &no_sni, SigningResult::Unavailable, 0),
        (
            "malformed SNI",
            true,
            &malformed,
            SigningResult::Unavailable,
            0,
        ),
        (
            "incomplete",
            true,
            &vec![0x16],
            SigningResult::Unavailable,
            0,
        ),
    ] {
        let mut policy = if allowed {
            NetworkPolicyV1::allow_all()
        } else {
            NetworkPolicyV1::deny_all()
        };
        policy.tls_interception = pplx_space_net_common::TlsInterception::On;
        let (network_state, _) = ready_policy(policy).await;
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let tls_interception = TlsInterception::new(Arc::new(move |host, port| {
            let send = send.clone();
            Box::pin(async move {
                send.send((host, port)).await.unwrap();
                match signing_result {
                    SigningResult::Unavailable => Err(tonic::Status::unavailable("test failure")),
                    SigningResult::Pending => std::future::pending().await,
                }
            })
        }));
        let proxy = TcpProxy {
            sandbox_id: "test",
            stats: Arc::new(ProxyStats::new()),
            network_state,
            dns_allow_cache: DnsAllowCache::default(),
            dns_attributed_connections: DnsAttributedTcpTracker::default(),
            tls_interception,
            reserved_dns: crate::reserved_dns::test_support::test_core(),
            reserved_dns_cancellation: ReservedDnsCancellation::default(),
            known_egress_state: KnownEgressState::default(),
            debug_state: GatewayDebugState::default(),
            shutdown_token: CancellationToken::new(),
            refusal_warnings: Arc::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (guest, accepted) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept(),
        );
        let mut guest = guest.unwrap();
        guest.write_all(bytes).await.unwrap();
        guest.shutdown().await.unwrap();
        let connection = accepted.unwrap().0;
        let shutdown = CancellationToken::new();
        let (host, outcome) = tokio::time::timeout(
            Duration::from_secs(3),
            proxy.handle(connection, dst, &shutdown),
        )
        .await
        .expect(name);
        assert!(matches!(outcome, FlowOutcome::Rejected(_)), "{name}");
        assert!(
            upstream.accept().now_or_never().is_none(),
            "{name}: must not dial upstream"
        );
        if requests == 1 {
            assert_eq!(
                receive.try_recv().unwrap(),
                ("good.com".parse().unwrap(), dst.port()),
                "{name}"
            );
        }
        assert!(
            receive.try_recv().is_err(),
            "{name}: unexpected certificate request"
        );
        let intercepted = matches!(
            outcome,
            FlowOutcome::Rejected(Rejection::TlsInterception(_))
        );
        assert_eq!(intercepted, allowed, "{name}: unexpected rejection path");
        if intercepted {
            let layer = CapturedEventsLayer::new().with_event_filter(|event| {
                field_value(event, "message")
                    == Some("experimental TLS interception closed the connection")
            });
            let events = layer.events();
            let warning_layer = CapturedEventsLayer::new().with_event_filter(|event| {
                field_value(event, "message")
                    == Some("experimental TLS interception closed the connection")
            });
            let warning_events = warning_layer.events();
            tracing::subscriber::with_default(
                tracing_subscriber::registry()
                    .with(layer)
                    .with(warning_layer.with_filter(tracing::level_filters::LevelFilter::WARN)),
                || {
                    proxy.record(dst, host.as_ref(), outcome);
                },
            );
            let events = events.events();
            let [event] = events.as_slice() else {
                panic!("{name}: expected one TLS rejection event");
            };
            // Every interception rejection is a fail-closed guest breakage, so
            // all of them must survive a production log level, not only the
            // missing-SNI case.
            assert_eq!(warning_events.events().len(), 1, "{name}");
            assert!(
                event
                    .iter()
                    .all(|(_, value)| !value.contains(&dst.ip().to_string())),
                "{name}: raw destination leaked: {event:?}",
            );
            assert_eq!(
                field_value(event, "destination_port"),
                Some(dst.port().to_string().as_str())
            );
        }
    }
}

#[tokio::test]
async fn interception_exemptions_relay_original_tls_only_after_admission() {
    use crate::tls_sni::test_fixtures::client_hello;
    use tokio::io::AsyncReadExt as _;

    for (name, exempt_ip, host, allowed, denied_host, passes) in [
        (
            "exempt host",
            false,
            Some("sensor.example.test"),
            true,
            false,
            true,
        ),
        ("exempt IP without SNI", true, None, true, false, true),
        (
            "other host intercepted",
            false,
            Some("other.example.test"),
            true,
            false,
            false,
        ),
        (
            "exemption grants no access",
            false,
            Some("sensor.example.test"),
            false,
            false,
            false,
        ),
        (
            "explicit host deny",
            true,
            Some("sensor.example.test"),
            true,
            true,
            false,
        ),
        (
            "DNS-bound host",
            false,
            Some("sensor.example.test"),
            false,
            false,
            true,
        ),
        (
            "unattributed host",
            false,
            Some("sensor.example.test"),
            false,
            false,
            false,
        ),
        (
            "explicit IP deny",
            true,
            Some("sensor.example.test"),
            true,
            false,
            false,
        ),
        (
            "unbound host on open policy",
            false,
            Some("sensor.example.test"),
            true,
            false,
            false,
        ),
        (
            "wrong address on open policy",
            false,
            Some("sensor.example.test"),
            true,
            false,
            false,
        ),
        (
            "wrong host on open policy",
            false,
            Some("sensor.example.test"),
            true,
            false,
            false,
        ),
        (
            "unbound host with IP allow",
            false,
            Some("sensor.example.test"),
            false,
            false,
            false,
        ),
        (
            "bound host with IP allow",
            false,
            Some("sensor.example.test"),
            false,
            false,
            true,
        ),
        (
            "exempt IP with other SNI",
            true,
            Some("other.example.test"),
            true,
            false,
            true,
        ),
        (
            "platform deny with host deny",
            true,
            Some("sensor.example.test"),
            true,
            true,
            false,
        ),
    ] {
        let upstream = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let mut dst = upstream.local_addr().unwrap();
        if name == "platform deny with host deny" {
            dst.set_ip("127.0.0.1".parse().unwrap());
        }
        let mut policy = if allowed {
            NetworkPolicyV1::allow_all()
        } else {
            NetworkPolicyV1::deny_all()
        };
        policy.tls_interception = pplx_space_net_common::TlsInterception::On;
        policy.tls_interception_exemptions.push(if exempt_ip {
            EgressDestinationV1::Ip(dst.ip())
        } else {
            EgressDestinationV1::Domain("sensor.example.test".parse().unwrap())
        });
        if denied_host {
            policy.egress.deny.push(EgressDestinationV1::Domain(
                "sensor.example.test".parse().unwrap(),
            ));
        }
        if matches!(
            name,
            "DNS-bound host"
                | "unattributed host"
                | "unbound host with IP allow"
                | "bound host with IP allow"
        ) {
            policy.egress.allow.push(EgressDestinationV1::Domain(
                "sensor.example.test".parse().unwrap(),
            ));
        }
        if matches!(
            name,
            "unbound host with IP allow" | "bound host with IP allow"
        ) {
            policy.egress.allow.push(EgressDestinationV1::Ip(dst.ip()));
        }
        if name == "explicit IP deny" {
            policy.egress.deny.push(EgressDestinationV1::Ip(dst.ip()));
        }
        if name == "wrong host on open policy" {
            policy.egress.allow.push(EgressDestinationV1::Domain(
                "other.example.test".parse().unwrap(),
            ));
        }
        let (network_state, installed) = ready_policy(policy).await;
        let dns_allow_cache = DnsAllowCache::default();
        if matches!(
            name,
            "exempt host" | "DNS-bound host" | "bound host with IP allow"
        ) {
            assert!(dns_allow_cache.observe_response(
                &dns_a_response("sensor.example.test", [0, 0, 0, 0], 60),
                &installed,
            ));
        }
        if name == "wrong address on open policy" {
            assert!(dns_allow_cache.observe_response(
                &dns_a_response("sensor.example.test", [192, 0, 2, 1], 60),
                &installed,
            ));
        }
        if name == "wrong host on open policy" {
            assert!(dns_allow_cache.observe_response(
                &dns_a_response("other.example.test", [0, 0, 0, 0], 60),
                &installed,
            ));
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = Arc::clone(&calls);
        let proxy = TcpProxy {
            sandbox_id: "test",
            stats: Arc::new(ProxyStats::new()),
            network_state,
            dns_allow_cache,
            dns_attributed_connections: DnsAttributedTcpTracker::default(),
            tls_interception: TlsInterception::new(Arc::new(move |_, _| {
                recorded.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Err(tonic::Status::unavailable("synthetic signer")) })
            })),
            reserved_dns: crate::reserved_dns::test_support::test_core(),
            reserved_dns_cancellation: ReservedDnsCancellation::default(),
            known_egress_state: KnownEgressState::default(),
            debug_state: GatewayDebugState::default(),
            shutdown_token: CancellationToken::new(),
            refusal_warnings: Arc::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (guest, accepted) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let mut guest = guest.unwrap();
        let bytes = client_hello(host);
        guest.write_all(&bytes).await.unwrap();
        guest.shutdown().await.unwrap();
        let shutdown = CancellationToken::new();
        let handled = proxy.handle(accepted.unwrap().0, dst, &shutdown);
        if passes {
            let origin = async {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let mut received = Vec::new();
                stream.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, bytes, "{name}: ClientHello must remain unchanged");
                stream.write_all(b"origin TLS bytes").await.unwrap();
            };
            let ((_, outcome), ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(handled, origin)
            })
            .await
            .expect(name);
            assert!(matches!(outcome, FlowOutcome::Relayed(_)), "{name}");
            let mut response = Vec::new();
            guest.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, b"origin TLS bytes", "{name}");
        } else {
            let (_, outcome) = tokio::time::timeout(Duration::from_secs(3), handled)
                .await
                .expect(name);
            if name == "platform deny with host deny" {
                assert!(
                    matches!(
                        outcome,
                        FlowOutcome::Rejected(Rejection::Admission(
                            TcpAdmissionDenial::DnsAttribution(
                                DenyReason::PlatformGuardrail { .. }
                            )
                        ))
                    ),
                    "{name}"
                );
            } else {
                assert!(matches!(outcome, FlowOutcome::Rejected(_)), "{name}");
            }
            assert!(upstream.accept().now_or_never().is_none(), "{name}");
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            usize::from(matches!(
                name,
                "other host intercepted"
                    | "unbound host on open policy"
                    | "wrong address on open policy"
                    | "wrong host on open policy"
                    | "unbound host with IP allow"
            )),
            "{name}"
        );
    }
}

/// Go's `net/http` dials an `https://` proxy with the transport's ordinary TLS
/// configuration, so its ClientHello offers `h2, http/1.1`, and then always
/// writes an HTTP/1.1 `CONNECT` regardless of the negotiated protocol. An
/// interceptor that selects `h2` answers that request line with HTTP/2 frames,
/// which Go reports as `malformed HTTP status code "\x00\x04..."`. The
/// inspected session must serve that CONNECT as HTTP/1.1, still authorize it,
/// and offer the origin only `http/1.1` so the proxy hop stays HTTP/1.1.
#[tokio::test]
async fn go_style_https_proxy_connect_over_h2_alpn_is_served_as_http1() {
    use tokio::io::AsyncReadExt as _;

    let state = GatewayNetworkState::pending_policy();
    let (root, _, provider) = crate::certificates::local_provider_with_state(state.clone()).await;
    // 0.0.0.0 reaches this local listener without using the forbidden 127/8
    // range; nothing here leaves the host.
    let origin = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let destination = origin.local_addr().unwrap();
    let host: pplx_space_net_common::DomainName = "proxy.allowed.test".parse().unwrap();
    let lease = provider
        .get(host.clone(), destination.port())
        .await
        .unwrap();
    let origin_key = lease.certified_key().unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(root.der().clone()).unwrap();

    // A forward proxy that would prefer h2 if the gateway offered it.
    let origin_task = tokio::spawn(async move {
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(origin_key)));
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let (socket, _) = origin.accept().await.unwrap();
        let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(config))
            .accept(socket)
            .await
            .unwrap();
        let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut tunnelled = [0; 4];
        stream.read_exact(&mut tunnelled).await.unwrap();
        stream.write_all(b"pong").await.unwrap();
        stream.flush().await.unwrap();
        (alpn, String::from_utf8(headers).unwrap(), tunnelled)
    });

    let proxy = TcpProxy {
        sandbox_id: "test",
        stats: Arc::new(ProxyStats::new()),
        network_state: state,
        dns_allow_cache: DnsAllowCache::default(),
        dns_attributed_connections: DnsAttributedTcpTracker::default(),
        tls_interception: TlsInterception::default().with_upstream_roots(roots.clone()),
        reserved_dns: crate::reserved_dns::test_support::test_core(),
        reserved_dns_cancellation: ReservedDnsCancellation::default(),
        known_egress_state: KnownEgressState::default(),
        debug_state: GatewayDebugState::default(),
        shutdown_token: CancellationToken::new(),
        refusal_warnings: Arc::default(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (guest, accepted) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept(),
    );
    let gateway = tokio::spawn(async move {
        proxy
            .terminate_tls(
                accepted.unwrap().0,
                admission::AdmittedTcp::Tunnel(destination),
                lease,
                crate::tls_sni::ClientAlpnOffer::Http2ThenHttp1,
            )
            .await
    });

    // The guest behaves like Go: offer h2 first, then speak HTTP/1.1 CONNECT.
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let mut guest = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from(host.as_str().to_owned()).unwrap(),
            guest.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(guest.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    guest
        .write_all(
            b"CONNECT inner.test:443 HTTP/1.1\r\nHost: inner.test:443\r\n\
              Proxy-Authorization: Basic Zml4dHVyZQ==\r\n\r\n",
        )
        .await
        .unwrap();
    guest.flush().await.unwrap();

    let mut response = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), async {
        while !response.ends_with(b"\r\n\r\n") {
            match guest.read_u8().await {
                Ok(byte) => response.push(byte),
                Err(_) => break,
            }
        }
    })
    .await;
    assert!(
        read.is_ok() && response.starts_with(b"HTTP/1.1 200"),
        "Go-style CONNECT over h2 ALPN was not answered with HTTP/1.1: {:?}",
        String::from_utf8_lossy(&response),
    );
    guest.write_all(b"ping").await.unwrap();
    guest.flush().await.unwrap();
    let mut echoed = [0; 4];
    tokio::time::timeout(Duration::from_secs(3), guest.read_exact(&mut echoed))
        .await
        .expect("tunnel stalled")
        .unwrap();
    assert_eq!(&echoed, b"pong");

    let (origin_alpn, origin_headers, tunnelled) =
        tokio::time::timeout(Duration::from_secs(3), origin_task)
            .await
            .expect("origin stalled")
            .unwrap();
    assert_eq!(
        origin_alpn.as_deref(),
        Some(&b"http/1.1"[..]),
        "the proxy hop must not be upgraded to h2"
    );
    assert!(
        origin_headers.starts_with("CONNECT inner.test:443 HTTP/1.1\r\n"),
        "{origin_headers}"
    );
    assert!(origin_headers.contains("proxy-authorization: Basic Zml4dHVyZQ==\r\n"));
    assert_eq!(&tunnelled, b"ping");
    drop(guest);
    gateway.abort();
}
