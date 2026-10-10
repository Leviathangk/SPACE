use super::*;
use crate::service::tests::service_tests::insert_test_sandbox;

#[tokio::test]
async fn periodic_inventory_progresses_in_pages_without_treating_other_pages_as_absent() {
    let cache = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let mut ids = Vec::new();
    for _ in 0..DIRECTORY_BATCH_SIZE + 1 {
        let id = SandboxId::new();
        std::fs::create_dir(cache.path().join(id.to_string())).unwrap();
        insert_test_sandbox(&conn, &id.to_string(), SandboxState::Stopped);
        ids.push(id);
    }
    let stale = SandboxId::new();
    insert_test_sandbox(&conn, &stale.to_string(), SandboxState::Stopped);
    let service = SandboxManagerService::new(conn);
    let mut scan = PeriodicInventoryScan::default();
    let shutdown = CancellationToken::new();
    let paths = SandboxInventoryPaths::new(cache.path(), runtime.path());
    let batch = service
        .discover_periodic_inventory_in(&mut scan, paths, &shutdown)
        .unwrap();
    service
        .reconcile_periodic_inventory_batch(batch, &Default::default())
        .unwrap();
    assert!(crate::db::sandbox_exists(&service.db_conn().unwrap(), &stale.to_string()).unwrap());
    let batch = service
        .discover_periodic_inventory_in(&mut scan, paths, &shutdown)
        .unwrap();
    service
        .reconcile_periodic_inventory_batch(batch, &Default::default())
        .unwrap();
    assert!(!crate::db::sandbox_exists(&service.db_conn().unwrap(), &stale.to_string()).unwrap());
    for id in ids {
        assert_eq!(
            crate::db::get_sandbox(&service.db_conn().unwrap(), &id.to_string())
                .unwrap()
                .unwrap()
                .state,
            SandboxState::Stopped
        );
        assert!(cache.path().join(id.to_string()).is_dir());
    }
}

#[test]
fn directory_entry_errors_consume_budget_without_rewinding_or_losing_other_entries() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..DIRECTORY_BATCH_SIZE + 1 {
        std::fs::create_dir(dir.path().join(index.to_string())).unwrap();
    }
    let mut entries = std::iter::once(Err(std::io::Error::other("unreadable entry")))
        .chain(std::fs::read_dir(dir.path()).unwrap());
    let (first, exhausted) = read_directory_batch(&mut entries);
    assert!(!exhausted);
    assert_eq!(first.len(), DIRECTORY_BATCH_SIZE - 1);
    let (second, exhausted) = read_directory_batch(&mut entries);
    assert!(exhausted);
    assert_eq!(second.len(), 2);
    assert_eq!(
        first
            .into_iter()
            .chain(second)
            .map(|entry| entry.file_name())
            .collect::<BTreeSet<_>>()
            .len(),
        DIRECTORY_BATCH_SIZE + 1
    );
}

#[test]
fn directory_discovery_bounds_each_batch_and_wraps_without_retaining_names() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..DIRECTORY_BATCH_SIZE + 1 {
        std::fs::create_dir(dir.path().join(index.to_string())).unwrap();
    }
    let mut scan = DirectoryScan::default();
    let first = scan.next_batch(dir.path()).unwrap();
    assert_eq!(first.len(), DIRECTORY_BATCH_SIZE);
    let second = scan.next_batch(dir.path()).unwrap();
    assert_eq!(second.len(), 1);
    let names = first
        .into_iter()
        .chain(second)
        .map(|entry| entry.file_name())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), DIRECTORY_BATCH_SIZE + 1);
    assert_eq!(
        scan.next_batch(dir.path()).unwrap().len(),
        DIRECTORY_BATCH_SIZE
    );
}

#[tokio::test]
async fn periodic_inventory_names_only_rowless_cache_directories_for_inspection() {
    let cache = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let conn = crate::db::test_conn();
    let (owned, orphan) = (SandboxId::new(), SandboxId::new());
    for id in [owned, orphan] {
        std::fs::create_dir(cache.path().join(id.to_string())).unwrap();
    }
    insert_test_sandbox(&conn, &owned.to_string(), SandboxState::Suspended);
    let service = SandboxManagerService::new(conn);
    let batch = service
        .discover_periodic_inventory_in(
            &mut PeriodicInventoryScan::default(),
            SandboxInventoryPaths::new(cache.path(), runtime.path()),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        batch.orphan_candidates,
        vec![cache.path().join(orphan.to_string())]
    );
}
