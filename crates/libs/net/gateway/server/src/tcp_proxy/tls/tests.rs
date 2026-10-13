use std::sync::{Arc, Mutex};

use pplx_space_observability::test_capture::{CapturedEvent, RecordingVisitor};
use tracing_subscriber::layer::SubscriberExt;

use super::{HttpProtocol, TlsTransportError, upstream_alpn_offer};
use crate::tls_sni::{ClientAlpnOffer, HttpAlpn};

/// The origin leg follows the guest's ClientHello unless the guest offered h2
/// and then spoke HTTP/1.1, in which case the origin must not be able to pick
/// h2 (the gateway tunnels CONNECT over HTTP/1.1 only).
#[test]
fn origin_alpn_offer_matches_what_the_guest_actually_speaks() {
    use ClientAlpnOffer as Offer;
    use HttpProtocol as Http;

    for (client_alpn, guest_protocol, expected) in [
        (Offer::Http2ThenHttp1, Http::Http1, Offer::Http1),
        (Offer::Http1ThenHttp2, Http::Http1, Offer::Http1),
        (Offer::Http2, Http::Http1, Offer::Http1),
        (Offer::Http2ThenHttp1, Http::Http2, Offer::Http2ThenHttp1),
        (Offer::Http1ThenHttp2, Http::Http2, Offer::Http1ThenHttp2),
        (Offer::Http2, Http::Http2, Offer::Http2),
        (Offer::Http1, Http::Http1, Offer::Http1),
        (Offer::NoAlpn, Http::Http1, Offer::NoAlpn),
        (Offer::Unavailable, Http::Http1, Offer::Unavailable),
        (Offer::Unsupported, Http::Http1, Offer::Unsupported),
    ] {
        assert_eq!(
            upstream_alpn_offer(client_alpn, guest_protocol),
            expected,
            "{client_alpn:?} with guest {guest_protocol:?}"
        );
    }
}

#[test]
fn upstream_protocol_requires_guest_compatibility() {
    use ClientAlpnOffer as Offer;
    use HttpAlpn as Alpn;
    use HttpProtocol as Http;

    for (offered, selected, expected) in [
        (Offer::NoAlpn, None, Some(Http::Http1)),
        (Offer::Http1, None, Some(Http::Http1)),
        (Offer::Http2ThenHttp1, None, Some(Http::Http1)),
        (Offer::Http1ThenHttp2, None, Some(Http::Http1)),
        (Offer::Http2, None, None),
        (Offer::Http1, Some(Alpn::Http1), Some(Http::Http1)),
        (Offer::Http2, Some(Alpn::Http2), Some(Http::Http2)),
        (Offer::Http2ThenHttp1, Some(Alpn::Http1), Some(Http::Http1)),
        (Offer::Http2ThenHttp1, Some(Alpn::Http2), Some(Http::Http2)),
        (Offer::Http1ThenHttp2, Some(Alpn::Http1), Some(Http::Http1)),
        (Offer::Http1ThenHttp2, Some(Alpn::Http2), Some(Http::Http2)),
        (Offer::NoAlpn, Some(Alpn::Http1), None),
        (Offer::NoAlpn, Some(Alpn::Http2), None),
        (Offer::Http1, Some(Alpn::Http2), None),
        (Offer::Http2, Some(Alpn::Http1), None),
        (Offer::Unavailable, None, None),
        (Offer::Unsupported, None, None),
    ] {
        let result = HttpProtocol::negotiated_upstream(offered, selected);
        assert_eq!(
            result.as_ref().ok().copied(),
            expected,
            "{offered:?} selected {selected:?}"
        );
        if expected.is_none() {
            assert!(matches!(result, Err(TlsTransportError::Protocol)));
        }
    }
}

#[test]
fn tls_diagnostics_do_not_export_the_guest_hostname() {
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<CapturedEvent>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_new_span(
            &self,
            attributes: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = RecordingVisitor::default();
            attributes.record(&mut visitor);
            self.0.lock().unwrap().extend(visitor.into_fields());
        }
    }

    let captured = Capture::default();
    let subscriber = tracing_subscriber::registry().with(captured.clone());
    let hostname = "private-customer-destination.example";
    tracing::subscriber::with_default(subscriber, || {
        let _span = super::upstream_span(&hostname.parse().unwrap(), 443);
    });
    let fields = captured.0.lock().unwrap();
    assert!(fields.iter().all(|(_, value)| !value.contains(hostname)));
    assert!(fields.contains(&(
        "server.address_length".to_owned(),
        hostname.len().to_string(),
    )));
    assert!(fields.contains(&("server.port".to_owned(), "443".to_owned())));

    // TLS library errors can include expected SNI and certificate SAN values.
    // The enforcement event logs the outer error's Display, not just the span.
    for error in [
        super::TlsTransportError::Downstream(std::io::Error::other(hostname)),
        super::TlsTransportError::Upstream(std::io::Error::other(hostname)),
        super::TlsTransportError::Configuration(rustls::Error::General(hostname.to_owned())),
        super::TlsTransportError::Certificate(crate::certificates::CertificateError::Tls(
            rustls::Error::General(hostname.to_owned()),
        )),
    ] {
        let outcome = error.as_str();
        assert!(std::error::Error::source(&error).is_some());
        assert!(!error.to_string().contains(hostname));
        let message = crate::tls_interception::TlsInterceptionError::from(error).to_string();
        assert!(!message.contains(hostname), "{outcome}: {message}");
    }

    let rpc_error = crate::tls_interception::TlsInterceptionError::CertificateRequest(
        tonic::Status::permission_denied(hostname),
    );
    assert!(std::error::Error::source(&rpc_error).is_some());
    assert!(!rpc_error.to_string().contains(hostname));
    assert!(rpc_error.to_string().contains("PermissionDenied"));
}
