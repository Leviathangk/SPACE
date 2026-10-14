use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use pplx_space_common_grpc::SandboxTraceContext;
use pplx_space_common_svc::core::guest::linux::network::IPV4_ADDR as GUEST_IPV4_ADDR;
use pplx_space_common_svc::core::types::{GuestPort, SandboxId, SpaceApiRouteIdentity};
use pplx_space_common_svc::space_api_route::{
    SpaceApiRouteIdentityDimension, SpaceApiRouteIdentityMatchError, SpaceApiRouteProtocol,
    SpaceApiRoutePurpose, match_present_route_identity_dimensions,
};
use pplx_space_common_svc::space_api_route_ed25519::{
    SpaceApiRouteEd25519Claims, SpaceApiRouteEd25519Error,
    space_api_route_ed25519_token_from_metadata,
};
use pplx_space_metrics::gateway::{ServicePortIngressCloseReason, ServicePortIngressOpenResult};
use pplx_space_net_gateway_client::SERVICE_PORT_INGRESS_START_TIMEOUT;
use pplx_space_net_gateway_client::proto::{
    NetworkState, ServicePortIngressFrame, ServicePortIngressStart, service_port_ingress_frame,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{Status, metadata::MetadataMap};
use tracing::Instrument;

use crate::execution::ExecutionIdentity;
pub use crate::execution::{
    RouteIdentityParseError, RouteIdentityUpdate, RouteIdentityUpdateError,
    route_identity_from_proto, route_identity_to_proto, status_for_route_identity_parse,
    status_for_route_identity_update,
};
use crate::resource_budget::MAX_ACTIVE_SERVICE_PORT_INGRESS_STREAMS;
use crate::route_verification::RouteVerificationState;
use crate::state::GatewayNetworkState;

const INGRESS_BUFFER_BYTES: usize = 16 * 1024;
const INGRESS_CHANNEL_CAPACITY: usize = 32;
const SERVICE_PORT_INGRESS_GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub use pplx_space_net_gateway_client::{
    DEFAULT_SERVICE_PORT_INGRESS_IDLE_TIMEOUT_SECS, service_port_ingress_idle_timeout_from_env,
};
pub struct ServicePortIngressStream {
    inner: ReceiverStream<Result<ServicePortIngressFrame, Status>>,
    pump: JoinHandle<()>,
    _admission: OwnedSemaphorePermit,
}

impl ServicePortIngressStream {
    fn new(
        inner: ReceiverStream<Result<ServicePortIngressFrame, Status>>,
        pump: JoinHandle<()>,
        admission: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            inner,
            pump,
            _admission: admission,
        }
    }
}

