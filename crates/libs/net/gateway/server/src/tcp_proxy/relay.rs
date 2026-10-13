//! Carrying one admitted flow to its destination.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::flow::{Flow, Phase, Side};
use super::leg::RelayWitness;
use tokio::net::TcpStream;

use crate::orig_dst;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which half of the relay failed.
pub(super) enum Failure {
    Connect(io::Error),
    Copy(io::Error),
}

/// Dial `dst` and shuttle bytes until both directions close. Returns the wire
/// bytes carried, guest-to-destination first.
pub(super) async fn run(
    client: TcpStream,
    dst: SocketAddr,
    flow: &Flow,
) -> Result<(u64, u64), Failure> {
    flow.at_phase(Phase::TcpConnect);
    let upstream = connect(dst)
        .await
        .inspect_err(|error| flow.io_failed(error, Some(Phase::TcpConnect), Side::Upstream))
        .map_err(Failure::Connect)?;
    transfer(client, upstream, flow)
        .await
        .map_err(Failure::Copy)
}

async fn transfer(
    mut client: TcpStream,
    mut upstream: TcpStream,
    flow: &Flow,
) -> io::Result<(u64, u64)> {
    flow.at_phase(Phase::Relay);
    // Borrowed TCP halves avoid the mutex in tokio::io::split. Each EOF shuts
    // down only the opposite writer; the reverse direction continues draining.
    let (client_read, client_write) = client.split();
    let (upstream_read, upstream_write) = upstream.split();
    let witness = RelayWitness::default();
    let result = tokio::try_join!(
        copy(witness.guest(client_read), witness.origin(upstream_write)),
        copy(witness.origin(upstream_read), witness.guest(client_write)),
    );
    if let (Err(error), Some(fault)) = (&result, witness.fault()) {
        flow.io_failed(error, Some(Phase::Relay), fault.side().into());
    }
    if let Some(side) = witness.first_close() {
        flow.closed(side.into());
    }
    result
}

async fn copy<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut read: R,
    mut write: W,
) -> io::Result<u64> {
    let bytes = tokio::io::copy(&mut read, &mut write).await?;
    write.shutdown().await?;
    Ok(bytes)
}

/// `SO_MARK` keeps the reply from being re-captured by this gateway's own TPROXY
/// rule, which would otherwise make it connect to itself.
pub(super) async fn connect(dst: SocketAddr) -> io::Result<TcpStream> {
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
        Err(error) => return Err(error),
    }

    // A non-blocking connect signals completion by becoming writable, and
    // reports failure only through SO_ERROR — a failed connect is writable too.
    let upstream = TcpStream::from_std(std::net::TcpStream::from(socket))?;
    tokio::time::timeout(CONNECT_TIMEOUT, upstream.writable())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TCP connect timed out"))??;
    match upstream.take_error()? {
        Some(error) => Err(error),
        None => Ok(upstream),
    }
}

fn connect_in_progress(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINPROGRESS) | Some(libc::EALREADY)
    ) || error.kind() == io::ErrorKind::WouldBlock
}

#[cfg(test)]
mod tests;
