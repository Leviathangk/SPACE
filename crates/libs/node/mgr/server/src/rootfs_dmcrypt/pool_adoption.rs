//! PoC: replace a paused pool member's COW without replacing its open rootfs device.

use super::*;

pub(crate) async fn encrypt_pool_rootfs(
    sandbox_id: SandboxId,
    origin_path: PathBuf,
    plain_path: PathBuf,
    encrypted_path: PathBuf,
    dek: &[u8],
) -> Result<PreparedRootfs, DmCryptError> {
    let composite = composite_name(&sandbox_id);
    let cow = cow_mapper_name(&sandbox_id);
    let mapper_path = PathBuf::from(format!("/dev/mapper/{composite}"));
    let cow_mapper = format!("/dev/mapper/{cow}");
    let origin =
        device_backing::blocking(move || Backing::require(Backing::open(&origin_path, false)?))
            .await?;
    reuse_live_composite(&composite, origin, mapper_path.clone()).await?;
    let table = run("dmsetup", &["table", &composite]).await?;
    let table = String::from_utf8_lossy(&table);
    let fields: Vec<_> = table.split_whitespace().collect();
    let [
        "0",
        sectors,
        "snapshot",
        origin_device,
        cow_device,
        "P",
        "8",
    ] = fields.as_slice()
    else {
        return Err(DmCryptError::InvalidParams(
            "pool rootfs is not the expected persistent snapshot".into(),
        ));
    };
    let encrypted = {
        let path = encrypted_path.clone();
        device_backing::blocking(move || Backing::open(&path, true)).await?
    };
    if let Some(encrypted) = encrypted {
        let device = if !dm_exists(&cow).await? {
            let device =
                device_backing::blocking(move || LoopDevice::attach(&encrypted, false)).await?;
            run_with_stdin(
                "cryptsetup",
                &[
                    "open",
                    "--key-file=-",
                    &device.path().to_string_lossy(),
                    &cow,
                ],
                dek,
            )
            .await?;
            device
        } else {
            let status = run("cryptsetup", &["status", &cow]).await?;
            let device = String::from_utf8_lossy(&status)
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find_map(|(key, value)| {
                    (key.trim() == "device").then(|| PathBuf::from(value.trim()))
                })
                .ok_or_else(|| {
                    DmCryptError::InvalidParams("encrypted COW has no backing device".into())
                })?;
            device_backing::blocking(move || {
                LoopDevice::matching(&device, &encrypted.file)?
                    .ok_or_else(|| std::io::Error::other("encrypted COW backing changed"))
            })
            .await?
        };
        run_with_stdin(
            "cryptsetup",
            &[
                "open",
                "--test-passphrase",
                "--key-file=-",
                &device.path().to_string_lossy(),
            ],
            dek,
        )
        .await?;
        if parse_device_numbers(cow_device) == Some(crypt_mapper_numbers(&cow).await?) {
            run_bounded("dmsetup", &["resume", &composite], FREEZE_COMMAND_BUDGET).await?;
            return Ok(PreparedRootfs { mapper_path });
        }
    } else {
        // A failed unpublished attempt may still own a mapper. Never overwrite
        // that device or let it masquerade as a committed encrypted COW.
        if dm_exists(&cow).await? {
            return Err(DmCryptError::InvalidParams(
                "uncommitted encrypted mapper requires cleanup".into(),
            ));
        }
        let numbers = parse_device_numbers(cow_device)
            .filter(|(major, _)| *major == 7)
            .ok_or_else(|| DmCryptError::InvalidParams("pool COW is not a loop device".into()))?;
        let source_device = PathBuf::from(format!("/dev/loop{}", numbers.1));
        let source = device_backing::blocking(move || {
            let plain = Backing::require(Backing::open(&plain_path, false)?)?;
            LoopDevice::matching(&source_device, &plain.file)?
                .ok_or_else(|| std::io::Error::other("pool COW backing changed"))
        })
        .await?;
        let source_bytes = blockdev_sectors(source.path())
            .await?
            .saturating_mul(SECTOR_BYTES);
        let path = encrypted_path.clone();
        let (backing, device) = device_backing::blocking(move || {
            let backing = Backing::create_unpublished(&path)?;
            backing.resize(source_bytes + COW_HEADROOM_BYTES)?;
            let device = LoopDevice::attach(&backing, false)?;
            Ok((backing, device))
        })
        .await?;
        let device_path = device.path().to_string_lossy();
        run_with_stdin("cryptsetup", &luks_format_args(&device_path), dek).await?;
        run_with_stdin(
            "cryptsetup",
            &["open", "--key-file=-", &device_path, &cow],
            dek,
        )
        .await?;

        // The caller holds the paused VM's lifecycle ownership. Suspend the
        // block device too so all submitted writes and exception metadata drain.
        // After this point errors leave I/O suspended for retry or deletion.
        suspend_device(&composite).await?;
        let status = run("dmsetup", &["status", &composite]).await?;
        let copied_bytes = copy_prefix_bytes(&String::from_utf8_lossy(&status), source_bytes)?;
        // Snapshot bios bypass the loop device's page cache, which may still
        // contain the zeros written at provisioning. Buffered reads lose data.
        run_bounded(
            "dd",
            &[
                &format!("if={}", source.path().display()),
                &format!("of={cow_mapper}"),
                "bs=1M",
                "iflag=direct,count_bytes",
                "oflag=direct",
                &format!("count={copied_bytes}"),
                "conv=fsync",
                "status=none",
            ],
            FREEZE_COMMAND_BUDGET,
        )
        .await?;
        let name = encrypted_path
            .file_name()
            .ok_or_else(|| DmCryptError::InvalidParams("missing encrypted COW name".into()))?
            .to_owned();
        device_backing::blocking(move || backing.publish(&name)).await?;
    }

    // The canonical encrypted backing is the commit record. A retry completes
    // this switch; it never resumes writes on the old plaintext backing.
    suspend_device(&composite).await?;
    let replacement = format!("0 {sectors} snapshot {origin_device} {cow_mapper} P 8");
    run_bounded(
        "dmsetup",
        &["reload", &composite, "--table", &replacement],
        FREEZE_COMMAND_BUDGET,
    )
    .await?;
    run_bounded("dmsetup", &["resume", &composite], FREEZE_COMMAND_BUDGET).await?;
    Ok(PreparedRootfs { mapper_path })
}

