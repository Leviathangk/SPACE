use super::*;
use crate::egress::test_support::TestNetwork;
use crate::proto::NetworkState;

#[tokio::test]
async fn ingress_stream_holds_admission_until_stream_drop() {
    let state = ServicePortIngressState::new(None);
    let admission = state.try_acquire_stream_slot().expect("first stream slot");
    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    let pump = tokio::spawn(std::future::pending());
    let stream = ServicePortIngressStream::new(ReceiverStream::new(rx), pump, admission);
    let remaining = (1..MAX_ACTIVE_SERVICE_PORT_INGRESS_STREAMS)
        .map(|_| {
            state
                .try_acquire_stream_slot()
                .expect("remaining stream slot")
        })
        .collect::<Vec<_>>();

    let error = state
        .try_acquire_stream_slot()
        .expect_err("stream capacity must be enforced");
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);

    drop(stream);
    let _released_slot = state
        .try_acquire_stream_slot()
        .expect("dropping the response stream releases admission");
    drop(remaining);
}

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::Ed25519KeyPair;
use pplx_space_common_svc::core::types::{
    ClusterId, ExecutionGeneration, NodeId, NodeManagedBy, PodUid, RouteGeneration,
    SandboxActivationAttemptId, SandboxRouteIdentity,
};
use pplx_space_common_svc::space_api_route_ed25519::{
    SpaceApiRouteEd25519Claims, SpaceApiRouteEd25519Issuer, SpaceApiRouteEd25519KeyId,
    SpaceApiRouteEd25519Signer, SpaceApiRouteEd25519VerificationGeneration,
    insert_space_api_route_ed25519_token,
};
use pplx_space_net_common::{EffectiveNetworkPolicyRef, EffectiveNetworkPolicyRefScope};
use tonic::metadata::MetadataMap;

