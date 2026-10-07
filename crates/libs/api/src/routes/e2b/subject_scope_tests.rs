//! Asserted ECW subjects that share one trusted integration key reach only
//! their own federated sandboxes, on every sandbox route, through the real
//! ECW router and authentication middleware. The integration key's local
//! rows stay reachable without owner headers.

use pplx_space_common_core::sql;
use pplx_space_common_core::types::{
    EcwCredentialEpoch, IdempotencyKey, OrgId, SandboxId, UpstreamIssuer, UpstreamOrgId,
    UpstreamSubjectId, UserId,
};
use pplx_space_common_svc::federated_identity::FederatedIdentityPolicy;
use pplx_space_db_test_fixtures::{SEED_ORG_ID, SEED_USER_ID, TestDb, pg_test_db};
use pplx_space_test_env::TestResult;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use rstest::rstest;
use tower::util::ServiceExt;

use super::{EcwHostConfig, path_root_router_with_ecw_hosts};
use crate::federated_identity::{
    FEDERATED_ORG_ID_HEADER, FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
    FEDERATED_SERVICE_GRANT_HEADER, FEDERATED_USER_ID_HEADER, FederatedIdentityConfig,
    FederatedOwner,
};
use crate::fork_operations::{
    AdmitForkOperation, ForkRequest, ForkTargetOutcome, admit_locked_fork_operation,
    begin_fork_operation_admission,
};
use crate::public_operations::{CreatePublicOperationResult, PublicOperationStore};
use crate::routes::AppState;

const ECW_API_HOST: &str = "api.e2b.example.test";
const INTEGRATION_KEY: &str = "e2b_subject_scope_integration_0123456789abcdef";
const ISSUER: &str = "example-issuer";
const UPSTREAM_ORG: &str = "example-upstream-org";
const SUBJECT_A: &str = "subject-a";
const SUBJECT_B: &str = "subject-b";
const FORK_IDEMPOTENCY_KEY: &str = "subject-scope-local-fork";

struct Fixture {
    app: Router,
    /// Local row in the integration key's org.
    local_id: SandboxId,
    /// Federated row owned by subject A.
    own_id: SandboxId,
    /// Federated row owned by subject B.
    other_id: SandboxId,
    db: TestDb,
}

fn owner(subject_id: &str) -> TestResult<FederatedOwner> {
    Ok(FederatedOwner {
        issuer: UpstreamIssuer::parse(ISSUER)?,
        org_id: UpstreamOrgId::parse(UPSTREAM_ORG)?,
        subject_id: UpstreamSubjectId::parse(subject_id)?,
    })
}

async fn insert_sandbox(db: &TestDb, org_id: OrgId, user_id: UserId) -> TestResult<SandboxId> {
    let sandbox_id = SandboxId::new();
    sqlx::query(sql!(
        "INSERT INTO sandboxes (id, user_id, org_id, network_policy_generation)
         VALUES ($1, $2, $3, 1)"
    ))
    .bind(sandbox_id)
    .bind(user_id)
    .bind(org_id)
    .execute(&db.pool)
    .await?;
    Ok(sandbox_id)
}

async fn insert_federated_sandbox(db: &TestDb, subject_id: &str) -> TestResult<SandboxId> {
    let record =
        crate::federated_identity::materialize_owner(&db.pool, &owner(subject_id)?).await?;
    insert_sandbox(db, record.org_id, record.user_id).await
}

