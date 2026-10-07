use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::unistd::{ForkResult, fork};
use pplx_space_common_core::types::RuntimeReadiness;

use super::{InvocationConfig, read_declaration, resolve_config};

mod output;
use crate::identity::GuestAccount;
use output::Output;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(io::Error::from)
}

/// Called by the CLI before starting any runtime, telemetry, or worker threads.
/// On success this process becomes the application; the relay is reparented to PID1.
pub fn run_runtime_launcher() -> io::Result<()> {
    let declaration =
        read_declaration()?.ok_or_else(|| io::Error::other("runtime declaration missing"))?;
    let launch = resolve_config(declaration).map_err(io::Error::other)?;
    let account = GuestAccount::resolve(launch.declaration.user().clone()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime user cannot be resolved",
        )
    })?;
    let notify = std::env::var_os("NOTIFY_SOCKET").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "missing runtime notification socket",
        )
    })?;
    // SPACE's systemd notify socket is a pathname; do not silently ignore an
    // unsupported socket format and report successful launch.
    let socket = UnixDatagram::unbound()?;
    socket.connect(notify)?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(1)))?;
    let (output_read, output_write) = pipe()?;
    let (exec_read, exec_write) = pipe()?;
    let main_pid = rustix::process::Pid::from_raw(std::process::id() as i32)
        .ok_or_else(|| io::Error::other("invalid launcher pid"))?;
    let main_pidfd = rustix::process::pidfd_open(main_pid, rustix::process::PidfdFlags::empty())?;
    // SAFETY: this entry point is single threaded and has not initialized Tokio.
    match unsafe { fork() }.map_err(io::Error::from)? {
        ForkResult::Child => {
            // Double fork: an arbitrary application must not inherit a logger
            // child that it would have to wait on or mistake for one of its own.
            // SAFETY: still single threaded, and the intermediate child _exits.
            match unsafe { fork() } {
                // SAFETY: exit the intermediate fork without running destructors.
                Ok(ForkResult::Parent { .. }) => unsafe { libc::_exit(0) },
                // SAFETY: failed child setup must not return into the launcher.
                Err(_) => unsafe { libc::_exit(1) },
                Ok(ForkResult::Child) => {}
            }
            drop(exec_write);
            let result = relay(
                output_read,
                output_write,
                exec_read,
                socket,
                main_pidfd,
                &launch,
                &account,
            );
            // SAFETY: no return into the launcher's control flow or inherited destructors.
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 1 }) }
        }
        ForkResult::Parent { child } => {
            drop(output_read);
            drop(exec_read);
            drop(socket);
            nix::sys::wait::waitpid(child, None).map_err(io::Error::from)?;
        }
    }
    // From here, report every setup failure on the close-on-exec pipe. EOF is
    // positive exec evidence only while PID1 still observes this MainPID alive.
    let mut exec_error = File::from(exec_write);
    let result: io::Result<()> = (|| {
        for target in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            // SAFETY: valid pipe descriptor, destination is standard output/error.
            if unsafe { libc::dup2(output_write.as_raw_fd(), target) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        drop(output_write);
        let argv = launch.declaration.command().argv();
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty runtime command"))?;
        let mut command = Command::new(program);
        command.args(args).env_clear().envs(
            launch
                .env
                .as_map()
                .iter()
                .map(|(key, value)| (key.as_str(), value.expose())),
        );
        // Neither user input nor service environment may expose notify authority
        // accidentally to the application after the relay closes NotifyAccess.
        command
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_PID")
            .env_remove("WATCHDOG_USEC");
        account.assume_process_credentials()?;
        std::env::set_current_dir(launch.declaration.working_directory().expose())?;
        // SAFETY: restore the standard disposition before exec; Rust ignores it.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        Err(command.exec())
    })();
    let error = result.expect_err("successful exec replaces this process");
    let _ = exec_error.write_all(&error.raw_os_error().unwrap_or(libc::EIO).to_ne_bytes());
    eprintln!(
        "SPACE runtime setup/exec failed (errno {})",
        error.raw_os_error().unwrap_or(libc::EIO)
    );
    Err(error)
}

