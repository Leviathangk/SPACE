use super::*;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn shutdown_releases_owned_connections() {
    for abort in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let stats = Arc::new(ProxyStats::new());
        let network_state = GatewayNetworkState::pending_policy();
        let proxy = tokio::spawn(run(TcpProxyConfig {
            sandbox_id: "connection-lifetime-test",
            listener,
            stats: stats.clone(),
            network_state: network_state.clone(),
            dns_allow_cache: DnsAllowCache::default(),
            dns_attributed_connections: DnsAttributedTcpTracker::default(),
            tls_interception: Default::default(),
            reserved_dns: crate::reserved_dns::test_support::test_core(),
            reserved_dns_cancellation: ReservedDnsCancellation::default(),
            known_egress_state: KnownEgressState::default(),
            debug_state: GatewayDebugState::default(),
            shutdown_token: shutdown.clone(),
        }));
        let mut client = TcpStream::connect(address).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while network_state.snapshot().await.pending_tcp_connections != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the accepted connection must be waiting for policy");
        assert_eq!(stats.tcp_active(), 1);

        if abort {
            proxy.abort();
            assert!(proxy.await.unwrap_err().is_cancelled());
        } else {
            shutdown.cancel();
            proxy.await.unwrap().unwrap();
        }

        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
            .await
            .expect("returning or dropping the proxy must close its accepted sockets");
        assert!(
            matches!(closed, Ok(0))
                || matches!(closed, Err(ref error) if error.kind() == io::ErrorKind::ConnectionReset),
            "abort={abort}: {closed:?}"
        );
        assert_eq!(network_state.snapshot().await.pending_tcp_connections, 0);
        assert_eq!(stats.tcp_active(), 0, "abort={abort}");
    }
}

#[tokio::test(start_paused = true)]
async fn drain_joins_failed_and_overdue_connections_with_one_deadline() {
    use std::sync::atomic::AtomicBool;

    let shutdown = CancellationToken::new();
    let permits = Arc::new(Semaphore::new(3));
    let mut connections = JoinSet::new();
    let graceful_finished = Arc::new(AtomicBool::new(false));

    let permit = permits.clone().try_acquire_owned().unwrap();
    let cancelled = shutdown.clone();
    let finished = graceful_finished.clone();
    connections.spawn(async move {
        let _permit = permit;
        cancelled.cancelled().await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        finished.store(true, Ordering::Relaxed);
    });
    let permit = permits.clone().try_acquire_owned().unwrap();
    connections.spawn(async move {
        let _permit = permit;
        std::future::pending::<()>().await;
    });
    let permit = permits.clone().try_acquire_owned().unwrap();
    connections.spawn(async move {
        let _permit = permit;
        panic!("synthetic connection failure");
    });

    let started = tokio::time::Instant::now();
    let grace = Duration::from_secs(5);
    drain_connections(
        "connection-lifetime-test",
        &mut connections,
        &shutdown,
        started + grace,
    )
    .await;

    assert!(graceful_finished.load(Ordering::Relaxed));
    assert_eq!(tokio::time::Instant::now() - started, grace);
    assert_eq!(permits.available_permits(), 3);
}
