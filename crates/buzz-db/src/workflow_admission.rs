//! Serialized idempotent admission for dynamically routed webhook workflow runs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use buzz_core::{CommunityId, StoredEvent};

use buzz_datastore_tracing::datastore_span;

use crate::error::{DbError, Result};
use crate::workflow::{row_to_run_record, RunStatus, WorkflowRunFailure, WorkflowRunRecord};
use crate::Db;

const MATCHED_IDENTITY_TIERS: &[&str] = &["d_tag", "alias", "clone_basename", "display_name"];

const SELECT_EXISTING_RUN: &str = r#"
        SELECT community_id, id, workflow_id, status::text AS status,
               trigger_event_id, current_step, execution_trace, trigger_context, started_at, completed_at,
               error_message, error_code, created_at, idempotency_key_hash, payload_hash, route_snapshot
        FROM workflow_runs
        WHERE community_id = $1 AND workflow_id = $2 AND idempotency_key_hash = $3
        "#;

const INSERT_ADMITTED_RUN: &str = r#"
            INSERT INTO workflow_runs (
                community_id, id, workflow_id, status, trigger_event_id, current_step,
                execution_trace, trigger_context, started_at, completed_at,
                error_code, error_message, idempotency_key_hash, payload_hash, route_snapshot
            )
            VALUES (
                $1, $2, $3, $4::run_status, NULL, 0,
                '[]'::jsonb, $5, NULL,
                CASE WHEN $4 = 'failed' THEN NOW() ELSE NULL END,
                $6, $7, $8, $9, $10
            )
            RETURNING community_id, id, workflow_id, status::text AS status,
                      trigger_event_id, current_step, execution_trace, trigger_context, started_at, completed_at,
                      error_message, error_code, created_at, idempotency_key_hash, payload_hash, route_snapshot
            "#;

/// Server-resolved destination captured when a dynamic webhook run is admitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRouteSnapshot {
    /// Community that owns the admitted run.
    pub community_id: Uuid,
    /// Canonical `30617:<owner>:<d>` repository coordinate.
    pub repository_coordinate: String,
    /// Canonical `30621:<owner>:<d>` project coordinate.
    pub project_coordinate: String,
    /// Destination channel selected by the route.
    pub channel_id: Uuid,
    /// Identity tier that produced the unique repository match.
    ///
    /// One of `d_tag`, `alias`, `clone_basename`, or `display_name`.
    pub matched_identity_tier: String,
}

/// Outcome of attempting to admit a run for an idempotency key.
pub enum BeginWorkflowAdmission {
    /// A run for this key and payload hash already exists.
    Existing(WorkflowRunRecord),
    /// A run for this key exists with a different payload hash.
    PayloadConflict {
        /// Previously admitted run for this idempotency key.
        existing: WorkflowRunRecord,
    },
    /// No run exists; the caller holds the admission lock until finalize or drop.
    Vacant(WorkflowAdmissionGuard),
}

/// In-flight admission transaction holding the per-key advisory lock.
///
/// Dropping the guard rolls the transaction back and inserts nothing.
pub struct WorkflowAdmissionGuard {
    tx: Transaction<'static, Postgres>,
    community_id: CommunityId,
    workflow_id: Uuid,
    idempotency_key_hash: [u8; 32],
    payload_hash: [u8; 32],
}

/// Destination channel fields needed while an admission transaction is open.
pub struct WorkflowAdmissionChannel {
    /// Channel identifier.
    pub id: Uuid,
    /// When the channel was archived, if applicable.
    pub archived_at: Option<DateTime<Utc>>,
}

impl WorkflowAdmissionGuard {
    /// List latest live global parameterized heads of `kind` on this transaction.
    pub async fn list_latest_parameterized_heads(&mut self, kind: i32) -> Result<Vec<StoredEvent>> {
        crate::project_heads::list_latest_parameterized_heads_on(
            &mut self.tx,
            self.community_id,
            kind,
        )
        .await
    }

