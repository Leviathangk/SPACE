use crate::proto::{NetworkPolicyErrorKind, NetworkState};
use pplx_space_net_common::{EffectiveNetworkPolicyRef, NetworkPolicyV1, TlsInterception};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod credentials;
pub use credentials::{InvalidNetworkCredentialSnapshot, NetworkCredentialSnapshot};
mod source;
pub(crate) use source::host_control_policy_loader;
pub use source::{
    HydrateNetworkStateError, NetworkPolicyFromHostControlError, PolicyLoader, PulledNetworkPolicy,
};
mod lifecycle;
pub(super) use lifecycle::{HydrationFailure, InstalledPolicy, PolicyLifecycle};

pub type ReadinessObserver = Arc<dyn Fn(bool) + Send + Sync>;

// Credential-bearing policy hydration includes the node-agent and authority
// provider budgets. The state stays fail-closed while this deadline runs.
pub const HYDRATION_DEADLINE: Duration = Duration::from_secs(30);
pub const TCP_PENDING_TIMEOUT: Duration = HYDRATION_DEADLINE.saturating_add(Duration::from_secs(1));
pub const TCP_PENDING_WARN_AFTER: Duration = Duration::from_millis(500);
pub(crate) const INITIAL_HYDRATION_RETRY_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const READY_HYDRATION_RECHECK_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const POLICY_REF_MISMATCH_RECHECK_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const MAX_HYDRATION_RETRY_BACKOFF: Duration = Duration::from_secs(5);
pub(crate) const MAX_SELF_HYDRATION_RETRIES: u64 = 12;

#[derive(Clone, Debug)]
pub struct NetworkStateSnapshot {
    /// Policy lifecycle only. `Ready` means a policy is installed; it says
    /// nothing about TLS-interception dependencies (see
    /// [`Self::interception_readiness`]), so a missing CA never demotes a
    /// Ready gateway to `PendingPolicy` for its control-plane peers.
    pub mode: NetworkState,
    pub policy: Option<Arc<NetworkPolicyV1>>,
    pub installed_policy_ref: String,
    pub last_error: String,
    pub last_error_kind: NetworkPolicyErrorKind,
    pub last_hydration_latency_ms: u64,
    pub last_transition_unix_ms: u64,
    pub pending_tcp_connections: u64,
    pub current_retry_backoff_ms: u64,
    pub consecutive_hydration_failures: u64,
    /// Whether guest traffic under the installed policy can currently be
    /// admitted with respect to TLS-interception dependencies. Reported
    /// separately from `mode` so sandbox start never depends on it.
    pub interception_readiness: InterceptionReadiness,
}

/// Readiness of the gateway CA relative to the installed policy. Orthogonal to [`NetworkState`]:
/// a gateway is `Ready` with `DependenciesNotReady` while it denies traffic
/// until the CA is available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterceptionReadiness {
    /// No policy is installed, or the installed policy has TLS interception
    /// off. Nothing beyond policy hydration gates admission.
    NotRequired,
    /// The installed policy has TLS interception on and every dependency
    /// behind the traffic gate is usable.
    Ready,
    /// The installed policy has TLS interception on and the gateway CA is not
    /// usable yet. New TCP and UDP traffic is denied.
    DependenciesNotReady,
}

impl InterceptionReadiness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Ready => "ready",
            Self::DependenciesNotReady => "dependencies_not_ready",
        }
    }

    /// Whether admission is currently blocked on interception dependencies.
    pub const fn blocks_admission(self) -> bool {
        matches!(self, Self::DependenciesNotReady)
    }
}

impl std::fmt::Display for InterceptionReadiness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str((*self).as_str())
    }
}

#[derive(Clone, Debug)]
pub enum TrafficPolicy {
    Ready(Arc<NetworkPolicyV1>),
    NotReady(PolicyNotReady),
}