impl Stream for ServicePortIngressStream {
    type Item = Result<ServicePortIngressFrame, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl Drop for ServicePortIngressStream {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

#[derive(Clone, Debug)]
pub struct ServicePortIngressState {
    execution: ExecutionIdentity,
    route_verification: RouteVerificationState,
    stream_slots: Arc<Semaphore>,
}

impl ServicePortIngressState {
    pub fn new(identity: Option<SpaceApiRouteIdentity>) -> Self {
        Self::with_execution(ExecutionIdentity::new(identity))
    }

    pub fn new_for_sandbox(
        trusted_sandbox_id: SandboxId,
        identity: Option<SpaceApiRouteIdentity>,
    ) -> Self {
        Self::with_execution(ExecutionIdentity::new_for_sandbox(
            trusted_sandbox_id,
            identity,
        ))
    }

    fn with_execution(execution: ExecutionIdentity) -> Self {
        Self {
            execution,
            route_verification: RouteVerificationState::default(),
            stream_slots: Arc::new(Semaphore::new(MAX_ACTIVE_SERVICE_PORT_INGRESS_STREAMS)),
        }
    }

    /// The shared execution identity, independent of ingress keys and streams.
    pub fn execution_identity(&self) -> ExecutionIdentity {
        self.execution.clone()
    }

    fn try_acquire_stream_slot(&self) -> Result<OwnedSemaphorePermit, Status> {
        self.stream_slots.clone().try_acquire_owned().map_err(|_| {
            pplx_space_metrics::gateway::service_port_ingress_open(
                ServicePortIngressOpenResult::CapacityExceeded,
            );
            Status::resource_exhausted("too many active service port ingress streams")
        })
    }

    pub fn update_identity(
        &self,
        identity: SpaceApiRouteIdentity,
    ) -> Result<RouteIdentityUpdate, RouteIdentityUpdateError> {
        self.execution.update_identity(identity)
    }

    /// Snapshot the installed identity and an epoch cancelled on replacement.
    pub fn identity_with_epoch(
        &self,
    ) -> Result<(Option<SpaceApiRouteIdentity>, CancellationToken), Status> {
        self.execution.identity_with_epoch().map_err(|_| {
            Status::internal("ServicePortIngress route identity epoch mutex is poisoned")
        })
    }

    pub fn route_verification(&self) -> &RouteVerificationState {
        &self.route_verification
    }
}

impl Default for ServicePortIngressState {
    fn default() -> Self {
        Self::new(None)
    }
}

pub async fn open_service_port_ingress(
    state: ServicePortIngressState,
    network_state: GatewayNetworkState,
    metadata: MetadataMap,
    mut inbound: tonic::Streaming<ServicePortIngressFrame>,
    shutdown_token: CancellationToken,
) -> Result<tonic::Response<ServicePortIngressStream>, Status> {
    let admission = state.try_acquire_stream_slot()?;
    let first = tokio::select! {
        _ = shutdown_token.cancelled() => return Err(Status::cancelled(
            "service port ingress shut down before its start frame",
        )),
        result = tokio::time::timeout(SERVICE_PORT_INGRESS_START_TIMEOUT, inbound.message()) => {
            match result {
                Ok(result) => result?,
                Err(_) => {
                    pplx_space_metrics::gateway::service_port_ingress_open(
                        ServicePortIngressOpenResult::StartTimeout,
                    );
                    return Err(Status::deadline_exceeded(
                        "service port ingress start frame timed out",
                    ));
                }
            }
        }
    }
    .ok_or_else(|| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::InvalidStart,
        );
        Status::invalid_argument("service port ingress start frame is required")
    })?;
    let start = match first.message {
        Some(service_port_ingress_frame::Message::Start(start)) => start,
        _ => {
            pplx_space_metrics::gateway::service_port_ingress_open(
                ServicePortIngressOpenResult::InvalidStart,
            );
            return Err(Status::invalid_argument(
                "service port ingress first frame must be start",
            ));
        }
    };
    // Missing opt-in uses ordinary stream termination for older callers.
    let downstream_completion = metadata
        .get(pplx_space_net_gateway_client::INGRESS_COMPLETION_METADATA)
        .is_some_and(|value| value == "1");
    let (port, identity_token, trace_context) =
        authorize_service_port_ingress(&state, &network_state, &metadata, &start).await?;
    trace_context.record(&tracing::Span::current());
    let upstream = connect_guest_endpoint(port, &trace_context).await?;
    pplx_space_metrics::gateway::service_port_ingress_open(ServicePortIngressOpenResult::Ok);

    let (tx, rx) = tokio::sync::mpsc::channel(INGRESS_CHANNEL_CAPACITY);
    let idle_timeout = service_port_ingress_idle_timeout_from_env();
    let pump = tokio::spawn(async move {
        let result = pump_service_port_ingress(
            inbound,
            upstream,
            tx.clone(),
            shutdown_token,
            identity_token,
            idle_timeout,
            downstream_completion,
        )
        .await;
        pplx_space_metrics::gateway::service_port_ingress_closed(
            service_port_ingress_close_reason(&result),
        );
        if let Err(error) = result {
            let _ = tx.send(Err(error)).await;
        }
    });
    let mut response = tonic::Response::new(ServicePortIngressStream::new(
        ReceiverStream::new(rx),
        pump,
        admission,
    ));
    if downstream_completion {
        response.metadata_mut().insert(
            pplx_space_net_gateway_client::INGRESS_COMPLETION_METADATA,
            tonic::metadata::MetadataValue::from_static("1"),
        );
    }
    Ok(response)
}

