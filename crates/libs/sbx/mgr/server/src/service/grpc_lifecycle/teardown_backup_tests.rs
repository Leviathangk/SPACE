use super::*;
use pplx_space_sbx_mgr_client::proto::SuspendSnapshotFormat;

use pplx_space_node_api_client::proto::{
    BeginArtifactBackupRequest, BeginArtifactBackupResponse, GetArtifactBackupLocationRequest,
    GetArtifactBackupLocationResponse,
};

#[derive(Clone)]
struct ReadyBackup;

impl tonic::server::NamedService for ReadyBackup {
    const NAME: &'static str = "space.node.api.v1.ClusterApi";
}

impl tonic::server::UnaryService<GetArtifactBackupLocationRequest> for ReadyBackup {
    type Response = GetArtifactBackupLocationResponse;
    type Future = std::future::Ready<Result<Response<Self::Response>, Status>>;

    fn call(&mut self, request: Request<GetArtifactBackupLocationRequest>) -> Self::Future {
        let request = request.into_inner();
        std::future::ready(Ok(Response::new(GetArtifactBackupLocationResponse {
            local_artifact_id: request.local_artifact_id,
            artifact_kind: request.artifact_kind,
            backup_attempt_id: uuid::Uuid::now_v7().to_string(),
            ..Default::default()
        })))
    }
}

impl tonic::server::UnaryService<BeginArtifactBackupRequest> for ReadyBackup {
    type Response = BeginArtifactBackupResponse;
    type Future = std::future::Ready<Result<Response<Self::Response>, Status>>;

    fn call(&mut self, request: Request<BeginArtifactBackupRequest>) -> Self::Future {
        let request = request.into_inner();
        let open = !request.backup_attempt_id.ends_with("0002");
        let mut response = Response::new(BeginArtifactBackupResponse {
            local_artifact_id: request.local_artifact_id,
            backup_attempt_id: request.backup_attempt_id,
        });
        if open {
            response.metadata_mut().insert(
                pplx_space_node_api_client::BACKUP_ATTEMPT_OPEN_METADATA,
                tonic::metadata::MetadataValue::from_static("true"),
            );
        }
        std::future::ready(Ok(response))
    }
}

