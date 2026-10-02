use std::sync::{Arc, Mutex};

use pplx_space_observability::test_capture::{CapturedEvent, RecordingVisitor};
use tracing_subscriber::layer::SubscriberExt;

use super::{HttpProtocol, TlsTransportError};
use crate::tls_sni::{ClientAlpnOffer, HttpAlpn};

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

mod http_failure {
    use std::convert::Infallible;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http_body_util::{Empty, StreamBody};
    use hyper::body::Frame;
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

    use super::super::HttpFailure;

    /// Delivers one request, then fails every write the way a guest reset does.
    struct ResetAfterRequest(&'static [u8]);

    impl AsyncRead for ResetAfterRequest {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let take = self.0.len().min(buf.remaining());
            if take == 0 {
                return Poll::Pending;
            }
            buf.put_slice(&self.0[..take]);
            self.0 = &self.0[take..];
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for ResetAfterRequest {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn empty() -> hyper::Response<Empty<Bytes>> {
        hyper::Response::new(Empty::new())
    }

    #[tokio::test]
    async fn guest_socket_reset_is_io_with_its_kind() {
        let io = ResetAfterRequest(b"GET / HTTP/1.1\r\nhost: a\r\n\r\n");
        let error = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(io),
                service_fn(|_| async { Ok::<_, Infallible>(empty()) }),
            )
            .await
            .expect_err("writes fail");
        let failure = HttpFailure::classify(&error);
        assert_eq!(failure.reason, "io");
        assert_eq!(failure.io_kind, Some(io::ErrorKind::ConnectionReset));
        assert_eq!(failure.h2_reason, None);
    }

    #[tokio::test]
    async fn response_body_error_is_user() {
        let (mut client, server) = tokio::io::duplex(4096);
        let serve = hyper::server::conn::http1::Builder::new().serve_connection(
            TokioIo::new(server),
            service_fn(|_| async {
                let body = StreamBody::new(futures_util::stream::iter([Err::<Frame<Bytes>, _>(
                    io::Error::other("origin body reset"),
                )]));
                Ok::<_, Infallible>(hyper::Response::new(body))
            }),
        );
        client
            .write_all(b"GET / HTTP/1.1\r\nhost: a\r\n\r\n")
            .await
            .unwrap();
        let error = serve.await.expect_err("body stream fails");
        let failure = HttpFailure::classify(&error);
        assert_eq!(failure.reason, "user");
        assert_eq!(failure.h2_reason, None);
    }

    #[tokio::test]
    async fn h2_peer_violation_reports_code_and_origin() {
        let (mut client, server) = tokio::io::duplex(4096);
        let serve = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(server),
                service_fn(|_| async { Ok::<_, Infallible>(empty()) }),
            );
        let task = tokio::spawn(serve);
        // Valid preface and empty SETTINGS, then a PING on stream 1: a
        // connection-level PROTOCOL_ERROR under RFC 9113 section 6.7.
        let mut wire = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        wire.extend_from_slice(&[0, 0, 0, 0x4, 0, 0, 0, 0, 0]);
        wire.extend_from_slice(&[0, 0, 8, 0x6, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        client.write_all(&wire).await.unwrap();
        let error = task.await.unwrap().expect_err("peer violated h2");
        let failure = HttpFailure::classify(&error);
        assert_eq!(failure.reason, "h2");
        assert_eq!(failure.h2_reason, Some("protocol_error"));
        assert_eq!(failure.h2_origin, Some("library"));
    }

    #[tokio::test]
    async fn historical_buckets_keep_their_names() {
        let (mut client, server) = tokio::io::duplex(4096);
        let serve = hyper::server::conn::http1::Builder::new().serve_connection(
            TokioIo::new(server),
            service_fn(|_| async { Ok::<_, Infallible>(empty()) }),
        );
        client
            .write_all(b"\x00\x01 not http\r\n\r\n")
            .await
            .unwrap();
        let error = serve.await.expect_err("garbage request");
        assert_eq!(HttpFailure::classify(&error).reason, "parse");
    }
}

mod guest_protocol {
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};

    use super::super::{HttpProtocol, guest_protocol};

    /// Delivers each scripted read as one TLS record would, then EOF.
    struct Script(VecDeque<io::Result<Vec<u8>>>);

    impl AsyncRead for Script {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.0.pop_front() {
                None => Poll::Ready(Ok(())),
                Some(Err(error)) => Poll::Ready(Err(error)),
                Some(Ok(mut chunk)) => {
                    let rest = chunk.split_off(chunk.len().min(buf.remaining()));
                    buf.put_slice(&chunk);
                    if !rest.is_empty() {
                        self.0.push_front(Ok(rest));
                    }
                    Poll::Ready(Ok(()))
                }
            }
        }
    }

    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

    async fn sniff(chunks: &[&[u8]], negotiated: HttpProtocol) -> (HttpProtocol, Vec<u8>) {
        let script = Script(chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect());
        let (mut rewound, protocol) = guest_protocol(script, negotiated).await;
        let mut replayed = Vec::new();
        rewound.read_to_end(&mut replayed).await.unwrap();
        (protocol, replayed)
    }

    #[tokio::test]
    async fn only_an_http1_connect_downgrades_an_h2_session_and_bytes_replay_exactly() {
        let connect: &[u8] = b"CONNECT inner.test:443 HTTP/1.1\r\nHost: inner.test:443\r\n\r\n";
        for (name, chunks, negotiated, expected) in [
            (
                "h2 preface",
                vec![PREFACE],
                HttpProtocol::Http2,
                HttpProtocol::Http2,
            ),
            (
                "Go CONNECT",
                vec![connect],
                HttpProtocol::Http2,
                HttpProtocol::Http1,
            ),
            (
                "CONNECT split across records",
                vec![&connect[..3], &connect[3..7], &connect[7..]],
                HttpProtocol::Http2,
                HttpProtocol::Http1,
            ),
            (
                "other HTTP/1 method stays h2",
                vec![&b"GET / HTTP/1.1\r\n\r\n"[..]],
                HttpProtocol::Http2,
                HttpProtocol::Http2,
            ),
            (
                "CONNECT prefix then EOF stays h2",
                vec![&b"CONN"[..]],
                HttpProtocol::Http2,
                HttpProtocol::Http2,
            ),
            ("empty", vec![], HttpProtocol::Http2, HttpProtocol::Http2),
            (
                "http1 is never inspected",
                vec![connect],
                HttpProtocol::Http1,
                HttpProtocol::Http1,
            ),
        ] {
            let (protocol, replayed) = sniff(&chunks, negotiated).await;
            assert_eq!(protocol, expected, "{name}");
            assert_eq!(
                replayed,
                chunks.concat(),
                "{name}: bytes must replay unchanged"
            );
        }
    }

    #[tokio::test]
    async fn read_errors_are_replayed_after_the_buffered_prefix() {
        let guest = Script(VecDeque::from([
            Ok(b"CON".to_vec()),
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
        ]));
        let (mut rewound, protocol) = guest_protocol(guest, HttpProtocol::Http2).await;
        assert_eq!(protocol, HttpProtocol::Http2);
        let mut buffered = [0; 3];
        rewound.read_exact(&mut buffered).await.unwrap();
        assert_eq!(&buffered, b"CON");
        let error = rewound.read_u8().await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_guest_keeps_the_negotiated_protocol_and_writes_pass_through() {
        let (guest, mut peer) = tokio::io::duplex(64);
        let (mut rewound, protocol) = guest_protocol(guest, HttpProtocol::Http2).await;
        assert_eq!(protocol, HttpProtocol::Http2);
        rewound.write_all(b"settings").await.unwrap();
        let mut written = [0; 8];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut written))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&written, b"settings");
    }
}