async fn authorize_service_port_ingress(
    state: &ServicePortIngressState,
    network_state: &GatewayNetworkState,
    metadata: &MetadataMap,
    start: &ServicePortIngressStart,
) -> Result<(GuestPort, CancellationToken, SandboxTraceContext), Status> {
    let network_snapshot = network_state.snapshot().await;
    // Policy installation only. Interception readiness is deliberately not a
    // condition here: the CA exists to inspect guest egress, and this path
    // relays inbound connections to a guest port without consulting it. An
    // earlier gateway refused ingress whenever the CA was missing, but only
    // because a closed traffic gate demoted this mode to PendingPolicy.
    if network_snapshot.mode != NetworkState::Ready {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::PolicyNotReady,
        );
        return Err(Status::failed_precondition("network policy is not ready"));
    }
    let sandbox_id = start.sandbox_id.parse::<SandboxId>().map_err(|_| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::InvalidStart,
        );
        Status::invalid_argument("sandbox_id must be a UUID")
    })?;
    let port = GuestPort::try_from_u16(u16::try_from(start.port).map_err(|_| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::InvalidStart,
        );
        Status::invalid_argument("service port out of range")
    })?)
    .map_err(|error| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::InvalidStart,
        );
        Status::invalid_argument(format!("service port {error}"))
    })?;
    let (local_identity, identity_token) = state.identity_with_epoch()?;

    let token = space_api_route_ed25519_token_from_metadata(metadata).map_err(|error| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::InvalidToken,
        );
        Status::permission_denied(format!("invalid SPACE API route token: {error}"))
    })?;
    let Some(token) = token else {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::MissingToken,
        );
        return Err(Status::permission_denied(
            "ServicePortIngress requires a SPACE API route token",
        ));
    };
    let claims = state.route_verification().verify(&token).map_err(|error| {
        pplx_space_metrics::gateway::service_port_ingress_open(route_token_ed25519_error_label(
            &error,
        ));
        Status::permission_denied(format!("invalid SPACE API route token: {error}"))
    })?;
    authorize_service_port_ingress_claims(local_identity.as_ref(), sandbox_id, port, &claims)?;
    let mut trace_context = SandboxTraceContext::new(sandbox_id, port);
    if let Some(identity) = local_identity.as_ref() {
        trace_context.route_generation = identity.route_generation;
        trace_context.execution_generation = identity.execution_generation;
        trace_context.current_node_id = identity.current_node_id;
    }
    Ok((port, identity_token, trace_context))
}

