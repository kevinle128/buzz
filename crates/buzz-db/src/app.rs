//! Community-scoped App lifecycle rows and transaction-local command helpers.
//!
//! The database API accepts only already-validated fields and 32-byte secret
//! digests. Raw callback secrets are never generated or stored here.

use chrono::{DateTime, Utc};
use nostr::Event;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use buzz_core::app::AppStatus;
use buzz_core::kind::KIND_APP_METADATA;
use buzz_core::{CommunityId, StoredEvent};

use buzz_datastore_tracing::datastore_span;

use crate::error::{DbError, Result};
use crate::event::insert_event_with_thread_metadata_tx;
use crate::Db;

const GET_APP: &str = "SELECT community_id, id, name, description, icon_url, status, \
     secret_hash, created_by, created_at, updated_at \
     FROM apps WHERE community_id = $1 AND id = $2";

const LIST_APPS: &str = "SELECT community_id, id, name, description, icon_url, status, \
     secret_hash, created_by, created_at, updated_at \
     FROM apps WHERE community_id = $1 ORDER BY created_at ASC, id ASC";

const INSERT_APP: &str = "INSERT INTO apps (
            community_id, id, name, description, icon_url, status,
            secret_hash, created_by
        ) VALUES ($1, $2, $3, $4, $5, 'active', $6, $7)
        RETURNING community_id, id, name, description, icon_url, status, \
                  secret_hash, created_by, created_at, updated_at";

const UPDATE_APP: &str = "UPDATE apps SET
            name = COALESCE($3, name),
            description = CASE WHEN $4 THEN NULLIF($5, '') ELSE description END,
            icon_url = CASE WHEN $6 THEN NULLIF($7, '') ELSE icon_url END,
            updated_at = NOW()
         WHERE community_id = $1 AND id = $2
         RETURNING community_id, id, name, description, icon_url, status, \
                   secret_hash, created_by, created_at, updated_at";

const ROTATE_APP_SECRET: &str = "UPDATE apps SET secret_hash = $3, updated_at = NOW()
         WHERE community_id = $1 AND id = $2
         RETURNING community_id, id, name, description, icon_url, status, \
                   secret_hash, created_by, created_at, updated_at";

const SET_APP_STATUS: &str = "UPDATE apps SET status = $3, updated_at = NOW()
         WHERE community_id = $1 AND id = $2
         RETURNING community_id, id, name, description, icon_url, status, \
                   secret_hash, created_by, created_at, updated_at";

/// Tenant-scoped App row. `secret_hash` is SHA-256 of the callback secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRecord {
    /// Host-derived community that owns this App.
    pub community_id: CommunityId,
    /// Stable App UUID (callback and attribution identifier).
    pub id: Uuid,
    /// Public display name.
    pub name: String,
    /// Optional public description.
    pub description: Option<String>,
    /// Optional public icon URL.
    pub icon_url: Option<String>,
    /// Lifecycle status.
    pub status: AppStatus,
    /// SHA-256 digest of the callback secret (32 bytes).
    pub secret_hash: Vec<u8>,
    /// Creating command signer's 32-byte pubkey. Immutable after create.
    pub created_by: Vec<u8>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last metadata, status, or secret-rotation timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Fields required to insert a new App row.
#[derive(Debug, Clone, Copy)]
pub struct CreateAppParams<'a> {
    /// Caller-generated App UUID.
    pub id: Uuid,
    /// Validated display name.
    pub name: &'a str,
    /// Optional validated description.
    pub description: Option<&'a str>,
    /// Optional validated icon URL.
    pub icon_url: Option<&'a str>,
    /// SHA-256 of the raw secret; must be 32 bytes.
    pub secret_hash: &'a [u8],
    /// Command signer's 32-byte pubkey.
    pub created_by: &'a [u8],
}

/// Optional public-metadata updates. `None` leaves a field unchanged; `Some("")`
/// clears optional description and icon URL.
#[derive(Debug, Clone, Copy)]
pub struct UpdateAppParams<'a> {
    /// Replacement display name.
    pub name: Option<&'a str>,
    /// Replacement or cleared description.
    pub description: Option<&'a str>,
    /// Replacement or cleared icon URL.
    pub icon_url: Option<&'a str>,
}

/// Load one App in `community_id`, or `None` if it does not exist there.
pub async fn get_app(
    pool: &PgPool,
    community_id: CommunityId,
    app_id: Uuid,
) -> Result<Option<AppRecord>> {
    let row = sqlx::query(GET_APP)
        .bind(community_id.as_uuid())
        .bind(app_id)
        .fetch_optional(pool)
        .await?;
    row.map(row_to_app).transpose()
}

