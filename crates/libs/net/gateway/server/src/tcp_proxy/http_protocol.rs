//! The HTTP version a request was parsed with or an origin leg negotiated.
use crate::tls_sni::HttpAlpn;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HttpProtocol {
    Http1,
    Http2,
}

impl HttpProtocol {
    /// The protocol hyper actually parsed a request with. This, not the ALPN
    /// result, decides how the request is forwarded.
    pub(super) fn of(version: http::Version) -> Self {
        if version == http::Version::HTTP_2 {
            Self::Http2
        } else {
            Self::Http1
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Http1 => "http1",
            Self::Http2 => "http2",
        }
    }
}

impl From<HttpAlpn> for HttpProtocol {
    fn from(protocol: HttpAlpn) -> Self {
        match protocol {
            HttpAlpn::Http1 => Self::Http1,
            HttpAlpn::Http2 => Self::Http2,
        }
    }
}