async fn ready_network_state() -> PolicyView {
    let state = TestNetwork::pending_policy();
    let policy_ref = EffectiveNetworkPolicyRef::local(
        EffectiveNetworkPolicyRefScope::Sandbox,
        "sandbox",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .expect("policy ref parses");
    let loader: crate::egress::PolicyLoader = Arc::new(move || {
        let policy_ref = policy_ref.clone();
        Box::pin(async move {
            Ok(crate::egress::PulledNetworkPolicy {
                policy: pplx_space_net_common::NetworkPolicyV1::allow_all(),
                policy_ref,
                network_credentials: Default::default(),
            })
        })
    });
    let snapshot = state
        .refresh(loader, None, crate::egress::NetworkStateReason::Startup)
        .await;
    assert_eq!(snapshot.mode, NetworkState::Ready);
    state.policies()
}

fn test_activation_attempt_id() -> SandboxActivationAttemptId {
    "018f1e9d-7b7a-7d3a-8000-000000000001"
        .parse()
        .expect("activation attempt id parses")
}

fn route_identity(node_id: NodeId, pod_uid: Option<PodUid>) -> SpaceApiRouteIdentity {
    SpaceApiRouteIdentity {
        sandbox_id: None,
        route_generation: Some(RouteGeneration::new(7).expect("route generation parses")),
        current_node_id: Some(node_id),
        substrate_kind: Some(if pod_uid.is_some() {
            NodeManagedBy::Kubernetes
        } else {
            NodeManagedBy::Space
        }),
        execution_generation: Some(
            ExecutionGeneration::new(11).expect("execution generation parses"),
        ),
        pod_uid,
        activation_attempt_id: Some(test_activation_attempt_id()),
    }
}

fn ed25519_signer(key_id: &str) -> SpaceApiRouteEd25519Signer {
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("test Ed25519 key generates");
    SpaceApiRouteEd25519Signer::from_pkcs8(
        SpaceApiRouteEd25519KeyId::parse(key_id).expect("key id parses"),
        pkcs8.as_ref(),
    )
    .expect("test signer constructs")
}

fn complete_route_identity(
    sandbox_id: SandboxId,
    node_id: NodeId,
    pod_uid: Option<PodUid>,
) -> SandboxRouteIdentity {
    let mut identity = route_identity(node_id, pod_uid);
    identity.sandbox_id = Some(sandbox_id);
    identity
        .try_into_complete()
        .expect("test route identity is complete")
}

fn start_for(sandbox_id: SandboxId, port: GuestPort) -> ServicePortIngressStart {
    ServicePortIngressStart {
        sandbox_id: sandbox_id.to_string(),
        port: port.get().get().into(),
    }
}

fn data_frame(data: impl Into<Vec<u8>>) -> ServicePortIngressFrame {
    ServicePortIngressFrame {
        message: Some(service_port_ingress_frame::Message::Data(data.into())),
    }
}

async fn authorize_with(
    identity: Option<SpaceApiRouteIdentity>,
    metadata: MetadataMap,
    start: ServicePortIngressStart,
) -> Result<GuestPort, Status> {
    let network_state = ready_network_state().await;
    let sandbox_id = start
        .sandbox_id
        .parse()
        .ok()
        .or_else(|| identity.as_ref().and_then(|identity| identity.sandbox_id))
        .unwrap_or_else(SandboxId::new);
    let state = ServicePortIngressState::new_for_sandbox(sandbox_id, None);
    if let Some(identity) = identity {
        state
            .update_identity(identity)
            .expect("test route identity installs");
    }
    authorize_service_port_ingress(&state, &network_state, &metadata, &start)
        .await
        .map(|(port, _, _)| port)
}

#[tokio::test]
async fn service_port_ingress_authorizes_ed25519_on_the_full_request_path() {
    if !pplx_space_test_env::enter_test_process(&[
        ("SPACE_API_ROUTE_GENERATION_FENCE_ENABLED", None),
        ("SPACE_NODE_ID", None),
        ("SPACE_NODE_ROLE", None),
    ]) {
        return;
    }

    let sandbox_id = SandboxId::new();
    let node_id = NodeId::new();
    let port = GuestPort::try_from_u16(8080).expect("port parses");
    let route_identity = complete_route_identity(sandbox_id, node_id, None);
    let signer = ed25519_signer("current");
    let claims = SpaceApiRouteEd25519Claims::service_port_ingress(
        SpaceApiRouteEd25519Issuer::parse("cluster-api").unwrap(),
        ClusterId::parse("testing").expect("cluster id parses"),
        route_identity.clone(),
        port,
        SpaceApiRouteProtocol::Tcp,
    )
    .expect("claims construct");
    let token = signer.mint(&claims).expect("token mints");
    let mut metadata = MetadataMap::new();
    insert_space_api_route_ed25519_token(&mut metadata, &token).expect("metadata accepts token");
    let state = ServicePortIngressState::new_for_sandbox(sandbox_id, Some(route_identity.into()));
    state
        .route_verification()
        .update(
            SpaceApiRouteEd25519VerificationGeneration::new(1).unwrap(),
            SpaceApiRouteEd25519Issuer::parse("cluster-api").unwrap(),
            ClusterId::parse("testing").expect("cluster id parses"),
            vec![signer.public_key()],
        )
        .expect("verification key installs");

    authorize_service_port_ingress(
        &state,
        &ready_network_state().await,
        &metadata,
        &start_for(sandbox_id, port),
    )
    .await
    .expect("Ed25519 request authorizes");
}

#[tokio::test]
async fn service_port_ingress_rejects_missing_token() {
    if !pplx_space_test_env::enter_test_process(&[
        ("SPACE_API_ROUTE_GENERATION_FENCE_ENABLED", None),
        ("SPACE_NODE_ID", None),
        ("SPACE_NODE_ROLE", None),
    ]) {
        return;
    }

    let sandbox_id = SandboxId::new();
    let node_id = NodeId::new();
    let port = GuestPort::try_from_u16(8080).expect("port parses");
    let error = authorize_with(
        Some(route_identity(node_id, None)),
        MetadataMap::new(),
        start_for(sandbox_id, port),
    )
    .await
    .expect_err("missing token rejects");

    assert_eq!(error.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn service_port_ingress_rejects_malformed_start_fields() {
    if !pplx_space_test_env::enter_test_process(&[
        ("SPACE_API_ROUTE_GENERATION_FENCE_ENABLED", None),
        ("SPACE_NODE_ID", None),
        ("SPACE_NODE_ROLE", None),
    ]) {
        return;
    }

    let sandbox_id = SandboxId::new();
    let node_id = NodeId::new();
    let port = GuestPort::try_from_u16(8080).expect("port parses");
    let metadata = MetadataMap::new();
    let identity = route_identity(node_id, None);

    let invalid_sandbox = authorize_with(
        Some(identity.clone()),
        metadata.clone(),
        ServicePortIngressStart {
            sandbox_id: "not-a-uuid".to_string(),
            port: port.get().get().into(),
        },
    )
    .await
    .expect_err("invalid sandbox id rejects");
    assert_eq!(invalid_sandbox.code(), tonic::Code::InvalidArgument);

    let invalid_port = authorize_with(
        Some(identity),
        metadata,
        ServicePortIngressStart {
            sandbox_id: sandbox_id.to_string(),
            port: 0,
        },
    )
    .await
    .expect_err("invalid port rejects");
    assert_eq!(invalid_port.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn service_port_ingress_idle_timeout_closes_stream() {
    let (_inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let (upstream, _peer) = tokio::io::duplex(64);
    let (outbound_tx, _outbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let shutdown = CancellationToken::new();

    let task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound_rx),
        upstream,
        outbound_tx,
        shutdown,
        CancellationToken::new(),
        Duration::from_millis(10),
        true,
    ));

    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("idle stream should close promptly")
        .expect("pump task should not panic")
        .expect_err("idle stream should return a timeout");

    assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
}

#[test]
fn service_port_ingress_close_reasons_are_bounded_and_semantic() {
    assert_eq!(
        service_port_ingress_close_reason(&Err(Status::aborted("identity changed"))),
        ServicePortIngressCloseReason::IdentityChanged
    );
    assert_eq!(
        service_port_ingress_close_reason(&Err(Status::deadline_exceeded("idle"))),
        ServicePortIngressCloseReason::IdleTimeout
    );
    assert_eq!(
        service_port_ingress_close_reason(&Err(Status::unavailable("transport"))),
        ServicePortIngressCloseReason::TransportLoss
    );
    assert_eq!(
        service_port_ingress_close_reason(&Err(Status::invalid_argument("protocol"))),
        ServicePortIngressCloseReason::ProtocolError
    );
    assert_eq!(
        service_port_ingress_close_reason(&Ok(ServicePortIngressCloseReason::ServiceShutdown,)),
        ServicePortIngressCloseReason::ServiceShutdown
    );
}

#[tokio::test]
async fn service_port_ingress_identity_change_aborts_active_stream() {
    let (_inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let (upstream, _peer) = tokio::io::duplex(64);
    let (outbound_tx, _outbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let identity_token = CancellationToken::new();

    let task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound_rx),
        upstream,
        outbound_tx,
        CancellationToken::new(),
        identity_token.clone(),
        Duration::from_secs(60),
        true,
    ));
    identity_token.cancel();

    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("identity change must close the stream promptly")
        .expect("pump task should not panic")
        .expect_err("identity change must abort the stream");
    assert_eq!(error.code(), tonic::Code::Aborted);
}

#[tokio::test]
async fn service_port_ingress_idle_timeout_resets_on_bidirectional_traffic() {
    let (inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(4);
    let (upstream, mut peer) = tokio::io::duplex(64);
    let (outbound_tx, mut outbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(4);
    let shutdown = CancellationToken::new();
    let mut task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound_rx),
        upstream,
        outbound_tx,
        shutdown,
        CancellationToken::new(),
        Duration::from_millis(200),
        true,
    ));

    tokio::time::sleep(Duration::from_millis(120)).await;
    inbound_tx
        .send(Ok(data_frame("client")))
        .await
        .expect("inbound frame sends");
    let mut buffer = [0_u8; 16];
    let read = peer
        .read(&mut buffer)
        .await
        .expect("peer reads inbound data");
    assert_eq!(&buffer[..read], b"client");

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut task)
            .await
            .is_err(),
        "client-to-guest traffic should refresh the idle deadline"
    );

    peer.write_all(b"guest")
        .await
        .expect("peer writes upstream data");
    let frame = outbound_rx
        .recv()
        .await
        .expect("outbound frame arrives")
        .expect("outbound frame is ok");
    let Some(service_port_ingress_frame::Message::Data(data)) = frame.message else {
        panic!("expected data frame");
    };
    assert_eq!(data, b"guest");

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut task)
            .await
            .is_err(),
        "guest-to-client traffic should refresh the idle deadline"
    );

    drop(inbound_tx);
    drop(peer);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("closed stream should finish promptly")
        .expect("pump task should not panic")
        .expect("closed stream should finish cleanly");
}

