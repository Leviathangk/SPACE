//! Native failure details shared by transport and HTTP diagnostics.
//!
//! The observer supplies the peer and operation. An error code alone does not
//! establish who closed a connection, or whether a request completed. Keep
//! native errors borrowed here; only bounded labels leave the logging methods.

use std::io;

use super::tls::TlsTransportError;

#[derive(Default)]
pub(super) struct ErrorDetails<'a> {
    pub(super) io: Option<&'a io::Error>,
    pub(super) tls: Option<&'a rustls::Error>,
    pub(super) h2: Option<&'a h2::Error>,
    pub(super) http: Option<HttpReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HttpReason {
    Parse,
    Timeout,
    Cancelled,
    IncompleteMessage,
    Closed,
    BodyWriteAborted,
    Shutdown,
    User,
    H2,
    Io,
    Other,
}

impl HttpReason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::IncompleteMessage => "incomplete_message",
            Self::Closed => "closed",
            Self::BodyWriteAborted => "body_write_aborted",
            Self::Shutdown => "shutdown",
            Self::User => "user",
            Self::H2 => "h2",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}

impl<'a> From<&'a io::Error> for ErrorDetails<'a> {
    fn from(error: &'a io::Error) -> Self {
        Self {
            io: Some(error),
            tls: error.get_ref().and_then(|inner| inner.downcast_ref()),
            ..Self::default()
        }
    }
}

impl<'a> From<&'a hyper::Error> for ErrorDetails<'a> {
    fn from(error: &'a hyper::Error) -> Self {
        let h2 = cause::<h2::Error>(error);
        // Prefer the first socket cause; h2 can retain I/O outside its source chain.
        let io = cause::<io::Error>(error).or_else(|| h2.and_then(h2::Error::get_io));
        // Preserve Hyper's predicate precedence over the underlying protocol.
        let http = if error.is_parse() {
            HttpReason::Parse
        } else if error.is_timeout() {
            HttpReason::Timeout
        } else if error.is_canceled() {
            HttpReason::Cancelled
        } else if error.is_incomplete_message() {
            HttpReason::IncompleteMessage
        } else if error.is_closed() {
            HttpReason::Closed
        } else if error.is_body_write_aborted() {
            HttpReason::BodyWriteAborted
        } else if error.is_shutdown() {
            HttpReason::Shutdown
        } else if error.is_user() {
            HttpReason::User
        } else if h2.is_some() {
            HttpReason::H2
        } else if io.is_some() {
            HttpReason::Io
        } else {
            HttpReason::Other
        };
        Self {
            io,
            h2,
            http: Some(http),
            ..Self::default()
        }
    }
}

impl<'a> From<&'a TlsTransportError> for ErrorDetails<'a> {
    fn from(error: &'a TlsTransportError) -> Self {
        match error {
            TlsTransportError::Downstream(error) | TlsTransportError::Upstream(error) => {
                Self::from(error)
            }
            TlsTransportError::Http(error) => Self::from(error),
            _ => Self::default(),
        }
    }
}

impl ErrorDetails<'_> {
    pub(super) fn tls_reason(&self) -> Option<&'static str> {
        Some(match self.tls? {
            rustls::Error::AlertReceived(_) => "peer_alert",
            rustls::Error::InvalidCertificate(error) => certificate_reason(error),
            rustls::Error::NoApplicationProtocol => "alpn_mismatch",
            _ => "protocol",
        })
    }

    pub(super) fn tls_alert(&self) -> Option<rustls::AlertDescription> {
        match self.tls? {
            rustls::Error::AlertReceived(alert) => Some(*alert),
            _ => None,
        }
    }

    pub(super) fn tls_alert_name(&self) -> Option<&'static str> {
        self.tls_alert()
            .map(|alert| alert.as_str().unwrap_or("unknown"))
    }

    /// Preserve the existing bounded handshake metric vocabulary.
    pub(super) fn tls_alert_class(&self) -> &'static str {
        match self.tls_alert() {
            Some(rustls::AlertDescription::UnknownCA) => "unknown_ca",
            Some(rustls::AlertDescription::BadCertificate) => "bad_certificate",
            Some(rustls::AlertDescription::CertificateUnknown) => "certificate_unknown",
            Some(rustls::AlertDescription::CertificateExpired) => "certificate_expired",
            Some(rustls::AlertDescription::CertificateRevoked) => "certificate_revoked",
            Some(rustls::AlertDescription::HandshakeFailure) => "handshake_failure",
            Some(rustls::AlertDescription::ProtocolVersion) => "protocol_version",
            Some(_) => "other",
            None => "none",
        }
    }

    pub(super) fn certificate_reason(&self) -> Option<&'static str> {
        match self.tls? {
            rustls::Error::InvalidCertificate(error) => Some(certificate_reason(error)),
            _ => None,
        }
    }

    pub(super) fn http_reason(&self) -> &'static str {
        self.http.map_or("not_http", HttpReason::as_str)
    }

    pub(super) fn h2_reason(&self) -> Option<&'static str> {
        self.h2.and_then(h2::Error::reason).map(h2_reason)
    }

    pub(super) fn h2_origin(&self) -> Option<&'static str> {
        self.h2.map(|error| {
            if error.is_remote() {
                "remote"
            } else if error.is_library() {
                "library"
            } else {
                "local"
            }
        })
    }
}

/// First error of type `T` in the source chain, including the error itself.
fn cause<'a, T: std::error::Error + 'static>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a T> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(found) = error.downcast_ref::<T>() {
            return Some(found);
        }
        current = error.source();
    }
    None
}

fn certificate_reason(error: &rustls::CertificateError) -> &'static str {
    match error {
        rustls::CertificateError::UnknownIssuer => "unknown_issuer",
        rustls::CertificateError::Expired | rustls::CertificateError::ExpiredContext { .. } => {
            "expired"
        }
        rustls::CertificateError::NotValidYet
        | rustls::CertificateError::NotValidYetContext { .. } => "not_valid_yet",
        rustls::CertificateError::NotValidForName
        | rustls::CertificateError::NotValidForNameContext { .. } => "name_mismatch",
        rustls::CertificateError::Revoked => "revoked",
        _ => "invalid_certificate",
    }
}

const fn h2_reason(reason: h2::Reason) -> &'static str {
    match reason {
        h2::Reason::NO_ERROR => "no_error",
        h2::Reason::PROTOCOL_ERROR => "protocol_error",
        h2::Reason::INTERNAL_ERROR => "internal_error",
        h2::Reason::FLOW_CONTROL_ERROR => "flow_control_error",
        h2::Reason::SETTINGS_TIMEOUT => "settings_timeout",
        h2::Reason::STREAM_CLOSED => "stream_closed",
        h2::Reason::FRAME_SIZE_ERROR => "frame_size_error",
        h2::Reason::REFUSED_STREAM => "refused_stream",
        h2::Reason::CANCEL => "cancel",
        h2::Reason::COMPRESSION_ERROR => "compression_error",
        h2::Reason::CONNECT_ERROR => "connect_error",
        h2::Reason::ENHANCE_YOUR_CALM => "enhance_your_calm",
        h2::Reason::INADEQUATE_SECURITY => "inadequate_security",
        h2::Reason::HTTP_1_1_REQUIRED => "http_1_1_required",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests;