fn relay(
    output: OwnedFd,
    output_write: OwnedFd,
    exec: OwnedFd,
    socket: UnixDatagram,
    main_pidfd: OwnedFd,
    config: &InvocationConfig,
    account: &GuestAccount,
) -> io::Result<()> {
    // SAFETY: handler only sets a lock-free atomic; no async runtime exists here.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }
    let mut output = File::from(output);
    let mut exec = Some(File::from(exec));
    nix::fcntl::fcntl(
        &output,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )?;
    let mut file = Output::open().ok();
    let mut probe_output = Some(File::from(output_write));
    let mut dropped: u64 = 0;
    let mut reported_dropped = 0;
    let mut buffer = [0u8; 8192];
    let deadline = std::time::Instant::now() + config.declaration.startup_timeout();
    let mut exec_ready = false;
    let mut notified = false;
    let mut probe: Option<Probe> = None;
    let mut next_probe = std::time::Instant::now();
    let mut output_closed = false;
    loop {
        let mut stopping = STOP.load(Ordering::Relaxed);
        let mut poll = [
            libc::pollfd {
                fd: if output_closed {
                    -1
                } else {
                    output.as_raw_fd()
                },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: exec.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: main_pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: poll points to three initialized descriptors for the call duration.
        let polled = unsafe { libc::poll(poll.as_mut_ptr(), 3, if stopping { 0 } else { 100 }) };
        if polled < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
        if poll.last().is_some_and(|entry| entry.revents != 0) {
            // Drain queued crash output before exiting, even when descendants
            // hold the writer open. Never launch a probe or notify for a dead app.
            stopping = true;
        }
        if poll.get(1).is_some_and(|entry| entry.revents != 0) {
            let mut errno = [0; 4];
            exec_ready = exec.as_mut().expect("polled exec pipe").read(&mut errno)? == 0;
            exec = None;
        }
        if !notified && !stopping && std::time::Instant::now() >= deadline {
            // Fail this invocation so PID1 owns retry policy. A PID fd prevents
            // killing a later process if the original app has already exited.
            let _ = rustix::process::pidfd_send_signal(&main_pidfd, rustix::process::Signal::KILL);
            stopping = true;
        }
        if exec_ready && !notified && !stopping {
            let ready = match config.declaration.readiness() {
                RuntimeReadiness::Launch => true,
                RuntimeReadiness::Exec(command) => {
                    if probe.is_none() && std::time::Instant::now() >= next_probe {
                        probe = Some(
                            Probe::spawn(
                                command.argv(),
                                config,
                                account,
                                probe_output.as_ref().expect("probe output before ready"),
                            )
                            .inspect_err(|_| {
                                let _ = rustix::process::pidfd_send_signal(
                                    &main_pidfd,
                                    rustix::process::Signal::KILL,
                                );
                            })?,
                        );
                    }
                    let status = probe
                        .as_mut()
                        .map(|probe| probe.child.try_wait())
                        .transpose()?
                        .flatten();
                    if let Some(status) = status {
                        probe = None;
                        next_probe =
                            std::time::Instant::now() + std::time::Duration::from_millis(300);
                        status.success()
                    } else {
                        false
                    }
                }
            };
            if ready {
                socket.send(b"READY=1\nNOTIFYACCESS=none")?;
                notified = true;
                probe_output = None;
            }
        }
        // Bounded work per iteration also bounds final SIGTERM drain even when
        // descendants keep the pipe open and keep writing.
        for _ in 0..16 {
            if output_closed {
                break;
            }
            match output.read(&mut buffer) {
                Ok(0) => {
                    output_closed = true;
                    break;
                }
                Ok(count) => {
                    if let Some(log) = &mut file {
                        if log
                            .append(
                                buffer
                                    .get(..count)
                                    .ok_or_else(|| io::Error::other("invalid pipe read length"))?,
                            )
                            .is_err()
                        {
                            file = None;
                            dropped += count as u64;
                        }
                    } else {
                        dropped += count as u64;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        // Best-effort, bounded diagnostic counter; never fall back to journald.
        if dropped != reported_dropped {
            let _ = std::fs::write("/run/space-runtime/output-dropped", dropped.to_string());
            reported_dropped = dropped;
        }
        if stopping || (output_closed && notified) {
            return Ok(());
        }
    }
}

struct Probe {
    child: std::process::Child,
    group: i32,
}
impl Probe {
    fn spawn(
        argv: &[String],
        config: &InvocationConfig,
        account: &GuestAccount,
        output: &File,
    ) -> io::Result<Self> {
        use std::process::Stdio;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty readiness command"))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(
                config
                    .env
                    .as_map()
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.expose())),
            )
            .current_dir(config.declaration.working_directory().expose())
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output.try_clone()?)
            .process_group(0);
        let account = account.clone();
        // SAFETY: only precomputed credential syscalls run after fork, before exec.
        unsafe {
            command.pre_exec(move || account.assume_process_credentials());
        }
        let child = command.spawn()?;
        let group = child.id() as i32;
        Ok(Self { child, group })
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.group),
            nix::sys::signal::Signal::SIGKILL,
        );
        // Do not wait indefinitely on a task blocked in the kernel. PID1 owns
        // the whole service cgroup and reaps children when this relay exits.
        let _ = self.child.try_wait();
    }
}
