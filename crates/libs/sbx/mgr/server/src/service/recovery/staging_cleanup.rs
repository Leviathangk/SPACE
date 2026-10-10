//! Abandoned suspend directories are the cleanup record, including after the
//! lifecycle row has returned to a stable state. The inventory loop owns retry.
use pplx_space_common_core::types::LocalArtifactId;

use super::*;
use std::fs::ReadDir;
use std::path::PathBuf;

const SCAN_ENTRIES: usize = 128;
const DELETE_BATCH: usize = 8;
/// Entries under a sandbox's `snapshots/` with this prefix are logically
/// deleted. The periodic sweep removes them through node-manager, which can
/// delete nested subvolumes this service cannot.
pub(in crate::service) const RETIRED_PREFIX: &str = ".suspend-gc-pending-";

/// Two directory handles, independent of sandbox count. Keep enumeration across
/// ticks so retained/busy entries and large snapshot inventories cannot hide the
/// tail forever. Process restart simply begins another sweep.
#[derive(Default)]
pub(super) struct StagingCleanupScan {
    sandboxes: Option<ReadDir>,
    staging: Option<(SandboxId, ReadDir)>,
}

impl SandboxManagerService {
    pub(super) fn abandoned_suspend_staging_batch(
        &self,
        cache_dir: &Path,
        scan: &mut StagingCleanupScan,
    ) -> std::io::Result<Vec<PathBuf>> {
        if scan.sandboxes.is_none() {
            scan.sandboxes = match std::fs::read_dir(cache_dir) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
        }
        let mut paths = Vec::new();
        let mut remaining = SCAN_ENTRIES;
        while remaining > 0 && paths.len() < DELETE_BATCH {
            if scan.staging.is_none() {
                remaining -= 1;
                let Some(entry) = scan.sandboxes.as_mut().and_then(Iterator::next) else {
                    scan.sandboxes = None;
                    break;
                };
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let Some(id) = name
                    .to_str()
                    .and_then(|name| name.parse::<SandboxId>().ok())
                else {
                    continue;
                };
                if name != id.to_string().as_str() {
                    continue;
                }
                let parent = suspend_snapshot_publication::suspend_snapshot_staging_parent_dir(
                    &entry.path(),
                );
                match std::fs::read_dir(&parent) {
                    Ok(entries) => scan.staging = Some((id, entries)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        tracing::warn!(sandbox_id = %id, %error, "cannot enumerate abandoned suspend staging; will retry");
                        continue;
                    }
                }
            }
            let (id, entries) = scan.staging.as_mut().ok_or_else(|| {
                std::io::Error::other("suspend staging scan lost its open directory")
            })?;
            let id = id.to_string();
            // Retire names while no publication/cleanup can begin. Once outside
            // the publication namespace, a later recovery cannot adopt files
            // whose deletion is pending. No unlink occurs until node-manager
            // confirms exclusion from surviving captures and transfers.
            let wake = self.auto_lifecycle.wake_lock(&id);
            let artifact = self.auto_lifecycle.artifact_lock(&id);
            let guards = match (
                wake.try_lock(),
                artifact.try_lock(),
                self.try_cleanup_fence(&id),
            ) {
                (Ok(wake), Ok(artifact), CleanupLockTryResult::Acquired(cleanup)) => {
                    Some((wake, artifact, cleanup))
                }
                _ => None,
            };
            let eligible = if guards.is_some() {
                self.startup_recovery_db().and_then(|db| {
                    crate::db::get_sandbox(&db, &id).map_err(StartupRecoveryError::from)
                })
            } else {
                Ok(None)
            };
            let may_retire = match eligible {
                Ok(Some(row))
                    if matches!(
                        row.state,
                        SandboxState::Started
                            | SandboxState::Paused
                            | SandboxState::Stopped
                            | SandboxState::Suspended
                    ) =>
                {
                    true
                }
                Ok(_) => false,
                Err(error) => {
                    tracing::warn!(sandbox_id = %id, %error, "cannot establish suspend staging owner; retaining files");
                    false
                }
            };
            while remaining > 0 && paths.len() < DELETE_BATCH {
                remaining -= 1;
                let Some(entry) = entries.next() else {
                    scan.staging = None;
                    break;
                };
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if name
                    .strip_prefix(RETIRED_PREFIX)
                    .is_some_and(|id| id.parse::<LocalArtifactId>().is_ok())
                {
                    paths.push(entry.path());
                } else if guards.is_some()
                    && name
                        .strip_prefix(suspend_snapshot_publication::RETIRED_HOLD_PREFIX)
                        .and_then(|id| id.parse::<LocalArtifactId>().ok())
                        .is_some()
                {
                    // A hold is created by moving the canonical snapshot aside,
                    // so while no canonical snapshot exists it is the rollback
                    // candidate of a publication that may still be in flight.
                    // Once any canonical snapshot exists the hold is garbage:
                    // either its publication committed and failed to release
                    // it, or a later publication has replaced that artifact
                    // too. Matching the hold's name against the canonical
                    // artifact would strand holds after that later publication.
                    // The lifecycle guards make that observation current: a
                    // publication retry restores the hold to the canonical
                    // path and moves it back before replacing it, so a sweep
                    // racing it could read a canonical snapshot that is the
                    // hold itself.
                    let sandbox_dir = entry
                        .path()
                        .parent()
                        .and_then(|p| p.parent().map(Path::to_path_buf));
                    let Some(sandbox_dir) = sandbox_dir else {
                        continue;
                    };
                    let canonical =
                        suspend_snapshot_publication::canonical_suspend_snapshot_dir(&sandbox_dir);
                    if sidecar_artifact_id(&canonical).is_some() {
                        match suspend_snapshot_publication::release_retired_suspend_snapshot(
                            &sandbox_dir,
                            &entry.path(),
                        ) {
                            Ok(Some(released)) => paths.push(released),
                            Ok(None) => {}
                            Err(error) => {
                                tracing::warn!(sandbox_id = %id, %error, "cannot release retired snapshot; will retry")
                            }
                        }
                    } else if may_retire {
                        // No canonical snapshot and a stable lifecycle state
                        // under the guards: the publication that moved this
                        // snapshot aside is over, and recovery rolled it back
                        // without restoring the hold. The hold is the
                        // sandbox's snapshot if the row still pins its
                        // artifact; otherwise the live VM superseded it.
                        match self.hold_disposition(&id, &entry.path()) {
                            Ok(HoldDisposition::Restore) => {
                                match std::fs::rename(entry.path(), &canonical) {
                                    Ok(()) => {
                                        tracing::warn!(sandbox_id = %id, "restored the held snapshot of a rolled-back publication")
                                    }
                                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                    Err(error) => {
                                        tracing::warn!(sandbox_id = %id, %error, "cannot restore held snapshot; will retry")
                                    }
                                }
                            }
                            Ok(HoldDisposition::Release) => {
                                match suspend_snapshot_publication::release_retired_suspend_snapshot(
                                    &sandbox_dir,
                                    &entry.path(),
                                ) {
                                    Ok(Some(released)) => paths.push(released),
                                    Ok(None) => {}
                                    Err(error) => {
                                        tracing::warn!(sandbox_id = %id, %error, "cannot release retired snapshot; will retry")
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(sandbox_id = %id, %error, "cannot establish held snapshot owner; retaining it")
                            }
                        }
                    }
                } else if may_retire
                    && let Some(artifact) = name
                        .strip_prefix(suspend_snapshot_publication::SUSPEND_STAGING_PREFIX)
                        .and_then(|id| id.parse::<LocalArtifactId>().ok())
                {
                    let retired = entry
                        .path()
                        .with_file_name(format!("{RETIRED_PREFIX}{artifact}"));
                    match std::fs::rename(entry.path(), &retired) {
                        Ok(()) => paths.push(retired),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::warn!(sandbox_id = %id, %error, "cannot retire abandoned suspend staging; will retry")
                        }
                    }
                }
            }
        }
        Ok(paths)
    }
}

/// The artifact a snapshot directory's sidecar names, read without validating
/// the artifact: enough to know a hold was displaced, or which artifact it is.
fn sidecar_artifact_id(snapshot_dir: &Path) -> Option<LocalArtifactId> {
    #[derive(serde::Deserialize)]
    struct Identity {
        local_artifact_id: Option<LocalArtifactId>,
    }
    let path = snapshot_dir.join(crate::snapshot_upload::SNAPSHOT_METADATA_FILENAME);
    serde_json::from_slice::<Identity>(&std::fs::read(path).ok()?)
        .ok()?
        .local_artifact_id
}

enum HoldDisposition {
    /// The row still pins the held artifact while suspended: the hold is the
    /// snapshot a wake needs, moved aside by a publication that never
    /// committed.
    Restore,
    /// Nothing pins it: the sandbox runs on, or suspended on another artifact.
    Release,
}

impl SandboxManagerService {
    fn hold_disposition(
        &self,
        sandbox_id: &str,
        hold: &Path,
    ) -> Result<HoldDisposition, StartupRecoveryError> {
        let Some(held) = sidecar_artifact_id(hold) else {
            // An unreadable hold cannot be the snapshot a wake would use.
            return Ok(HoldDisposition::Release);
        };
        let db = self.startup_recovery_db()?;
        let pinned =
            crate::db::sandbox_suspended_with_pinned_artifact(&db, sandbox_id, &held.to_string())?;
        Ok(if pinned {
            HoldDisposition::Restore
        } else {
            HoldDisposition::Release
        })
    }
}

pub(super) async fn delete_batch(
    client: &mut pplx_space_node_mgr_client::NodeManagerGrpcClient,
    paths: Vec<PathBuf>,
    shutdown: &CancellationToken,
) {
    // Bound how long this hygiene pass delays lifecycle recovery. Expiry drops
    // only the RPC waiter: node-manager retains deletion ownership. Unconfirmed
    // paths stay on disk and are rediscovered on the next complete sweep.
    let work = async {
        for path in paths {
            if shutdown.is_cancelled() {
                break;
            }
            if let Err(error) = client
                .delete_cache_artifact(
                    pplx_space_node_mgr_client::proto::DeleteCacheArtifactRequest {
                        local_path: path.to_string_lossy().into_owned(),
                    },
                )
                .await
            {
                tracing::warn!(staging_dir = %path.display(), %error, "abandoned suspend staging deletion failed; will retry");
            }
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .is_err()
    {
        tracing::warn!(
            "suspend staging cleanup budget reached; retained paths will be rediscovered"
        );
    }
}

#[cfg(test)]
mod tests;
