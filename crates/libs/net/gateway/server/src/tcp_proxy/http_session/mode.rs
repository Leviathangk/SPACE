//! Everything that differs between plaintext host-bound HTTP and inspected
//! (TLS-terminated) HTTP.
//!
//! The forwarding pipeline in `forward` is mode-agnostic: it asks the mode what
//! a request targets, which extra checks apply before and after origin
//! readiness, how the request is rewritten for the origin, and how outcomes
//! are labelled. Only this module matches on [`Mode`].
use std::sync::{Arc, Mutex};

use http::{StatusCode, Version, header, uri::Authority};
use pplx_space_net_common::{DomainName, NetworkPolicyV1};

use super::super::accounting::Attempt;
use super::super::http_protocol::HttpProtocol;
use super::super::routing_headers::SanitizedRequest;
use super::super::tls::TlsTransportError;
use super::binding::{BindingOutcome, HostBinding};
use super::credentials::{credential_headers, inject};
use super::error::{HttpError, RequestError, http_reason};
use super::{Served, connect, upgrade};
use crate::certificates::CertificateLease;

pub(super) enum Mode {
    /// Host-bound HTTP/1 over a TCP flow admitted by a DNS-domain allow.
    Plaintext,
    /// HTTP inside TLS terminated with a certificate lease.
    Inspected(Arc<CertificateLease>),
}

pub(super) enum Kind {
    Request,
    WebSocket(upgrade::Handshake),
    Connect,
}

/// What a request asks to reach, as established before any policy check.
pub(super) struct Target {
    pub(super) host: DomainName,
    /// Canonical plaintext authority the origin request is rewritten to.
    authority: Option<Authority>,
    pub(super) kind: Kind,
}

impl Mode {
    pub(super) fn is_inspected(&self) -> bool {
        matches!(self, Self::Inspected(_))
    }

    /// Resolves when this mode's credentials stop being valid for the session.
    pub(super) async fn invalidated(&self) -> HttpError {
        match self {
            Self::Plaintext => std::future::pending().await,
            Self::Inspected(lease) => {
                lease.invalidated().await;
                TlsTransportError::Invalidated.into()
            }
        }
    }

    /// Parse what the request targets and refuse forms this mode cannot carry.
    /// Runs before policy, so a refused form never consults or reaches policy.
    pub(super) fn target<B>(
        &self,
        request: &mut SanitizedRequest<B>,
    ) -> Result<Target, RequestError> {
        match self {
            Self::Plaintext => {
                let (host, authority) = validated_request_authority(request)
                    .map_err(|()| BindingOutcome::InvalidHost)?;
                if request.method() == http::Method::CONNECT
                    || request.headers().contains_key(header::UPGRADE)
                {
                    return Err(RequestError::UnsupportedUpgrade);
                }
                Ok(Target {
                    host,
                    authority: Some(authority),
                    kind: Kind::Request,
                })
            }
            Self::Inspected(lease) => {
                if !lease.is_usable() {
                    return Err(RequestError::Cancelled);
                }
                if request.method() == http::Method::CONNECT {
                    let target = connect::target(request)
                        .map_err(|()| RequestError::InvalidConnect)?
                        .to_string();
                    *request.uri_mut() =
                        target.parse().map_err(|_| RequestError::InvalidConnect)?;
                    request.set_host(target.parse().map_err(|_| RequestError::InvalidConnect)?);
                    return Ok(Target {
                        host: lease.host().clone(),
                        authority: None,
                        kind: Kind::Connect,
                    });
                }
                authorize_authority(request, lease.host(), lease.port())?;
                let handshake = upgrade::Handshake::request(request)
                    .map_err(|()| RequestError::UnsupportedUpgrade)?;
                Ok(Target {
                    host: lease.host().clone(),
                    authority: None,
                    kind: handshake.map_or(Kind::Request, Kind::WebSocket),
                })
            }
        }
    }