async fn suspend_device(composite: &str) -> Result<(), DmCryptError> {
    let state = run(
        "dmsetup",
        &["info", "-c", "--noheadings", "-o", "suspended", composite],
    )
    .await?;
    match String::from_utf8_lossy(&state).trim() {
        "Active" => run_bounded("dmsetup", &["suspend", composite], FREEZE_COMMAND_BUDGET)
            .await
            .map(|_| ()),
        "Suspended" => Ok(()),
        _ => Err(DmCryptError::InvalidParams(
            "unrecognized rootfs device suspend state".into(),
        )),
    }
}

fn copy_prefix_bytes(status: &str, source_bytes: u64) -> Result<u64, DmCryptError> {
    let fields: Vec<_> = status.split_whitespace().collect();
    let used = fields
        .get(3)
        .and_then(|field| field.split_once('/'))
        .and_then(|(used, _)| used.parse::<u64>().ok());
    match used.filter(|_| fields.get(2) == Some(&"snapshot")) {
        // Include the next metadata area, including its zero terminator.
        Some(used) => used
            .checked_mul(SECTOR_BYTES)
            .and_then(|bytes| bytes.checked_add(FRESH_COW_ZERO_PREFIX_BYTES))
            .map(|bytes| bytes.min(source_bytes))
            .ok_or_else(|| DmCryptError::InvalidParams("pool COW size overflow".into())),
        None => Err(DmCryptError::InvalidParams(
            "pool snapshot is invalid or has unrecognized usage".into(),
        )),
    }
}

#[cfg(test)]
mod tests;