async fn fixture() -> TestResult<Fixture> {
    let db = pg_test_db().await?;
    sqlx::query(sql!(
        "INSERT INTO api_keys (org_id, name, key_hash, created_by,
                               federated_owner_assertions_enabled)
         VALUES ($1, 'subject-scope-integration', $2, $3, true)"
    ))
    .bind(SEED_ORG_ID)
    .bind(crate::auth::hash_token(INTEGRATION_KEY))
    .bind(SEED_USER_ID)
    .execute(&db.pool)
    .await?;

    let local_id = insert_sandbox(
        &db,
        OrgId::from_uuid(SEED_ORG_ID),
        UserId::from_uuid(SEED_USER_ID),
    )
    .await?;
    let own_id = insert_federated_sandbox(&db, SUBJECT_A).await?;
    let other_id = insert_federated_sandbox(&db, SUBJECT_B).await?;

    let mut state = AppState::from_pool(db.pool.clone()).await?;
    state.federated_identity = FederatedIdentityConfig {
        enabled: true,
        policy: FederatedIdentityPolicy::parse(Some(ISSUER), None, None)?,
    };
    let hosts = EcwHostConfig::from_env_value(Some(
        r#"[{"apiHost":"api.e2b.example.test","host":"e2b.example.test"}]"#,
    ));
    let app = path_root_router_with_ecw_hosts(state.clone(), hosts).with_state(state);
    Ok(Fixture {
        app,
        local_id,
        own_id,
        other_id,
        db,
    })
}

impl Fixture {
    /// Sends one ECW request with the integration key; `subject` adds a full
    /// owner assertion and a valid service grant, so only the sandbox scope
    /// decides the outcome.
    async fn send(
        &self,
        subject: Option<&str>,
        method: Method,
        path: &str,
        body: &str,
    ) -> TestResult<(StatusCode, String)> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, ECW_API_HOST)
            .header("x-api-key", INTEGRATION_KEY)
            .header("idempotency-key", FORK_IDEMPOTENCY_KEY)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(subject) = subject {
            let expires_at = time::OffsetDateTime::now_utc().unix_timestamp() + 24 * 3600;
            request = request
                .header(FEDERATED_ORG_ID_HEADER, UPSTREAM_ORG)
                .header(FEDERATED_USER_ID_HEADER, subject)
                .header(FEDERATED_SERVICE_GRANT_HEADER, "opaque-grant")
                .header(
                    FEDERATED_SERVICE_GRANT_EXPIRES_AT_HEADER,
                    expires_at.to_string(),
                );
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_owned()))?)
            .await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await?;
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }

    async fn listed_ids(&self, subject: Option<&str>, path: &str) -> TestResult<Vec<String>> {
        let (status, body) = self.send(subject, Method::GET, path, "").await?;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let listed: Vec<serde_json::Value> = serde_json::from_str(&body)?;
        Ok(listed
            .iter()
            .filter_map(|item| item["sandboxID"].as_str().map(str::to_owned))
            .collect())
    }

    /// Everything a denied request could have mutated: the target rows and
    /// any durable operation (delete, fork) recorded for them.
    async fn mutable_state(&self) -> TestResult<serde_json::Value> {
        let (state,): (serde_json::Value,) = sqlx::query_as(sql!(
            "SELECT jsonb_build_object(
                 'sandboxes', (SELECT jsonb_agg(to_jsonb(s) ORDER BY s.id)
                               FROM sandboxes s WHERE s.id = ANY($1)),
                 'operations', (SELECT jsonb_agg(to_jsonb(o) ORDER BY o.operation_id)
                                FROM public_operations o))"
        ))
        .bind(vec![self.local_id, self.other_id])
        .fetch_one(&self.db.pool)
        .await?;
        Ok(state)
    }

    /// A completed header-less fork of the local row, recorded in the
    /// integration key's org with no subject, as the local path records it.
    async fn seed_completed_local_fork(&self) -> TestResult {
        let store = PublicOperationStore::new(self.db.pool.clone());
        let request = ForkRequest::from_wire(None, None)?;
        let idempotency_key = IdempotencyKey::parse(FORK_IDEMPOTENCY_KEY)?;
        let org_id = OrgId::from_uuid(SEED_ORG_ID);
        let admission = begin_fork_operation_admission(
            &store,
            org_id,
            self.local_id,
            request,
            None,
            &idempotency_key,
        )
        .await?;
        let CreatePublicOperationResult::Created(operation_id) = admit_locked_fork_operation(
            admission,
            AdmitForkOperation {
                org_id,
                actor_user_id: Some(UserId::from_uuid(SEED_USER_ID)),
                source_sandbox_id: self.local_id,
                request,
                ecw_host_domain: "e2b.example.test",
                hard_deadline: std::time::Duration::from_secs(240),
                federated_owner: None,
                federated_service_grant: None,
                idempotency_key: &idempotency_key,
            },
        )
        .await?
        else {
            return Err("local fork admission did not create the operation".into());
        };
        let child = ForkTargetOutcome::Succeeded {
            sandbox_id: SandboxId::new(),
            template_id: "base".to_string(),
            ecw_host_domain: Some("e2b.example.test".to_string()),
            credential_epoch: EcwCredentialEpoch::new(1)?,
        };
        sqlx::query(sql!(
            "UPDATE public_operations
             SET state = 'succeeded', result_json = $2, completed_at = now()
             WHERE operation_id = $1"
        ))
        .bind(operation_id.as_uuid())
        .bind(serde_json::json!([child]))
        .execute(&self.db.pool)
        .await?;
        Ok(())
    }
}