impl<B> tower::Service<http::Request<B>> for ReadyBackup
where
    B: http_body::Body + Send + 'static,
    B::Error: Into<tonic::codegen::StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let begin = request.uri().path().ends_with("/BeginArtifactBackup");
        assert!(begin || request.uri().path().ends_with("/GetArtifactBackupLocation"));
        Box::pin(async move {
            if begin {
                let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                    BeginArtifactBackupResponse,
                    BeginArtifactBackupRequest,
                >::default());
                Ok(grpc.unary(ReadyBackup, request).await)
            } else {
                let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                    GetArtifactBackupLocationResponse,
                    GetArtifactBackupLocationRequest,
                >::default());
                Ok(grpc.unary(ReadyBackup, request).await)
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn teardown_reuses_exact_remote_backup_without_touching_local_files_or_jobs() {
    use crate::service::tests::service_tests::insert_stranded_teardown_suspend_sandbox;
    use pplx_space_common_core::types::ExecutionGeneration;
    use std::ffi::OsStr;

    // The parent serves while the isolated child exercises environment-based
    // client wiring. A second runtime thread keeps the server making progress.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(ReadyBackup)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                stop.clone().cancelled_owned(),
            ),
    );
    let token = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(token.path(), "synthetic-backup-test-token").unwrap();
    let child = pplx_space_test_env::enter_test_process(&[
        ("SPACE_INTERNAL_RPC_INSECURE_DEV", Some(OsStr::new("1"))),
        (
            pplx_space_node_api_client::cluster::CLUSTER_API_ENDPOINT_ENV,
            Some(OsStr::new(&endpoint)),
        ),
        (
            pplx_space_common_svc::bearer_token::SPACE_CLUSTER_API_TOKEN_PATH_ENV,
            Some(token.path().as_os_str()),
        ),
    ]);
    if child {
        for superseded in [false, true] {
            let conn = crate::db::test_conn();
            let sandbox_id = SandboxId::new();
            let generation = ExecutionGeneration::new(7).unwrap();
            let artifact_id =
                insert_stranded_teardown_suspend_sandbox(&conn, sandbox_id, generation);
            let row = crate::db::get_sandbox(&conn, &sandbox_id.to_string())
                .unwrap()
                .unwrap();
            let artifact = crate::db::get_local_artifact(&conn, artifact_id)
                .unwrap()
                .unwrap();
            let service = SandboxManagerService::new(conn);
            let result = service
                .ensure_teardown_suspend_backup(
                    &row,
                    super::TeardownSuspendBackup {
                        execution_generation: if superseded {
                            ExecutionGeneration::new(6).unwrap()
                        } else {
                            generation
                        },
                        artifact: &artifact,
                        format: SuspendSnapshotFormat::Full,
                    },
                )
                .await;
            if superseded {
                assert_eq!(result.unwrap_err().code(), tonic::Code::Aborted);
            } else {
                assert!(
                    result.is_ok(),
                    "{result:?}; lookup: {:?}",
                    service
                        .existing_ready_artifact_backup_response(&artifact)
                        .await
                );
            }
            let db = service.db.lock().unwrap();
            let status = crate::db::teardown_suspend_status(&db, &sandbox_id.to_string())
                .unwrap()
                .unwrap();
            assert!(status.backup_completed);
            assert!(
                pplx_space_common_jobs::db::list_jobs(&db, Some(&sandbox_id.to_string()), None, 0)
                    .unwrap()
                    .iter()
                    .all(|job| job.kind != crate::snapshot_upload_jobs::SNAPSHOT_UPLOAD_JOB_KIND)
            );
        }
        retry_unstaged_backup_requires_open_attempt_and_preserves_history().await;
    }
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_teardown_defers_repeated_polls_without_holding_driver_capacity() {
    use crate::service::tests::service_tests::insert_stranded_teardown_suspend_sandbox;
    use pplx_space_common_core::types::ExecutionGeneration;
    let conn = crate::db::test_conn();
    let sandbox = SandboxId::new();
    let generation = ExecutionGeneration::new(7).unwrap();
    insert_stranded_teardown_suspend_sandbox(&conn, sandbox, generation);
    let service = SandboxManagerService::new(conn);
    service
        .drive_teardown_suspend(sandbox, generation, SandboxEventTrigger::System)
        .await
        .unwrap_err();
    let capacity = service.teardown_suspend_driver_sem.available_permits();
    for _ in 0..4 {
        service.spawn_teardown_suspend_driver(sandbox, generation, SandboxEventTrigger::System);
        assert!(
            service.teardown_suspend_drivers.is_empty(),
            "a failed operation must defer repeated polls"
        );
        assert_eq!(
            service.teardown_suspend_driver_sem.available_permits(),
            capacity
        );
    }
    {
        let db = service.db.lock().unwrap();
        db.execute("UPDATE jobs SET next_attempt_at=datetime('now','-1 second') WHERE subject_id=?1 AND kind='teardown_suspend'", [sandbox.to_string()]).unwrap();
    }
    service.spawn_teardown_suspend_driver(sandbox, generation, SandboxEventTrigger::System);
    assert_eq!(
        service.teardown_suspend_drivers.len(),
        1,
        "expired backoff must allow another attempt"
    );
    service.shutdown_token.cancel();
    service.task_tracker.close();
    service.task_tracker.wait().await;
}

