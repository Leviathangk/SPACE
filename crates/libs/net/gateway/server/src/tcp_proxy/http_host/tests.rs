use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use http_body_util::{BodyExt, Empty};
use pplx_space_net_common::{
    EffectiveNetworkPolicyRef, EffectiveNetworkPolicyRefScope, EgressDefaultAction,
    EgressDestinationV1, EgressPolicyV1, NetworkPolicyV1,
};
use tokio::io::duplex;

use std::convert::Infallible;
use std::io;

use http::{Request, Response, StatusCode, header};
use hyper::client::conn::http1 as client;
use hyper::server::conn::http1 as server;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use super::*;
use crate::dns::DnsAllowCache;
use crate::dns::test_fixtures::a_response as dns_a_response;
use crate::proto::NetworkState;
use crate::state::GatewayNetworkState;
use crate::state::{NetworkStateReason, PolicyLoader, PulledNetworkPolicy};
use hyper::body::Incoming;
use tokio::sync::Mutex;

const ALLOWED_HOST: &str = "pypi.org";
const CO_TENANT_HOST: &str = "fiddle.fastly.dev";
const OTHER_ALLOWED_HOST: &str = "files.pythonhosted.org";
const DESTINATION: &str = "93.184.216.34:80";
const PIPE: usize = 64 * 1024;

fn policy() -> NetworkPolicyV1 {
    NetworkPolicyV1 {
        version: Default::default(),
        tls_interception: Default::default(),
        tls_interception_exemptions: Vec::new(),
        network_credential_rules: vec![],
        egress: EgressPolicyV1::new(
            EgressDefaultAction::Deny,
            vec![EgressDestinationV1::Domain(ALLOWED_HOST.parse().unwrap())],
            Vec::new(),
        )
        .unwrap(),
    }
}

fn policy_denying(host: &str) -> NetworkPolicyV1 {
    NetworkPolicyV1 {
        version: Default::default(),
        tls_interception: Default::default(),
        tls_interception_exemptions: Vec::new(),
        network_credential_rules: vec![],
        egress: EgressPolicyV1::new(
            EgressDefaultAction::Deny,
            vec![
                EgressDestinationV1::Domain(ALLOWED_HOST.parse().unwrap()),
                EgressDestinationV1::Domain(OTHER_ALLOWED_HOST.parse().unwrap()),
            ],
            vec![EgressDestinationV1::Domain(host.parse().unwrap())],
        )
        .unwrap(),
    }
}

fn policy_loader(policy: NetworkPolicyV1) -> PolicyLoader {
    let policy_ref = EffectiveNetworkPolicyRef::local(
        EffectiveNetworkPolicyRefScope::Sandbox,
        "host-binding-test",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap()
    .to_string();
    Arc::new(move || {
        let policy = policy.clone();
        let policy_ref = policy_ref.clone();
        Box::pin(async move {
            Ok(PulledNetworkPolicy {
                policy,
                policy_ref,
                network_credentials: Default::default(),
            })
        })
    })
}

async fn ready_state(policy: NetworkPolicyV1) -> GatewayNetworkState {
    let state = GatewayNetworkState::pending_policy();
    let snapshot = state
        .refresh(policy_loader(policy), None, NetworkStateReason::Startup)
        .await;
    assert_eq!(snapshot.mode, NetworkState::Ready);
    state
}

#[tokio::test]
async fn authorization_waits_through_a_transient_policy_refresh() {
    let policy = policy();
    let state = GatewayNetworkState::pending_policy();
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &policy,
    );
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state.clone(), cache);
    let authorizing =
        tokio::spawn(async move { binding.authorize(&ALLOWED_HOST.parse().unwrap()).await });
    tokio::task::yield_now().await;
    assert!(
        !authorizing.is_finished(),
        "an established flow should use the same PendingPolicy grace as TCP admission"
    );

    let snapshot = state
        .refresh(
            policy_loader(policy),
            None,
            NetworkStateReason::ControlRefresh,
        )
        .await;
    assert_eq!(snapshot.mode, NetworkState::Ready);
    authorizing.await.unwrap().unwrap();
}

