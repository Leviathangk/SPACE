use pplx_space_common_core::types::{OrgId, UserId};

use pplx_space_common_core::types::{UpstreamIssuer, UpstreamOrgId, UpstreamSubjectId};
use pplx_space_db_test_fixtures::pg_test_db;
use pplx_space_public_api::PublicErrorCode;

use super::*;

use pplx_space_test_env::TestResult;

#[test]
fn fork_source_uses_the_bare_id_emitted_by_ecw() -> TestResult {
    let sandbox_id = SandboxId::new();

    assert_eq!(
        parse_fork_source_sandbox_id(&sandbox_id.to_string())?,
        sandbox_id
    );
    Ok(())
}

#[test]
fn fork_source_rejects_the_native_prefixed_id_form() {
    let error = parse_fork_source_sandbox_id(&format!("sbx_{}", SandboxId::new()))
        .expect_err("ECW sandbox IDs are bare UUIDs");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
}

#[test]
fn fork_internal_errors_are_sanitized() {
    let error = fork_internal_error(
        "persist fork operation",
        "database constraint public_operations_owner_key was violated",
    );

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.message, "Internal server error");
}

#[test]
fn terminal_fork_failures_preserve_the_public_error_status() {
    let cases = [
        (PublicErrorCode::InvalidRequest, StatusCode::BAD_REQUEST),
        (PublicErrorCode::Unauthorized, StatusCode::UNAUTHORIZED),
        (PublicErrorCode::Forbidden, StatusCode::FORBIDDEN),
        (PublicErrorCode::NotFound, StatusCode::NOT_FOUND),
        (PublicErrorCode::Conflict, StatusCode::CONFLICT),
        (
            PublicErrorCode::FailedPrecondition,
            StatusCode::PRECONDITION_FAILED,
        ),
        (
            PublicErrorCode::CapacityUnavailable,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (PublicErrorCode::RateLimited, StatusCode::TOO_MANY_REQUESTS),
        (
            PublicErrorCode::OperationFailed,
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];

    for (code, expected) in cases {
        assert_eq!(fork_terminal_failure_status(&code), expected, "{code}");
    }
}

#[test]
fn internal_fork_failures_do_not_expose_persisted_details() {
    assert_eq!(
        public_fork_failure_message(
            Some(&PublicErrorCode::Internal),
            "sqlx error: duplicate key violates constraint public_operations_pkey",
        ),
        "Sandbox fork failed"
    );
    assert_eq!(
        public_fork_failure_message(
            Some(&PublicErrorCode::OperationFailed),
            "node rpc failed: transport error for http://10.0.0.8:9800",
        ),
        "Sandbox fork failed"
    );
    assert_eq!(
        public_fork_failure_message(
            Some(&PublicErrorCode::Other(
                "snapshot_quota_exceeded".to_string()
            )),
            "btrfs subvolume snapshot failed: /var/lib/space/snapshots/9f3c: no space left",
        ),
        "Sandbox fork failed"
    );
    assert_eq!(
        public_fork_failure_message(
            Some(&PublicErrorCode::FailedPrecondition),
            "fork source changed during capture",
        ),
        "fork source changed during capture"
    );
}

#[test]
fn internal_per_target_failures_do_not_expose_persisted_details() {
    assert_eq!(
        public_fork_target_failure_message(
            500,
            "controller error: postgres connection refused at 10.0.0.9",
        ),
        "Sandbox fork failed"
    );
    assert_eq!(
        public_fork_target_failure_message(409, "sandbox already exists"),
        "sandbox already exists"
    );
}

#[tokio::test]
async fn federated_replay_uses_the_owner_organization_namespace() -> TestResult {
    let db = pg_test_db().await?;
    let api_key_org_id = OrgId::new();
    let owner_org_id = OrgId::new();
    let owner = crate::federated_identity::FederatedOwner {
        issuer: UpstreamIssuer::parse("example-issuer")?,
        org_id: UpstreamOrgId::parse("upstream-org")?,
        subject_id: UpstreamSubjectId::parse("upstream-user")?,
    };
    sqlx::query(pplx_space_common_core::sql!(
        "INSERT INTO organizations (
             id, name, slug, owner_source, upstream_issuer, upstream_org_id
         )
         VALUES ($1, 'Federated owner', $2, 'federated_identity', $3, $4)"
    ))
    .bind(owner_org_id)
    .bind(format!("federated-{owner_org_id}"))
    .bind(owner.issuer.as_str())
    .bind(owner.org_id.as_str())
    .execute(&db.pool)
    .await?;

    assert_eq!(
        resolve_fork_replay_org_id(&db.pool, api_key_org_id, Some(&owner)).await?,
        Some(owner_org_id)
    );
    Ok(())
}

#[tokio::test]
async fn first_federated_fork_materializes_the_owner_namespace() -> TestResult {
    let db = pg_test_db().await?;
    let owner = crate::federated_identity::FederatedOwner {
        issuer: UpstreamIssuer::parse("example-issuer")?,
        org_id: UpstreamOrgId::parse("new-upstream-org")?,
        subject_id: UpstreamSubjectId::parse("new-upstream-user")?,
    };

    assert_eq!(
        resolve_fork_replay_org_id(&db.pool, OrgId::new(), Some(&owner)).await?,
        None
    );
    let materialized = crate::federated_identity::materialize_owner(&db.pool, &owner).await?;

    assert_eq!(
        resolve_fork_replay_org_id(&db.pool, OrgId::new(), Some(&owner)).await?,
        Some(materialized.org_id)
    );
    Ok(())
}

#[tokio::test]
async fn invalid_federated_grant_does_not_materialize_owner_namespace() -> TestResult {
    let db = pg_test_db().await?;
    let owner = crate::federated_identity::FederatedOwner {
        issuer: UpstreamIssuer::parse("example-issuer")?,
        org_id: UpstreamOrgId::parse("unauthorized-upstream-org")?,
        subject_id: UpstreamSubjectId::parse("unauthorized-upstream-user")?,
    };

    let error = prepare_fork_owner(
        &db.pool,
        OrgId::new(),
        None,
        &HeaderMap::new(),
        Some(&owner),
    )
    .await
    .expect_err("a federated fork without a service grant is unauthorized");
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);

    let owner_org_exists: bool = sqlx::query_scalar(pplx_space_common_core::sql!(
        "SELECT EXISTS (
             SELECT 1 FROM organizations
             WHERE owner_source = 'federated_identity'
               AND upstream_issuer = $1
               AND upstream_org_id = $2
         )"
    ))
    .bind(owner.issuer.as_str())
    .bind(owner.org_id.as_str())
    .fetch_one(&db.pool)
    .await?;
    assert!(
        !owner_org_exists,
        "unauthorized fork persisted its owner org"
    );
    Ok(())
}