async fn retry_unstaged_backup_requires_open_attempt_and_preserves_history() {
    use crate::service::tests::service_tests::insert_stranded_teardown_suspend_sandbox;
    use pplx_space_common_core::types::{
        ExecutionGeneration, ExecutionMetadata, SnapshotExecutionMetadata,
    };
    use pplx_space_common_jobs::{JobPriority, db as jobs};
    for (old_server, staged, superseded) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        for file in ["vmstate", "memory", "rootfs.btrfs-send"] {
            std::fs::write(dir.path().join(file), b"synthetic snapshot").unwrap();
        }
        let conn = crate::db::test_conn();
        let sandbox = SandboxId::new();
        let generation = ExecutionGeneration::new(7).unwrap();
        let artifact_id = insert_stranded_teardown_suspend_sandbox(&conn, sandbox, generation);
        conn.execute(
            "UPDATE local_artifacts SET local_path=?1 WHERE local_artifact_id=?2",
            rusqlite::params![dir.path().to_str().unwrap(), artifact_id.to_string()],
        )
        .unwrap();
        let artifact = crate::db::get_local_artifact(&conn, artifact_id)
            .unwrap()
            .unwrap();
        let mut payload = crate::snapshot_upload_jobs::tests::reclaim_fixture_payload(
            &artifact_id.to_string(),
            dir.path().to_owned(),
        );
        payload.meta.sandbox_id = sandbox.to_string();
        payload.meta.execution = Some(
            SnapshotExecutionMetadata::new(ExecutionMetadata::initial_firecracker(
                std::env::consts::ARCH.parse().unwrap(),
                vec![],
            ))
            .unwrap(),
        );
        if old_server {
            payload.snapshot_id = "01939e3e-ffff-7000-8000-000000000002".into();
        }
        if staged {
            payload.job_ids.push(JobId::new());
        }
        let old_job = JobId::new();
        jobs::insert_job(
            &conn,
            jobs::InsertJob {
                job_id: &old_job,
                kind: crate::snapshot_upload_jobs::SNAPSHOT_UPLOAD_JOB_KIND,
                payload: &serde_json::to_string(&payload).unwrap(),
                subject_id: Some(&sandbox.to_string()),
                dedup_key: Some(&artifact_id.to_string()),
                max_attempts: 1,
                traceparent: None,
                priority: JobPriority::default(),
            },
        )
        .unwrap();
        jobs::fail_job_permanent(&conn, &old_job, "source temporarily unavailable", None).unwrap();
        crate::db::mark_teardown_suspend_backup_permanently_failed(
            &conn,
            &sandbox.to_string(),
            artifact_id,
        )
        .unwrap();
        let failed = jobs::get_job(&conn, &old_job).unwrap().unwrap();
        let mut service = SandboxManagerService::new(conn);
        // Keep the persisted retry inspectable without dispatching volume I/O.
        service.snapshot_upload_worker = crate::snapshot_upload_jobs::build_worker(
            Arc::clone(&service.db),
            crate::service::tests::closed_drain_for_test(),
        );
        let result = service
            .retry_unstaged_teardown_backup(
                &super::TeardownSuspendBackup {
                    execution_generation: if superseded {
                        ExecutionGeneration::new(6).unwrap()
                    } else {
                        generation
                    },
                    artifact: &artifact,
                    format: SuspendSnapshotFormat::Full,
                },
                &failed,
            )
            .await;
        if superseded {
            assert_eq!(result.unwrap_err().code(), tonic::Code::Aborted);
        } else if old_server || staged {
            assert!(result.unwrap().is_none());
        } else {
            let new_job = result
                .unwrap()
                .expect("an acknowledged unstaged intent can retry");
            assert_ne!(new_job, old_job);
            let new = service
                .snapshot_upload_worker
                .get_job(&new_job)
                .unwrap()
                .unwrap();
            let prepared = new
                .payload_as::<crate::snapshot_upload::SnapshotUploadJobs>()
                .unwrap();
            assert_eq!(prepared.snapshot_id, payload.snapshot_id);
            assert_eq!(prepared.meta.local_artifact_id, Some(artifact_id));
            crate::snapshot_upload::cleanup_metadata_staging_dir(&prepared);
        }
        let db = service.db.lock().unwrap();
        assert_eq!(
            jobs::get_job(&db, &old_job).unwrap().unwrap().failure,
            failed.failure
        );
        assert_eq!(
            crate::db::get_sandbox(&db, &sandbox.to_string())
                .unwrap()
                .unwrap()
                .failure_reason
                .is_some(),
            old_server || staged || superseded
        );
    }
}