    /// Load destination channel `id` and `archived_at` in this admission community.
    ///
    /// Soft-deleted rows are excluded. A channel that exists only in another
    /// community is not visible.
    pub async fn load_destination_channel(
        &mut self,
        channel_id: Uuid,
    ) -> Result<Option<WorkflowAdmissionChannel>> {
        let row = sqlx::query(
            "SELECT id, archived_at FROM channels \
             WHERE community_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(self.community_id.as_uuid())
        .bind(channel_id)
        .fetch_optional(&mut *self.tx)
        .await?;
        row.map(|row| {
            Ok(WorkflowAdmissionChannel {
                id: row.try_get("id")?,
                archived_at: row.try_get("archived_at")?,
            })
        })
        .transpose()
    }

    /// Active membership role for `pubkey` in `channel_id` in this community.
    ///
    /// Uses the same predicates as [`crate::channel::get_member_role`].
    pub async fn destination_member_role(
        &mut self,
        channel_id: Uuid,
        pubkey: &[u8],
    ) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT cm.role::text AS role FROM channel_members cm \
             JOIN channels c ON cm.community_id = c.community_id AND cm.channel_id = c.id AND c.deleted_at IS NULL \
             WHERE cm.community_id = $1 AND cm.channel_id = $2 AND cm.pubkey = $3 AND cm.removed_at IS NULL",
        )
        .bind(self.community_id.as_uuid())
        .bind(channel_id)
        .bind(pubkey)
        .fetch_optional(&mut *self.tx)
        .await?;
        Ok(row.map(|r| r.try_get("role")).transpose()?)
    }

    /// Persist a pending run with the resolved route snapshot and commit.
    pub async fn accept(
        self,
        trigger_context: &Value,
        route_snapshot: &WorkflowRouteSnapshot,
    ) -> Result<WorkflowRunRecord> {
        validate_matched_identity_tier(&route_snapshot.matched_identity_tier)?;
        let snapshot_json = serde_json::to_value(route_snapshot)?;
        self.finalize(trigger_context, Some(snapshot_json), None)
            .await
    }

    /// Persist a failed admission row with a stable code and redacted message, then commit.
    pub async fn reject(
        self,
        trigger_context: &Value,
        failure: WorkflowRunFailure<'_>,
    ) -> Result<WorkflowRunRecord> {
        self.finalize(trigger_context, None, Some(failure)).await
    }

    async fn finalize(
        self,
        trigger_context: &Value,
        route_snapshot: Option<Value>,
        failure: Option<WorkflowRunFailure<'_>>,
    ) -> Result<WorkflowRunRecord> {
        let mut tx = self.tx;
        let status = if failure.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Pending
        };
        let status_str = status.to_string();
        let (error_code, error_message) = failure
            .map(|failure| (Some(failure.code), Some(failure.message)))
            .unwrap_or((None, None));
        let id = Uuid::new_v4();
        let row = sqlx::query(INSERT_ADMITTED_RUN)
            .bind(self.community_id.as_uuid())
            .bind(id)
            .bind(self.workflow_id)
            .bind(&status_str)
            .bind(trigger_context)
            .bind(error_code)
            .bind(error_message)
            .bind(self.idempotency_key_hash.as_slice())
            .bind(self.payload_hash.as_slice())
            .bind(route_snapshot)
            .fetch_one(&mut *tx)
            .await?;
        let record = row_to_run_record(row)?;
        tx.commit().await?;
        Ok(record)
    }
}

/// Begin serialized admission for `(community, workflow, idempotency_key_hash)`.
///
/// The advisory lock is the first statement in the transaction so a duplicate
/// waiter cannot observe a vacant key while the first caller is still inserting.
pub async fn begin_workflow_admission(
    pool: &PgPool,
    community_id: CommunityId,
    workflow_id: Uuid,
    idempotency_key_hash: &[u8; 32],
    payload_hash: &[u8; 32],
) -> Result<BeginWorkflowAdmission> {
    let mut tx = pool.begin().await?;
    let lock_key = format!(
        "buzz_workflow_admission:{}:{}:{}",
        community_id.as_uuid(),
        workflow_id,
        hex::encode(idempotency_key_hash)
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&lock_key)
        .execute(&mut *tx)
        .await?;

    let row = sqlx::query(SELECT_EXISTING_RUN)
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .bind(idempotency_key_hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;

    match row {
        Some(row) => {
            let existing = row_to_run_record(row)?;
            let payload_matches = existing.payload_hash.as_deref() == Some(payload_hash.as_slice());
            tx.commit().await?;
            if payload_matches {
                Ok(BeginWorkflowAdmission::Existing(existing))
            } else {
                Ok(BeginWorkflowAdmission::PayloadConflict { existing })
            }
        }
        None => Ok(BeginWorkflowAdmission::Vacant(WorkflowAdmissionGuard {
            tx,
            community_id,
            workflow_id,
            idempotency_key_hash: *idempotency_key_hash,
            payload_hash: *payload_hash,
        })),
    }
}

impl Db {
    /// Begin serialized idempotent admission for a dynamically routed workflow run.
    #[datastore_span(name = "begin_workflow_admission", system = "postgresql")]
    pub async fn begin_workflow_admission(
        &self,
        community_id: CommunityId,
        workflow_id: Uuid,
        idempotency_key_hash: &[u8; 32],
        payload_hash: &[u8; 32],
    ) -> Result<BeginWorkflowAdmission> {
        begin_workflow_admission(
            &self.pool,
            community_id,
            workflow_id,
            idempotency_key_hash,
            payload_hash,
        )
        .await
    }
}

pub(crate) fn route_snapshot_from_stored_json(value: Value) -> Result<WorkflowRouteSnapshot> {
    let snapshot: WorkflowRouteSnapshot = serde_json::from_value(value)
        .map_err(|err| DbError::InvalidData(format!("malformed workflow route snapshot: {err}")))?;
    validate_matched_identity_tier(&snapshot.matched_identity_tier)?;
    Ok(snapshot)
}

fn validate_matched_identity_tier(tier: &str) -> Result<()> {
    if MATCHED_IDENTITY_TIERS.contains(&tier) {
        Ok(())
    } else {
        Err(DbError::InvalidData(format!(
            "unknown matched identity tier: {tier}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_snapshot_round_trips_all_server_fields() {
        let community_id = Uuid::new_v4();
        let channel_id = Uuid::new_v4();
        let snapshot = WorkflowRouteSnapshot {
            community_id,
            repository_coordinate: format!("30617:{}:group:agentic-os-plan", "ab".repeat(32)),
            project_coordinate: format!("30621:{}:gigo-harness", "cd".repeat(32)),
            channel_id,
            matched_identity_tier: "d_tag".into(),
        };
        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");
        let parsed: WorkflowRouteSnapshot =
            serde_json::from_value(value).expect("deserialize snapshot");
        assert_eq!(parsed, snapshot);
    }
}