#[test]
fn federated_fork_executes_as_the_scoped_source_owner() {
    let authenticated_actor_id = UserId::new();
    let authenticated_org_id = OrgId::new();
    let source_owner_user_id = UserId::new();
    let source_org_id = OrgId::new();

    assert_eq!(
        fork_execution_actor_id(
            authenticated_actor_id,
            authenticated_org_id,
            source_owner_user_id,
            source_org_id,
        ),
        source_owner_user_id
    );
}

#[test]
fn local_fork_preserves_the_authenticated_actor() {
    let authenticated_actor_id = UserId::new();
    let org_id = OrgId::new();

    assert_eq!(
        fork_execution_actor_id(authenticated_actor_id, org_id, UserId::new(), org_id,),
        authenticated_actor_id
    );
}

#[test]
fn idempotent_replay_does_not_need_an_additional_client_slot() {
    assert!(fork_admission_needs_client_slot(true));
    assert!(!fork_admission_needs_client_slot(false));
}

#[test]
fn fork_outcome_polling_backs_off_to_a_bounded_interval() {
    let mut interval = Duration::from_millis(250);
    for _ in 0..10 {
        interval = next_fork_poll_interval(interval);
    }

    assert_eq!(interval, Duration::from_secs(5));
    assert_eq!(next_fork_poll_interval(interval), Duration::from_secs(5));
}