    /// Mode checks that follow a policy allow and precede origin setup.
    pub(super) fn admit<B>(
        &self,
        request: &SanitizedRequest<B>,
        target: &Target,
    ) -> Result<(), RequestError> {
        if let Self::Inspected(lease) = self
            && !matches!(target.kind, Kind::Connect)
        {
            let headers = credential_headers(lease, request)?;
            if matches!(target.kind, Kind::WebSocket(_)) && !headers.is_empty() {
                return Err(RequestError::CredentialUpgrade);
            }
        }
        Ok(())
    }

    /// Final check once the origin is ready. No request headers are submitted
    /// before this passes.
    pub(super) async fn recheck<B>(
        &self,
        binding: &HostBinding,
        target: &Target,
        policy: &Arc<NetworkPolicyV1>,
        request: &mut SanitizedRequest<B>,
    ) -> Result<(), RequestError> {
        match self {
            Self::Plaintext => Ok(binding.reauthorize(&target.host, policy).await?),
            Self::Inspected(lease) => {
                if !lease.is_usable() {
                    return Err(RequestError::Cancelled);
                }
                if !matches!(target.kind, Kind::Connect) {
                    let headers = credential_headers(lease, request)?;
                    inject(request, headers);
                }
                Ok(())
            }
        }
    }

    /// Rewrite request routing for the origin leg.
    pub(super) fn route<B>(
        &self,
        target: &Target,
        request: &mut SanitizedRequest<B>,
        origin_http1: bool,
    ) -> Result<(), RequestError> {
        if let Some(authority) = &target.authority {
            normalize_upstream_routing(request, authority)
                .map_err(|()| BindingOutcome::InvalidHost)?;
        } else if origin_http1 && !matches!(target.kind, Kind::Connect) {
            translate_to_http1(request);
        }
        Ok(())
    }

    /// Inspected HTTP reports per-stage accounting attempts; plaintext reports
    /// per-flow tallies instead.
    pub(super) fn attempt(&self, start: impl FnOnce() -> Attempt) -> Option<Attempt> {
        self.is_inspected().then(start)
    }

    pub(super) fn reason(&self, error: &RequestError) -> &'static str {
        error.reason(self.is_inspected())
    }

    pub(super) fn tally<T>(&self, served: &Mutex<Served>, result: &Result<T, RequestError>) {
        if self.is_inspected() {
            return;
        }
        let mut served = served.lock().expect("HTTP tally lock poisoned");
        served.requests += 1;
        match result {
            Ok(_) => served.forwarded += 1,
            Err(error) if error.is_upstream() => served.upstream_failed += 1,
            Err(_) => served.refused += 1,
        }
    }

    pub(super) fn log_failure(&self, binding: &HostBinding, error: &RequestError) {
        match error {
            RequestError::Setup(error) if !self.is_inspected() => {
                binding.record_setup_failure(error)
            }
            RequestError::Setup(error) => error.record("upstream_setup"),
            RequestError::Upstream(error) => tracing::info!(
                http_reason = http_reason(error),
                "HTTP request forwarding failed"
            ),
            _ => {}
        }
    }

    pub(super) fn record(&self, protocol: HttpProtocol, outcome: &'static str, status: StatusCode) {
        let binding = if self.is_inspected() {
            "tls_http"
        } else {
            "http_host"
        };
        pplx_space_metrics::gateway::domain_binding("tcp", binding, outcome, None);
        tracing::debug!(
            target: super::LOG_TARGET,
            protocol = protocol.as_str(),
            outcome,
            status = status.as_u16(),
            "HTTP request forwarding result"
        );
        if self.is_inspected() && !matches!(outcome, "allowed" | "websocket" | "connect_forwarded")
        {
            tracing::info!(
                target: super::LOG_TARGET,
                protocol = protocol.as_str(),
                outcome,
                status = status.as_u16(),
                "inspected HTTP request refused"
            );
        }
    }
}

#[cfg(test)]
mod tests;

// Plaintext authority: the request names its own host, which policy binds to
// the admitted address.

/// Make the bytes forwarded upstream express the same authority that passed
/// policy. For absolute-form targets RFC 7230 requires a proxy to replace a
/// conflicting `Host`; leaving the guest's original value in place would let a
/// CDN route on unchecked metadata. Known provider-routing headers have already
/// been removed by the sanitized request boundary.
fn normalize_upstream_routing<B>(
    request: &mut SanitizedRequest<B>,
    authority: &Authority,
) -> Result<(), ()> {
    let value = authority.as_str().parse().map_err(|_| ())?;
    request.set_host(value);
    let origin_form = request
        .uri()
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or("/")
        .parse()
        .map_err(|_| ())?;
    *request.uri_mut() = origin_form;
    Ok(())
}

