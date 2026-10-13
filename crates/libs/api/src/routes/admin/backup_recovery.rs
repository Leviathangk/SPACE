//! Operator authority for one verified retained snapshot. Creating a grant
//! preserves bytes; it does not enqueue work, wake a sandbox, or select a restore.
use super::support::{api_error, require_managing_org_admin};
use crate::{middleware::auth::AuthUser, routes::AppState};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pplx_space_common_core::types::{ExecutionGeneration, K8sNodeUid, LocalArtifactId, NodeId};
use pplx_space_public_api::PublicSandboxId;
use pplx_space_snapshot_store::recovery::{self, RecoveryGrantId, RecoveryManifest};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approval {
    local_artifact_id: LocalArtifactId,
    execution_generation: ExecutionGeneration,
    uploader_node_id: NodeId,
    uploader_node_uid: K8sNodeUid,
    captured_at: String,
    expires_at: String,
    reason: String,
    manifest: RecoveryManifest,
}

fn timestamp(value: &str) -> Result<time::OffsetDateTime, &'static str> {
    let value: jiff::Timestamp = value
        .parse()
        .map_err(|_| "expected an absolute capture/expiry timestamp")?;
    time::OffsetDateTime::from_unix_timestamp_nanos(value.as_nanosecond())
        .map_err(|_| "timestamp out of range")
}
fn store_error(error: recovery::RecoveryError) -> Response {
    match error {
        recovery::RecoveryError::Refused(message) => {
            api_error(StatusCode::CONFLICT, message).into_response()
        }
        other => {
            tracing::error!(error=%other,"backup recovery grant transaction failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backup recovery transaction failed",
            )
            .into_response()
        }
    }
}

pub(super) async fn approve(
    State(state): State<AppState>,
    Extension(AuthUser(actor)): Extension<AuthUser>,
    Path(sandbox): Path<PublicSandboxId>,
    Json(request): Json<Approval>,
) -> Response {
    if let Err(error) = require_managing_org_admin(&state.pool, &actor).await {
        return error.into_response();
    }
    if !state.feature_flags.bool_value(
        pplx_space_feature_flags::BACKUP_RECOVERY_GRANTS_ENABLED,
        "cluster",
    ) {
        return api_error(StatusCode::SERVICE_UNAVAILABLE,"backup recovery approval is disabled until all backup services support grant enforcement").into_response();
    }
    let captured_at = match timestamp(&request.captured_at) {
        Ok(value) => value,
        Err(message) => return api_error(StatusCode::BAD_REQUEST, message).into_response(),
    };
    let expires_at = match timestamp(&request.expires_at) {
        Ok(value) => value,
        Err(message) => return api_error(StatusCode::BAD_REQUEST, message).into_response(),
    };
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(error) => return store_error(error.into()),
    };
    let id = RecoveryGrantId::new();
    let result = recovery::approve(
        &mut tx,
        recovery::ApproveRecovery {
            id,
            sandbox_id: sandbox.into(),
            artifact_id: request.local_artifact_id,
            generation: request.execution_generation,
            uploader_node_id: request.uploader_node_id,
            uploader_node_uid: &request.uploader_node_uid,
            captured_at,
            expires_at,
            actor: actor.id,
            reason: &request.reason,
            manifest: &request.manifest,
        },
    )
    .await;
    if let Err(error) = result {
        return store_error(error);
    }
    if let Err(error) = tx.commit().await {
        return store_error(error.into());
    }
    Json(serde_json::json!({"grant_id":id,"status":"approved"})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revocation {
    reason: String,
}
pub(super) async fn revoke(
    State(state): State<AppState>,
    Extension(AuthUser(actor)): Extension<AuthUser>,
    Path((sandbox, id)): Path<(PublicSandboxId, RecoveryGrantId)>,
    Json(request): Json<Revocation>,
) -> Response {
    if let Err(error) = require_managing_org_admin(&state.pool, &actor).await {
        return error.into_response();
    }
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(error) => return store_error(error.into()),
    };
    match recovery::revoke(&mut tx, sandbox.into(), id, actor.id, &request.reason).await {
        Ok(true) => {}
        Ok(false) => {
            return api_error(
                StatusCode::CONFLICT,
                "approval is absent, revoked, or consumed",
            )
            .into_response();
        }
        Err(error) => return store_error(error),
    }
    if let Err(error) = tx.commit().await {
        return store_error(error.into());
    }
    Json(serde_json::json!({"grant_id":id,"status":"revoked"})).into_response()
}

#[derive(Deserialize)]
pub(super) struct ShadowQuery {
    after: Option<pplx_space_common_core::types::SandboxId>,
    limit: Option<u32>,
}
pub(super) async fn shadow(
    State(state): State<AppState>,
    Extension(AuthUser(actor)): Extension<AuthUser>,
    Path(node): Path<NodeId>,
    axum::extract::Query(query): axum::extract::Query<ShadowQuery>,
) -> Response {
    if let Err(error) = require_managing_org_admin(&state.pool, &actor).await {
        return error.into_response();
    }
    match pplx_space_internal_api_server::backup_admission_shadow_page(
        &state.pool,
        node,
        query.after,
        query.limit.unwrap_or(100),
    )
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => store_error(error.into()),
    }
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(AuthUser(actor)): Extension<AuthUser>,
    Path(sandbox): Path<PublicSandboxId>,
) -> Response {
    if let Err(error) = require_managing_org_admin(&state.pool, &actor).await {
        return error.into_response();
    }
    match recovery::list_approvals(&state.pool, sandbox.into()).await {
        Ok(grants) => Json(serde_json::json!({"grants":grants})).into_response(),
        Err(error) => store_error(error.into()),
    }
}