fn authorize_service_port_ingress_claims(
    local_identity: Option<&SpaceApiRouteIdentity>,
    sandbox_id: SandboxId,
    port: GuestPort,
    claims: &SpaceApiRouteEd25519Claims,
) -> Result<(), Status> {
    let rejected = |result, message: &str| {
        pplx_space_metrics::gateway::service_port_ingress_open(result);
        Err(Status::permission_denied(message))
    };
    if claims.route_identity.sandbox_id() != sandbox_id {
        return rejected(
            ServicePortIngressOpenResult::SandboxMismatch,
            "ServicePortIngress token targets a different sandbox",
        );
    }
    if claims.purpose != SpaceApiRoutePurpose::ServicePortIngress {
        return rejected(
            ServicePortIngressOpenResult::PurposeMismatch,
            "ServicePortIngress token purpose is not allowed",
        );
    }
    if claims.service_port != Some(port) {
        return rejected(
            ServicePortIngressOpenResult::ServicePortMismatch,
            "ServicePortIngress token targets a different service port",
        );
    }
    if claims.protocol != Some(SpaceApiRouteProtocol::Tcp) {
        return rejected(
            ServicePortIngressOpenResult::InvalidToken,
            "ServicePortIngress token protocol is not allowed",
        );
    }
    let local_identity = local_identity.ok_or_else(|| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::IdentityMissing,
        );
        Status::permission_denied("ServicePortIngress has no local route identity")
    })?;
    match_present_route_identity_dimensions(local_identity, &claims.route_identity).map_err(
        |error| {
            pplx_space_metrics::gateway::service_port_ingress_open(
                service_port_identity_mismatch_result(error),
            );
            Status::permission_denied(format!(
                "ServicePortIngress route identity is not authorized: {error}"
            ))
        },
    )?;
    local_identity.clone().try_into_complete().map_err(|_| {
        pplx_space_metrics::gateway::service_port_ingress_open(
            ServicePortIngressOpenResult::IdentityMissing,
        );
        Status::permission_denied("ServicePortIngress local route identity is incomplete")
    })?;
    Ok(())
}

fn service_port_identity_mismatch_result(
    error: SpaceApiRouteIdentityMatchError,
) -> ServicePortIngressOpenResult {
    match error {
        SpaceApiRouteIdentityMatchError::Mismatch(dimension) => match dimension {
            SpaceApiRouteIdentityDimension::SandboxId => {
                ServicePortIngressOpenResult::SandboxMismatch
            }
            SpaceApiRouteIdentityDimension::RouteGeneration => {
                ServicePortIngressOpenResult::RouteGenerationMismatch
            }
            SpaceApiRouteIdentityDimension::CurrentNodeId => {
                ServicePortIngressOpenResult::NodeMismatch
            }
            SpaceApiRouteIdentityDimension::SubstrateKind => {
                ServicePortIngressOpenResult::SubstrateMismatch
            }
            SpaceApiRouteIdentityDimension::ExecutionGeneration => {
                ServicePortIngressOpenResult::ExecutionGenerationMismatch
            }
            SpaceApiRouteIdentityDimension::ActivationAttemptId => {
                ServicePortIngressOpenResult::ActivationAttemptMismatch
            }
            SpaceApiRouteIdentityDimension::PodUid => ServicePortIngressOpenResult::PodUidMismatch,
        },
    }
}

fn route_token_ed25519_error_label(
    error: &SpaceApiRouteEd25519Error,
) -> ServicePortIngressOpenResult {
    match error {
        SpaceApiRouteEd25519Error::Expired => ServicePortIngressOpenResult::Expired,
        SpaceApiRouteEd25519Error::MissingVerificationKeys => {
            ServicePortIngressOpenResult::VerifierSecretUnset
        }
        _ => ServicePortIngressOpenResult::InvalidToken,
    }
}

