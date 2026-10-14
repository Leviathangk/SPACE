//! Kubernetes node inventory: role upserts, demotion of unobserved nodes, and
//! tombstone checks.

use super::*;

impl K8sStore {
    pub(in crate::controllers::substrate::kubernetes) async fn persist_k8s_node_inventory(
        &self,
        node_name: &str,
        node_uid: &str,
        node_id: NodeId,
        role: NodeRole,
        address: Option<&str>,
        state: NodeStateWrite,
    ) -> Result<bool, SubstrateError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .db_context("failed to start Kubernetes node inventory transaction")?;

        let changed = self
            .upsert_k8s_node_role(
                &mut tx,
                K8sNodeRoleUpsert {
                    node_name,
                    node_uid,
                    node_id,
                    role,
                    address,
                    state,
                },
            )
            .await?;

        tx.commit()
            .await
            .db_context("failed to commit Kubernetes node inventory transaction")?;
        Ok(changed)
    }

    /// Kubernetes-managed registrations of `role` that are not deleted, as
    /// `(node id, Kubernetes node name)`.
    pub(in crate::controllers::substrate::kubernetes) async fn registered_k8s_nodes(
        &self,
        role: NodeRole,
    ) -> Result<Vec<(NodeId, String)>, SubstrateError> {
        sqlx::query_as(sql!(
            "SELECT n.id, nk.k8s_node_name
             FROM nodes n
             JOIN node_k8s nk ON nk.node_id = n.id
             WHERE n.managed_by = 'k8s'
               AND n.role = $1
               AND COALESCE(n.state, 'registered') != 'deleted'
             ORDER BY nk.k8s_node_name"
        ))
        .bind(role.as_str())
        .fetch_all(&self.pool)
        .await
        .db_context("failed to list Kubernetes node registrations")
    }

    /// Bind a Kubernetes Node's UID onto the registration internal-api already
    /// made for it, without creating a row or taking over its lifecycle.
    ///
    /// Builder nodes are registered by internal-api with no UID, and no
    /// sandbox pod ever records one for them, so the credential-manager
    /// authority's node check (name and UID, `registered_node`) refused every
    /// caller on them. The UID comes from the Kubernetes API here, never from
    /// the node. A Node replaced under the same name (a different UID) clears
    /// drain state as [`Self::upsert_k8s_node_role`] does, so a drain aimed at
    /// the old instance cannot reach its replacement; the first binding of a
    /// registration that had no UID is not a replacement and leaves it alone.
    ///
    /// Returns whether the stored UID changed. A node with no registration, or
    /// one not managed by Kubernetes, matches nothing and returns `false`.
    pub(in crate::controllers::substrate::kubernetes) async fn bind_registered_k8s_node_uid(
        &self,
        node_name: &str,
        node_uid: &str,
        node_id: NodeId,
    ) -> Result<bool, SubstrateError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .db_context("failed to start Kubernetes node UID binding transaction")?;

        // Before the UID update below, which would hide the replacement.
        let drain_cleared = sqlx::query(sql!(
            "UPDATE nodes
             SET drain_phase = NULL,
                 drain_mode = NULL,
                 drain_started_at = NULL,
                 drain_last_error = NULL
             WHERE id = $1
               AND managed_by = 'k8s'
               AND EXISTS (
                 SELECT 1 FROM node_k8s
                 WHERE node_id = $1
                   AND k8s_node_name = $2
                   AND k8s_node_uid IS NOT NULL
                   AND k8s_node_uid IS DISTINCT FROM $3
               )"
        ))
        .bind(node_id)
        .bind(node_name)
        .bind(node_uid)
        .execute(&mut *tx)
        .await
        .db_context("failed to clear drain state on Kubernetes node UID replacement")?
        .rows_affected();
        if drain_cleared > 0 {
            tracing::warn!(
                node_id = %node_id,
                node_name = %node_name,
                new_k8s_node_uid = %node_uid,
                "k8s Node UID changed for a registered node; cleared drain_phase/drain_mode/drain_started_at/drain_last_error — \
                 any in-progress drain was abandoned"
            );
        }

        let bound = sqlx::query(sql!(
            "UPDATE node_k8s AS nk
             SET k8s_node_uid = $3,
                 updated_at = NOW()
             FROM nodes AS n
             WHERE nk.node_id = $1
               AND n.id = nk.node_id
               AND n.managed_by = 'k8s'
               AND nk.k8s_node_name = $2
               AND nk.k8s_node_uid IS DISTINCT FROM $3"
        ))
        .bind(node_id)
        .bind(node_name)
        .bind(node_uid)
        .execute(&mut *tx)
        .await
        .db_context("failed to bind Kubernetes node UID")?
        .rows_affected()
            > 0;

        tx.commit()
            .await
            .db_context("failed to commit Kubernetes node UID binding transaction")?;
        Ok(bound)
    }

    /// Demote stale ready rows missing from a credible full-cluster Kubernetes
    /// membership snapshot without deleting node or sandbox placement records.
    pub(in crate::controllers::substrate::kubernetes) async fn demote_unobserved_k8s_nodes(
        &self,
        live_node_names: &[String],
        missing_grace: Duration,
        max_per_tick: u32,
    ) -> Result<Vec<String>, SubstrateError> {
        if live_node_names.is_empty() {
            return Ok(Vec::new());
        }

        let missing_grace_secs = i64::try_from(missing_grace.as_secs()).unwrap_or(i64::MAX);
        let mut changed = sqlx::query_scalar::<_, String>(sql!(
            "WITH candidates AS MATERIALIZED (
                 SELECT candidate.id,
                        candidate.last_heartbeat,
                        candidate_k8s.updated_at AS k8s_updated_at
                 FROM nodes AS candidate
                 JOIN node_k8s AS candidate_k8s
                   ON candidate_k8s.node_id = candidate.id
                 WHERE candidate.managed_by = 'k8s'
                   AND candidate.state = 'ready'
                   AND candidate.drain_phase IS NULL
                   AND NOT (candidate_k8s.k8s_node_name = ANY($1::TEXT[]))
                   AND (
                        candidate.last_heartbeat IS NULL
                        OR candidate.last_heartbeat
                           < NOW() - ($2 * INTERVAL '1 second')
                   )
                   AND candidate_k8s.updated_at
                       < NOW() - ($2 * INTERVAL '1 second')
                 ORDER BY candidate_k8s.updated_at, candidate.id
                 LIMIT $3
             )
             UPDATE nodes AS n
             SET state = 'registered'
             FROM node_k8s AS nk
             JOIN candidates AS candidate
               ON candidate.id = nk.node_id
             WHERE n.id = candidate.id
               AND n.managed_by = 'k8s'
               AND n.state = 'ready'
               AND n.drain_phase IS NULL
               AND n.last_heartbeat IS NOT DISTINCT FROM candidate.last_heartbeat
               AND nk.updated_at = candidate.k8s_updated_at
             RETURNING nk.k8s_node_name"
        ))
        .bind(live_node_names)
        .bind(missing_grace_secs)
        .bind(i64::from(max_per_tick))
        .fetch_all(&self.pool)
        .await
        .db_context("failed to demote unobserved Kubernetes nodes")?;
        changed.sort_unstable();
        Ok(changed)
    }

    pub(super) async fn upsert_k8s_node_role(
        &self,
        conn: &mut PgConnection,
        upsert: K8sNodeRoleUpsert<'_>,
    ) -> Result<bool, SubstrateError> {
        let K8sNodeRoleUpsert {
            node_name,
            node_uid,
            node_id,
            role,
            address,
            state,
        } = upsert;

        // `manage_state` decides whether a conflict-update writes `state`.
        // The lifecycle owner writes it on every upsert; insert-only writers
        // (pod placement, the admin inventory listing) supply an initial state
        // for a new row only and must leave an existing node's state alone.
        let (state, manage_state) = match state {
            NodeStateWrite::Manage(value) => (value.as_str(), true),
            NodeStateWrite::InsertOnly(value) => (value.as_str(), false),
        };

        // Tombstone gate. The bootstrap path on `cluster.rs` refuses
        // re-registration, but inventory upserts (kube_node_sync, late
        // pod placement on a not-yet-cordoned terminated node) reach this
        // function too. Without this check they re-create the `nodes`
        // row under the same deterministic id and subsequent heartbeats
        // resolve a live row before they hit the bootstrap guard, so the
        // ghost row returns. The whole transaction is rolled back.
        if is_node_k8s_tombstoned(conn, node_id, "check").await? {
            return Err(SubstrateError::NodeTombstoned {
                message: format!(
                    "kube node {node_name} role {role} is tombstoned (admin-terminated); \
                     refusing to re-create the SPACE node row"
                ),
            });
        }

        let node_changed = sqlx::query_scalar::<_, bool>(sql!(
            "WITH upsert AS (
                 INSERT INTO nodes (id, address, role, managed_by, state)
                 VALUES ($1, $2::INET, $3, 'k8s', $4)
                 ON CONFLICT (id) DO UPDATE
                 SET address = COALESCE(EXCLUDED.address, nodes.address),
                     role = EXCLUDED.role,
                     managed_by = EXCLUDED.managed_by,
                     state = CASE WHEN $5 THEN EXCLUDED.state ELSE nodes.state END
                 WHERE nodes.managed_by = 'k8s'
                   AND (
                     nodes.address IS DISTINCT FROM COALESCE(EXCLUDED.address, nodes.address)
                     OR nodes.role IS DISTINCT FROM EXCLUDED.role
                     OR ($5 AND nodes.state IS DISTINCT FROM EXCLUDED.state)
                   )
                 RETURNING TRUE AS changed
             )
             SELECT changed FROM upsert
             UNION ALL
             SELECT FALSE FROM nodes
             WHERE id = $1
               AND managed_by = 'k8s'
               AND NOT EXISTS (SELECT 1 FROM upsert)"
        ))
        .bind(node_id)
        .bind(address)
        .bind(role.as_str())
        .bind(state)
        .bind(manage_state)
        .fetch_optional(&mut *conn)
        .await
        .db_context("failed to upsert Kubernetes node registry")?;

        let node_changed = match node_changed {
            Some(changed) => changed,
            None => {
                // The upsert returned no row. Under READ COMMITTED a
                // concurrent insert of this same node id can commit while our
                // `INSERT ... ON CONFLICT` waits on the unique index: the
                // ON CONFLICT arm then locks and reads that row, the
                // change-detection WHERE skips the now-redundant update
                // (address and role already match, and placement does not own
                // `state`), and the fallback SELECT — bound to our stale
                // statement snapshot — cannot see the row, so the whole query
                // returns nothing. Only a genuinely non-Kubernetes conflict
                // should abort the caller, so re-check node ownership with a
                // fresh statement snapshot before erroring.
                let existing_is_k8s = sqlx::query_scalar::<_, bool>(sql!(
                    "SELECT TRUE FROM nodes WHERE id = $1 AND managed_by = 'k8s'"
                ))
                .bind(node_id)
                .fetch_optional(&mut *conn)
                .await
                .db_context("failed to re-check Kubernetes node ownership")?
                .is_some();

                if !existing_is_k8s {
                    return Err(SubstrateError::Failed {
                        message: format!(
                            "Kubernetes node uid {node_uid} role {role} for {node_name} conflicts with a non-Kubernetes node"
                        ),
                    });
                }
                // The node already exists as a Kubernetes row; the concurrent
                // writer established it and this upsert changed nothing.
                false
            }
        };

        // Detect same-name kube Node UID replacement BEFORE the upsert
        // below overwrites `node_k8s.k8s_node_uid`. If a replacement
        // node arrives and we leave `nodes.drain_phase` intact, the
        // node-drain reconciler will resolve the providerID of the
        // REPLACEMENT instance and ASG-terminate it on the operator's
        // behalf — the original (now-gone) instance was the intended
        // target. Clear drain state atomically with the UID change so
        // in-progress drains are abandoned (operator can re-drain
        // against the new identity if they still want to).
        //
        // `drain_mode` is cleared with them. It records *how* a drain is being
        // serviced, so it means nothing once the phase is gone — and leaving it
        // set is worse than meaningless: the admin listing and `space-admin-cli get-node`
        // read the column independently of the phase, so a surviving mode makes
        // a node that is not draining report `drain_phase: null` next to
        // `drain_mode: gentle`.
        let drain_cleared = sqlx::query(sql!(
            "UPDATE nodes
             SET drain_phase = NULL,
                 drain_mode = NULL,
                 drain_started_at = NULL,
                 drain_last_error = NULL
             WHERE id = $1
               AND EXISTS (
                 SELECT 1 FROM node_k8s
                 WHERE node_id = $1 AND k8s_node_uid IS DISTINCT FROM $2
               )"
        ))
        .bind(node_id)
        .bind(node_uid)
        .execute(&mut *conn)
        .await
        .db_context("failed to clear drain state on UID change")?
        .rows_affected();
        if drain_cleared > 0 {
            tracing::warn!(
                node_id = %node_id,
                node_name = %node_name,
                new_k8s_node_uid = %node_uid,
                "k8s Node UID changed for existing node row; cleared drain_phase/drain_mode/drain_started_at/drain_last_error — \
                 any in-progress drain was abandoned"
            );
        }

        let node_k8s_changed = sqlx::query(sql!(
            "INSERT INTO node_k8s (node_id, k8s_node_name, k8s_node_uid)
             VALUES ($1, $2, $3)
             ON CONFLICT (node_id) DO UPDATE
             SET k8s_node_name = EXCLUDED.k8s_node_name,
                 k8s_node_uid = EXCLUDED.k8s_node_uid,
                 updated_at = NOW()
             WHERE node_k8s.k8s_node_name IS DISTINCT FROM EXCLUDED.k8s_node_name
                OR node_k8s.k8s_node_uid IS DISTINCT FROM EXCLUDED.k8s_node_uid"
        ))
        .bind(node_id)
        .bind(node_name)
        .bind(node_uid)
        .execute(&mut *conn)
        .await
        .db_context("failed to upsert Kubernetes node metadata")?
        .rows_affected()
            > 0;

        // Race-close re-check, paired with the pre-check above. A
        // concurrent `:terminate` that commits during our row-lock wait
        // would not have been visible to the pre-check but is now
        // visible — returning Err here causes the caller's transaction
        // to roll back, undoing the resurrection insert. See
        // `bootstrap_k8s_origin_node` for the same pattern.
        if is_node_k8s_tombstoned(conn, node_id, "re-check").await? {
            return Err(SubstrateError::NodeTombstoned {
                message: format!(
                    "kube node {node_name} role {role} was tombstoned during upsert; \
                     rolling back to keep the SPACE node row deleted"
                ),
            });
        }
        Ok(node_changed || drain_cleared > 0 || node_k8s_changed)
    }
}

async fn is_node_k8s_tombstoned(
    conn: &mut PgConnection,
    node_id: NodeId,
    op: &str,
) -> Result<bool, SubstrateError> {
    sqlx::query_scalar(sql!(
        "SELECT EXISTS(SELECT 1 FROM node_k8s_tombstones WHERE node_id = $1)"
    ))
    .bind(node_id)
    .fetch_one(conn)
    .await
    .db_context_with(|| format!("failed to {op} node tombstone"))
}
