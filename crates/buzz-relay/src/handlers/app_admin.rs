//! Community App lifecycle admin command handler (kind 9038).
//!
//! Kind `9038` is a signed command: ingest verifies the signature, timestamp,
//! identity, `Scope::AdminUsers`, global-token requirement, and global-only
//! scoping, then [`super::command_executor::handle_command`] dispatches here.
//! This handler additionally requires an active community `owner` or `admin`
//! row in `relay_members`.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nostr::{Event, EventBuilder, Kind, Tag, Timestamp};
use rand::rngs::SysRng as OsRng;
use rand::TryRng;
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};
use uuid::Uuid;

use buzz_audit::{AuditAction, NewAuditEntry};
use buzz_core::app::{parse_app_admin_command, AppAdminCommand, AppStatus, APP_SECRET_BYTES};
use buzz_core::kind::KIND_APP_METADATA;
use buzz_core::tenant::TenantContext;
use buzz_core::StoredEvent;
use buzz_db::app::{
    claim_app_command_event, create_app, next_app_metadata_created_at, replace_app_metadata,
    rotate_app_secret, set_app_status, update_app, AppRecord, CreateAppParams, UpdateAppParams,
};
use buzz_db::DbError;

use crate::state::AppState;

use super::event::dispatch_persistent_event;
use super::ingest::{IngestAuth, IngestError, IngestResult};

/// Execute a kind `9038` App admin command after ingest authorization.
pub(crate) async fn handle_app_admin(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    event: &Event,
    _auth: &IngestAuth,
) -> Result<IngestResult, IngestError> {
    let command = parse_app_admin_command(&event.content)
        .map_err(|err| IngestError::Rejected(format!("invalid: {err}")))?;
    authorize_app_admin(tenant, state, event).await?;

    let mut tx = state
        .db
        .begin_transaction()
        .await
        .map_err(|err| IngestError::Internal(format!("error: begin transaction: {err}")))?;
    buzz_deletion::store(&state.db)
        .guard_transaction(&mut tx, tenant.community())
        .await
        .map_err(|err| {
            IngestError::Rejected(format!("restricted: community writes are fenced: {err}"))
        })?;

    let claimed = claim_app_command_event(&mut tx, tenant.community(), event)
        .await
        .map_err(map_db_error)?;
    if !claimed {
        return Ok(IngestResult {
            event_id: event.id.to_hex(),
            accepted: true,
            message: "duplicate: already processed".into(),
        });
    }

    let outcome = execute_claimed_command(tenant, state, event, command, &mut tx).await?;
    tx.commit()
        .await
        .map_err(|err| IngestError::Internal(format!("error: commit transaction: {err}")))?;

    if let Some(metadata) = &outcome.metadata {
        dispatch_persistent_event(
            tenant,
            state,
            metadata,
            KIND_APP_METADATA,
            &event.pubkey.to_hex(),
            None,
        )
        .await;
    }
    enqueue_lifecycle_audit(tenant, state, event, &outcome).await;

    info!(
        app_id = %outcome.app_id,
        action = outcome.audit.as_str(),
        "app admin command applied"
    );

    Ok(IngestResult {
        event_id: event.id.to_hex(),
        accepted: true,
        message: format!("response:{}", outcome.response_json()),
    })
}

struct CommandOutcome {
    app_id: Uuid,
    webhook_secret: Option<String>,
    metadata: Option<StoredEvent>,
    audit: AuditAction,
    status: AppStatus,
}

impl CommandOutcome {
    fn response_json(&self) -> serde_json::Value {
        let mut body = serde_json::json!({ "app_id": self.app_id });
        if let Some(secret) = &self.webhook_secret {
            body["webhook_secret"] = serde_json::Value::String(secret.clone());
        }
        body
    }

    fn audit_detail(&self) -> serde_json::Value {
        serde_json::json!({
            "app_id": self.app_id,
            "action": self.audit.as_str(),
            "status": status_wire(self.status),
        })
    }
}

