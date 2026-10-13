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

use super::{ErrorDetails, HttpReason};

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
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
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
    let failure = ErrorDetails::from(&error);
    assert_eq!(failure.http, Some(HttpReason::Io));
    assert_eq!(
        failure.io.map(io::Error::kind),
        Some(io::ErrorKind::ConnectionReset)
    );
    assert_eq!(failure.h2.and_then(h2::Error::reason), None);
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
    let failure = ErrorDetails::from(&error);
    assert_eq!(failure.http, Some(HttpReason::User));
    assert_eq!(failure.h2.and_then(h2::Error::reason), None);
}

#[tokio::test]
async fn h2_peer_violation_reports_code_and_origin() {
    let (mut client, server) = tokio::io::duplex(4096);
    let serve = hyper::server::conn::http2::Builder::new(TokioExecutor::new()).serve_connection(
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
    let failure = ErrorDetails::from(&error);
    assert_eq!(failure.http, Some(HttpReason::H2));
    assert_eq!(
        failure.h2.and_then(h2::Error::reason),
        Some(h2::Reason::PROTOCOL_ERROR)
    );
    assert!(failure.h2.unwrap().is_library());
}

#[tokio::test]
async fn malformed_http_is_a_parse_error() {
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
    assert_eq!(ErrorDetails::from(&error).http, Some(HttpReason::Parse));
}
