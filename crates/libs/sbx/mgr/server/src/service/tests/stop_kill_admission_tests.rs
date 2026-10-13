//! Stop and Kill from every node-local SQLite `sandboxes.state` source state,
//! with and without a live runtime, blocking and not.
//!
//! Both RPCs stop through lifecycle cleanup, which claims `stopping` and ends
//! `stopped`. A fatal Kill admits exactly the states a stop may supersede
//! (`SandboxState::can_transition_to(Stopping)`): every state whose VM is live
//! or launching, mid-operation included. It reads only the row, so a live
//! runtime does not change its answer. A non-fatal signal starts no cleanup
//! and reaches only a started or paused sandbox. Stop selects only a live
//! runtime, then admits every state its cleanup admission does not leave to a
//! delete or quarantine. The expected outcomes are written out here, not
//! derived from the code under test.
//!
//! Node cleanup reaches only the executor's test dispatch, which completes one
//! attempt per granted permit, and node-manager is an in-process fake that
//! answers `StopProcess`. The runtime probe reads a test-owned run directory.
//! An admitted stop or kill removes watchdog monitoring, which stats the fixed
//! sidecar pid path of a fresh sandbox id; nothing exists there.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pplx_space_common_core::types::SandboxId;
use pplx_space_node_mgr_client::proto::{StopProcessRequest, StopProcessResponse};
use tonic::Status;

use super::service_tests::insert_test_sandbox;
use crate::db::CleanupCompletion;
use crate::node_manager_transport::fake::{FakeNodeManager, ServedNodeManager};
use crate::proto::{KillRequest, StopRequest};
use crate::service::activation_support::internal_request;
use crate::service::{SandboxManagerService, SandboxManagerServiceOptions};
use crate::state::SandboxState;

const WAIT: Duration = Duration::from_secs(30);

/// Whether a fatal Kill admits a row in `state`. Wildcard-free, so a new
/// lifecycle state does not compile until it is placed.
pub(crate) fn fatal_kill_admits(state: SandboxState) -> bool {
    match state {
        SandboxState::Starting
        | SandboxState::Started
        | SandboxState::Stopping
        | SandboxState::Pausing
        | SandboxState::Paused
        | SandboxState::Resuming
        | SandboxState::Checkpointing
        | SandboxState::Suspending
        | SandboxState::Restoring => true,
        // No VM to stop, a cleanup or failure that owns the row's remaining
        // resources, or a delete or quarantine.
        SandboxState::Creating
        | SandboxState::Created
        | SandboxState::Stopped
        | SandboxState::Deleting
        | SandboxState::Suspended
        | SandboxState::CleanupRequired
        | SandboxState::CleanupFailed
        | SandboxState::Quarantined
        | SandboxState::Failed => false,
    }
}

/// Whether a non-fatal Kill forwards its signal to a row in `state`.
fn signal_delivered(state: SandboxState) -> bool {
    match state {
        SandboxState::Started | SandboxState::Paused => true,
        SandboxState::Creating
        | SandboxState::Created
        | SandboxState::Starting
        | SandboxState::Stopping
        | SandboxState::Stopped
        | SandboxState::Deleting
        | SandboxState::Pausing
        | SandboxState::Resuming
        | SandboxState::Checkpointing
        | SandboxState::Suspending
        | SandboxState::Suspended
        | SandboxState::Restoring
        | SandboxState::CleanupRequired
        | SandboxState::CleanupFailed
        | SandboxState::Quarantined
        | SandboxState::Failed => false,
    }
}

/// Whether Stop admits a row in `state` whose runtime is live. Without a live
/// runtime Stop selects nothing from any state.
fn stop_admits_live(state: SandboxState) -> bool {
    match state {
        SandboxState::Creating
        | SandboxState::Created
        | SandboxState::Starting
        | SandboxState::Started
        | SandboxState::Stopping
        | SandboxState::Stopped
        | SandboxState::Pausing
        | SandboxState::Paused
        | SandboxState::Resuming
        | SandboxState::Checkpointing
        | SandboxState::Suspending
        | SandboxState::Suspended
        | SandboxState::Restoring
        | SandboxState::CleanupRequired
        | SandboxState::CleanupFailed
        | SandboxState::Failed => true,
        SandboxState::Deleting | SandboxState::Quarantined => false,
    }
}