async fn assert_request_refused_before_connect(
    policy: NetworkPolicyV1,
    cache: DnsAllowCache,
    request: Request<Empty<hyper::body::Bytes>>,
) {
    let state = ready_state(policy).await;
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state, cache);
    let connect_called = Arc::new(AtomicBool::new(false));
    let (guest, guest_peer) = duplex(PIPE);
    let relay = tokio::spawn(serve(guest, binding, {
        let connect_called = connect_called.clone();
        move |_| async move {
            connect_called.store(true, Ordering::Relaxed);
            Err::<tokio::io::DuplexStream, _>(
                io::Error::new(io::ErrorKind::ConnectionRefused, "unexpected connect").into(),
            )
        }
    }));
    let (mut sender, guest_connection) =
        client::handshake::<_, Empty<hyper::body::Bytes>>(TokioIo::new(guest_peer))
            .await
            .unwrap();
    let guest_connection = tokio::spawn(guest_connection);

    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers().get(header::CONNECTION).unwrap(), "close");
    response.into_body().collect().await.unwrap();
    drop(sender);
    guest_connection.await.unwrap().unwrap();
    let served = relay.await.unwrap().unwrap();

    assert_eq!((served.requests, served.refused), (1, 1));
    assert!(
        !connect_called.load(Ordering::Relaxed),
        "a refused request must not invoke the lazy upstream connector"
    );
}

#[tokio::test]
async fn a_forbidden_first_host_does_not_invoke_the_connector() {
    let request = Request::builder()
        .uri("/simple/")
        .header(header::HOST, CO_TENANT_HOST)
        .body(Empty::new())
        .unwrap();
    assert_request_refused_before_connect(policy(), DnsAllowCache::default(), request).await;
}

#[tokio::test]
async fn an_explicitly_denied_host_is_refused_despite_a_live_binding() {
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &policy(),
    );
    let deny_policy = policy_denying(ALLOWED_HOST);
    cache.observe_response(
        &dns_a_response(OTHER_ALLOWED_HOST, [93, 184, 216, 34], 60),
        &deny_policy,
    );
    assert!(
        cache.allows_ip(
            DESTINATION.parse::<SocketAddr>().unwrap().ip(),
            &deny_policy,
        ),
        "the other allowed hostname must keep the shared IP eligible",
    );
    let request = Request::builder()
        .uri("/simple/")
        .header(header::HOST, ALLOWED_HOST)
        .body(Empty::new())
        .unwrap();
    assert_request_refused_before_connect(deny_policy, cache, request).await;
}

#[tokio::test]
async fn plaintext_upgrades_are_refused_before_connect() {
    for (connection, upgrade) in [
        ("upgrade", "websocket"),
        ("keep-alive, UpGrAdE", "h2c"),
        ("keep-alive", "test-protocol"),
    ] {
        let policy = policy();
        let cache = DnsAllowCache::default();
        cache.observe_response(
            &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
            &policy,
        );
        let request = Request::builder()
            .uri("/upgrade")
            .header(header::HOST, ALLOWED_HOST)
            .header(header::CONNECTION, connection)
            .header(header::UPGRADE, upgrade)
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Empty::new())
            .unwrap();
        assert_request_refused_before_connect(policy, cache, request).await;
    }
}

#[tokio::test]
async fn independent_ip_allow_applies_to_an_existing_host_bound_flow() {
    let destination: SocketAddr = DESTINATION.parse().unwrap();
    let direct_policy = NetworkPolicyV1 {
        version: Default::default(),
        tls_interception: Default::default(),
        tls_interception_exemptions: Vec::new(),
        network_credential_rules: vec![],
        egress: EgressPolicyV1::new(
            EgressDefaultAction::Deny,
            vec![
                EgressDestinationV1::Ip(destination.ip()),
                EgressDestinationV1::Domain(ALLOWED_HOST.parse().unwrap()),
            ],
            vec![EgressDestinationV1::Domain(ALLOWED_HOST.parse().unwrap())],
        )
        .unwrap(),
    };
    let binding = HostBinding::new(
        "test",
        destination,
        ready_state(direct_policy).await,
        DnsAllowCache::default(),
    );
    binding
        .authorize(&ALLOWED_HOST.parse().unwrap())
        .await
        .unwrap();
}

async fn send_request(
    sender: &mut client::SendRequest<Empty<hyper::body::Bytes>>,
    uri: &str,
    host: &str,
) -> StatusCode {
    let request = Request::builder()
        .uri(uri)
        .header(header::HOST, host)
        .body(Empty::new())
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    response.into_body().collect().await.unwrap();
    status
}

async fn send_fastly_host_override(
    sender: &mut client::SendRequest<Empty<hyper::body::Bytes>>,
) -> StatusCode {
    let request = Request::builder()
        .uri("/simple/")
        .header(header::HOST, ALLOWED_HOST)
        .header("fastly-host", CO_TENANT_HOST)
        .header("fastly-ssl", "1")
        .header("fastly-key", "fixture-token")
        .body(Empty::new())
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    response.into_body().collect().await.unwrap();
    status
}

