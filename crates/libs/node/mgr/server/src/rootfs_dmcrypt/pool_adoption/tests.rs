use super::*;
use std::os::unix::fs::FileExt;

struct DeviceCleanup(Option<SandboxId>);

impl Drop for DeviceCleanup {
    fn drop(&mut self) {
        let Some(id) = self.0 else { return };
        for device in [
            ReleaseDevice::Snapshot(&composite_name(&id)),
            ReleaseDevice::Crypt(&cow_mapper_name(&id)),
        ] {
            if let Err(error) = device.blocking_with(crate::host_command::output) {
                eprintln!("pool test device cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn snapshot_usage_rejects_invalid_stores_and_bounds_the_copy() {
    assert_eq!(
        copy_prefix_bytes("0 32768 snapshot 24/196608 16", 96 * 1024 * 1024).unwrap(),
        24 * 512 + 4 * 1024 * 1024
    );
    assert_eq!(
        copy_prefix_bytes("0 32768 snapshot 196608/196608 16", 96 * 1024 * 1024).unwrap(),
        96 * 1024 * 1024
    );
    for status in [
        "0 32768 snapshot Invalid",
        "0 32768 snapshot Overflow",
        "",
        "0 32768 linear 24/196608 16",
    ] {
        copy_prefix_bytes(status, 96 * 1024 * 1024).expect_err("invalid snapshot usage");
    }
}

#[tokio::test]
#[ignore = "requires root, loop/dm-crypt and an explicitly supplied disposable btrfs fixture directory"]
async fn open_pool_disk_survives_encryption_and_reopen() {
    let Some(root) = std::env::var_os("SPACE_NODEMGR_TEST_BTRFS_DIR") else {
        eprintln!("not opted in: set SPACE_NODEMGR_TEST_BTRFS_DIR to a disposable btrfs directory");
        return;
    };
    assert!(nix::unistd::geteuid().is_root());
    let temp = tempfile::tempdir_in(root).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let sandbox_id = SandboxId::new();
    let mut cleanup = DeviceCleanup(Some(sandbox_id));
    let origin = root.join("rootfs.ext4");
    let plain = root.join("cow.plain");
    let encrypted = root.join("cow.luks");
    let origin_file = Backing::create(&origin).unwrap();
    origin_file.resize(64 * 1024 * 1024).unwrap();
    let prepared = prepare_plaintext_snapshot(&sandbox_id, &origin, &plain, &encrypted)
        .await
        .unwrap();
    let disk = std::fs::File::options()
        .read(true)
        .write(true)
        .open(&prepared.mapper_path)
        .unwrap();
    // Cross several exception-metadata areas, keeping the same open device
    // descriptor that a booted Firecracker process would retain.
    for index in 0..1024_u64 {
        disk.write_all_at(&[index as u8; 4096], index * 8192)
            .unwrap();
    }
    disk.sync_all().unwrap();
    let dek = [42_u8; 32];
    let composite = composite_name(&sandbox_id);
    let original_table = run("dmsetup", &["table", &composite]).await.unwrap();
    let (_, cow_minor) = String::from_utf8_lossy(&original_table)
        .split_whitespace()
        .nth(4)
        .and_then(parse_device_numbers)
        .unwrap();
    let plain_backing = Backing::require(Backing::open(&plain, false).unwrap()).unwrap();
    let _original_cow = LoopDevice::matching(
        &PathBuf::from(format!("/dev/loop{cow_minor}")),
        &plain_backing.file,
    )
    .unwrap()
    .unwrap();

    let started = std::time::Instant::now();
    for attempt in 0..3 {
        if attempt == 1 {
            // Reconstruct a crash after the encrypted backing was committed
            // but before the live table switched away from the plaintext COW.
            suspend_device(&composite).await.unwrap();
            run(
                "dmsetup",
                &[
                    "reload",
                    &composite,
                    "--table",
                    &String::from_utf8_lossy(&original_table),
                ],
            )
            .await
            .unwrap();
            run("dmsetup", &["resume", &composite]).await.unwrap();
            suspend_device(&composite).await.unwrap();
        }
        let adopted = encrypt_pool_rootfs(
            sandbox_id,
            origin.clone(),
            plain.clone(),
            encrypted.clone(),
            &dek,
        )
        .await
        .unwrap();
        assert_eq!(adopted.mapper_path, prepared.mapper_path);
        run(
            "blockdev",
            &["--flushbufs", &prepared.mapper_path.to_string_lossy()],
        )
        .await
        .unwrap();
        for index in 0..1024_u64 {
            let mut actual = [0; 4096];
            disk.read_exact_at(&mut actual, index * 8192).unwrap();
            assert!(
                actual == [index as u8; 4096],
                "lost pre-adoption block {index}"
            );
        }
    }
    eprintln!(
        "pool rootfs encryption and retry completed in {:?}",
        started.elapsed()
    );
    disk.write_all_at(b"customer-write-after-encryption", 16 * 1024 * 1024)
        .unwrap();
    disk.sync_all().unwrap();
    let table = run("dmsetup", &["table", &composite_name(&sandbox_id)])
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&table)
            .split_whitespace()
            .nth(4)
            .and_then(parse_device_numbers),
        Some(
            crypt_mapper_numbers(&cow_mapper_name(&sandbox_id))
                .await
                .unwrap()
        )
    );
    encrypt_pool_rootfs(
        sandbox_id,
        origin.clone(),
        plain.clone(),
        encrypted.clone(),
        &[43; 32],
    )
    .await
    .expect_err("an incorrect DEK must not acknowledge adoption");
    drop(disk);
    release(&sandbox_id, &origin).await.unwrap();

    let reopened = prepare(PrepareParams {
        sandbox_id,
        origin_path: origin.clone(),
        cow_path: encrypted,
        restore_cow_source: None,
        rootfs_size_bytes: 0,
        dek: &dek,
    })
    .await
    .unwrap();
    let disk = std::fs::File::open(reopened.mapper_path).unwrap();
    let mut actual = [0; b"customer-write-after-encryption".len()];
    disk.read_exact_at(&mut actual, 16 * 1024 * 1024).unwrap();
    assert_eq!(&actual, b"customer-write-after-encryption");
    for index in 0..1024_u64 {
        let mut actual = [0; 4096];
        disk.read_exact_at(&mut actual, index * 8192).unwrap();
        assert!(actual == [index as u8; 4096], "lost reopened block {index}");
    }
    drop(disk);
    release(&sandbox_id, &origin).await.unwrap();
    cleanup.0 = None;
}

#[tokio::test]
#[ignore = "requires root, loop/dm and an explicitly supplied disposable btrfs fixture directory"]
async fn plaintext_pool_adoption_preserves_open_disk_and_regular_file_snapshots() {
    let Some(root) = std::env::var_os("SPACE_NODEMGR_TEST_BTRFS_DIR") else {
        eprintln!("not opted in: set SPACE_NODEMGR_TEST_BTRFS_DIR to a disposable btrfs directory");
        return;
    };
    assert!(nix::unistd::geteuid().is_root());
    let temp = tempfile::tempdir_in(root).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let id = SandboxId::new();
    let mut cleanup = DeviceCleanup(Some(id));
    let origin = root.join("rootfs.ext4");
    let plain = root.join("cow.plain");
    Backing::create(&origin)
        .unwrap()
        .resize(64 * 1024 * 1024)
        .unwrap();
    let prepared = prepare_plaintext_snapshot(&id, &origin, &plain, &root.join("cow.luks"))
        .await
        .unwrap();
    let disk = std::fs::File::options()
        .read(true)
        .write(true)
        .open(&prepared.mapper_path)
        .unwrap();
    for index in 0..1024_u64 {
        disk.write_all_at(&[index as u8; 4096], index * 8192)
            .unwrap();
    }
    disk.sync_all().unwrap();

    for _ in 0..2 {
        let adopted = collapse_pool_rootfs(id, origin.clone(), plain.clone())
            .await
            .unwrap();
        assert_eq!(adopted.mapper_path, prepared.mapper_path);
    }
    disk.write_all_at(b"after-adoption", 16 * 1024 * 1024)
        .unwrap();
    disk.sync_all().unwrap();
    run(
        "blockdev",
        &["--flushbufs", &prepared.mapper_path.to_string_lossy()],
    )
    .await
    .unwrap();

    assert!(!plain.exists());
    let regular = std::fs::File::open(&origin).unwrap();
    for source in [&disk, &regular] {
        for index in 0..1024_u64 {
            let mut actual = [0; 4096];
            source.read_exact_at(&mut actual, index * 8192).unwrap();
            assert!(
                actual == [index as u8; 4096],
                "lost plaintext block {index}"
            );
        }
        let mut actual = [0; 14];
        source.read_exact_at(&mut actual, 16 * 1024 * 1024).unwrap();
        assert_eq!(&actual, b"after-adoption");
    }
    drop(disk);
    release_plaintext(&id, &origin, &plain, false)
        .await
        .unwrap();
    cleanup.0 = None;
}