#[tokio::test]
async fn service_port_ingress_idle_timeout_closes_when_response_channel_is_backpressured() {
    let (_inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let (upstream, mut peer) = tokio::io::duplex(64);
    let (outbound_tx, _outbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let outbound_probe = outbound_tx.clone();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound_rx),
        upstream,
        outbound_tx,
        shutdown,
        CancellationToken::new(),
        Duration::from_millis(20),
        true,
    ));

    peer.write_all(b"first")
        .await
        .expect("first upstream write fits");
    tokio::time::timeout(Duration::from_secs(1), async {
        while outbound_probe.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first upstream frame should fill response channel");
    peer.write_all(b"second")
        .await
        .expect("second upstream write reaches pump");

    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("backpressured response channel should time out promptly")
        .expect("pump task should not panic")
        .expect_err("blocked response send should return idle timeout");
    assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
    assert!(error.message().contains("response send timed out"));
}

#[tokio::test]
async fn service_port_ingress_idle_timeout_closes_when_guest_write_is_backpressured() {
    let (inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let (upstream, _peer) = tokio::io::duplex(64);
    let (outbound_tx, _outbound_rx) =
        tokio::sync::mpsc::channel::<Result<ServicePortIngressFrame, Status>>(1);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound_rx),
        upstream,
        outbound_tx,
        shutdown,
        CancellationToken::new(),
        Duration::from_millis(20),
        true,
    ));

    inbound_tx
        .send(Ok(data_frame(vec![b'x'; 1024])))
        .await
        .expect("inbound data sends");

    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("backpressured guest write should time out promptly")
        .expect("pump task should not panic")
        .expect_err("blocked guest write should return idle timeout");
    assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
    assert!(error.message().contains("write timed out"));
}