fn canonical_authority(host: &DomainName, port: Option<u16>) -> Result<Authority, ()> {
    match port {
        Some(port) => format!("{}:{port}", host.as_str()),
        None => host.as_str().to_string(),
    }
    .parse()
    .map_err(|_| ())
}

fn validated_request_authority<B>(
    request: &SanitizedRequest<B>,
) -> Result<(DomainName, Authority), ()> {
    let authority = request_authority(request)?;
    let host: DomainName = authority.host().parse().map_err(|_| ())?;
    let authority = canonical_authority(&host, authority.port_u16())?;
    Ok((host, authority))
}

/// Parse the effective HTTP request authority. Absolute-form request-target
/// authority outranks `Host`; accepting the header first would permit a
/// proxy-form request to name an allowed host in the header while targeting a
/// different service. Origin-form requests must carry exactly one `Host`, so
/// the value authorized here cannot differ from one a CDN chooses upstream.
fn request_authority<B>(request: &SanitizedRequest<B>) -> Result<Authority, ()> {
    if let Some(authority) = request.uri().authority() {
        return Ok(authority.clone());
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host = hosts.next().ok_or(())?;
    if hosts.next().is_some() {
        return Err(());
    }
    host.to_str()
        .ok()
        .and_then(|value| value.parse::<Authority>().ok())
        .ok_or(())
}

// Inspected authority: every routing field must name the leased TLS origin.

#[derive(Debug, thiserror::Error)]
pub(super) enum AuthorityError {
    #[error("HTTP request authority is missing or malformed")]
    Invalid,
    #[error("HTTP request authority differs from the admitted TLS origin")]
    Mismatch,
}

impl AuthorityError {
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_authority",
            Self::Mismatch => "authority_mismatch",
        }
    }
}

pub(super) fn authorize_authority<B>(
    request: &SanitizedRequest<B>,
    host: &DomainName,
    port: u16,
) -> Result<(), AuthorityError> {
    let validate = |authority: &Authority| {
        if authority.as_str().contains('@')
            || authority.as_str().ends_with(':')
            || (authority.as_str().contains(':') && authority.port_u16().is_none())
        {
            return Err(AuthorityError::Invalid);
        }
        let name: DomainName = authority
            .host()
            .parse()
            .map_err(|_| AuthorityError::Invalid)?;
        if &name != host || authority.port_u16().unwrap_or(443) != port {
            return Err(AuthorityError::Mismatch);
        }
        Ok(())
    };
    if request
        .uri()
        .scheme_str()
        .is_some_and(|scheme| scheme != "https")
    {
        return Err(AuthorityError::Mismatch);
    }
    if let Some(authority) = request.uri().authority() {
        validate(authority)?;
    } else if request.version() == Version::HTTP_2 {
        return Err(AuthorityError::Invalid);
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    if let Some(value) = hosts.next() {
        if hosts.next().is_some() {
            return Err(AuthorityError::Invalid);
        }
        let authority = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Authority>().ok())
            .ok_or(AuthorityError::Invalid)?;
        validate(&authority)?;
    } else if request.version() != Version::HTTP_2 {
        return Err(AuthorityError::Invalid);
    }
    Ok(())
}

fn translate_to_http1<B>(request: &mut SanitizedRequest<B>) {
    // Preserve the exact wire authority when translating: default ports can
    // change signatures.
    if !request.headers().contains_key(header::HOST) {
        let authority = request
            .uri()
            .authority()
            .expect("validated HTTP/2 authority");
        let host = authority.as_str().parse().expect("validated authority");
        request.set_host(host);
    }
    *request.uri_mut() = request
        .uri()
        .path_and_query()
        .map_or("/", |path| path.as_str())
        .parse()
        .expect("Hyper parsed path");
    *request.version_mut() = Version::HTTP_11;
}