/// Why no policy was available to evaluate a flow against. Doubles as the
/// `decision` tag on `gateway.policy.decision` for flows denied this way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyNotReady {
    InterceptionNotReady,
    PendingPolicyTimeout,
    NoIdentity,
    PolicyError,
    Draining,
    Stopped,
    /// Reached only by the UDP path, which buffers rather than drops here.
    PendingPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NetworkCredentialSnapshotError {
    TemporarilyUnavailable,
    Stale,
}

impl PolicyNotReady {
    /// Interception refusal has an installed TLS-On policy; other unavailable
    /// states have no policy mode for decision accounting.
    pub(crate) fn interception_label(self) -> &'static str {
        match self {
            Self::InterceptionNotReady => TlsInterception::On.as_str(),
            _ => "unknown",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InterceptionNotReady => "interception_not_ready",
            Self::PendingPolicyTimeout => "pending_policy_timeout",
            Self::NoIdentity => "no_identity",
            Self::PolicyError => "policy_error",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
            Self::PendingPolicy => "pending_policy",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct NetworkStateInner {
    pub(super) lifecycle: PolicyLifecycle,
    pub(super) diagnostics: HydrationDiagnostics,
}

#[derive(Clone, Debug, Default)]
pub(super) struct HydrationDiagnostics {
    // Retained after policy removal for control-plane diagnostics only.
    pub(super) installed_policy_ref: Option<EffectiveNetworkPolicyRef>,
    pub(super) last_hydration_latency_ms: u64,
    pub(super) last_transition_unix_ms: u64,
    pub(super) current_retry_backoff_ms: u64,
    pub(super) consecutive_hydration_failures: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkStateReason {
    Startup,
    SelfRetry,
    ControlRefresh,
    PoolAdoption,
    PoolAdoptionRollback,
    Reconcile,
    SnapshotRestore,
    Start,
    NoIdentity,
    Shutdown,
}

impl NetworkStateReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::SelfRetry => "self_retry",
            Self::ControlRefresh => "control_refresh",
            Self::PoolAdoption => "pool_adoption",
            Self::PoolAdoptionRollback => "pool_adoption_rollback",
            Self::Reconcile => "reconcile",
            Self::SnapshotRestore => "snapshot_restore",
            Self::Start => "start",
            Self::NoIdentity => "no_identity",
            Self::Shutdown => "shutdown",
        }
    }

    pub fn from_external(value: &str) -> Self {
        match value {
            "pool-adoption" | "pool_adoption" => Self::PoolAdoption,
            "pool-adoption-rollback"
            | "pool_adoption_rollback"
            | "pool-rollback"
            | "pool_rollback" => Self::PoolAdoptionRollback,
            "reconcile" => Self::Reconcile,
            "snapshot-restore" | "snapshot_restore" => Self::SnapshotRestore,
            "start" => Self::Start,
            "no_identity" | "no-identity" => Self::NoIdentity,
            "shutdown" => Self::Shutdown,
            "startup" => Self::Startup,
            "self_retry" | "self-retry" => Self::SelfRetry,
            _ => Self::ControlRefresh,
        }
    }
}

impl std::fmt::Display for NetworkStateReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str((*self).as_str())
    }
}

/// Policy observation without ownership of traffic, mutation, or CA readiness.
#[derive(Clone)]
pub struct PolicyView {
    pub(super) state_rx: watch::Receiver<Arc<NetworkStateInner>>,
}

impl PolicyView {
    pub(crate) fn is_ready(&self) -> bool {
        self.state_rx.borrow().lifecycle.ready().is_some()
    }

    /// Keep CA preparation dormant until an installed policy requests it.
    pub(crate) async fn wait_for_interception_policy(&self, shutdown: &CancellationToken) -> bool {
        let mut state = self.state_rx.clone();
        tokio::select! {
            biased;
            () = shutdown.cancelled() => false,
            installed = state.wait_for(|inner| {
                inner.lifecycle.ready().is_some_and(|installed| {
                    installed.policy.tls_interception == TlsInterception::On
                })
            }) => installed.is_ok(),
        }
    }
}