#[tokio::test]
async fn guest_eof_keeps_completion_control_and_opposite_direction_alive() {
    let (input, rx) = tokio::sync::mpsc::channel(4);
    let (upstream, mut guest) = tokio::io::duplex(64);
    let (output, mut response) = tokio::sync::mpsc::channel(4);
    let task = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(rx),
        upstream,
        output,
        CancellationToken::new(),
        CancellationToken::new(),
        Duration::from_secs(30),
        true,
    ));
    guest.write_all(b"tail").await.unwrap();
    guest.shutdown().await.unwrap();
    assert!(matches!(
        response.recv().await.unwrap().unwrap().message,
        Some(service_port_ingress_frame::Message::Data(_))
    ));
    assert!(matches!(
        response.recv().await.unwrap().unwrap().message,
        Some(service_port_ingress_frame::Message::Eof(_))
    ));
    assert!(!task.is_finished());
    input.send(Ok(data_frame("upload"))).await.unwrap();
    let mut data = [0; 6];
    guest.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"upload");
    input
        .send(Ok(ServicePortIngressFrame {
            message: Some(service_port_ingress_frame::Message::Complete(())),
        }))
        .await
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap(),
        ServicePortIngressCloseReason::ClientClosed
    );
}

#[tokio::test]
async fn unnegotiated_ingress_closes_on_guest_eof_without_control_frames() {
    let (_input, inbound) = tokio::sync::mpsc::channel(1);
    let (upstream, mut guest) = tokio::io::duplex(64);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let pump = tokio::spawn(pump_service_port_ingress(
        ReceiverStream::new(inbound),
        upstream,
        tx,
        CancellationToken::new(),
        CancellationToken::new(),
        Duration::from_secs(30),
        false,
    ));
    guest.write_all(b"legacy response").await.unwrap();
    guest.shutdown().await.unwrap();
    let mut bytes = Vec::new();
    while let Some(frame) = rx.recv().await {
        match frame.unwrap().message {
            Some(service_port_ingress_frame::Message::Data(data)) => bytes.extend(data),
            other => panic!("legacy peer received a control frame: {other:?}"),
        }
    }
    assert_eq!(bytes, b"legacy response");
    assert_eq!(
        pump.await.unwrap().unwrap(),
        ServicePortIngressCloseReason::TransportLoss
    );
}
