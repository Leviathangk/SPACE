//! Fixtures needing both policy control and traffic admission use one owner.
use std::ops::Deref;

pub(crate) use super::admission::{admit_udp, evaluate_egress};
pub(crate) use super::attribution::{DnsAllowCache, test_fixtures};
pub(crate) use super::tcp::admit_tcp;
pub(crate) use super::tracker::DnsAttributedTcpTracker;
use super::{
    Egress, GatewayNetwork, InitialPolicyState, PolicyControl, PolicyView, TrafficGate,
    TrafficPolicy,
};

#[derive(Clone)]
pub(crate) struct TestNetwork {
    network: GatewayNetwork,
    control: PolicyControl,
}

impl TestNetwork {
    pub(crate) fn pending_policy() -> Self {
        Self::new(InitialPolicyState::PendingPolicy, None)
    }
    pub(crate) fn no_identity() -> Self {
        Self::new(InitialPolicyState::NoIdentity, None)
    }
    pub(crate) fn with_gate(gate: TrafficGate) -> Self {
        Self::new(InitialPolicyState::PendingPolicy, Some(gate))
    }

    fn new(initial: InitialPolicyState, gate: Option<TrafficGate>) -> Self {
        let network = GatewayNetwork::new(initial, gate);
        let control = network.control();
        Self { network, control }
    }

    pub(crate) fn egress(&self) -> Egress {
        self.network.egress()
    }
    pub(crate) fn control(&self) -> PolicyControl {
        self.control.clone()
    }
    pub(crate) fn policies(&self) -> PolicyView {
        self.network.policies()
    }
    pub(crate) fn cache(&self) -> &DnsAllowCache {
        &self.network.coordinator.dns_allow_cache
    }
    pub(crate) async fn wait_for_tcp_policy(&self) -> TrafficPolicy {
        self.egress().wait_for_tcp_policy().await
    }
}

impl Deref for TestNetwork {
    type Target = PolicyControl;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

impl Egress {
    pub(crate) fn test_cache(&self) -> &DnsAllowCache {
        &self.coordinator.dns_allow_cache
    }
}
