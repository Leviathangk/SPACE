use super::*;

#[test]
fn configuration_absence_is_distinct_from_corruption_or_unsupported_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime");
    assert_eq!(read_regular(&path, 10).unwrap(), None);
    std::fs::write(&path, b"12345678901").unwrap();
    read_regular(&path, 10).unwrap_err();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(directory.path().join("missing"), &path).unwrap();
    read_regular(&path, 10).unwrap_err();
    std::fs::remove_file(&path).unwrap();
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
    read_regular(&path, 10).unwrap_err();
}

#[test]
fn boot_readiness_distinguishes_pending_ready_failed_and_unexpectedly_masked() {
    for (load, active, pid, notify, attempted, expected) in [
        (
            "loaded",
            "inactive",
            0,
            "all",
            0,
            Some(BootObservation::Starting),
        ),
        (
            "loaded",
            "activating",
            12,
            "all",
            5,
            Some(BootObservation::Starting),
        ),
        (
            "loaded",
            "active",
            12,
            "none",
            5,
            Some(BootObservation::Ready),
        ),
        ("loaded", "failed", 0, "none", 5, None),
        ("loaded", "inactive", 0, "none", 5, None),
        ("masked", "inactive", 0, "all", 0, None),
    ] {
        let output = format!(
            "LoadState={load}\nActiveState={active}\nMainPID={pid}\nNotifyAccess={notify}\nInactiveExitTimestampMonotonic={attempted}\n"
        );
        assert_eq!(observe_boot(output.as_bytes()).ok(), expected);
    }
    assert!(builder_boot(
        "root=/dev/vda systemd.mask=space-runtime@.service quiet"
    ));
    assert!(!builder_boot(
        "root=/dev/vda other=systemd.mask=space-runtime@.service"
    ));
}

#[test]
fn all_apps_must_be_ready_and_a_missing_or_failed_peer_cannot_be_ignored() {
    let units = vec![
        "space-runtime@web.service".to_owned(),
        "space-runtime@worker.service".to_owned(),
    ];
    let web = "Id=space-runtime@web.service\nLoadState=loaded\nActiveState=active\nMainPID=11\nNotifyAccess=none\n";
    for (state, expected) in [
        ("active", Some(true)),
        ("activating", Some(false)),
        ("failed", None),
    ] {
        let worker = format!(
            "Id=space-runtime@worker.service\nLoadState=loaded\nActiveState={state}\nMainPID=12\nNotifyAccess=none\n"
        );
        assert_eq!(
            observe_apps(format!("{web}\n{worker}").as_bytes(), &units).ok(),
            expected
        );
    }
    assert_eq!(
        observe_apps(web.as_bytes(), &units).unwrap_err().code(),
        tonic::Code::Unavailable
    );
}
