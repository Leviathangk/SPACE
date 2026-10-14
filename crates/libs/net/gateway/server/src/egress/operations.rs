use std::net::SocketAddr;
use std::sync::Arc;

use pplx_space_net_common::{DomainName, EgressDecision, NetworkPolicyV1, TlsInterception};
use tokio_util::sync::CancellationToken;

use super::admission::{self, EgressContext, EgressProtocol, HostBindingError};
use super::attribution::DnsAllowCache;
use super::coordinator::Egress;
use super::evidence::{AdmittedTcp, TcpAdmissionEvidence};
use super::policy::{
    NetworkCredentialSnapshot, NetworkCredentialSnapshotError, NetworkStateSnapshot,
    PolicyNotReady, TrafficPolicy,
};
use super::tcp::{self, TcpAdmissionDenial};
use super::tracker::TcpConnectionGuard;
use crate::tls_sni::{ClientHelloScan, MissingClientHello};

impl Egress {
    pub(crate) async fn snapshot(&self) -> NetworkStateSnapshot {
        self.coordinator.snapshot().await
    }

    pub async fn wait_for_tcp_policy(&self) -> TrafficPolicy {
        self.coordinator.wait_for_tcp_policy().await
    }

    pub(crate) fn is_current_tcp_policy(&self, policy: &Arc<NetworkPolicyV1>) -> bool {
        self.coordinator.is_current_tcp_policy(policy)
    }

    pub(crate) fn with_network_credentials<T>(
        &self,
        policy: &Arc<NetworkPolicyV1>,
        use_credentials: impl FnOnce(&NetworkCredentialSnapshot) -> T,
    ) -> Result<T, NetworkCredentialSnapshotError> {
        self.coordinator
            .with_network_credentials(policy, use_credentials)
    }

    pub(crate) fn missing_client_hello(
        &self,
        destination: SocketAddr,
        policy: &TrafficPolicy,
    ) -> MissingClientHello {
        match policy {
            TrafficPolicy::Ready(policy) => {
                tcp::missing_client_hello(destination, policy, &self.coordinator.dns_allow_cache)
            }
            TrafficPolicy::NotReady(_) => MissingClientHello::Proceeds,
        }
    }

    pub(crate) fn tcp(
        &self,
        destination: SocketAddr,
        scan: &ClientHelloScan,
        policy: Arc<NetworkPolicyV1>,
        cancellation: CancellationToken,
    ) -> Result<TcpPermit, TcpAdmissionDenial> {
        let admission = tcp::admit_tcp(
            destination,
            scan,
            &policy,
            &self.coordinator.dns_allow_cache,
        )?;
        // Register before the caller's final generation check. A concurrent
        // install must see this registration or invalidate that check.
        let registration = self
            .coordinator
            .tcp_connections
            .register(admission.clone(), cancellation);
        Ok(TcpPermit {
            egress: self.clone(),
            policy,
            admission,
            _registration: registration,
        })
    }

    pub(crate) async fn authorize_host(
        &self,
        destination: SocketAddr,
        host: &DomainName,
    ) -> Result<HostAuthorization, HostBindingError> {
        let TrafficPolicy::Ready(policy) = self.wait_for_tcp_policy().await else {
            return Err(HostBindingError::PolicyUnavailable);
        };
        let authorization = HostAuthorization {
            egress: self.clone(),
            policy,
            destination,
            host: host.clone(),
        };
        authorization.recheck()?;
        Ok(authorization)
    }

    pub(crate) async fn dns_tcp(&self) -> Result<DnsOperation, DnsPolicyError> {
        let cancellation = self.coordinator.reserved_dns_cancellation.operation_token();
        let policy = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(DnsPolicyError::Cancelled),
            policy = self.wait_for_tcp_policy() => policy,
        };
        match policy {
            TrafficPolicy::Ready(policy) => Ok(DnsOperation {
                policy,
                cancellation,
                cache: self.coordinator.dns_allow_cache.clone(),
            }),
            TrafficPolicy::NotReady(reason) => Err(DnsPolicyError::NotReady(reason)),
        }
    }

    pub async fn udp(&self) -> Result<UdpPolicy, PolicyNotReady> {
        // Capture the epoch before reading policy, including replayed datagrams.
        let cancellation = self.coordinator.reserved_dns_cancellation.operation_token();
        match self.coordinator.udp_policy().await {
            TrafficPolicy::Ready(policy) => Ok(UdpPolicy(DnsOperation {
                policy,
                cancellation,
                cache: self.coordinator.dns_allow_cache.clone(),
            })),
            TrafficPolicy::NotReady(reason) => Err(reason),
        }
    }

    pub(crate) fn prune_dns_attribution(&self) {
        self.coordinator.dns_allow_cache.prune_expired();
    }
}

/// Exact policy generation and authority selected for one HTTP request.
/// Recheck synchronously after origin readiness and before dispatch.
pub(crate) struct HostAuthorization {
    egress: Egress,
    policy: Arc<NetworkPolicyV1>,
    destination: SocketAddr,
    host: DomainName,
}

impl HostAuthorization {
    pub(crate) fn recheck(&self) -> Result<(), HostBindingError> {
        if !self.egress.is_current_tcp_policy(&self.policy) {
            return Err(HostBindingError::PolicyUnavailable);
        }
        admission::authorize_host(
            &self.policy,
            self.destination,
            &self.host,
            &self.egress.coordinator.dns_allow_cache,
        )
    }
}

/// An admitted TCP generation and its registration share the same owner.
pub(crate) struct TcpPermit {
    egress: Egress,
    policy: Arc<NetworkPolicyV1>,
    admission: TcpAdmissionEvidence,
    _registration: TcpConnectionGuard,
}

impl TcpPermit {
    pub(crate) fn route(&self) -> AdmittedTcp {
        self.admission.route()
    }
    pub(crate) fn interception_mode(&self) -> TlsInterception {
        self.admission.interception_mode()
    }

    pub(crate) fn recheck(&self) -> Result<(), TcpAdmissionDenial> {
        if self.egress.is_current_tcp_policy(&self.policy) {
            Ok(())
        } else {
            Err(TcpAdmissionDenial::PolicyChanged)
        }
    }
}

#[derive(Debug)]
pub(crate) enum DnsPolicyError {
    Cancelled,
    NotReady(PolicyNotReady),
}

/// Policy and cancellation captured together, before any resolver work starts.
pub(crate) struct DnsOperation {
    policy: Arc<NetworkPolicyV1>,
    cancellation: CancellationToken,
    cache: DnsAllowCache,
}

impl DnsOperation {
    pub(crate) fn policy(&self) -> &NetworkPolicyV1 {
        &self.policy
    }
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub(crate) fn admit_destination(
        &self,
        destination: SocketAddr,
        protocol: EgressProtocol,
    ) -> EgressDecision {
        admission::evaluate_egress(
            EgressContext::Admission(protocol),
            destination,
            false,
            &self.policy,
        )
    }

    pub(crate) fn observe_response(&self, wire: &[u8]) -> bool {
        self.cache
            .observe_current_response(wire, &self.policy, &self.cancellation)
    }
}

pub struct UdpPolicy(DnsOperation);

impl UdpPolicy {
    pub(crate) fn admit(&self, destination: SocketAddr) -> EgressDecision {
        admission::admit_udp(&self.0.cache, destination, &self.0.policy)
    }

    pub(crate) fn admit_dns(&self, destination: SocketAddr) -> EgressDecision {
        self.0.admit_destination(destination, EgressProtocol::Udp)
    }

    pub(crate) fn into_dns(self) -> DnsOperation {
        self.0
    }
}
