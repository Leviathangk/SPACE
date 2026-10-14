//! Establish a marked outbound TCP connection.
use crate::orig_dst;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::{io, net::SocketAddr, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("TCP connect exceeded its ten-second deadline")]
    Timeout,
    #[error("TCP connect failed ({kind:?})", kind = .0.kind())]
    Io(#[from] io::Error),
}

impl Error {
    pub(super) fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Timeout => None,
            Self::Io(error) => Some(error.kind()),
        }
    }
}

/// `SO_MARK` keeps the reply from being re-captured by this gateway's own TPROXY
/// rule, which would otherwise make it connect to itself.
pub(super) async fn connect(dst: SocketAddr) -> Result<TcpStream, Error> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    orig_dst::set_so_mark(&socket, pplx_space_net_gateway_client::OUTBOUND_MARK)?;
    socket.set_nonblocking(true)?;
    match socket.connect(&dst.into()) {
        Ok(()) => {}
        Err(error) if connect_in_progress(&error) => {}
        Err(error) => return Err(error.into()),
    }

    // A non-blocking connect signals completion by becoming writable, and
    // reports failure only through SO_ERROR — a failed connect is writable too.
    let upstream = TcpStream::from_std(std::net::TcpStream::from(socket))?;
    tokio::time::timeout(CONNECT_TIMEOUT, upstream.writable())
        .await
        .map_err(|_| Error::Timeout)??;
    match upstream.take_error()? {
        Some(error) => Err(error.into()),
        None => Ok(upstream),
    }
}

fn connect_in_progress(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINPROGRESS) | Some(libc::EALREADY)
    ) || error.kind() == io::ErrorKind::WouldBlock
}

/// Counts bytes that cross an origin socket for flow accounting.
pub(super) struct MeteredIo<T> {
    pub(super) inner: T,
    pub(super) read: Arc<AtomicU64>,
    pub(super) written: Arc<AtomicU64>,
}

impl<T: AsyncRead + Unpin> AsyncRead for MeteredIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            let read = buffer.filled().len().saturating_sub(before) as u64;
            self.read.fetch_add(read, Ordering::Relaxed);
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for MeteredIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let result = Pin::new(&mut self.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(written)) = result {
            self.written.fetch_add(written as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}
