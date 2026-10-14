//! HTTP versions supported by the gateway, shared by ALPN and HTTP framing.

/// An HTTP application protocol that the interception proxy can terminate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HttpProtocol {
    Http2,
    Http1,
}

impl HttpProtocol {
    /// The protocol Hyper parsed, independently of TLS ALPN negotiation.
    pub(crate) fn of(version: http::Version) -> Self {
        if version == http::Version::HTTP_2 {
            Self::Http2
        } else {
            Self::Http1
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http1 => "http1",
            Self::Http2 => "http2",
        }
    }

    pub(crate) fn from_wire(value: &[u8]) -> Option<Self> {
        match value {
            b"h2" => Some(Self::Http2),
            b"http/1.1" => Some(Self::Http1),
            _ => None,
        }
    }

    pub(crate) const fn wire_value(self) -> &'static [u8] {
        match self {
            Self::Http2 => b"h2",
            Self::Http1 => b"http/1.1",
        }
    }
}