/// List Apps in `community_id` ordered by creation time.
pub async fn list_apps(pool: &PgPool, community_id: CommunityId) -> Result<Vec<AppRecord>> {
    let rows = sqlx::query(LIST_APPS)
        .bind(community_id.as_uuid())
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(row_to_app).collect()
}

impl Db {
    /// Load one App in `community_id`, or `None` if it does not exist there.
    #[datastore_span(name = "get_app", system = "postgresql")]
    pub async fn get_app(
        &self,
        community_id: CommunityId,
        app_id: Uuid,
    ) -> Result<Option<AppRecord>> {
        get_app(&self.pool, community_id, app_id).await
    }

    /// List Apps in `community_id` ordered by creation time.
    #[datastore_span(name = "list_apps", system = "postgresql")]
    pub async fn list_apps(&self, community_id: CommunityId) -> Result<Vec<AppRecord>> {
        list_apps(&self.pool, community_id).await
    }
}

/// Insert the kind `9038` command event with `ON CONFLICT DO NOTHING`.
///
/// Returns `true` when this transaction claimed the event and `false` when it
/// already existed. The caller must generate create/rotate credentials only
/// after a successful claim.
pub async fn claim_app_command_event(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    event: &Event,
) -> Result<bool> {
    let (_, was_inserted) =
        insert_event_with_thread_metadata_tx(tx, community_id, event, None, None).await?;
    Ok(was_inserted)
}

/// Insert a new App row on the caller's transaction.
pub async fn create_app(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    params: CreateAppParams<'_>,
) -> Result<AppRecord> {
    let secret_hash = require_digest("secret_hash", params.secret_hash)?;
    let created_by = require_digest("created_by", params.created_by)?;
    let description = nonempty_optional(params.description);
    let icon_url = nonempty_optional(params.icon_url);
    let row = sqlx::query(INSERT_APP)
        .bind(community_id.as_uuid())
        .bind(params.id)
        .bind(params.name)
        .bind(description)
        .bind(icon_url)
        .bind(secret_hash.as_slice())
        .bind(created_by.as_slice())
        .fetch_one(&mut **tx)
        .await?;
    row_to_app(row)
}

/// Update public metadata of an existing App. `created_by` is not writable.
pub async fn update_app(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    app_id: Uuid,
    params: UpdateAppParams<'_>,
) -> Result<AppRecord> {
    let row = sqlx::query(UPDATE_APP)
        .bind(community_id.as_uuid())
        .bind(app_id)
        .bind(params.name)
        .bind(params.description.is_some())
        .bind(params.description)
        .bind(params.icon_url.is_some())
        .bind(params.icon_url)
        .fetch_optional(&mut **tx)
        .await?;
    require_app_row(app_id, row)
}

/// Replace the stored secret digest and bump private `updated_at`.
///
/// Does not write kind `39008` metadata.
pub async fn rotate_app_secret(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    app_id: Uuid,
    secret_hash: &[u8],
) -> Result<AppRecord> {
    let secret_hash = require_digest("secret_hash", secret_hash)?;
    let row = sqlx::query(ROTATE_APP_SECRET)
        .bind(community_id.as_uuid())
        .bind(app_id)
        .bind(secret_hash.as_slice())
        .fetch_optional(&mut **tx)
        .await?;
    require_app_row(app_id, row)
}

/// Set App status to `active` or `disabled`.
pub async fn set_app_status(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    app_id: Uuid,
    status: AppStatus,
) -> Result<AppRecord> {
    let status = match status {
        AppStatus::Active => "active",
        AppStatus::Disabled => "disabled",
    };
    let row = sqlx::query(SET_APP_STATUS)
        .bind(community_id.as_uuid())
        .bind(app_id)
        .bind(status)
        .fetch_optional(&mut **tx)
        .await?;
    require_app_row(app_id, row)
}

