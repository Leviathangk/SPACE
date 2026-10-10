//! Incremental local discovery; partial batches never escape as inventory RPCs.

use super::*;

const DIRECTORY_BATCH_SIZE: usize = 128;

#[derive(Default)]
struct DirectoryScan(Option<std::fs::ReadDir>);

impl DirectoryScan {
    fn next_batch(&mut self, path: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
        if self.0.is_none() {
            match std::fs::read_dir(path) {
                Ok(entries) => self.0 = Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            }
        }
        let Some(entries) = self.0.as_mut() else {
            return Ok(Vec::new());
        };
        let (batch, exhausted) = read_directory_batch(entries);
        if exhausted {
            self.0 = None;
        }
        Ok(batch)
    }
}

fn read_directory_batch(
    entries: &mut impl Iterator<Item = std::io::Result<std::fs::DirEntry>>,
) -> (Vec<std::fs::DirEntry>, bool) {
    let mut batch = Vec::new();
    for _ in 0..DIRECTORY_BATCH_SIZE {
        match entries.next() {
            Some(Ok(entry)) => batch.push(entry),
            // Count failed entries against the budget, but do not rewind the
            // iterator and repeatedly hide all identities after this entry.
            Some(Err(error)) => {
                tracing::warn!(%error, "skipping unreadable inventory directory entry")
            }
            None => return (batch, true),
        }
    }
    (batch, false)
}

/// Retains two directory iterators and one finite database cursor, not a growing
/// inventory. New filesystem entries may wait for a later directory sweep.
/// Limits discovery/admission, not the duration of an individual filesystem op.
#[derive(Default)]
pub(in crate::service) struct PeriodicInventoryScan {
    database: crate::db::inventory::SandboxInventoryScan,
    cache: DirectoryScan,
    runtime: DirectoryScan,
}

impl PeriodicInventoryScan {
    fn discover(
        &mut self,
        service: &SandboxManagerService,
        paths: SandboxInventoryPaths<'_>,
        shutdown: &CancellationToken,
    ) -> Result<InventoryDiscovery, Status> {
        let rows = self
            .database
            .next_batch(&*service.db_conn()?)
            .map_err(|error| Status::internal(format!("inventory discovery: {error}")))?;
        let mut ids = BTreeSet::new();
        for (scan, path, is_cache) in [
            (&mut self.cache, paths.cache_dir, true),
            (&mut self.runtime, paths.run_dir, false),
        ] {
            let entries = match scan.next_batch(path) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "deferring unreadable inventory directory");
                    continue;
                }
            };
            for entry in entries {
                if shutdown.is_cancelled() {
                    break;
                }
                let name = entry.file_name();
                if let Some(id) = name
                    .to_str()
                    .and_then(|name| SandboxId::from_str(name).ok())
                {
                    ids.insert(id);
                } else if is_cache {
                    match service.reclaim_abandoned_transfer_staging_entry(&entry.path()) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(path = %entry.path().display(), %error, "failed to reclaim transfer staging");
                            continue;
                        }
                    }
                    if let Some(id) =
                        crate::snapshot_restore::remote_restore_metadata_staging_sandbox_id(&name)
                        && let Some(_guard) =
                            service.auto_lifecycle.try_staging_guard(&id.to_string())
                        && let Err(error) = std::fs::remove_dir_all(entry.path())
                    {
                        tracing::warn!(sandbox_id = %id, %error, "failed to reclaim abandoned restore metadata staging");
                    }
                } else {
                    // Non-sandbox runtime junk has no node-manager identity. Only
                    // remove it when neither cache nor a live process retains it.
                    let retained = paths.cache_dir.join(&name).try_exists().and_then(|cached| {
                        entry
                            .path()
                            .join("fc.pid")
                            .try_exists()
                            .map(|live| cached || live)
                    });
                    match retained {
                        Ok(false) => {
                            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                                tracing::warn!(path = %entry.path().display(), %error, "failed to remove orphaned run directory");
                            }
                        }
                        Ok(true) => {}
                        Err(error) => {
                            tracing::warn!(path = %entry.path().display(), %error, "preserving unreadable run directory")
                        }
                    }
                }
            }
        }
        let mut discovery = InventoryDiscovery {
            rows,
            disk_ids: Vec::new(),
            live_ids: Vec::new(),
            runtime_artifact_ids: Vec::new(),
        };
        for id in ids {
            // Each discovered identity gets a complete local presence witness,
            // even when its cache and runtime entries fall on different pages.
            match SandboxInventoryPresence::inspect(paths, id) {
                Ok(presence) => {
                    if presence.has_cache_root {
                        discovery.disk_ids.push(id);
                    }
                    if presence.has_live_runtime {
                        discovery.live_ids.push(id);
                    }
                    if presence.has_runtime_artifact {
                        discovery.runtime_artifact_ids.push(id);
                    }
                }
                Err(error) => {
                    tracing::warn!(sandbox_id = %id, %error, "deferring unreadable inventory identity")
                }
            }
        }
        Ok(discovery)
    }
}

impl SandboxManagerService {
    pub(in crate::service) fn reconcile_periodic_inventory(
        &self,
        scan: &mut PeriodicInventoryScan,
        shutdown: &CancellationToken,
        retained_sources: &crate::retained_source::RetainedSourceVerdicts,
    ) -> Result<(), Status> {
        self.reconcile_periodic_inventory_in(
            scan,
            SandboxInventoryPaths::new(Path::new(CACHE_DIR), Path::new(RUN_DIR)),
            shutdown,
            retained_sources,
        )
    }

    fn reconcile_periodic_inventory_in(
        &self,
        scan: &mut PeriodicInventoryScan,
        paths: SandboxInventoryPaths<'_>,
        shutdown: &CancellationToken,
        retained_sources: &crate::retained_source::RetainedSourceVerdicts,
    ) -> Result<(), Status> {
        let discovery = scan.discover(self, paths, shutdown)?;
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let report = self.apply_route_identity_mutations("periodic_inventory_reconcile", || {
            let observe_execution = self.orphan_execution_observer();
            let report = reconcile_inventory_batch(
                self.db.as_ref(),
                paths,
                discovery,
                ReconcileSource::Periodic,
                InventoryOwnershipGuards::new(
                    |id: &str| self.try_cleanup_fence(id),
                    |id: SandboxId| self.auto_lifecycle.try_staging_guard(&id.to_string()),
                    &observe_execution,
                )
                .with_retirement(|id: &str| self.try_registration_retirement(id))
                .with_retained_sources(retained_sources),
            )
            .map_err(|error| {
                if matches!(error, InventoryError::DatabaseMutexPoisoned) {
                    self.fail_poisoned_database();
                }
                Status::internal(format!("inventory reconciliation: {error}"))
            })?;
            let changed = report.changed_ids();
            Ok::<_, Status>((report, changed))
        })?;
        let changed = report.changed_ids();
        if !changed.is_empty() {
            mark_sandboxes_dirty(&self.db, changed.iter().map(ToString::to_string));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
