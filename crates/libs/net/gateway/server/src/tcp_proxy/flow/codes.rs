//! Only native values observed through supported error interfaces.
use super::super::tls::{TlsTransportError, http_error_reason};
use nix::errno::Errno;
use std::{borrow::Cow, io};

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
    pub(in crate::tcp_proxy) tls_alert: Option<&'static str>,
    pub(in crate::tcp_proxy) tls_alert_dir: Option<&'static str>,
    pub(in crate::tcp_proxy) tls_cert_error: Option<&'static str>,
    pub(in crate::tcp_proxy) http_lib_reason: Option<&'static str>,
    pub(in crate::tcp_proxy) errno: Option<Errno>,
    pub(in crate::tcp_proxy) close: Option<Close>,
    timeout: bool,
}
impl Codes {
    pub(in crate::tcp_proxy) fn transport(error: &TlsTransportError) -> Self {
        match error {
            TlsTransportError::Downstream(error) | TlsTransportError::Upstream(error) => {
                Self::io(error)
            }
            TlsTransportError::Http(error) => Self::http(error),
            TlsTransportError::Timeout => Self {
                timeout: true,
                close: Some(Close::LocalReset),
                ..Self::default()
            },
            TlsTransportError::Withdrawn | TlsTransportError::Invalidated => Self {
                close: Some(Close::LocalReset),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
    pub(in crate::tcp_proxy) fn http(error: &hyper::Error) -> Self {
        Self {
            http_lib_reason: Some(http_error_reason(error)).filter(|reason| *reason != "_OTHER"),
            ..Self::default()
        }
    }
    pub(in crate::tcp_proxy) fn io(error: &io::Error) -> Self {
        let mut codes = Self {
            errno: error
                .raw_os_error()
                .map(Errno::from_raw)
                .filter(|e| *e != Errno::UnknownErrno),
            timeout: error.kind() == io::ErrorKind::TimedOut && error.raw_os_error().is_none(),
            close: match error.kind() {
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused => {
                    Some(Close::RemoteReset)
                }
                io::ErrorKind::UnexpectedEof => Some(Close::Normal),
                io::ErrorKind::TimedOut if error.raw_os_error().is_none() => {
                    Some(Close::LocalReset)
                }
                _ => None,
            },
            ..Self::default()
        };
        // tokio-rustls returns its native error directly inside io::Error.
        // Do not traverse Hyper's diagnostic source chain for transport details.
        match error
            .get_ref()
            .and_then(|error| error.downcast_ref::<rustls::Error>())
        {
            Some(rustls::Error::AlertReceived(alert)) => {
                codes.tls_alert = Some(alert.as_str().unwrap_or("_OTHER"));
                codes.tls_alert_dir = Some("received");
                codes.close = Some(Close::RemoteReset);
            }
            Some(rustls::Error::InvalidCertificate(error)) => {
                codes.tls_cert_error = Some(certificate_name(error));
                // Rustls queues this alert; tokio-rustls attempts to write it
                // before returning the error. Peer receipt is not acknowledged.
                let alert: rustls::AlertDescription = error.clone().into();
                codes.tls_alert = Some(alert.as_str().unwrap_or("_OTHER"));
                codes.tls_alert_dir = Some("sent");
                codes.close = Some(Close::LocalReset);
            }
            _ => {}
        }
        codes
    }
    pub(in crate::tcp_proxy) fn error_type(self) -> Cow<'static, str> {
        if let Some(code) = self
            .tls_alert
            .or(self.tls_cert_error)
            .or(self.http_lib_reason.filter(|reason| *reason != "_OTHER"))
        {
            code.into()
        } else if let Some(errno) = self.errno {
            format!("{errno:?}").into()
        } else if self.timeout {
            "timeout".into()
        } else {
            "_OTHER".into()
        }
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
