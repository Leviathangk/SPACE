use pplx_space_common_core::types::{OrgId, SandboxId};

use pplx_space_common_core::sql;
use pplx_space_common_core::types::SandboxOwnerSource;

use crate::federated_identity::FederatedOwner;

use super::*;

#[derive(Debug, Clone, Copy)]
pub struct SandboxPageCursor {
    pub created_at: time::OffsetDateTime,
    pub id: SandboxId,
}

/// Owner scope of an authenticated principal. Sandbox reads and actions
/// resolve through this scope so a principal only ever sees its own rows:
/// local principals see local rows of their org; federated principals see only
/// the rows owned by their upstream subject. One trusted integration key
/// carries many asserted subjects, so the key org's local rows are not part of
/// any federated scope; they stay reachable only without an owner assertion.
/// Federated rows are never visible to a local scope.
#[derive(Debug, Clone)]
pub enum SandboxAccessScope {
    LocalOrg { org_id: OrgId },
    FederatedSubject { owner: FederatedOwner },
}

fn sandbox_in_scope(sandbox: &Sandbox, scope: &SandboxAccessScope) -> bool {
    match scope {
        SandboxAccessScope::LocalOrg { org_id } => {
            sandbox.owner_source == SandboxOwnerSource::Local && sandbox.org_id == *org_id
        }
        SandboxAccessScope::FederatedSubject { owner } => {
            sandbox_matches_federated_owner(sandbox, owner)
        }
    }
}

/// A federated row matches only its exact upstream issuer, org, and subject.
pub(crate) fn sandbox_matches_federated_owner(sandbox: &Sandbox, owner: &FederatedOwner) -> bool {
    sandbox.owner_source == SandboxOwnerSource::FederatedIdentity
        && sandbox.upstream_issuer.as_deref() == Some(owner.issuer.as_str())
        && sandbox.upstream_org_id.as_deref() == Some(owner.org_id.as_str())
        && sandbox.upstream_subject_id.as_deref() == Some(owner.subject_id.as_str())
}

impl SandboxController {
    pub async fn sandbox(
        &self,
        pool: &PgPool,
        sandbox_id: SandboxId,
    ) -> Result<Option<Sandbox>, ControllerError> {
        fetch_sandbox(pool, sandbox_id)
            .await
            .map_err(|err| crate::controllers::db_contention_error("failed to fetch sandbox", &err))
    }

    pub async fn scoped_sandbox(
        &self,
        pool: &PgPool,
        scope: &SandboxAccessScope,
        sandbox_id: SandboxId,
    ) -> Result<Option<Sandbox>, ControllerError> {
        let sandbox = self.sandbox(pool, sandbox_id).await?;
        Ok(sandbox.filter(|sandbox| sandbox_in_scope(sandbox, scope)))
    }

    pub async fn scoped_sandbox_page(
        &self,
        pool: &PgPool,
        scope: &SandboxAccessScope,
        cursor: Option<SandboxPageCursor>,
        limit: u32,
        metadata_filter: Option<serde_json::Value>,
    ) -> Result<Vec<Sandbox>, ControllerError> {
        let fetch_limit = i64::from(limit.saturating_add(1));
        let rows = match scope {
            SandboxAccessScope::LocalOrg { org_id } => {
                let sql = sandbox_query(
                    "SELECT {sandbox_columns} FROM {sandbox_from} \
                     WHERE owner_user.owner_source = 'local' \
                       AND s.org_id = $1 \
                       AND ($2::timestamptz IS NULL OR (s.created_at, s.id) < ($2, $3)) \
                       AND ($5::jsonb IS NULL OR s.metadata @> $5::jsonb) \
                     ORDER BY s.created_at DESC, s.id DESC \
                     LIMIT $4",
                );
                sqlx::query_as::<_, Sandbox>(sqlx::AssertSqlSafe(sql!(sql.as_str())))
                    .bind(*org_id)
                    .bind(cursor.map(|cursor| cursor.created_at))
                    .bind(cursor.map(|cursor| cursor.id))
                    .bind(fetch_limit)
                    .bind(metadata_filter)
                    .fetch_all(pool)
                    .await
            }
            SandboxAccessScope::FederatedSubject { owner } => {
                let sql = sandbox_query(
                    "SELECT {sandbox_columns} FROM {sandbox_from} \
                     WHERE owner_user.owner_source = 'federated_identity' \
                       AND owner_user.upstream_issuer = $1 \
                       AND owner_org.upstream_issuer = $1 \
                       AND owner_org.upstream_org_id = $2 \
                       AND owner_user.upstream_subject_id = $3 \
                       AND ($4::timestamptz IS NULL OR (s.created_at, s.id) < ($4, $5)) \
                       AND ($7::jsonb IS NULL OR s.metadata @> $7::jsonb) \
                     ORDER BY s.created_at DESC, s.id DESC \
                     LIMIT $6",
                );
                sqlx::query_as::<_, Sandbox>(sqlx::AssertSqlSafe(sql!(sql.as_str())))
                    .bind(owner.issuer.as_str())
                    .bind(owner.org_id.as_str())
                    .bind(owner.subject_id.as_str())
                    .bind(cursor.map(|cursor| cursor.created_at))
                    .bind(cursor.map(|cursor| cursor.id))
                    .bind(fetch_limit)
                    .bind(metadata_filter)
                    .fetch_all(pool)
                    .await
            }
        }
        .map_err(|err| {
            crate::controllers::db_contention_error("failed to fetch sandbox page", &err)
        })?;
        Ok(rows)
    }

    pub fn cached_sandbox_response(&self, sandbox: &Sandbox) -> SandboxResponse {
        sandbox_to_response(sandbox, None)
    }
}

#[cfg(test)]
mod owner_scope_tests;
