use super::*;

#[test]
fn source_identity_is_required_before_touching_retained_subvolumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(root.join(DIRECTORY)).unwrap();
    let expected = Uuid::now_v7();
    for (version, identity) in [(2, expected), (1, Uuid::now_v7())] {
        let manifest = Manifest {
            version,
            source_id: identity,
            source_uuid: Uuid::now_v7(),
            parent_uuid: Uuid::now_v7(),
            received_uuid: Uuid::now_v7(),
        };
        std::fs::write(
            root.join(DIRECTORY).join(MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let artifact = trusted_paths::open_directory(&root).unwrap();
        assert!(
            matches!(SendSource::open(&artifact, expected), Err(error) if error.message() == "send source identity changed")
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires root and an explicitly supplied disposable btrfs fixture directory"]
fn retained_source_regenerates_after_producer_deletion_and_receives_on_same_filesystem() {
    let Some(root) = std::env::var_os("SPACE_NODEMGR_TEST_BTRFS_DIR") else {
        eprintln!("not opted in: SPACE_NODEMGR_TEST_BTRFS_DIR is unset");
        return;
    };
    assert!(nix::unistd::geteuid().is_root());
    let temp = tempfile::tempdir_in(root).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let directory = trusted_paths::open_directory(&root).unwrap();
    btrfs::create(&directory, "template".as_ref()).unwrap();
    let template = artifact_fs::directory(&directory, "template".as_ref()).unwrap();
    std::fs::write(root.join("template/rootfs.ext4"), b"original template").unwrap();
    btrfs::stamp(&template, Uuid::now_v7()).unwrap();
    btrfs::set_readonly(&template, true).unwrap();
    btrfs::snapshot(&template, &directory, "instance".as_ref(), false).unwrap();
    let instance = artifact_fs::directory(&directory, "instance".as_ref()).unwrap();
    let expected = b"frozen snapshot data: longer than the original";
    std::fs::write(root.join("instance/rootfs.ext4"), expected).unwrap();
    std::fs::create_dir(root.join("snapshot")).unwrap();
    let snapshot = root.join("snapshot");
    let id = capture(&snapshot, &root.join("template"), &root.join("instance")).unwrap();
    artifact_fs::remove_at(&directory, "instance".as_ref()).unwrap();
    artifact_fs::remove_at(&directory, "template".as_ref()).unwrap();
    drop((template, instance));
    let mut lengths = Vec::new();
    // Reopen all persisted state for every attempt; no in-memory pin survives.
    for _ in 0..2 {
        lengths.push(materialize(&snapshot, id, None).unwrap());
        let received = root.join("received");
        std::fs::create_dir(&received).unwrap();
        let stream = File::open(snapshot.join("rootfs.btrfs-send")).unwrap();
        let output = std::process::Command::new("btrfs")
            .arg("receive")
            .arg(&received)
            .stdin(stream)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(received.join("instance.ro/rootfs.ext4")).unwrap(),
            expected
        );
        artifact_fs::remove_at(&directory, "received".as_ref()).unwrap();
        std::fs::remove_file(snapshot.join("rootfs.btrfs-send")).unwrap();
    }
    assert!(lengths.iter().all(|bytes| *bytes > 16));
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        materialize(&snapshot, id, Some(&cancel))
            .unwrap_err()
            .code(),
        tonic::Code::Cancelled
    );
    assert!(!snapshot.join("rootfs.btrfs-send").exists());
    // Existing recursive artifact deletion reclaims nested RO subvolumes.
    artifact_fs::remove_at(&directory, "snapshot".as_ref()).unwrap();
}