/// Next kind `39008` `created_at` for `app_id`: `max(now_seconds, head + 1)`.
pub async fn next_app_metadata_created_at(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    relay_pubkey: &[u8],
    app_id: Uuid,
) -> Result<i64> {
    let head: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT created_at FROM events \
         WHERE community_id = $1 AND kind = $2 AND pubkey = $3 AND d_tag = $4 \
           AND deleted_at IS NULL \
         ORDER BY created_at DESC, id ASC LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(KIND_APP_METADATA as i32)
    .bind(relay_pubkey)
    .bind(app_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    let now = Utc::now().timestamp();
    Ok(match head {
        Some(created_at) => now.max(created_at.timestamp() + 1),
        None => now,
    })
}

/// Replace kind `39008` by `(community, kind, relay pubkey, d)` with NIP-33
/// ordering (`created_at` desc, then lexicographically lower event ID).
///
/// Does not call [`crate::Db::replace_parameterized_event`] or
/// `replace_addressable_event`.
pub async fn replace_app_metadata(
    tx: &mut Transaction<'_, Postgres>,
    community_id: CommunityId,
    event: &Event,
) -> Result<(StoredEvent, bool)> {
    let kind_i32 = buzz_core::kind::event_kind_i32(event);
    if kind_i32 != KIND_APP_METADATA as i32 {
        return Err(DbError::InvalidData(format!(
            "expected kind {KIND_APP_METADATA}, got {kind_i32}"
        )));
    }
    let d_tag = crate::event::extract_d_tag(event)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DbError::InvalidData("kind 39008 requires a d tag".into()))?;
    let pubkey_bytes = event.pubkey.to_bytes();
    let created_at_secs = event.created_at.as_secs() as i64;
    let created_at = DateTime::from_timestamp(created_at_secs, 0)
        .ok_or(DbError::InvalidTimestamp(created_at_secs))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(app_metadata_lock_key(
            community_id,
            kind_i32,
            pubkey_bytes.as_slice(),
            d_tag.as_bytes(),
        ))
        .execute(&mut **tx)
        .await?;

    let existing: Option<(DateTime<Utc>, Vec<u8>)> = sqlx::query_as(
        "SELECT created_at, id FROM events \
         WHERE community_id = $1 AND kind = $2 AND pubkey = $3 AND d_tag = $4 \
           AND deleted_at IS NULL \
         ORDER BY created_at DESC, id ASC LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(kind_i32)
    .bind(pubkey_bytes.as_slice())
    .bind(&d_tag)
    .fetch_optional(&mut **tx)
    .await?;

    let incoming_id = event.id.as_bytes().as_slice();
    if let Some((existing_ts, existing_id)) = &existing {
        let dominated = created_at < *existing_ts
            || (created_at == *existing_ts && incoming_id >= existing_id.as_slice());
        if dominated {
            return Ok((
                StoredEvent::with_received_at(event.clone(), Utc::now(), None, false),
                false,
            ));
        }
    }

    if existing.is_some() {
        sqlx::query(
            "UPDATE events SET deleted_at = NOW() \
             WHERE community_id = $1 AND kind = $2 AND pubkey = $3 AND d_tag = $4 \
               AND deleted_at IS NULL",
        )
        .bind(community_id.as_uuid())
        .bind(kind_i32)
        .bind(pubkey_bytes.as_slice())
        .bind(&d_tag)
        .execute(&mut **tx)
        .await?;
    }

    let (stored, was_inserted) =
        insert_event_with_thread_metadata_tx(tx, community_id, event, None, None).await?;
    if !was_inserted {
        return Err(DbError::InvalidData(
            "app metadata event id already exists".into(),
        ));
    }
    crate::insert_mentions_in_transaction(tx, community_id, event, None).await?;
    Ok((stored, true))
}

fn app_metadata_lock_key(community_id: CommunityId, kind: i32, pubkey: &[u8], d_tag: &[u8]) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let kind_bytes = kind.to_le_bytes();
    for bytes in [
        community_id.as_uuid().as_bytes().as_slice(),
        kind_bytes.as_slice(),
        pubkey,
        d_tag,
    ] {
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash as i64
}

fn require_digest(field: &str, bytes: &[u8]) -> Result<[u8; 32]> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        DbError::InvalidData(format!(
            "{field} must be a 32-byte digest, got {}",
            bytes.len()
        ))
    })
}

fn nonempty_optional(value: Option<&str>) -> Option<&str> {
    value.filter(|inner| !inner.is_empty())
}

fn require_app_row(app_id: Uuid, row: Option<sqlx::postgres::PgRow>) -> Result<AppRecord> {
    match row {
        Some(row) => row_to_app(row),
        None => Err(DbError::NotFound(format!("app {app_id}"))),
    }
}

fn row_to_app(row: sqlx::postgres::PgRow) -> Result<AppRecord> {
    let status: String = row.try_get("status")?;
    let status = match status.as_str() {
        "active" => AppStatus::Active,
        "disabled" => AppStatus::Disabled,
        other => return Err(DbError::InvalidData(format!("unknown app status: {other}"))),
    };
    Ok(AppRecord {
        community_id: CommunityId::from_uuid(row.try_get("community_id")?),
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        icon_url: row.try_get("icon_url")?,
        status,
        secret_hash: row.try_get("secret_hash")?,
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
