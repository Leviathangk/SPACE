//! HTTP/1.1 relay for TCP flows admitted by a DNS-domain allow.
//!
//! A CDN address is not a hostname identity: one edge IP can route requests for
//! many unrelated tenants. Hyper owns framing here so every request on a
//! keep-alive connection is checked before it reaches the already-admitted IP.
//! The upstream socket is created only after the first request passes that
//! check, so a rejected authority cannot produce network egress.

//! Policy, origin setup and forwarding are shared with inspected HTTP in
//! `http_session`; this module only meters the origin socket for the flow.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use super::http_protocol::HttpProtocol;
use super::http_session::{Failure, HostBinding, Served};
use super::transport::{self, MeteredIo};

pub(super) async fn serve<C, F, Fut, U>(
    guest: C,
    binding: HostBinding,
    connect: F,
    shutdown: CancellationToken,
) -> Result<Served, Failure>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnOnce(SocketAddr) -> Fut + Send + 'static,
    Fut: Future<Output = Result<U, transport::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let wire_tx = Arc::new(AtomicU64::new(0));
    let wire_rx = Arc::new(AtomicU64::new(0));
    let destination = binding.destination();
    let connector = {
        let read = wire_rx.clone();
        let written = wire_tx.clone();
        move |_| async move {
            let inner = connect(destination).await?;
            Ok((
                MeteredIo {
                    inner,
                    read,
                    written,
                },
                HttpProtocol::Http1,
            ))
        }
    };
    let mut served =
        super::http_session::serve_plaintext(guest, binding, connector, shutdown).await?;
    served.wire_tx = wire_tx.load(Ordering::Relaxed);
    served.wire_rx = wire_rx.load(Ordering::Relaxed);
    Ok(served)
}

#[cfg(test)]
mod tests;