async fn connect_guest_endpoint(
    port: GuestPort,
    trace_context: &SandboxTraceContext,
) -> Result<TcpStream, Status> {
    let guest_ip = GUEST_IPV4_ADDR.parse::<IpAddr>().map_err(|error| {
        Status::internal(format!(
            "guest ServicePort ingress address is invalid: {error}"
        ))
    })?;
    let endpoint = SocketAddr::new(guest_ip, port.get().get());
    let span = tracing::info_span!(
        "service_port_ingress.connect_guest",
        sandbox_id = tracing::field::Empty,
        sandbox_port = tracing::field::Empty,
        route_generation = tracing::field::Empty,
        execution_generation = tracing::field::Empty,
        current_node_id = tracing::field::Empty,
            traceparent = tracing::field::Empty,
        failure_origin = tracing::field::Empty,
        failure_reason = tracing::field::Empty,
        otel.kind = "client",
        "operation.name" = "tcp.client.connect",
        network.peer.address = %endpoint.ip(),
        network.peer.port = endpoint.port(),
        otel.status_code = tracing::field::Empty,
        otel.status_description = tracing::field::Empty,
    );
    trace_context.record(&span);
    let result = tokio::time::timeout(
        SERVICE_PORT_INGRESS_GUEST_CONNECT_TIMEOUT,
        TcpStream::connect(endpoint).instrument(span.clone()),
    )
    .await;
    match result {
        Ok(Ok(stream)) => {
            span.record("otel.status_code", "OK");
            Ok(stream)
        }
        Ok(Err(error)) => {
            let status = pplx_space_common_grpc::guest_service_connect_status(&error);
            let refused = pplx_space_common_grpc::record_guest_service_status(&status, &span);
            pplx_space_metrics::gateway::service_port_ingress_open(if refused {
                ServicePortIngressOpenResult::GuestConnectionRefused
            } else {
                ServicePortIngressOpenResult::UpstreamConnectError
            });
            if !refused {
                span.record("otel.status_code", "ERROR");
                span.record("otel.status_description", error.to_string().as_str());
            }
            Err(status)
        }
        Err(_) => {
            pplx_space_metrics::gateway::service_port_ingress_open(
                ServicePortIngressOpenResult::UpstreamConnectError,
            );
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_description", "connect timeout");
            Err(Status::deadline_exceeded(
                "service port ingress connect timed out",
            ))
        }
    }
}

async fn pump_service_port_ingress<I, U>(
    mut inbound: I,
    upstream: U,
    tx: tokio::sync::mpsc::Sender<Result<ServicePortIngressFrame, Status>>,
    shutdown_token: CancellationToken,
    identity_token: CancellationToken,
    idle_timeout: Duration,
    downstream_completion: bool,
) -> Result<ServicePortIngressCloseReason, Status>
where
    I: Stream<Item = Result<ServicePortIngressFrame, Status>> + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let mut buffer = vec![0_u8; INGRESS_BUFFER_BYTES];
    let mut idle_deadline = Instant::now() + idle_timeout;
    let mut read_closed = false;
    let mut write_closed = false;

    loop {
        tokio::select! {
            frame = inbound.next() => {
                let Some(frame) = frame.transpose()? else {
                    let shutdown_result = await_ingress_activity(
                        upstream_write.shutdown(),
                        idle_deadline,
                        &shutdown_token,
                        &identity_token,
                        "service port ingress shutdown timed out",
                    ).await?;
                    let Some(result) = shutdown_result else {
                        return Ok(ServicePortIngressCloseReason::ServiceShutdown);
                    };
                    result.map_err(|error| {
                        Status::unavailable(format!("service port ingress shutdown failed: {error}"))
                    })?;
                    return Ok(ServicePortIngressCloseReason::ClientClosed);
                };
                match frame.message {
                Some(service_port_ingress_frame::Message::Complete(_)) => {
                    return Ok(ServicePortIngressCloseReason::ClientClosed);
                }
                Some(service_port_ingress_frame::Message::Eof(_)) => {
                    if write_closed {
                        return Err(Status::invalid_argument("duplicate ingress EOF"));
                    }
                    let result = await_ingress_activity(upstream_write.shutdown(), idle_deadline,
                        &shutdown_token, &identity_token, "service port ingress half-close timed out").await?;
                    let Some(result) = result else { return Ok(ServicePortIngressCloseReason::ServiceShutdown); };
                    result.map_err(|error| Status::unavailable(error.to_string()))?;
                    write_closed = true;
                }
                Some(service_port_ingress_frame::Message::Data(data)) => {
                    if write_closed {
                        return Err(Status::invalid_argument("ingress data after EOF"));
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let write_result = await_ingress_activity(
                        upstream_write.write_all(&data),
                        idle_deadline,
                        &shutdown_token,
                        &identity_token,
                        "service port ingress write timed out",
                    ).await?;
                    let Some(result) = write_result else {
                        return Ok(ServicePortIngressCloseReason::ServiceShutdown);
                    };
                    result.map_err(|error| {
                        Status::unavailable(format!("service port ingress write failed: {error}"))
                    })?;
                    idle_deadline = Instant::now() + idle_timeout;
                }
                Some(service_port_ingress_frame::Message::Start(_)) => {
                    return Err(Status::invalid_argument(
                        "service port ingress start frame may only appear first",
                    ));
                }
                None => {}
                }
            }
            read = upstream_read.read(&mut buffer), if !read_closed => {
                let read = read.map_err(|error| {
                    Status::unavailable(format!("service port ingress read failed: {error}"))
                })?;
                if read == 0 {
                    if !downstream_completion {
                        return Ok(ServicePortIngressCloseReason::TransportLoss);
                    }
                    read_closed = true;
                    let sent = await_ingress_activity(
                        tx.send(Ok(ServicePortIngressFrame {
                            message: Some(service_port_ingress_frame::Message::Eof(())),
                        })), idle_deadline, &shutdown_token, &identity_token,
                        "service port ingress EOF send timed out",
                    ).await?;
                    if sent.is_none() {
                        return Ok(ServicePortIngressCloseReason::ServiceShutdown);
                    }
                    if sent.is_some_and(|result| result.is_err()) {
                        return Err(Status::cancelled("service port ingress client disconnected"));
                    }
                    continue;
                }
                let Some(data) = buffer.get(..read) else {
                    return Err(Status::internal("invalid service port ingress read"));
                };
                let send_result = await_ingress_activity(
                    tx.send(Ok(ServicePortIngressFrame {
                    message: Some(service_port_ingress_frame::Message::Data(data.to_vec())),
                    })),
                    idle_deadline,
                    &shutdown_token,
                    &identity_token,
                    "service port ingress response send timed out",
                ).await?;
                let Some(result) = send_result else {
                    return Ok(ServicePortIngressCloseReason::ServiceShutdown);
                };
                result.map_err(|_| Status::cancelled("service port ingress client disconnected"))?;
                idle_deadline = Instant::now() + idle_timeout;
            }
            _ = tokio::time::sleep_until(idle_deadline), if !read_closed => {
                return Err(Status::deadline_exceeded("service port ingress stream idle timed out"));
            }
            _ = shutdown_token.cancelled() => {
                return Ok(ServicePortIngressCloseReason::ServiceShutdown);
            }
            _ = identity_token.cancelled() => {
                return Err(Status::aborted(
                    "service port ingress execution identity changed",
                ));
            }
        }
    }
}

