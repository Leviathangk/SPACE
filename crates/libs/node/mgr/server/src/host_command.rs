//! Bounded foreground host commands. Call from blocking work that retains its
//! resource claims; a command deadline requests termination, never abandonment.
//! This is not a launcher for daemons or shell pipelines.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

pub(crate) const PROBE_BUDGET: Duration = Duration::from_secs(5);
pub(crate) const MUTATION_BUDGET: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Child::kill is a no-op after this handle has reaped its child, so it
        // cannot signal a reused PID. Keep the enclosing ownership guard alive
        // through wait even when SIGKILL is delayed by uninterruptible I/O.
        if let Err(error) = self.0.kill() {
            tracing::warn!(%error, "failed to terminate host command; waiting for exit");
        }
        if let Err(error) = self.0.wait() {
            tracing::warn!(%error, "failed to reap host command");
        }
    }
}

fn nonblocking(fd: &impl AsFd) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_retain(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

fn read_available(pipe: &mut impl Read, output: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0; 8192];
    // Bound CPU work as well as retained output, allowing the deadline and
    // stdin to progress while a child continuously writes.
    for _ in 0..8 {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if output.len() + n > OUTPUT_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "host command output limit exceeded",
                    ));
                }
                output.extend_from_slice(buffer.get(..n).ok_or(io::ErrorKind::InvalidData)?);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Capture at most 1 MiB per output stream. Input, output and process execution
/// share one deadline. On every error the child is killed and reaped before
/// returning, so a resource owner must not wrap this blocking call in a timeout.
/// Uninterruptible kernel work can extend the reap; that is an operator repair
/// obligation, not permission to race the still-running command.
pub(crate) fn output(command: &mut Command, budget: Duration, input: &[u8]) -> io::Result<Output> {
    let started = Instant::now();
    let child = OwnedChild(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    capture(child, started, budget, input, None)
}

/// Stream a pinned file directly to the child without buffering it in memory.
/// The same deadline, output bound, kill and reap contract applies.
pub(crate) fn stream(
    command: &mut Command,
    budget: Duration,
    input: Stdio,
    output: Stdio,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> io::Result<Output> {
    check_cancelled(cancel)?;
    let started = Instant::now();
    let child = OwnedChild(
        command
            .stdin(input)
            .stdout(output)
            .stderr(Stdio::piped())
            .spawn()?,
    );
    capture(child, started, budget, &[], cancel)
}

fn check_cancelled(cancel: Option<&tokio_util::sync::CancellationToken>) -> io::Result<()> {
    if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "host command cancelled; terminating and reaping",
        ))
    } else {
        Ok(())
    }
}

fn capture(
    mut child: OwnedChild,
    started: Instant,
    budget: Duration,
    mut input: &[u8],
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> io::Result<Output> {
    // Exit readiness avoids charging every short command a full polling tick.
    // Unsupported kernels retain the bounded I/O and cancellation polling.
    #[cfg(target_os = "linux")]
    let exit = rustix::process::pidfd_open(
        rustix::process::Pid::from_child(&child.0),
        rustix::process::PidfdFlags::empty(),
    )
    .ok();
    #[cfg(not(target_os = "linux"))]
    let exit: Option<std::os::fd::OwnedFd> = None;
    let mut exit_poll = exit
        .as_ref()
        .map(|fd| PollFd::new(fd.as_fd(), PollFlags::POLLIN));
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take();
    let mut stderr = child.0.stderr.take().expect("stderr configured as piped");
    if let Some(pipe) = &stdout {
        nonblocking(pipe)?;
    }
    nonblocking(&stderr)?;
    if let Some(pipe) = stdin.as_ref() {
        nonblocking(pipe)?;
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        check_cancelled(cancel)?;
        if let Some(pipe) = &mut stdout {
            read_available(pipe, &mut out)?;
        }
        read_available(&mut stderr, &mut err)?;
        if input.is_empty() {
            stdin.take();
        } else if let Some(pipe) = stdin.as_mut() {
            match pipe.write(input) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => input = input.get(n..).ok_or(io::ErrorKind::InvalidData)?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(status) = child.0.try_wait()? {
            // All foreground writers have closed their pipes. Drain what
            // remains, including output written just before exit.
            loop {
                let before = (out.len(), err.len());
                if let Some(pipe) = &mut stdout {
                    read_available(pipe, &mut out)?;
                }
                read_available(&mut stderr, &mut err)?;
                if before == (out.len(), err.len()) {
                    break;
                }
            }
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        if started.elapsed() >= budget {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("host command exceeded {budget:?}; terminating and reaping"),
            ));
        }
        let timeout =
            PollTimeout::try_from(POLL_INTERVAL.min(budget.saturating_sub(started.elapsed())))
                .map_err(io::Error::other)?;
        match poll(exit_poll.as_mut_slice(), timeout) {
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests;