#[tokio::test]
async fn validates_every_host_on_a_keep_alive_connection() {
    let policy = policy();
    let state = ready_state(policy.clone()).await;
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &policy,
    );
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state, cache);

    let (guest, guest_peer) = duplex(PIPE);
    let (upstream, upstream_peer) = duplex(PIPE);
    let seen_hosts = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_fastly_key = Arc::new(AtomicUsize::new(0));
    let destination = tokio::spawn({
        let seen_hosts = seen_hosts.clone();
        let seen_fastly_key = seen_fastly_key.clone();
        async move {
            let _ = server::Builder::new()
                .serve_connection(
                    TokioIo::new(upstream_peer),
                    service_fn(|request: Request<Incoming>| {
                        let seen_hosts = seen_hosts.clone();
                        let seen_fastly_key = seen_fastly_key.clone();
                        async move {
                            let host = request
                                .headers()
                                .get(header::HOST)
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_string();
                            // Only the non-routing API credential survives.
                            let fastly = request
                                .headers()
                                .keys()
                                .map(|name| name.as_str())
                                .filter(|name| name.starts_with("fastly-"))
                                .collect::<Vec<_>>();
                            assert!(
                                fastly.iter().all(|name| *name == "fastly-key"),
                                "guest-supplied Fastly routing metadata must be stripped: {fastly:?}"
                            );
                            if !fastly.is_empty() {
                                seen_fastly_key.fetch_add(1, Ordering::SeqCst);
                            }
                            seen_hosts.lock().await.push(host);
                            Ok::<_, Infallible>(Response::new(Empty::<hyper::body::Bytes>::new()))
                        }
                    }),
                )
                .await;
        }
    });
    let relay = tokio::spawn(serve(guest, binding, move |_| async move {
        Ok::<_, super::super::transport::Error>(upstream)
    }));

    let (mut sender, guest_connection) =
        client::handshake::<_, Empty<hyper::body::Bytes>>(TokioIo::new(guest_peer))
            .await
            .unwrap();
    let guest_connection = tokio::spawn(guest_connection);
    assert_eq!(
        send_request(&mut sender, "/simple/", ALLOWED_HOST).await,
        StatusCode::OK
    );
    assert_eq!(
        send_request(
            &mut sender,
            &format!("http://{ALLOWED_HOST}/simple/"),
            CO_TENANT_HOST,
        )
        .await,
        StatusCode::OK,
        "absolute-form authority wins and its value must replace Host upstream"
    );
    assert_eq!(
        send_fastly_host_override(&mut sender).await,
        StatusCode::OK,
        "Fastly routing headers are stripped after the ordinary Host is validated"
    );
    assert_eq!(
        send_request(&mut sender, "/relay", CO_TENANT_HOST).await,
        StatusCode::FORBIDDEN,
        "a refusal terminates this keep-alive connection, so it must be last"
    );
    drop(sender);
    guest_connection.abort();

    let served = relay.await.unwrap().unwrap();
    assert_eq!(
        (
            served.requests,
            served.forwarded,
            served.refused,
            served.upstream_failed,
        ),
        (4, 3, 1, 0)
    );
    assert!(served.wire_tx > 0);
    assert!(served.wire_rx > 0);
    destination.await.unwrap();
    assert_eq!(
        *seen_hosts.lock().await,
        vec![ALLOWED_HOST.to_string(); 3],
        "the co-tenant request must never reach the allowed CDN address"
    );
    assert_eq!(
        seen_fastly_key.load(Ordering::SeqCst),
        1,
        "the guest Fastly-Key credential must survive routing-header stripping"
    );
}

#[tokio::test]
async fn an_unsolicited_upstream_protocol_upgrade_fails_and_closes() {
    let policy = policy();
    let state = ready_state(policy.clone()).await;
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &policy,
    );
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state, cache);
    let (guest, guest_peer) = duplex(PIPE);
    let (upstream, upstream_peer) = duplex(PIPE);
    let destination = tokio::spawn(async move {
        let _ = server::Builder::new()
            .serve_connection(
                TokioIo::new(upstream_peer),
                service_fn(|_request: Request<Incoming>| async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(StatusCode::SWITCHING_PROTOCOLS)
                            .header(header::CONNECTION, "upgrade")
                            .header(header::UPGRADE, "test-protocol")
                            .body(Empty::<hyper::body::Bytes>::new())
                            .unwrap(),
                    )
                }),
            )
            .await;
    });
    let relay = tokio::spawn(serve(guest, binding, move |_| async move {
        Ok::<_, super::super::transport::Error>(upstream)
    }));
    let (mut sender, guest_connection) =
        client::handshake::<_, Empty<hyper::body::Bytes>>(TokioIo::new(guest_peer))
            .await
            .unwrap();
    let guest_connection = tokio::spawn(guest_connection);
    let response = sender
        .send_request(
            Request::builder()
                .uri("/upgrade")
                .header(header::HOST, ALLOWED_HOST)
                .body(Empty::new())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers().get(header::CONNECTION).unwrap(), "close");
    drop(sender);
    guest_connection.abort();
    let served = relay.await.unwrap().unwrap();
    assert_eq!(
        (served.forwarded, served.refused, served.upstream_failed),
        (0, 0, 1)
    );
    destination.await.unwrap();
}

