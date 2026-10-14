//! Host-control policy loading and validation at the source boundary.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pplx_space_events::{
    SandboxEventDetails, SandboxEventSource, SandboxEventType, emit_sandbox_event,
};
use pplx_space_net_common::{
    EffectiveNetworkPolicyRef, EffectiveNetworkPolicyRefParseError, NetworkPolicyV1,
    proto::NetworkPolicyV1 as NetworkPolicyProto,
};
use pplx_space_spaced_common::proto::host_control::{
    GetNetworkPolicyRequest, host_control_client::HostControlClient,
};

use super::credentials::{InvalidNetworkCredentialSnapshot, NetworkCredentialSnapshot};
use crate::stats::ProxyStats;

pub type PolicyLoader = Arc<
    dyn Fn() -> Pin<
            Box<dyn Future<Output = Result<PulledNetworkPolicy, HydrateNetworkStateError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug)]
pub struct PulledNetworkPolicy {
    pub policy: NetworkPolicyV1,
    /// Validated reference; snapshots render its canonical wire representation.
    pub policy_ref: EffectiveNetworkPolicyRef,
    pub network_credentials: NetworkCredentialSnapshot,
}

/// Why a host-control response did not yield a usable V1 network policy.
#[derive(Debug, thiserror::Error)]
pub enum NetworkPolicyFromHostControlError {
    #[error("host-control returned no V1 policy")]
    MissingV1Policy,
    #[error(transparent)]
    InvalidV1Policy(#[from] pplx_space_net_common::NetworkPolicyProtoError),
    #[error("host-control returned invalid network-credential material: {0}")]
    InvalidNetworkCredentials(#[from] InvalidNetworkCredentialSnapshot),
}

/// Why the gateway could not hydrate its policy from host-control. Reported
/// through a sandbox event and, for a refresh, back to the caller as a string,
/// so each variant names the hop that failed.
#[derive(Debug, thiserror::Error)]
pub enum HydrateNetworkStateError {
    #[error("failed to connect to host-control while hydrating policy for {sandbox_id}: {source}")]
    ConnectHostControl {
        sandbox_id: String,
        #[source]
        source: pplx_space_spaced_common::UdsConnectError,
    },
    #[error("failed to pull initial network policy for {sandbox_id} from host-control: {source}")]
    GetNetworkPolicy {
        sandbox_id: String,
        #[source]
        source: tonic::Status,
    },
    #[error(transparent)]
    Policy(#[from] NetworkPolicyFromHostControlError),
    #[error("policy ref is invalid: {source}: {reference}")]
    InvalidReference {
        reference: String,
        #[source]
        source: EffectiveNetworkPolicyRefParseError,
    },
}

fn network_policy_from_host_control_proto(
    policy: Option<&NetworkPolicyProto>,
) -> Result<NetworkPolicyV1, NetworkPolicyFromHostControlError> {
    let policy = policy.ok_or(NetworkPolicyFromHostControlError::MissingV1Policy)?;
    Ok(NetworkPolicyV1::try_from(policy)?)
}

struct InitialNetworkState {
    policy: NetworkPolicyV1,
    network_credentials: NetworkCredentialSnapshot,
    policy_ref: String,
    network_tx_bytes: u64,
    network_rx_bytes: u64,
}

async fn pull_initial_network_state(
    sandbox_id: &str,
) -> Result<InitialNetworkState, HydrateNetworkStateError> {
    let details = || SandboxEventDetails::default().operation("initial_policy_hydration");
    emit_sandbox_event(
        sandbox_id,
        SandboxEventType::NetworkPolicyEnforcementStarted,
        SandboxEventSource::NetGateway,
        details(),
    );
    let result = pull_initial_network_state_inner(sandbox_id).await;
    match &result {
        Ok(state) => emit_sandbox_event(
            sandbox_id,
            SandboxEventType::NetworkPolicyEnforcementCompleted,
            SandboxEventSource::NetGateway,
            details()
                .installed_network_policy_ref(state.policy_ref.clone())
                .result("policy_hydrated"),
        ),
        Err(error) => emit_sandbox_event(
            sandbox_id,
            SandboxEventType::NetworkPolicyEnforcementFailed,
            SandboxEventSource::NetGateway,
            details().error(error.to_string()),
        ),
    }
    result
}

async fn pull_initial_network_state_inner(
    sandbox_id: &str,
) -> Result<InitialNetworkState, HydrateNetworkStateError> {
    let socket = std::path::Path::new(
        pplx_space_common_core::host::linux::paths::SANDBOX_HOST_CONTROL_SOCKET,
    );
    let channel = pplx_space_spaced_common::connect_uds_channel("sandbox-host-control", socket)
        .await
        .map_err(|source| HydrateNetworkStateError::ConnectHostControl {
            sandbox_id: sandbox_id.to_string(),
            source,
        })?;
    let mut client = HostControlClient::new(
        pplx_space_common_svc::bootstrap::wrap_grpc_client_channel(channel),
    );
    let response = client
        .get_network_policy(GetNetworkPolicyRequest {})
        .await
        .map_err(|source| HydrateNetworkStateError::GetNetworkPolicy {
            sandbox_id: sandbox_id.to_string(),
            source,
        })?
        .into_inner();
    let policy = network_policy_from_host_control_proto(response.effective_policy.as_ref())?;
    let network_credentials =
        NetworkCredentialSnapshot::from_wire(response.network_credentials, &policy)
            .map_err(NetworkPolicyFromHostControlError::InvalidNetworkCredentials)?;
    Ok(InitialNetworkState {
        policy,
        network_credentials,
        policy_ref: response.effective_policy_ref,
        network_tx_bytes: response.network_tx_bytes,
        network_rx_bytes: response.network_rx_bytes,
    })
}

pub(crate) fn host_control_policy_loader(
    sandbox_id: String,
    stats: Arc<ProxyStats>,
) -> PolicyLoader {
    Arc::new(move || {
        let sandbox_id = sandbox_id.clone();
        let stats = stats.clone();
        Box::pin(async move {
            let pulled = pull_initial_network_state(&sandbox_id).await?;
            // Persisted accounting belongs to the successful pull even when
            // its policy reference cannot be installed.
            stats.merge_persisted_network_bytes(pulled.network_tx_bytes, pulled.network_rx_bytes);
            pulled.into_policy()
        })
    })
}

impl InitialNetworkState {
    fn into_policy(self) -> Result<PulledNetworkPolicy, HydrateNetworkStateError> {
        let policy_ref = self.policy_ref.parse().map_err(|source| {
            HydrateNetworkStateError::InvalidReference {
                reference: self.policy_ref,
                source,
            }
        })?;
        Ok(PulledNetworkPolicy {
            policy: self.policy,
            policy_ref,
            network_credentials: self.network_credentials,
        })
    }
}

#[cfg(test)]
mod tests;
