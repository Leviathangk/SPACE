//! Immutable facts selected while admitting a TCP flow.

use std::net::SocketAddr;

use pplx_space_net_common::{DomainName, TlsInterception};

use crate::tls_sni::ClientHelloScan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmittedTcp {
    Tunnel(SocketAddr),
    HostBoundHttp(SocketAddr),
}

impl AdmittedTcp {
    pub(crate) const fn destination(self) -> SocketAddr {
        match self {
            Self::Tunnel(destination) | Self::HostBoundHttp(destination) => destination,
        }
    }
}

/// Authorization evidence retained for a connection's entire lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TcpAdmissionEvidence {
    route: AdmittedTcp,
    // Only NotTls proves plaintext; incomplete or missing names can still
    // become opaque TLS when interception is off.
    scan: ClientHelloScan,
    // The effective mode after destination exemptions fixes this transport.
    interception_mode: TlsInterception,
    // DNS expiry limits new admissions, not an established exempt flow.
    exempt_host: Option<DomainName>,
}

impl TcpAdmissionEvidence {
    pub(super) fn new(
        route: AdmittedTcp,
        scan: ClientHelloScan,
        interception_mode: TlsInterception,
        exempt_host: Option<DomainName>,
    ) -> Self {
        Self {
            route,
            scan,
            interception_mode,
            exempt_host,
        }
    }

    pub(crate) const fn route(&self) -> AdmittedTcp {
        self.route
    }

    pub(super) const fn destination(&self) -> SocketAddr {
        self.route.destination()
    }

    pub(super) fn scan(&self) -> &ClientHelloScan {
        &self.scan
    }

    pub(crate) const fn interception_mode(&self) -> TlsInterception {
        self.interception_mode
    }

    pub(super) fn exempt_host(&self) -> Option<&DomainName> {
        self.exempt_host.as_ref()
    }
}