fn service_port_ingress_close_reason(
    result: &Result<ServicePortIngressCloseReason, Status>,
) -> ServicePortIngressCloseReason {
    match result {
        Ok(reason) => *reason,
        Err(status) => match status.code() {
            tonic::Code::Aborted => ServicePortIngressCloseReason::IdentityChanged,
            tonic::Code::DeadlineExceeded => ServicePortIngressCloseReason::IdleTimeout,
            tonic::Code::Cancelled => ServicePortIngressCloseReason::ClientClosed,
            tonic::Code::InvalidArgument => ServicePortIngressCloseReason::ProtocolError,
            _ => ServicePortIngressCloseReason::TransportLoss,
        },
    }
}

async fn await_ingress_activity<F, T>(
    operation: F,
    idle_deadline: Instant,
    shutdown_token: &CancellationToken,
    identity_token: &CancellationToken,
    timeout_message: &'static str,
) -> Result<Option<T>, Status>
where
    F: Future<Output = T>,
{
    tokio::select! {
        result = operation => Ok(Some(result)),
        _ = tokio::time::sleep_until(idle_deadline) => {
            Err(Status::deadline_exceeded(timeout_message))
        }
        _ = shutdown_token.cancelled() => Ok(None),
        _ = identity_token.cancelled() => {
            Err(Status::aborted("service port ingress execution identity changed"))
        }
    }
}

#[cfg(test)]
mod tests;
