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

/// Publication retires a superseded snapshot into this lane while the sandbox
/// is still `Suspending` and its locks are held. Once retired, the entry is
/// garbage the sweep collects regardless of state: only retiring a live
/// `.suspend-publish-*` directory needs the lifecycle gate above.
#[tokio::test]
async fn publication_retirement_is_owned_by_the_sweep_in_any_lifecycle_state() {
    let cache = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let id = SandboxId::new().to_string();
    insert_test_sandbox(&conn, &id, SandboxState::Suspending);
    let sandbox_dir = cache.path().join(&id);
    let snapshot_dir = suspend_snapshot_publication::canonical_suspend_snapshot_dir(&sandbox_dir);
    let (old_id, new_id) = (LocalArtifactId::new(), LocalArtifactId::new());
    suspend_snapshot_publication::write_full_suspend_artifact(&snapshot_dir, &id, old_id, b"old");
    let staging = suspend_snapshot_publication::suspend_snapshot_staging_dir(&sandbox_dir, new_id);
    suspend_snapshot_publication::write_full_suspend_artifact(&staging, &id, new_id, b"new");
    let service = SandboxManagerService::new(conn);
    let _wake = service
        .auto_lifecycle
        .wake_lock(&id)
        .try_lock_owned()
        .unwrap();
    let _artifact = service
        .auto_lifecycle
        .artifact_lock(&id)
        .try_lock_owned()
        .unwrap();

    suspend_snapshot_publication::publish_suspend_snapshot_dir(
        &staging,
        &snapshot_dir,
        new_id,
        &Default::default(),
    )
    .unwrap();

    assert!(
        !suspend_snapshot_publication::retired_suspend_snapshot_dir(&sandbox_dir, new_id).exists()
    );
    let swept = sweep(&service, cache.path());
    assert_eq!(swept.len(), 1);
    let garbage = swept.into_iter().next().unwrap();
    assert_eq!(std::fs::read(garbage.join("vmstate")).unwrap(), b"old");
}

/// A hold is the canonical snapshot moved aside by a publication. Without a
/// canonical snapshot it is the rollback candidate that publication restores,
/// and the sweep leaves it alone. With one, it is garbage even when a later
/// publication has replaced the artifact that displaced it: a release that
/// failed after commit must not strand the hold until sandbox deletion.
#[tokio::test]
async fn sweep_releases_holds_once_a_canonical_snapshot_exists() {
    let cache = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let committed = SandboxId::new().to_string();
    let in_flight = SandboxId::new().to_string();
    insert_test_sandbox(&conn, &committed, SandboxState::Suspended);
    insert_test_sandbox(&conn, &in_flight, SandboxState::Suspended);
    let (a, b, c) = (
        LocalArtifactId::new(),
        LocalArtifactId::new(),
        LocalArtifactId::new(),
    );

    // A -> B committed but B's release failed; then B -> C committed.
    let committed_dir = cache.path().join(&committed);
    suspend_snapshot_publication::write_full_suspend_artifact(
        &suspend_snapshot_publication::canonical_suspend_snapshot_dir(&committed_dir),
        &committed,
        c,
        b"c",
    );
    let stale_hold = suspend_snapshot_publication::retired_suspend_snapshot_dir(&committed_dir, b);
    let fresh_hold = suspend_snapshot_publication::retired_suspend_snapshot_dir(&committed_dir, c);
    suspend_snapshot_publication::write_full_suspend_artifact(&stale_hold, &committed, a, b"a");
    suspend_snapshot_publication::write_full_suspend_artifact(&fresh_hold, &committed, b, b"b");

    // A publication of B interrupted between its renames: no canonical.
    let in_flight_dir = cache.path().join(&in_flight);
    let kept = suspend_snapshot_publication::retired_suspend_snapshot_dir(&in_flight_dir, b);
    suspend_snapshot_publication::write_full_suspend_artifact(&kept, &in_flight, a, b"a");
    let service = SandboxManagerService::new(conn);

    let swept = sweep(&service, cache.path());
    assert!(
        !stale_hold.exists(),
        "the hold B's failed release left behind"
    );
    assert!(
        !fresh_hold.exists(),
        "the hold displaced by the canonical artifact"
    );
    assert!(
        kept.exists(),
        "no canonical snapshot: the rollback candidate stays"
    );
    let mut contents: Vec<Vec<u8>> = swept
        .iter()
        .map(|garbage| {
            assert!(
                garbage
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(RETIRED_PREFIX)
            );
            std::fs::read(garbage.join("vmstate")).unwrap()
        })
        .collect();
    contents.sort();
    assert_eq!(contents, vec![b"a".to_vec(), b"b".to_vec()]);
}