async fn authorize_app_admin(
    tenant: &TenantContext,
    state: &AppState,
    event: &Event,
) -> Result<(), IngestError> {
    let restriction = state
        .db
        .moderation_restriction_state(tenant.community(), event.pubkey.as_bytes())
        .await
        .map_err(|err| {
            IngestError::Internal(format!("error: checking restriction state: {err}"))
        })?;
    if restriction.banned {
        return Err(IngestError::AuthFailed(
            "blocked: you are banned from this community".into(),
        ));
    }

    let sender_hex = event.pubkey.to_hex();
    let member = state
        .db
        .get_relay_member(tenant.community(), &sender_hex)
        .await
        .map_err(|err| IngestError::Internal(format!("error: looking up relay member: {err}")))?;
    match member.as_ref().map(|row| row.role.as_str()) {
        Some("owner") | Some("admin") => Ok(()),
        _ => Err(IngestError::Rejected(
            "forbidden: must be a community owner or admin".into(),
        )),
    }
}

async fn execute_claimed_command(
    tenant: &TenantContext,
    state: &AppState,
    event: &Event,
    command: AppAdminCommand,
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
) -> Result<CommandOutcome, IngestError> {
    let community = tenant.community();
    match command {
        AppAdminCommand::Create {
            name,
            description,
            icon_url,
        } => {
            let app_id = Uuid::new_v4();
            let (webhook_secret, secret_hash) = generate_app_secret()?;
            let created_by = event.pubkey.to_bytes();
            let app = create_app(
                tx,
                community,
                CreateAppParams {
                    id: app_id,
                    name: &name,
                    description: description.as_deref(),
                    icon_url: icon_url.as_deref(),
                    secret_hash: &secret_hash,
                    created_by: created_by.as_slice(),
                },
            )
            .await
            .map_err(map_db_error)?;
            let metadata = publish_app_metadata(tx, state, &app).await?;
            Ok(outcome(
                app,
                Some(webhook_secret),
                Some(metadata),
                AuditAction::AppCreated,
            ))
        }
        AppAdminCommand::Update {
            app_id,
            name,
            description,
            icon_url,
        } => {
            let app = update_app(
                tx,
                community,
                app_id,
                UpdateAppParams {
                    name: name.as_deref(),
                    description: description.as_deref(),
                    icon_url: icon_url.as_deref(),
                },
            )
            .await
            .map_err(map_db_error)?;
            let metadata = publish_app_metadata(tx, state, &app).await?;
            Ok(outcome(app, None, Some(metadata), AuditAction::AppUpdated))
        }
        AppAdminCommand::RotateSecret { app_id } => {
            require_existing_app(state, community, app_id).await?;
            let (webhook_secret, secret_hash) = generate_app_secret()?;
            let app = rotate_app_secret(tx, community, app_id, &secret_hash)
                .await
                .map_err(map_db_error)?;
            Ok(outcome(
                app,
                Some(webhook_secret),
                None,
                AuditAction::AppSecretRotated,
            ))
        }
        AppAdminCommand::Enable { app_id } => {
            let app = set_app_status(tx, community, app_id, AppStatus::Active)
                .await
                .map_err(map_db_error)?;
            let metadata = publish_app_metadata(tx, state, &app).await?;
            Ok(outcome(app, None, Some(metadata), AuditAction::AppEnabled))
        }
        AppAdminCommand::Disable { app_id } => {
            let app = set_app_status(tx, community, app_id, AppStatus::Disabled)
                .await
                .map_err(map_db_error)?;
            let metadata = publish_app_metadata(tx, state, &app).await?;
            Ok(outcome(app, None, Some(metadata), AuditAction::AppDisabled))
        }
    }
}

fn outcome(
    app: AppRecord,
    webhook_secret: Option<String>,
    metadata: Option<StoredEvent>,
    audit: AuditAction,
) -> CommandOutcome {
    CommandOutcome {
        app_id: app.id,
        webhook_secret,
        metadata,
        audit,
        status: app.status,
    }
}

async fn require_existing_app(
    state: &AppState,
    community: buzz_core::CommunityId,
    app_id: Uuid,
) -> Result<(), IngestError> {
    match state
        .db
        .get_app(community, app_id)
        .await
        .map_err(map_db_error)?
    {
        Some(_) => Ok(()),
        None => Err(IngestError::Rejected("invalid: app not found".into())),
    }
}

