//! Preserve the error returned by HTTP setup or serving without interpreting opaque sources.
use super::super::tls::TlsTransportError;
use super::super::{http_host::BindingOutcome, upstream};
use super::{HttpProtocol, forward::AuthorityError};
use crate::certificates::NetworkCredentialAccessError;
use http::{Response, StatusCode, header};
use http_body_util::Empty;
use hyper::body::Bytes;
use std::sync::Arc;

/// Failures before a request has a response. The original transport errors stay typed.
#[derive(Debug, thiserror::Error)]
pub(in crate::tcp_proxy) enum RequestError {
    #[error("HTTP request binding refused ({0:?})")]
    Binding(BindingOutcome),
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    #[error("HTTP credential access refused ({0:?})")]
    Credential(NetworkCredentialAccessError),
    #[error("unsupported HTTP upgrade")]
    UnsupportedUpgrade,
    #[error("credentials cannot be sent over an upgraded connection")]
    CredentialUpgrade,
    #[error("invalid HTTP CONNECT")]
    InvalidConnect,
    #[error("unexpected upstream HTTP upgrade")]
    UnexpectedUpgrade,
    #[error("origin setup failed")]
    Setup(#[from] Arc<upstream::Error>),
    #[error("HTTP request forwarding failed")]
    Upstream(#[from] hyper::Error),
    #[error("HTTP connection authorization ended")]
    Cancelled,
}

impl From<BindingOutcome> for RequestError {
    fn from(error: BindingOutcome) -> Self {
        Self::Binding(error)
    }
}

impl From<NetworkCredentialAccessError> for RequestError {
    fn from(error: NetworkCredentialAccessError) -> Self {
        Self::Credential(error)
    }
}

impl RequestError {
    pub(in crate::tcp_proxy) fn reason(&self) -> &'static str {
        match self {
            Self::Binding(error) => error.as_str(),
            Self::Authority(error) => error.as_str(),
            Self::Credential(error) => error.as_str(),
            Self::UnsupportedUpgrade => "unsupported_upgrade",
            Self::CredentialUpgrade => "credential_upgrade_unsupported",
            Self::InvalidConnect => "invalid_connect",
            Self::UnexpectedUpgrade => "unexpected_upgrade",
            Self::Setup(error) => error.as_str(),
            Self::Upstream(_) => "upstream_http_error",
            Self::Cancelled => "lease_invalidated",
        }
    }

    pub(in crate::tcp_proxy) fn binding_outcome(&self) -> BindingOutcome {
        match self {
            Self::Binding(outcome) => *outcome,
            Self::UnsupportedUpgrade => BindingOutcome::UpgradeUnsupported,
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade => {
                BindingOutcome::UpstreamFailed
            }
            Self::Authority(_)
            | Self::Credential(_)
            | Self::CredentialUpgrade
            | Self::InvalidConnect
            | Self::Cancelled => BindingOutcome::PolicyDenied,
        }
    }

    pub(in crate::tcp_proxy) fn response(&self, protocol: HttpProtocol) -> Response<Empty<Bytes>> {
        let mut response = Response::new(Empty::new());
        *response.status_mut() = match self {
            Self::Binding(error) => error.refusal_status(),
            Self::Credential(error) => error.status(),
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade => StatusCode::BAD_GATEWAY,
            Self::Authority(_)
            | Self::UnsupportedUpgrade
            | Self::CredentialUpgrade
            | Self::InvalidConnect
            | Self::Cancelled => StatusCode::FORBIDDEN,
        };
        if protocol == HttpProtocol::Http1 {
            response
                .headers_mut()
                .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
        }
        response
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::tcp_proxy) enum HttpError {
    #[error("TLS transport failed: {0}")]
    Transport(#[from] TlsTransportError),
    #[error("HTTP operation failed")]
    Protocol(#[from] hyper::Error),
    #[error("HTTP server failed")]
    Server(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl HttpError {
    pub(in crate::tcp_proxy) fn as_str(&self) -> &'static str {
        match self {
            Self::Transport(error) => error.as_str(),
            Self::Protocol(_) | Self::Server(_) => "http_connection_error",
        }
    }
    pub(in crate::tcp_proxy) fn record(&self, stage: &'static str) {
        match self {
            Self::Transport(error) => error.record(stage),
            Self::Protocol(error) => Self::record_protocol(error, stage),
            Self::Server(_) => {
                tracing::info!(stage, reason = self.as_str(), "HTTP connection ended")
            }
        }
    }

    pub(in crate::tcp_proxy) fn record_protocol(error: &hyper::Error, stage: &'static str) {
        tracing::info!(
            stage,
            http_reason = http_reason(error),
            "HTTP operation failed"
        );
    }
}

/// Only Hyper's documented predicates are stable; native codes behind its
/// opaque source chain remain unavailable.
pub(in crate::tcp_proxy) fn http_reason(error: &hyper::Error) -> &'static str {
    if error.is_parse() {
        "parse"
    } else if error.is_timeout() {
        "timeout"
    } else if error.is_canceled() {
        "cancelled"
    } else if error.is_incomplete_message() {
        "incomplete_message"
    } else if error.is_closed() {
        "closed"
    } else if error.is_body_write_aborted() {
        "body_write_aborted"
    } else if error.is_shutdown() {
        "shutdown"
    } else if error.is_user() {
        "user"
    } else {
        "other"
    }
}
