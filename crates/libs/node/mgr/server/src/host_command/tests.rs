use super::*;

#[test]
fn foreground_command_io_and_failure_paths_are_bounded() {
    let bytes = vec![b'x'; 128 * 1024];
    let copied = output(Command::new("cat").arg("-"), Duration::from_secs(2), &bytes).unwrap();
    assert!(copied.status.success());
    assert_eq!(copied.stdout, bytes);

    let closed = output(
        Command::new("sh").args(["-c", "exec 1>&- 2>&-; exec cat >/dev/null"]),
        Duration::from_secs(2),
        &bytes,
    )
    .unwrap();
    assert!(closed.status.success());
    assert!(closed.stdout.is_empty() && closed.stderr.is_empty());

    let mut input = tempfile::tempfile().unwrap();
    input.write_all(&bytes).unwrap();
    std::io::Seek::rewind(&mut input).unwrap();
    let copied = stream(
        Command::new("cat").arg("-"),
        Duration::from_secs(2),
        Stdio::from(input),
        Stdio::piped(),
        None,
    )
    .unwrap();
    assert!(copied.status.success());
    assert_eq!(copied.stdout, bytes);

    for script in ["exec yes x", "exec yes x >&2"] {
        // Static test commands only. exec keeps the fixture a single foreground
        // child, matching the host-command runner's contract.
        let error = output(
            Command::new("sh").args(["-c", script]),
            Duration::from_secs(2),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
    let failure = output(
        Command::new("sh").args(["-c", "printf failure >&2; exit 7"]),
        Duration::from_secs(2),
        &[],
    )
    .unwrap();
    assert_eq!(failure.status.code(), Some(7));
    assert_eq!(failure.stderr, b"failure");

    // Obtain the PID from spawn, not from a file the child may never get time
    // to write on a loaded host. Exercise the same capture/reap path.
    for (cancelled, script) in [
        (false, "exec sleep 30"),
        (true, "exec sleep 30"),
        (false, "exec 1>&- 2>&-; exec sleep 30"),
    ] {
        let cancel = tokio_util::sync::CancellationToken::new();
        let child = OwnedChild(
            Command::new("sh")
                .args(["-c", script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let pid = libc::pid_t::try_from(child.0.id()).unwrap();
        if cancelled {
            cancel.cancel();
        }
        let error = capture(
            child,
            Instant::now(),
            Duration::from_millis(20),
            &[],
            Some(&cancel),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            if cancelled {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::TimedOut
            }
        );
        assert_eq!(
            nix::sys::wait::waitpid(
                nix::unistd::Pid::from_raw(pid),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG)
            ),
            Err(nix::errno::Errno::ECHILD),
            "the timed-out child must already be reaped",
        );
    }
}