/// Every row of every table, so a refused call can be shown to change nothing.
fn snapshot(conn: &rusqlite::Connection) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut statement = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
            let columns = statement.column_count();
            let mut rows: Vec<String> = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| format!("{values:?}"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

struct Fixture {
    service: SandboxManagerService,
    id: String,
    signals: Arc<Mutex<Vec<StopProcessRequest>>>,
    dispatch_permits: Arc<tokio::sync::Semaphore>,
    _node_manager: ServedNodeManager,
    _scratch: tempfile::TempDir,
}

impl Fixture {
    /// A row in `state`, with a local restore's columns when it is
    /// `restoring`. Node cleanup completes `dispatch_permits` attempts and
    /// then holds.
    fn new(state: SandboxState, live: bool, dispatch_permits: usize) -> Self {
        let conn = crate::db::test_conn();
        let id = SandboxId::new().to_string();
        insert_test_sandbox(&conn, &id, state);
        if state == SandboxState::Restoring {
            conn.execute(
                "UPDATE sandboxes SET restore_origin = 'local', restore_phase = 'activation'
                  WHERE id = ?1",
                [&id],
            )
            .unwrap();
        }
        let signals = Arc::new(Mutex::new(Vec::new()));
        let node_manager = FakeNodeManager::default()
            .unary("StopProcess", {
                let signals = Arc::clone(&signals);
                move |request: StopProcessRequest| {
                    signals.lock().unwrap().push(request);
                    Ok::<_, Status>(StopProcessResponse::default())
                }
            })
            .serve();
        let scratch = tempfile::tempdir().unwrap();
        if live {
            let sandbox_dir = scratch.path().join(&id);
            std::fs::create_dir(&sandbox_dir).unwrap();
            std::os::unix::fs::symlink(scratch.path(), sandbox_dir.join("fc.pid")).unwrap();
        }
        let service = SandboxManagerService::new_with_options(
            conn,
            SandboxManagerServiceOptions {
                node_manager: node_manager.connector(),
                ..SandboxManagerServiceOptions::for_test_in(scratch.path())
            },
        );
        let dispatch_permits = Arc::new(tokio::sync::Semaphore::new(dispatch_permits));
        let permits = Arc::clone(&dispatch_permits);
        *service.cleanup_executor.dispatch.lock().unwrap() = Some(Arc::new(move |_, _| {
            let permits = Arc::clone(&permits);
            Box::pin(async move {
                match permits.acquire().await {
                    Ok(permit) => {
                        permit.forget();
                        Ok(())
                    }
                    Err(_) => Err(Status::unavailable("test node cleanup closed")),
                }
            })
        }));
        Self {
            service,
            id,
            signals,
            dispatch_permits,
            _node_manager: node_manager,
            _scratch: scratch,
        }
    }

    fn snapshot(&self) -> BTreeMap<String, Vec<String>> {
        snapshot(&self.service.db_conn().unwrap())
    }

    fn signals(&self) -> Vec<u32> {
        let signals = self.signals.lock().unwrap();
        for request in signals.iter() {
            assert_eq!(request.sandbox_id, self.id);
        }
        signals.iter().map(|request| request.signal).collect()
    }

    /// Assert that cleanup claimed the row: `stopping`, with a cleanup job and,
    /// when the caller did not wait, its persisted stop intent.
    fn assert_claimed(&self, persisted_intent: bool, case: &str) {
        let db = self.service.db_conn().unwrap();
        let row = crate::db::get_sandbox(&db, &self.id).unwrap().unwrap();
        assert_eq!(row.state, SandboxState::Stopping, "{case}");
        assert_eq!(
            (row.cleanup_completion, row.cleanup_keep_filesystem),
            if persisted_intent {
                (Some(CleanupCompletion::MarkStopped), Some(true))
            } else {
                (None, None)
            },
            "{case}"
        );
        assert!(
            pplx_space_common_jobs::db::find_active_job(
                &db,
                crate::db::cleanup_jobs::KIND,
                &self.id
            )
            .unwrap()
            .is_some(),
            "{case}: the claim persists its cleanup job"
        );
    }

    /// Assert that cleanup completed: `stopped`, with no cleanup work left.
    fn assert_stopped(&self, case: &str) {
        let db = self.service.db_conn().unwrap();
        let row = crate::db::get_sandbox(&db, &self.id).unwrap().unwrap();
        assert_eq!(row.state, SandboxState::Stopped, "{case}");
        assert_eq!(
            (row.cleanup_completion, row.cleanup_keep_filesystem),
            (None, None),
            "{case}"
        );
        assert!(
            pplx_space_common_jobs::db::find_active_job(
                &db,
                crate::db::cleanup_jobs::KIND,
                &self.id
            )
            .unwrap()
            .is_none(),
            "{case}"
        );
    }

    async fn kill(&self, signal: u32, blocking: bool) -> Result<(), Status> {
        tokio::time::timeout(
            WAIT,
            self.service.kill(internal_request(KillRequest {
                sandbox_id: self.id.clone(),
                signal,
                blocking,
            })),
        )
        .await
        .expect("Kill answers within the wait")
        .map(|_| ())
    }

    async fn stop(&self, blocking: bool) -> Result<Vec<String>, Status> {
        tokio::time::timeout(
            WAIT,
            self.service.stop(internal_request(StopRequest {
                sandbox_id: self.id.clone(),
                template_filter: None,
                blocking,
                immediate_kill: false,
            })),
        )
        .await
        .expect("Stop answers within the wait")
        .map(|response| response.into_inner().stopped_ids)
    }

    async fn shutdown(self) {
        self.dispatch_permits.close();
        self.service.shutdown_token.cancel();
        self.service.task_tracker.close();
        tokio::time::timeout(WAIT, self.service.task_tracker.wait())
            .await
            .expect("service tasks stop within the wait after shutdown");
    }
}

fn enter_isolated_test_process() -> bool {
    crate::service::tests::enter_route_test_process(&[
        ("EPPO_SDK_KEY", None),
        ("SPACE_FLAGD_ENABLED", None),
    ])
}

/// A fatal Kill stops every live state, mid-operation included, whether or not
/// the runtime probe finds the VM, and refuses every other state without a
/// write or a signal.
#[tokio::test]
async fn fatal_kill_admits_every_state_a_stop_may_supersede() {
    if !enter_isolated_test_process() {
        return;
    }
    for signal in [9, 15] {
        for live in [true, false] {
            for blocking in [true, false] {
                for state in SandboxState::all() {
                    let case =
                        format!("{state}, signal {signal}, live {live}, blocking {blocking}");
                    // A blocking kill waits for one node cleanup attempt; a
                    // non-blocking one returns while cleanup holds.
                    let fixture = Fixture::new(state, live, usize::from(blocking));
                    let before = fixture.snapshot();

                    let result = fixture.kill(signal, blocking).await;

                    if fatal_kill_admits(state) {
                        result.unwrap_or_else(|status| panic!("{case}: {status:?}"));
                        assert_eq!(fixture.signals(), [signal], "{case}");
                        if blocking {
                            fixture.assert_stopped(&case);
                        } else {
                            fixture.assert_claimed(true, &case);
                        }
                    } else {
                        let status = result.expect_err(&case);
                        assert_eq!(
                            status.code(),
                            tonic::Code::FailedPrecondition,
                            "{case}: {status:?}"
                        );
                        assert!(fixture.signals().is_empty(), "{case}");
                        assert_eq!(fixture.snapshot(), before, "{case}");
                    }
                    fixture.shutdown().await;
                }
            }
        }
    }
}

/// A non-fatal Kill only forwards its signal, and only to a started or paused
/// sandbox; it never writes.
#[tokio::test]
async fn non_fatal_kill_signals_only_a_started_or_paused_sandbox() {
    if !enter_isolated_test_process() {
        return;
    }
    for live in [true, false] {
        for state in SandboxState::all() {
            let case = format!("{state}, live {live}");
            let fixture = Fixture::new(state, live, 0);
            let before = fixture.snapshot();

            let result = fixture.kill(1, true).await;

            if signal_delivered(state) {
                result.unwrap_or_else(|status| panic!("{case}: {status:?}"));
                assert_eq!(fixture.signals(), [1], "{case}");
            } else {
                let status = result.expect_err(&case);
                assert_eq!(
                    status.code(),
                    tonic::Code::FailedPrecondition,
                    "{case}: {status:?}"
                );
                assert!(fixture.signals().is_empty(), "{case}");
            }
            assert_eq!(fixture.snapshot(), before, "{case}");
            fixture.shutdown().await;
        }
    }
}

/// Stop stops a live runtime under every state but a delete or quarantine,
/// which it refuses as retryable without a write. Without a live runtime it
/// selects nothing and writes nothing, whatever the state.
#[tokio::test]
async fn stop_admits_a_live_runtime_from_every_state_but_deleting_or_quarantined() {
    if !enter_isolated_test_process() {
        return;
    }
    for live in [true, false] {
        for blocking in [true, false] {
            for state in SandboxState::all() {
                let case = format!("{state}, live {live}, blocking {blocking}");
                let fixture = Fixture::new(state, live, usize::from(blocking));
                let before = fixture.snapshot();

                let result = fixture.stop(blocking).await;

                if !live {
                    let stopped = result.unwrap_or_else(|status| panic!("{case}: {status:?}"));
                    assert!(stopped.is_empty(), "{case}");
                    assert_eq!(fixture.snapshot(), before, "{case}");
                } else if stop_admits_live(state) {
                    let stopped = result.unwrap_or_else(|status| panic!("{case}: {status:?}"));
                    assert_eq!(stopped, std::slice::from_ref(&fixture.id), "{case}");
                    if blocking {
                        fixture.assert_stopped(&case);
                    } else {
                        fixture.assert_claimed(true, &case);
                    }
                } else {
                    let status = result.expect_err(&case);
                    assert_eq!(
                        status.code(),
                        tonic::Code::Unavailable,
                        "{case}: {status:?}"
                    );
                    assert_eq!(fixture.snapshot(), before, "{case}");
                }
                // Stop never signals the process itself.
                assert!(fixture.signals().is_empty(), "{case}");
                fixture.shutdown().await;
            }
        }
    }
}