/// Plaintext adoption returns to the regular rootfs file so every existing
/// checkpoint/export path sees guest writes. The open mapper stays in place.
pub(crate) async fn collapse_pool_rootfs(
    sandbox_id: SandboxId,
    origin_path: PathBuf,
    plain_path: PathBuf,
) -> Result<PreparedRootfs, DmCryptError> {
    let composite = composite_name(&sandbox_id);
    let mapper_path = PathBuf::from(format!("/dev/mapper/{composite}"));
    let path = origin_path.clone();
    let origin =
        device_backing::blocking(move || Backing::require(Backing::open(&path, false)?)).await?;
    reuse_live_composite(&composite, origin, mapper_path.clone()).await?;
    let table = run("dmsetup", &["table", &composite]).await?;
    let table = String::from_utf8_lossy(&table);
    let fields: Vec<_> = table.split_whitespace().collect();
    match fields.as_slice() {
        ["0", _, "linear", _, "0"] => {
            run_bounded("dmsetup", &["resume", &composite], FREEZE_COMMAND_BUDGET).await?;
        }
        [
            "0",
            sectors,
            target @ ("snapshot" | "snapshot-merge"),
            _,
            cow_device,
            "P",
            "8",
        ] => {
            let (_, minor) = parse_device_numbers(cow_device)
                .filter(|(major, _)| *major == 7)
                .ok_or_else(|| {
                    DmCryptError::InvalidParams("pool COW is not a loop device".into())
                })?;
            let path = plain_path.clone();
            let cow_path = PathBuf::from(format!("/dev/loop{minor}"));
            device_backing::blocking(move || {
                let plain = Backing::require(Backing::open(&path, false)?)?;
                LoopDevice::matching(&cow_path, &plain.file)?
                    .ok_or_else(|| std::io::Error::other("pool COW backing changed"))
            })
            .await?;
            let writable = device_backing::blocking(move || {
                let origin = Backing::require(Backing::open(&origin_path, true)?)?;
                LoopDevice::attach(&origin, false)
            })
            .await?;
            if *target == "snapshot" {
                suspend_device(&composite).await?;
                let merge = format!(
                    "0 {sectors} snapshot-merge {} {cow_device} P 8",
                    writable.path().display()
                );
                run_bounded(
                    "dmsetup",
                    &["reload", &composite, "--table", &merge],
                    FREEZE_COMMAND_BUDGET,
                )
                .await?;
            }
            run_bounded("dmsetup", &["resume", &composite], FREEZE_COMMAND_BUDGET).await?;
            let deadline = tokio::time::Instant::now() + FREEZE_COMMAND_BUDGET;
            loop {
                let status = run("dmsetup", &["status", &composite]).await?;
                let status = String::from_utf8_lossy(&status);
                let fields: Vec<_> = status.split_whitespace().collect();
                let complete = match fields.as_slice() {
                    ["0", _, "snapshot-merge", allocated, metadata] => {
                        let used = allocated
                            .split_once('/')
                            .and_then(|(used, _)| used.parse::<u64>().ok());
                        used.is_some() && used == metadata.parse::<u64>().ok()
                    }
                    _ => {
                        return Err(DmCryptError::InvalidParams(format!(
                            "pool COW merge failed: {status}"
                        )));
                    }
                };
                if complete {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(DmCryptError::InvalidParams(
                        "pool COW merge timed out".into(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            suspend_device(&composite).await?;
            let linear = format!("0 {sectors} linear {} 0", writable.path().display());
            run_bounded(
                "dmsetup",
                &["reload", &composite, "--table", &linear],
                FREEZE_COMMAND_BUDGET,
            )
            .await?;
            run_bounded("dmsetup", &["resume", &composite], FREEZE_COMMAND_BUDGET).await?;
        }
        _ => {
            return Err(DmCryptError::InvalidParams(
                "pool rootfs cannot be collapsed".into(),
            ));
        }
    }
    device_backing::blocking(move || device_backing::unlink(&plain_path)).await?;
    Ok(PreparedRootfs { mapper_path })
}