async fn publish_app_metadata(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    state: &AppState,
    app: &AppRecord,
) -> Result<StoredEvent, IngestError> {
    let relay_pubkey = state.relay_keypair.public_key().to_bytes();
    let created_at =
        next_app_metadata_created_at(tx, app.community_id, relay_pubkey.as_slice(), app.id)
            .await
            .map_err(map_db_error)?;
    let event = build_app_metadata_event(&state.relay_keypair, app, created_at)?;
    let (stored, inserted) = replace_app_metadata(tx, app.community_id, &event)
        .await
        .map_err(map_db_error)?;
    if !inserted {
        return Err(IngestError::Internal(
            "error: app metadata replacement was dominated".into(),
        ));
    }
    Ok(stored)
}

fn build_app_metadata_event(
    keys: &nostr::Keys,
    app: &AppRecord,
    created_at: i64,
) -> Result<Event, IngestError> {
    let created_at = u64::try_from(created_at).map_err(|_| {
        IngestError::Internal(format!("error: invalid metadata timestamp {created_at}"))
    })?;
    let app_id = app.id.to_string();
    let status = status_wire(app.status);
    let mut tags = vec![
        parse_tag(["d", app_id.as_str()])?,
        parse_tag(["name", app.name.as_str()])?,
        parse_tag(["status", status])?,
    ];
    if let Some(picture) = &app.icon_url {
        tags.push(parse_tag(["picture", picture.as_str()])?);
    }
    EventBuilder::new(
        Kind::Custom(KIND_APP_METADATA as u16),
        app.description.clone().unwrap_or_default(),
    )
    .tags(tags)
    .custom_created_at(Timestamp::from(created_at))
    .sign_with_keys(keys)
    .map_err(|err| IngestError::Internal(format!("error: signing app metadata: {err}")))
}

fn parse_tag(parts: [&str; 2]) -> Result<Tag, IngestError> {
    Tag::parse(parts).map_err(|err| IngestError::Internal(format!("error: building tag: {err}")))
}

fn generate_app_secret() -> Result<(String, [u8; 32]), IngestError> {
    let mut secret_bytes = [0u8; APP_SECRET_BYTES];
    OsRng
        .try_fill_bytes(&mut secret_bytes)
        .map_err(|err| IngestError::Internal(format!("error: generating app secret: {err}")))?;
    let encoded = URL_SAFE_NO_PAD.encode(secret_bytes);
    let digest: [u8; 32] = Sha256::digest(secret_bytes).into();
    Ok((encoded, digest))
}

fn status_wire(status: AppStatus) -> &'static str {
    match status {
        AppStatus::Active => "active",
        AppStatus::Disabled => "disabled",
    }
}

fn map_db_error(err: DbError) -> IngestError {
    match err {
        DbError::NotFound(_) => IngestError::Rejected("invalid: app not found".into()),
        other => IngestError::Internal(format!("error: {other}")),
    }
}

