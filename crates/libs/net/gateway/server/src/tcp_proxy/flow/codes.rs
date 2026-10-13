//! Protocol and library vocabulary only; never retain error Display strings.

use std::borrow::Cow;
use std::io;

use nix::errno::Errno;
use rustls::AlertDescription;

use super::super::tls::{HttpFailure, TlsTransportError, cause};

#[derive(Clone, Copy, Debug)]
pub(in crate::tcp_proxy) enum AlertDirection {
    Sent,
    Received,
}

impl AlertDirection {
    pub(in crate::tcp_proxy) const fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Received => "received",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(in crate::tcp_proxy) enum Close {
    Normal,
    LocalReset,
    RemoteReset,
}

impl Close {
    pub(in crate::tcp_proxy) const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::LocalReset => "local_reset",
            Self::RemoteReset => "remote_reset",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::tcp_proxy) struct Codes {
    pub(in crate::tcp_proxy) tls_alert: Option<(AlertDescription, AlertDirection)>,
    pub(in crate::tcp_proxy) tls_cert_error: Option<&'static str>,
    pub(in crate::tcp_proxy) http: Option<HttpFailure>,
    pub(in crate::tcp_proxy) errno: Option<Errno>,
    io_kind: Option<io::ErrorKind>,
    timeout: bool,
}

impl Codes {
    pub(in crate::tcp_proxy) fn transport(error: &TlsTransportError) -> Self {
        match error {
            TlsTransportError::Downstream(error) | TlsTransportError::Upstream(error) => {
                Self::io(error)
            }
            TlsTransportError::Http(error) => Self::error(error),
            TlsTransportError::Timeout => Self {
                timeout: true,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    pub(in crate::tcp_proxy) fn error(error: &(dyn std::error::Error + 'static)) -> Self {
        let h2 = cause::<h2::Error>(error);
        let io = cause::<io::Error>(error).or_else(|| h2.and_then(h2::Error::get_io));
        Self {
            http: cause::<hyper::Error>(error).map(HttpFailure::classify),
            ..io.map(Self::io).unwrap_or_default()
        }
    }

    pub(in crate::tcp_proxy) fn io(error: &io::Error) -> Self {
        let mut codes = Self {
            errno: error
                .raw_os_error()
                .map(Errno::from_raw)
                .filter(|e| *e != Errno::UnknownErrno),
            io_kind: Some(error.kind()),
            timeout: error.kind() == io::ErrorKind::TimedOut && error.raw_os_error().is_none(),
            ..Self::default()
        };
        match cause::<rustls::Error>(error) {
            Some(rustls::Error::AlertReceived(alert)) => {
                codes.tls_alert = Some((*alert, AlertDirection::Received))
            }
            Some(rustls::Error::InvalidCertificate(error)) => {
                codes.tls_cert_error = Some(certificate_name(error));
                // rustls queues this alert; tokio-rustls attempts to write it
                // before returning the certificate error. Delivery is not acknowledged.
                codes.tls_alert = Some((error.clone().into(), AlertDirection::Sent));
            }
            _ => {}
        }
        codes
    }

    pub(in crate::tcp_proxy) fn alert_name(self) -> Option<&'static str> {
        self.tls_alert
            .map(|(alert, _)| alert.as_str().unwrap_or("_OTHER"))
    }

    pub(in crate::tcp_proxy) fn h2_error_code(self) -> Option<String> {
        self.http.and_then(|http| http.h2_reason).map(|reason| {
            if reason == "unknown" {
                "_OTHER".into()
            } else {
                reason.to_ascii_uppercase()
            }
        })
    }

    pub(in crate::tcp_proxy) fn http_lib_reason(self) -> Option<&'static str> {
        match self.http?.reason {
            "parse" => Some("is_parse"),
            "timeout" => Some("is_timeout"),
            "cancelled" => Some("is_canceled"),
            "incomplete_message" => Some("is_incomplete_message"),
            "closed" => Some("is_closed"),
            "body_write_aborted" => Some("is_body_write_aborted"),
            "shutdown" => Some("is_shutdown"),
            "user" => Some("is_user"),
            _ => None,
        }
    }

    pub(in crate::tcp_proxy) fn error_type(self) -> Cow<'static, str> {
        if let Some(alert) = self.alert_name() {
            return alert.into();
        }
        if let Some(code) = self.h2_error_code() {
            return code.into();
        }
        if let Some(certificate) = self.tls_cert_error {
            return certificate.into();
        }
        if let Some(reason) = self.http_lib_reason() {
            return reason.into();
        }
        if let Some(errno) = self.errno {
            return format!("{errno:?}").into();
        }
        if self.timeout {
            return "timeout".into();
        }
        "_OTHER".into()
    }

    pub(in crate::tcp_proxy) fn close(self) -> Close {
        if let Some((_, direction)) = self.tls_alert {
            return match direction {
                AlertDirection::Sent => Close::LocalReset,
                AlertDirection::Received => Close::RemoteReset,
            };
        }
        if let Some(http) = self.http {
            match (http.h2_reason, http.h2_origin) {
                (Some("no_error"), _) => return Close::Normal,
                (Some(_), Some("remote")) => return Close::RemoteReset,
                (Some(_), _) => return Close::LocalReset,
                _ => {}
            }
        }
        match self.io_kind {
            Some(
                io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe,
            ) => Close::RemoteReset,
            Some(io::ErrorKind::UnexpectedEof) => Close::Normal,
            _ => Close::LocalReset,
        }
    }

    pub(super) fn has_protocol_code(self) -> bool {
        self.tls_alert.is_some() || self.http.is_some_and(|h| h.h2_reason.is_some())
    }
}

fn certificate_name(error: &rustls::CertificateError) -> &'static str {
    use rustls::CertificateError::*;
    match error {
        BadEncoding => "BadEncoding",
        Expired => "Expired",
        ExpiredContext { .. } => "ExpiredContext",
        NotValidYet => "NotValidYet",
        NotValidYetContext { .. } => "NotValidYetContext",
        Revoked => "Revoked",
        UnhandledCriticalExtension => "UnhandledCriticalExtension",
        UnknownIssuer => "UnknownIssuer",
        UnknownRevocationStatus => "UnknownRevocationStatus",
        ExpiredRevocationList => "ExpiredRevocationList",
        ExpiredRevocationListContext { .. } => "ExpiredRevocationListContext",
        BadSignature => "BadSignature",
        UnsupportedSignatureAlgorithmContext { .. } => "UnsupportedSignatureAlgorithmContext",
        UnsupportedSignatureAlgorithmForPublicKeyContext { .. } => {
            "UnsupportedSignatureAlgorithmForPublicKeyContext"
        }
        NotValidForName => "NotValidForName",
        NotValidForNameContext { .. } => "NotValidForNameContext",
        InvalidPurpose => "InvalidPurpose",
        InvalidPurposeContext { .. } => "InvalidPurposeContext",
        InvalidOcspResponse => "InvalidOcspResponse",
        ApplicationVerificationFailure => "ApplicationVerificationFailure",
        Other(_) => "Other",
        _ => "_OTHER",
    }
}

#[cfg(test)]
mod tests;