#[tokio::test]
async fn a_dead_upstream_returns_one_retryable_close() {
    let policy = policy();
    let state = ready_state(policy.clone()).await;
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &policy,
    );
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state, cache);
    let (guest, guest_peer) = duplex(PIPE);
    let (upstream, upstream_peer) = duplex(PIPE);
    drop(upstream_peer);
    let relay = tokio::spawn(serve(guest, binding, move |_| async move {
        Ok::<_, super::super::transport::Error>(upstream)
    }));
    let (mut sender, guest_connection) =
        client::handshake::<_, Empty<hyper::body::Bytes>>(TokioIo::new(guest_peer))
            .await
            .unwrap();
    let guest_connection = tokio::spawn(guest_connection);

    let response = sender
        .send_request(
            Request::builder()
                .uri("/simple/")
                .header(header::HOST, ALLOWED_HOST)
                .body(Empty::new())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers().get(header::CONNECTION).unwrap(), "close");
    response.into_body().collect().await.unwrap();
    drop(sender);
    tokio::time::timeout(std::time::Duration::from_secs(1), guest_connection)
        .await
        .expect("Connection: close must end the guest HTTP connection")
        .unwrap()
        .unwrap();

    let served = relay.await.unwrap().unwrap();
    assert_eq!((served.requests, served.upstream_failed), (1, 1));
}

#[tokio::test]
async fn policy_is_rechecked_after_waiting_for_origin_setup() {
    let initial = policy();
    let cache = DnsAllowCache::default();
    cache.observe_response(
        &dns_a_response(ALLOWED_HOST, [93, 184, 216, 34], 60),
        &initial,
    );
    let state = ready_state(initial).await;
    let binding = HostBinding::new("test", DESTINATION.parse().unwrap(), state.clone(), cache);
    let (guest, peer) = duplex(PIPE);
    let (origin, origin_peer) = duplex(PIPE);
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let forwarded = Arc::new(AtomicBool::new(false));
    let observed = forwarded.clone();
    let origin_task = tokio::spawn(async move {
        let _ = server::Builder::new()
            .serve_connection(
                TokioIo::new(origin_peer),
                service_fn(move |_| {
                    observed.store(true, Ordering::SeqCst);
                    async { Ok::<_, Infallible>(Response::new(Empty::<hyper::body::Bytes>::new())) }
                }),
            )
            .await;
    });
    let gateway = tokio::spawn(serve(guest, binding, move |_| async move {
        started.send(()).unwrap();
        release_rx.await.unwrap();
        Ok(origin)
    }));
    let (mut sender, driver) = client::handshake(TokioIo::new(peer)).await.unwrap();
    let client = tokio::spawn(driver);
    let response = tokio::spawn(
        sender.send_request(
            Request::get("/")
                .header(header::HOST, ALLOWED_HOST)
                .body(Empty::<hyper::body::Bytes>::new())
                .unwrap(),
        ),
    );
    started_rx.await.unwrap();
    let previous = state.snapshot().await.installed_policy_ref;
    let expected = EffectiveNetworkPolicyRef::local(
        EffectiveNetworkPolicyRefScope::Sandbox,
        "updated-host-binding-test",
        "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let next_ref = expected.to_string();
    let changed = policy_loader(policy_denying(ALLOWED_HOST));
    let changed: PolicyLoader = Arc::new(move || {
        let result = changed();
        let next_ref = next_ref.clone();
        Box::pin(async move {
            let mut pulled = result.await?;
            pulled.policy_ref = next_ref;
            Ok(pulled)
        })
    });
    let installed = state
        .refresh(changed, Some(expected), NetworkStateReason::ControlRefresh)
        .await;
    assert_ne!(installed.installed_policy_ref, previous);
    release.send(()).unwrap();
    let response = response.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers()[header::CONNECTION], "close");
    assert!(
        !forwarded.load(Ordering::SeqCst),
        "revoked request reached the origin"
    );
    drop(response);
    drop(sender);
    client.abort();
    gateway.await.unwrap().unwrap();
    origin_task.await.unwrap();
}

async fn serve<C, F, Fut, U>(guest: C, binding: HostBinding, connect: F) -> Result<Served, Failure>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnOnce(SocketAddr) -> Fut + Send + 'static,
    Fut: Future<Output = Result<U, super::super::transport::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let shutdown = CancellationToken::new();
    let result = super::serve(guest, binding, connect, shutdown.clone()).await;
    assert!(
        !shutdown.is_cancelled(),
        "HTTP cleanup cancelled its caller"
    );
    result
}