async fn enqueue_lifecycle_audit(
    tenant: &TenantContext,
    state: &AppState,
    event: &Event,
    outcome: &CommandOutcome,
) {
    let Some(audit_tx) = &state.audit_tx else {
        return;
    };
    let entry = NewAuditEntry {
        community_id: tenant.community(),
        action: outcome.audit.clone(),
        actor_pubkey: Some(event.pubkey.to_bytes().to_vec()),
        object_id: Some(outcome.app_id.to_string()),
        detail: outcome.audit_detail(),
    };
    if let Err(err) = audit_tx.send(entry).await {
        error!(app_id = %outcome.app_id, "Audit channel closed — app lifecycle entry lost: {err}");
        metrics::counter!("buzz_audit_send_errors_total").increment(1);
        warn!("app lifecycle audit enqueue failed");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use buzz_audit::AuditAction;
    use buzz_auth::Scope;
    use buzz_core::app::AppStatus;
    use buzz_core::kind::{KIND_APP_ADMIN_COMMAND, KIND_APP_METADATA};
    use buzz_core::tenant::TenantContext;
    use buzz_db::EventQuery;
    use nostr::{Event, EventBuilder, Keys, Kind};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;
    use uuid::Uuid;

    use crate::handlers::ingest::{
        ingest_event, HttpAuthMethod, IngestAuth, IngestError, IngestResult,
    };
    use crate::state::AppState;

    use super::CommandOutcome;

    const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz"; // sadscan:disable np.postgres.1

    async fn test_state() -> (Arc<AppState>, TenantContext) {
        let host = format!("app-admin-{}.example", Uuid::new_v4().simple());
        let mut config = crate::config::Config::from_env().expect("config from env");
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| TEST_DB_URL.to_string());
        config.database_url = database_url.clone();
        config.redis_url = "redis://127.0.0.1:1".to_string();
        config.relay_url = format!("wss://{host}");

        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("requires reachable Postgres");
        let db = buzz_db::Db::from_pool(pool.clone());
        db.migrate().await.expect("migrate");
        let record = db
            .ensure_configured_community(&host)
            .await
            .expect("ensure community");
        let tenant = TenantContext::resolved(record.id, host);

        let redis_pool = deadpool_redis::Config::from_url(&config.redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("redis pool config");
        let pubsub = Arc::new(
            buzz_pubsub::PubSubManager::new(&config.redis_url, redis_pool.clone())
                .await
                .expect("pubsub manager"),
        );
        let audit = buzz_audit::AuditService::new(pool.clone());
        let auth = buzz_auth::AuthService::new(config.auth.clone());
        let search = buzz_search::SearchService::new(pool.clone());
        let workflow_engine = Arc::new(buzz_workflow::WorkflowEngine::new(
            db.clone(),
            buzz_workflow::WorkflowConfig::default(),
        ));
        let media_storage = buzz_media::MediaStorage::new(&config.media).expect("media storage");
        let (state, _audit_shutdown) = AppState::new(
            config,
            db,
            redis_pool,
            audit,
            pubsub,
            auth,
            search,
            workflow_engine,
            Keys::generate(),
            media_storage,
        );
        (Arc::new(state), tenant)
    }

    async fn seed_role(state: &AppState, tenant: &TenantContext, keys: &Keys, role: &str) {
        state
            .db
            .add_relay_member(tenant.community(), &keys.public_key().to_hex(), role, None)
            .await
            .expect("seed relay member");
    }

    fn sign_command(keys: &Keys, content: Value) -> Event {
        EventBuilder::new(
            Kind::Custom(KIND_APP_ADMIN_COMMAND as u16),
            content.to_string(),
        )
        .sign_with_keys(keys)
        .expect("sign kind 9038")
    }

    fn admin_auth(keys: &Keys) -> IngestAuth {
        IngestAuth::Http {
            pubkey: keys.public_key(),
            scopes: vec![Scope::AdminUsers],
            auth_method: HttpAuthMethod::Nip98,
        }
    }

    async fn ingest(
        state: &Arc<AppState>,
        tenant: &TenantContext,
        keys: &Keys,
        event: Event,
    ) -> Result<IngestResult, IngestError> {
        ingest_event(state, tenant, event, admin_auth(keys)).await
    }

    async fn ingest_json(
        state: &Arc<AppState>,
        tenant: &TenantContext,
        keys: &Keys,
        content: Value,
    ) -> Result<IngestResult, IngestError> {
        ingest(state, tenant, keys, sign_command(keys, content)).await
    }

    fn response_json(result: &IngestResult) -> Value {
        assert!(
            result.accepted,
            "command was not accepted: {}",
            result.message
        );
        let payload = result
            .message
            .strip_prefix("response:")
            .unwrap_or_else(|| panic!("expected response: JSON, got {}", result.message));
        serde_json::from_str(payload).expect("parse response JSON")
    }

    fn decode_secret(secret: &str) -> Vec<u8> {
        assert!(
            !secret.contains('='),
            "webhook_secret must be base64url without padding"
        );
        let bytes = URL_SAFE_NO_PAD
            .decode(secret)
            .expect("webhook_secret must be base64url");
        assert_eq!(bytes.len(), 32, "secret must decode to 32 bytes");
        bytes
    }

    fn hash_secret(secret: &[u8]) -> [u8; 32] {
        Sha256::digest(secret).into()
    }

    #[test]
    fn app_lifecycle_audit_detail_contains_only_minimized_fields() {
        let app_id =
            Uuid::parse_str("6eb31227-8ed2-42ec-9024-863497cbeed2").expect("canonical app id");
        let outcome = CommandOutcome {
            app_id,
            webhook_secret: None,
            metadata: None,
            audit: AuditAction::AppCreated,
            status: AppStatus::Active,
        };

        assert_eq!(
            outcome.audit_detail(),
            json!({
                "app_id": app_id,
                "action": "app_created",
                "status": "active",
            })
        );
    }

    async fn live_metadata(
        state: &AppState,
        tenant: &TenantContext,
        app_id: Uuid,
    ) -> Option<Event> {
        let mut q = EventQuery::for_community(tenant.community());
        q.kinds = Some(vec![KIND_APP_METADATA as i32]);
        q.d_tag = Some(app_id.to_string());
        q.pubkey = Some(state.relay_keypair.public_key().to_bytes().to_vec());
        q.global_only = true;
        q.limit = Some(10);
        state
            .db
            .query_events(&q)
            .await
            .expect("query metadata")
            .into_iter()
            .next()
            .map(|stored| stored.event)
    }

    async fn create_app(
        state: &Arc<AppState>,
        tenant: &TenantContext,
        keys: &Keys,
        name: &str,
        description: Option<&str>,
        icon_url: Option<&str>,
    ) -> (Uuid, String) {
        let mut body = json!({ "action": "create", "name": name });
        if let Some(description) = description {
            body["description"] = json!(description);
        }
        if let Some(icon_url) = icon_url {
            body["icon_url"] = json!(icon_url);
        }
        let result = ingest_json(state, tenant, keys, body)
            .await
            .expect("create app");
        let resp = response_json(&result);
        let app_id = resp["app_id"]
            .as_str()
            .expect("app_id")
            .parse::<Uuid>()
            .expect("canonical app_id");
        let secret = resp["webhook_secret"]
            .as_str()
            .expect("webhook_secret")
            .to_owned();
        (app_id, secret)
    }

    #[tokio::test]
    async fn app_admin_rejects_non_admin() {
        let (state, tenant) = test_state().await;
        let member = Keys::generate();
        seed_role(&state, &tenant, &member, "member").await;

        let err = match ingest_json(
            &state,
            &tenant,
            &member,
            json!({ "action": "create", "name": "Gigo" }),
        )
        .await
        {
            Err(error) => error,
            Ok(result) => panic!(
                "member must not manage Apps, got accepted={} message={}",
                result.accepted, result.message
            ),
        };
        match err {
            IngestError::Rejected(message) | IngestError::AuthFailed(message) => {
                assert!(
                    message.contains("forbidden")
                        || message.contains("not authorized")
                        || message.contains("owner or admin"),
                    "unexpected rejection: {message}"
                );
            }
            IngestError::CanvasConflict(message) => {
                panic!("canvas conflict: {message}")
            }
            IngestError::Internal(message) => panic!("internal error: {message}"),
        }
        let apps = state
            .db
            .list_apps(tenant.community())
            .await
            .expect("list apps");
        assert!(apps.is_empty(), "non-admin must not create an App row");
    }

    #[tokio::test]
    async fn app_admin_create_returns_secret_once() {
        let (state, tenant) = test_state().await;
        let owner = Keys::generate();
        seed_role(&state, &tenant, &owner, "owner").await;

        let event = sign_command(
            &owner,
            json!({
                "action": "create",
                "name": "Gigo",
                "description": "CI notifications",
                "icon_url": "https://example.com/icon.png",
            }),
        );
        let first = ingest(&state, &tenant, &owner, event.clone())
            .await
            .expect("create");
        let resp = response_json(&first);
        let app_id = resp["app_id"]
            .as_str()
            .expect("app_id")
            .parse::<Uuid>()
            .expect("canonical app_id");
        let secret = resp["webhook_secret"].as_str().expect("webhook_secret");
        let secret_bytes = decode_secret(secret);
        let expected_hash = hash_secret(&secret_bytes);

        let app = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        assert_eq!(app.name, "Gigo");
        assert_eq!(app.status, AppStatus::Active);
        assert!(
            bool::from(app.secret_hash.as_slice().ct_eq(&expected_hash)),
            "stored digest must be SHA-256 of the 32 raw secret bytes"
        );
        assert!(!app.secret_hash.iter().all(|b| *b == 0));

        let replay = ingest(&state, &tenant, &owner, event)
            .await
            .expect("duplicate create");
        assert!(replay.accepted);
        assert!(
            !replay.message.contains("webhook_secret"),
            "duplicate create must not return a secret: {}",
            replay.message
        );

        let after = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        assert!(bool::from(
            after.secret_hash.as_slice().ct_eq(&expected_hash)
        ));
        assert_eq!(
            state
                .db
                .list_apps(tenant.community())
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn app_admin_duplicate_does_not_rotate() {
        let (state, tenant) = test_state().await;
        let owner = Keys::generate();
        seed_role(&state, &tenant, &owner, "owner").await;
        let (app_id, secret) = create_app(&state, &tenant, &owner, "Gigo", None, None).await;
        let original_hash = hash_secret(&decode_secret(&secret));

        let rotate = sign_command(
            &owner,
            json!({ "action": "rotate_secret", "app_id": app_id.to_string() }),
        );
        let first = ingest(&state, &tenant, &owner, rotate.clone())
            .await
            .expect("rotate");
        let first_secret = response_json(&first)["webhook_secret"]
            .as_str()
            .expect("rotate secret")
            .to_owned();
        assert_ne!(first_secret, secret, "rotate must issue a new secret");

        let replay = ingest(&state, &tenant, &owner, rotate)
            .await
            .expect("duplicate rotate");
        assert!(replay.accepted);
        assert!(
            !replay.message.contains("webhook_secret"),
            "duplicate rotate must not return a secret: {}",
            replay.message
        );

        let app = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        let rotated_hash = hash_secret(&decode_secret(&first_secret));
        assert!(bool::from(app.secret_hash.as_slice().ct_eq(&rotated_hash)));
        assert!(!bool::from(
            app.secret_hash.as_slice().ct_eq(&original_hash)
        ));
    }

    #[tokio::test]
    async fn app_admin_update_clears_optional_fields() {
        let (state, tenant) = test_state().await;
        let admin = Keys::generate();
        seed_role(&state, &tenant, &admin, "admin").await;
        let (app_id, _) = create_app(
            &state,
            &tenant,
            &admin,
            "Gigo",
            Some("CI notifications"),
            Some("https://example.com/icon.png"),
        )
        .await;

        let result = ingest_json(
            &state,
            &tenant,
            &admin,
            json!({
                "action": "update",
                "app_id": app_id.to_string(),
                "description": "",
                "icon_url": "",
            }),
        )
        .await
        .expect("update");
        let resp = response_json(&result);
        assert_eq!(resp["app_id"].as_str(), Some(app_id.to_string().as_str()));
        assert!(resp.get("webhook_secret").is_none());

        let app = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        assert_eq!(app.name, "Gigo");
        assert_eq!(app.description, None);
        assert_eq!(app.icon_url, None);

        let metadata = live_metadata(&state, &tenant, app_id)
            .await
            .expect("metadata after clear");
        assert_eq!(metadata.content, "");
        let has_picture = metadata
            .tags
            .iter()
            .any(|tag| tag.as_slice().first().map(String::as_str) == Some("picture"));
        assert!(!has_picture, "cleared icon must drop the picture tag");
    }

    #[tokio::test]
    async fn app_admin_rotate_replaces_hash_without_metadata() {
        let (state, tenant) = test_state().await;
        let owner = Keys::generate();
        seed_role(&state, &tenant, &owner, "owner").await;
        let (app_id, secret) = create_app(
            &state,
            &tenant,
            &owner,
            "Gigo",
            Some("keep me"),
            Some("https://example.com/icon.png"),
        )
        .await;
        let before = live_metadata(&state, &tenant, app_id)
            .await
            .expect("metadata after create");
        let before_id = before.id;
        let before_created_at = before.created_at;

        let result = ingest_json(
            &state,
            &tenant,
            &owner,
            json!({ "action": "rotate_secret", "app_id": app_id.to_string() }),
        )
        .await
        .expect("rotate");
        let new_secret = response_json(&result)["webhook_secret"]
            .as_str()
            .expect("new secret")
            .to_owned();
        assert_ne!(new_secret, secret);

        let app = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        assert!(bool::from(
            app.secret_hash
                .as_slice()
                .ct_eq(&hash_secret(&decode_secret(&new_secret)))
        ));

        let after = live_metadata(&state, &tenant, app_id)
            .await
            .expect("metadata after rotate");
        assert_eq!(after.id, before_id, "rotate must not replace metadata");
        assert_eq!(after.created_at, before_created_at);
        assert_eq!(after.content, "keep me");
    }

    #[tokio::test]
    async fn app_admin_disable_keeps_metadata() {
        let (state, tenant) = test_state().await;
        let owner = Keys::generate();
        seed_role(&state, &tenant, &owner, "owner").await;
        let (app_id, _) =
            create_app(&state, &tenant, &owner, "Gigo", Some("still public"), None).await;

        let result = ingest_json(
            &state,
            &tenant,
            &owner,
            json!({ "action": "disable", "app_id": app_id.to_string() }),
        )
        .await
        .expect("disable");
        assert!(response_json(&result).get("webhook_secret").is_none());

        let app = state
            .db
            .get_app(tenant.community(), app_id)
            .await
            .expect("get app")
            .expect("app exists");
        assert_eq!(app.status, AppStatus::Disabled);
        assert_eq!(app.description.as_deref(), Some("still public"));

        let metadata = live_metadata(&state, &tenant, app_id)
            .await
            .expect("disabled metadata remains queryable");
        assert_eq!(metadata.content, "still public");
        let status = metadata.tags.iter().find_map(|tag| {
            let parts = tag.as_slice();
            (parts.first().map(String::as_str) == Some("status"))
                .then(|| parts.get(1).map(String::as_str))
                .flatten()
        });
        assert_eq!(status, Some("disabled"));
    }

    #[tokio::test]
    async fn app_metadata_contains_no_authority_or_secret_tags() {
        let (state, tenant) = test_state().await;
        let owner = Keys::generate();
        seed_role(&state, &tenant, &owner, "owner").await;
        let (app_id, secret) = create_app(
            &state,
            &tenant,
            &owner,
            "Gigo",
            Some("CI notifications"),
            Some("https://example.com/icon.png"),
        )
        .await;

        let metadata = live_metadata(&state, &tenant, app_id)
            .await
            .expect("metadata");
        assert_eq!(
            metadata.pubkey,
            state.relay_keypair.public_key(),
            "kind 39008 must be relay-signed"
        );
        assert_eq!(metadata.kind.as_u16() as u32, KIND_APP_METADATA);
        assert_eq!(metadata.content, "CI notifications");
        assert!(!metadata.content.contains(&secret));

        let mut saw_d = false;
        let mut saw_name = false;
        let mut saw_status = false;
        let mut saw_picture = false;
        for tag in metadata.tags.iter() {
            let parts = tag.as_slice();
            let name = parts.first().map(String::as_str).unwrap_or("");
            assert!(
                !matches!(
                    name,
                    "secret"
                        | "webhook_secret"
                        | "secret_hash"
                        | "created_by"
                        | "role"
                        | "authority"
                        | "authorization"
                        | "p"
                ),
                "metadata must not carry authority or secret tag {name}"
            );
            match name {
                "d" => {
                    saw_d = true;
                    assert_eq!(
                        parts.get(1).map(String::as_str),
                        Some(app_id.to_string().as_str())
                    );
                }
                "name" => {
                    saw_name = true;
                    assert_eq!(parts.get(1).map(String::as_str), Some("Gigo"));
                }
                "status" => {
                    saw_status = true;
                    assert_eq!(parts.get(1).map(String::as_str), Some("active"));
                }
                "picture" => {
                    saw_picture = true;
                    assert_eq!(
                        parts.get(1).map(String::as_str),
                        Some("https://example.com/icon.png")
                    );
                }
                _ => {}
            }
        }
        assert!(saw_d && saw_name && saw_status && saw_picture);
    }
}
