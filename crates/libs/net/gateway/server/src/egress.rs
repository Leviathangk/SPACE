//! One constructed owner of policy, DNS attribution, and admitted connections.

mod admission;
mod attribution;
mod coordinator;
mod evidence;
mod operations;
mod policy;
mod startup_timing;
mod tcp;
mod tracker;

pub use admission::{EgressContext, EgressPhase, EgressProtocol};
pub(crate) use admission::{HostBindingError, allows_dns_query};
pub use coordinator::{Egress, GatewayNetwork, InitialPolicyState, PolicyControl, TrafficGate};
pub(crate) use evidence::AdmittedTcp;
pub use operations::UdpPolicy;
pub(crate) use operations::{DnsOperation, DnsPolicyError, HostAuthorization};
pub use policy::{
    HYDRATION_DEADLINE, HydrateNetworkStateError, InterceptionReadiness,
    InvalidNetworkCredentialSnapshot, NetworkCredentialSnapshot, NetworkPolicyFromHostControlError,
    NetworkStateReason, NetworkStateSnapshot, PolicyLoader, PolicyNotReady, PolicyView,
    PulledNetworkPolicy, ReadinessObserver, TCP_PENDING_TIMEOUT, TCP_PENDING_WARN_AFTER,
    TrafficPolicy,
};
pub(crate) use policy::{NetworkCredentialSnapshotError, host_control_policy_loader};
pub(crate) use tcp::TcpAdmissionDenial;

#[cfg(test)]
pub(crate) mod test_support;
