use super::*;
use crate::service::tests::service_tests::insert_test_sandbox;
use pplx_space_common_core::types::LocalArtifactId;
use std::collections::HashSet;

fn sweep(service: &SandboxManagerService, root: &Path) -> HashSet<PathBuf> {
    let mut scan = StagingCleanupScan::default();
    let mut found = HashSet::new();
    for _ in 0..128 {
        let batch = service
            .abandoned_suspend_staging_batch(root, &mut scan)
            .unwrap();
        assert!(batch.len() <= DELETE_BATCH);
        found.extend(batch);
        if scan.sandboxes.is_none() {
            return found;
        }
    }
    panic!("finite fixture must finish its sweep");
}

#[tokio::test]
async fn staging_cleanup_requires_stable_unowned_lifecycle() {
    use SandboxState::*;
    let cache = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let mut expected = HashSet::new();
    let mut paused = None;
    for state in [
        Creating,
        Created,
        Starting,
        Started,
        Stopping,
        Stopped,
        Deleting,
        Pausing,
        Paused,
        Resuming,
        Checkpointing,
        Suspending,
        Suspended,
        Restoring,
        CleanupRequired,
        CleanupFailed,
        Quarantined,
        Failed,
    ] {
        let id = SandboxId::new().to_string();
        insert_test_sandbox(&conn, &id, state);
        let artifact_id = LocalArtifactId::new();
        let path = suspend_snapshot_publication::suspend_snapshot_staging_dir(
            &cache.path().join(&id),
            artifact_id,
        );
        std::fs::create_dir_all(&path).unwrap();
        let retired = path.with_file_name(format!("{RETIRED_PREFIX}{artifact_id}"));
        // These are recovery evidence, not safe garbage, in transitional or
        // quarantined states even when their lifecycle locks are currently free.
        match state {
            Started | Paused | Stopped | Suspended => {
                expected.insert(retired.clone());
            }
            Creating | Created | Starting | Stopping | Deleting | Pausing | Resuming
            | Checkpointing | Suspending | Restoring | CleanupRequired | CleanupFailed
            | Quarantined | Failed => {}
        }
        if state == Paused {
            paused = Some((id, retired));
        }
    }
    let unknown = suspend_snapshot_publication::suspend_snapshot_staging_dir(
        &cache.path().join(SandboxId::new().to_string()),
        LocalArtifactId::new(),
    );
    std::fs::create_dir_all(&unknown).unwrap();
    let service = SandboxManagerService::new(conn);
    let (id, path) = paused.unwrap();
    for owner in 0..3 {
        let _wake = (owner == 0).then(|| {
            service
                .auto_lifecycle
                .wake_lock(&id)
                .try_lock_owned()
                .unwrap()
        });
        let _artifact = (owner == 1).then(|| {
            service
                .auto_lifecycle
                .artifact_lock(&id)
                .try_lock_owned()
                .unwrap()
        });
        let _cleanup = (owner == 2).then(|| match service.try_cleanup_fence(&id) {
            CleanupLockTryResult::Acquired(guard) => guard,
            _ => panic!("free fixture fence"),
        });
        let mut unowned = expected.clone();
        unowned.remove(&path);
        assert_eq!(sweep(&service, cache.path()), unowned);
    }
    assert_eq!(sweep(&service, cache.path()), expected);
}

#[tokio::test]
async fn staging_cleanup_rediscovery_survives_rollback_failed_deletion_and_restart() {
    let cache = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let id = SandboxId::new().to_string();
    insert_test_sandbox(&conn, &id, SandboxState::Suspending);
    let root = cache.path().join(&id);
    let snapshots = suspend_snapshot_publication::suspend_snapshot_staging_parent_dir(&root);
    std::fs::create_dir_all(&snapshots).unwrap();
    // Exceed both limits of the old one-shot sweep. Non-staging entries must
    // not starve the tail, and successful capture history must remain untouched.
    for n in 0..1100 {
        std::fs::write(snapshots.join(format!("d-1m-{n}")), b"history").unwrap();
    }
    let expected: HashSet<_> = (0..80)
        .map(|_| {
            let artifact_id = LocalArtifactId::new();
            let path =
                suspend_snapshot_publication::suspend_snapshot_staging_dir(&root, artifact_id);
            suspend_snapshot_publication::write_full_suspend_artifact(
                &path,
                &id,
                artifact_id,
                b"old attempt",
            );
            path.with_file_name(format!("{RETIRED_PREFIX}{artifact_id}"))
        })
        .collect();
    let service = SandboxManagerService::new(conn);
    assert!(sweep(&service, cache.path()).is_empty());
    assert!(crate::db::rollback_suspending_to_paused(&service.db.lock().unwrap(), &id).unwrap());
    // Simulate an unacknowledged deletion by leaving the selected files in
    // place. Both another sweep and a restarted scan must rediscover them.
    for state in [
        SandboxState::Paused,
        SandboxState::Started,
        SandboxState::Paused,
    ] {
        crate::db::update_state(&service.db.lock().unwrap(), &id, state)
            .unwrap()
            .require_applied()
            .unwrap();
        assert_eq!(sweep(&service, cache.path()), expected);
        assert!(
            suspend_snapshot_publication::find_complete_suspend_snapshot_staging_dir(
                &root,
                &Default::default()
            )
            .unwrap()
            .is_none(),
            "later recovery must never adopt an artifact pending deletion"
        );
    }
    // A replacement capture keeps its own name; old retired work remains
    // discoverable even while the replacement owns the lifecycle.
    let fresh_id = LocalArtifactId::new();
    let fresh = suspend_snapshot_publication::suspend_snapshot_staging_dir(&root, fresh_id);
    suspend_snapshot_publication::write_full_suspend_artifact(
        &fresh,
        &id,
        fresh_id,
        b"new attempt",
    );
    crate::db::update_state(&service.db.lock().unwrap(), &id, SandboxState::Suspending)
        .unwrap()
        .require_applied()
        .unwrap();
    let _wake = service
        .auto_lifecycle
        .wake_lock(&id)
        .try_lock_owned()
        .unwrap();
    assert_eq!(sweep(&service, cache.path()), expected);
    assert_eq!(
        suspend_snapshot_publication::find_complete_suspend_snapshot_staging_dir(
            &root,
            &Default::default()
        )
        .unwrap()
        .unwrap()
        .local_artifact_id,
        fresh_id
    );
    for path in &expected {
        std::fs::remove_dir_all(path).unwrap();
    }
    assert!(sweep(&service, cache.path()).is_empty());
    assert_eq!(std::fs::read_dir(snapshots).unwrap().count(), 1101);
}
