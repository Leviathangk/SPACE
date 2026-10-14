//! Preserve the error returned by HTTP setup or serving without interpreting opaque sources.
use super::super::tls::TlsTransportError;

#[derive(Debug, thiserror::Error)]
pub(in crate::tcp_proxy) enum HttpError {
    #[error("TLS transport failed: {0}")]
    Transport(#[from] TlsTransportError),
    #[error("HTTP operation failed")]
    Protocol(#[from] hyper::Error),
    #[error("HTTP session cancelled")]
    Cancelled,
    #[error("HTTP server failed")]
    Server(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl HttpError {
    pub(in crate::tcp_proxy) fn as_str(&self) -> &'static str {
        match self {
            Self::Transport(error) => error.as_str(),
            Self::Cancelled => "session_ended",
            Self::Protocol(_) | Self::Server(_) => "http_connection_error",
        }
    }
    pub(in crate::tcp_proxy) fn record(&self, stage: &'static str) {
        match self {
            Self::Transport(error) => error.record(stage),
            Self::Protocol(error) => tracing::info!(
                stage,
                http_reason = http_reason(error),
                "HTTP operation failed"
            ),
            Self::Server(_) | Self::Cancelled => {
                tracing::info!(stage, reason = self.as_str(), "HTTP connection ended")
            }
        }
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

use super::super::{http_host::BindingOutcome, upstream};
use super::{Outcome, forward::AuthorityError};
use crate::certificates::NetworkCredentialAccessError;
use http::StatusCode;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub(super) enum RequestError {
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
    pub(super) fn reason(&self, inspected: bool) -> &'static str {
        match self {
            Self::Binding(error) => error.as_str(),
            Self::Authority(error) => error.as_str(),
            Self::Credential(error) => error.as_str(),
            Self::UnsupportedUpgrade if inspected => "unsupported_upgrade",
            Self::UnsupportedUpgrade => "upgrade_unsupported",
            Self::CredentialUpgrade => "credential_upgrade_unsupported",
            Self::InvalidConnect => "invalid_connect",
            Self::UnexpectedUpgrade if inspected => "unexpected_upgrade",
            Self::Setup(error) if inspected => error.as_str(),
            Self::Upstream(_) if inspected => "upstream_http_error",
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade => "upstream_failed",
            Self::Cancelled => "lease_invalidated",
        }
    }
    pub(super) fn status(&self) -> StatusCode {
        match self {
            Self::Binding(error) => error.refusal_status(),
            Self::Credential(error) => error.status(),
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade => StatusCode::BAD_GATEWAY,
            _ => StatusCode::FORBIDDEN,
        }
    }
    pub(super) fn is_upstream(&self) -> bool {
        matches!(
            self,
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade
        )
    }
    pub(super) fn outcome(&self) -> Outcome {
        match self {
            Self::Setup(_) | Self::Upstream(_) | Self::UnexpectedUpgrade => Outcome::Error,
            Self::Cancelled | Self::Credential(NetworkCredentialAccessError::Stale) => {
                Outcome::Cancelled
            }
            _ => Outcome::Denied,
        }
    }
}