/// Every sandbox-id route, with a body each handler accepts.
const SANDBOX_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "", ""),
    ("POST", "/connect", r#"{"timeout":300}"#),
    ("POST", "/v2/connect", r#"{"timeout":300}"#),
    ("POST", "/pause", ""),
    ("POST", "/resume", "{}"),
    ("POST", "/timeout", r#"{"timeout":300}"#),
    ("POST", "/refreshes", "{}"),
    ("POST", "/fork", "{}"),
    ("DELETE", "", ""),
];

fn route_path(sandbox_id: SandboxId, suffix: &str) -> String {
    match suffix.strip_prefix("/v2") {
        Some(action) => format!("/v2/sandboxes/{sandbox_id}{action}"),
        None => format!("/sandboxes/{sandbox_id}{suffix}"),
    }
}

#[derive(Debug, Clone, Copy)]
enum Target {
    /// The integration key org's local row.
    KeyOrgLocal,
    /// Another asserted subject's federated row.
    OtherSubject,
}

#[rstest]
#[case::key_org_local(Target::KeyOrgLocal)]
#[case::other_subject(Target::OtherSubject)]
#[tokio::test]
async fn asserted_subject_cannot_reach_rows_it_does_not_own(#[case] target: Target) -> TestResult {
    let fixture = fixture().await?;
    let target_id = match target {
        Target::KeyOrgLocal => fixture.local_id,
        Target::OtherSubject => fixture.other_id,
    };
    // Fork replay must not cross into the key org's subject-less namespace.
    fixture.seed_completed_local_fork().await?;
    let before = fixture.mutable_state().await?;

    let mut violations = Vec::new();
    for path in ["/sandboxes", "/v2/sandboxes"] {
        let listed = fixture.listed_ids(Some(SUBJECT_A), path).await?;
        if listed != vec![fixture.own_id.to_string()] {
            violations.push(format!("GET {path} listed {listed:?}"));
        }
    }
    for (method, suffix, body) in SANDBOX_ROUTES {
        let path = route_path(target_id, suffix);
        let (status, response) = fixture
            .send(Some(SUBJECT_A), method.parse()?, &path, body)
            .await?;
        // A denial is the plain not-found error and mints no guest access.
        if status != StatusCode::NOT_FOUND || response.contains("AccessToken") {
            violations.push(format!("{method} {path}: {status} {response}"));
        }
    }
    assert!(violations.is_empty(), "{violations:#?}");

    assert_eq!(fixture.mutable_state().await?, before);
    Ok(())
}

/// The denial above is scope, not a broken route: the owning subject and the
/// header-less integration path still resolve their own rows.
#[tokio::test]
async fn owners_keep_their_own_rows() -> TestResult {
    let fixture = fixture().await?;

    assert_eq!(
        fixture.listed_ids(Some(SUBJECT_B), "/sandboxes").await?,
        vec![fixture.other_id.to_string()]
    );
    assert_eq!(
        fixture.listed_ids(None, "/sandboxes").await?,
        vec![fixture.local_id.to_string()]
    );
    for (subject, sandbox_id) in [(Some(SUBJECT_A), fixture.own_id), (None, fixture.local_id)] {
        let path = route_path(sandbox_id, "");
        let (status, body) = fixture.send(subject, Method::GET, &path, "").await?;
        assert_eq!(status, StatusCode::OK, "{subject:?} GET {path}: {body}");
    }
    Ok(())
}
