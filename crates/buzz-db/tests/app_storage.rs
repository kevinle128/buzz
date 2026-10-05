//! App lifecycle storage: constraints, tenant isolation, command claim, and
//! kind `39008` replacement.

use buzz_core::app::AppStatus;
use buzz_core::kind::{KIND_APP_ADMIN_COMMAND, KIND_APP_METADATA};
use buzz_core::CommunityId;
use buzz_db::app::{
    claim_app_command_event, create_app, next_app_metadata_created_at, replace_app_metadata,
    rotate_app_secret, set_app_status, update_app, CreateAppParams, UpdateAppParams,
};
use buzz_db::{Db, DbError};
use chrono::{DateTime, Utc};
use nostr::{Event, EventBuilder, Keys, Kind, Tag, Timestamp};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz";
const RAW_SECRET: [u8; 32] = [0x11; 32];
const ROTATED_SECRET: [u8; 32] = [0x22; 32];

fn secret_hash(secret: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

fn database_url() -> String {
    std::env::var("BUZZ_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| TEST_DB_URL.to_owned())
}

async fn setup() -> (Db, PgPool) {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url())
        .await
        .expect("connect to test DB");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("apply migrations");
    (db, pool)
}

async fn make_community(pool: &PgPool) -> CommunityId {
    let id = Uuid::new_v4();
    let host = format!("app-storage-{}.example", id.simple());
    sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
        .bind(id)
        .bind(host)
        .execute(pool)
        .await
        .expect("insert community");
    CommunityId::from_uuid(id)
}

fn command_event(keys: &Keys, content: &str) -> Event {
    EventBuilder::new(
        Kind::Custom(KIND_APP_ADMIN_COMMAND as u16),
        content.to_owned(),
    )
    .sign_with_keys(keys)
    .expect("sign command")
}

fn metadata_event(
    keys: &Keys,
    app_id: Uuid,
    name: &str,
    status: &str,
    description: &str,
    picture: Option<&str>,
    created_at: u64,
) -> Event {
    let mut tags = vec![
        Tag::parse(["d", &app_id.to_string()]).expect("d tag"),
        Tag::parse(["name", name]).expect("name tag"),
        Tag::parse(["status", status]).expect("status tag"),
    ];
    if let Some(picture) = picture {
        tags.push(Tag::parse(["picture", picture]).expect("picture tag"));
    }
    EventBuilder::new(
        Kind::Custom(KIND_APP_METADATA as u16),
        description.to_owned(),
    )
    .tags(tags)
    .custom_created_at(Timestamp::from(created_at))
    .sign_with_keys(keys)
    .expect("sign metadata")
}

fn pubkey_bytes(keys: &Keys) -> [u8; 32] {
    keys.public_key().to_bytes()
}

fn create_params<'a>(
    id: Uuid,
    name: &'a str,
    description: Option<&'a str>,
    icon_url: Option<&'a str>,
    secret_hash: &'a [u8],
    created_by: &'a [u8],
) -> CreateAppParams<'a> {
    CreateAppParams {
        id,
        name,
        description,
        icon_url,
        secret_hash,
        created_by,
    }
}

async fn live_metadata_count(pool: &PgPool, community: CommunityId, app_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM events \
         WHERE community_id = $1 AND kind = $2 AND d_tag = $3 AND deleted_at IS NULL",
    )
    .bind(community.as_uuid())
    .bind(KIND_APP_METADATA as i32)
    .bind(app_id.to_string())
    .fetch_one(pool)
    .await
    .expect("count live metadata")
}

async fn command_count(pool: &PgPool, community: CommunityId, event_id: &[u8]) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE community_id = $1 AND id = $2 AND kind = $3",
    )
    .bind(community.as_uuid())
    .bind(event_id)
    .bind(KIND_APP_ADMIN_COMMAND as i32)
    .fetch_one(pool)
    .await
    .expect("count command")
}

async fn live_metadata_content(
    pool: &PgPool,
    community: CommunityId,
    app_id: Uuid,
) -> Option<(DateTime<Utc>, Vec<u8>, String)> {
    sqlx::query_as(
        "SELECT created_at, id, content FROM events \
         WHERE community_id = $1 AND kind = $2 AND d_tag = $3 AND deleted_at IS NULL \
         ORDER BY created_at DESC, id ASC LIMIT 1",
    )
    .bind(community.as_uuid())
    .bind(KIND_APP_METADATA as i32)
    .bind(app_id.to_string())
    .fetch_optional(pool)
    .await
    .expect("read live metadata")
}

#[tokio::test]
async fn apps_table_enforces_bounded_text_status_hash_and_created_by() {
    let (_db, pool) = setup().await;
    let community = make_community(&pool).await;
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let created_by = [0xabu8; 32];

    let too_long_name = "n".repeat(129);
    let err = sqlx::query(
        "INSERT INTO apps (community_id, id, name, status, secret_hash, created_by) \
         VALUES ($1, $2, $3, 'active', $4, $5)",
    )
    .bind(community.as_uuid())
    .bind(app_id)
    .bind(&too_long_name)
    .bind(hash.as_slice())
    .bind(created_by.as_slice())
    .execute(&pool)
    .await
    .expect_err("name longer than 128 scalars must fail");
    assert!(
        err.to_string().to_lowercase().contains("check") || err.to_string().contains("apps"),
        "unexpected name-length error: {err:?}"
    );

    let err = sqlx::query(
        "INSERT INTO apps (community_id, id, name, status, secret_hash, created_by) \
         VALUES ($1, $2, 'ok', 'archived', $3, $4)",
    )
    .bind(community.as_uuid())
    .bind(Uuid::new_v4())
    .bind(hash.as_slice())
    .bind(created_by.as_slice())
    .execute(&pool)
    .await
    .expect_err("unknown status must fail");
    assert!(err.to_string().to_lowercase().contains("check") || err.to_string().contains("status"));

    let err = sqlx::query(
        "INSERT INTO apps (community_id, id, name, status, secret_hash, created_by) \
         VALUES ($1, $2, 'ok', 'active', $3, $4)",
    )
    .bind(community.as_uuid())
    .bind(Uuid::new_v4())
    .bind([0u8; 16].as_slice())
    .bind(created_by.as_slice())
    .execute(&pool)
    .await
    .expect_err("short secret_hash must fail");
    assert!(err.to_string().contains("check") || err.to_string().contains("secret"));

    let err = sqlx::query(
        "INSERT INTO apps (community_id, id, name, status, secret_hash, created_by) \
         VALUES ($1, $2, 'ok', 'active', $3, $4)",
    )
    .bind(community.as_uuid())
    .bind(Uuid::new_v4())
    .bind(hash.as_slice())
    .bind([0u8; 16].as_slice())
    .execute(&pool)
    .await
    .expect_err("short created_by must fail");
    assert!(err.to_string().contains("check") || err.to_string().contains("created_by"));
}

#[tokio::test]
async fn delivery_ledger_enforces_outcome_shape_and_immutability() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let created_by = pubkey_bytes(&admin);

    let mut tx = db.begin_transaction().await.expect("begin");
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Archon",
            None,
            None,
            hash.as_slice(),
            created_by.as_slice(),
        ),
    )
    .await
    .expect("create app for delivery constraints");
    tx.commit().await.expect("commit app");

    let delivered_id = Uuid::new_v4();
    let key_hash = secret_hash(&[0x33; 32]);
    let payload_hash = secret_hash(&[0x44; 32]);
    let event_id = [0x55u8; 32];

    sqlx::query(
        "INSERT INTO app_callback_deliveries \
         (community_id, id, app_id, idempotency_key_hash, payload_hash, event_type, \
          route_snapshot, status, event_id, failure_code, created_at, completed_at) \
         VALUES ($1, $2, $3, $4, $5, 'workflow.run.completed', $6, 'delivered', $7, NULL, NOW(), NOW())",
    )
    .bind(community.as_uuid())
    .bind(delivered_id)
    .bind(app_id)
    .bind(key_hash.as_slice())
    .bind(payload_hash.as_slice())
    .bind(serde_json::json!({"channel_id": Uuid::new_v4()}))
    .bind(event_id.as_slice())
    .execute(&pool)
    .await
    .expect("valid delivered row");

    let err = sqlx::query(
        "INSERT INTO app_callback_deliveries \
         (community_id, id, app_id, idempotency_key_hash, payload_hash, event_type, \
          route_snapshot, status, event_id, failure_code, created_at, completed_at) \
         VALUES ($1, $2, $3, $4, $5, 'workflow.run.completed', $6, 'delivered', NULL, NULL, NOW(), NOW())",
    )
    .bind(community.as_uuid())
    .bind(Uuid::new_v4())
    .bind(app_id)
    .bind(secret_hash(&[0x61; 32]).as_slice())
    .bind(payload_hash.as_slice())
    .bind(serde_json::json!({"channel_id": Uuid::new_v4()}))
    .execute(&pool)
    .await
    .expect_err("delivered without event_id must fail");
    assert!(err.to_string().contains("check") || err.to_string().contains("delivered"));

    let err = sqlx::query(
        "INSERT INTO app_callback_deliveries \
         (community_id, id, app_id, idempotency_key_hash, payload_hash, event_type, \
          route_snapshot, status, event_id, failure_code, created_at, completed_at) \
         VALUES ($1, $2, $3, $4, $5, 'workflow.run.completed', NULL, 'rejected', $6, NULL, NOW(), NOW())",
    )
    .bind(community.as_uuid())
    .bind(Uuid::new_v4())
    .bind(app_id)
    .bind(secret_hash(&[0x62; 32]).as_slice())
    .bind(payload_hash.as_slice())
    .bind(event_id.as_slice())
    .execute(&pool)
    .await
    .expect_err("rejected with event_id / without failure_code must fail");
    assert!(err.to_string().contains("check") || err.to_string().contains("rejected"));

    let err = sqlx::query(
        "UPDATE app_callback_deliveries SET status = 'rejected', failure_code = 'conflict', event_id = NULL \
         WHERE community_id = $1 AND id = $2",
    )
    .bind(community.as_uuid())
    .bind(delivered_id)
    .execute(&pool)
    .await
    .expect_err("delivery identity and outcome are immutable");
    assert!(
        err.to_string().contains("immutable")
            || err.to_string().contains("check")
            || err.to_string().contains("app_callback")
    );
}

#[tokio::test]
async fn tenant_isolation_keeps_same_app_uuid_independent() {
    let (db, pool) = setup().await;
    let a = make_community(&pool).await;
    let b = make_community(&pool).await;
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let creator_a = [0xa1u8; 32];
    let creator_b = [0xb2u8; 32];

    for (community, created_by, name) in [(a, creator_a, "Alpha"), (b, creator_b, "Beta")] {
        let mut tx = db.begin_transaction().await.expect("begin");
        create_app(
            &mut tx,
            community,
            create_params(
                app_id,
                name,
                None,
                None,
                hash.as_slice(),
                created_by.as_slice(),
            ),
        )
        .await
        .expect("create isolated app");
        tx.commit().await.expect("commit");
    }

    let loaded_a = db.get_app(a, app_id).await.expect("load a").expect("app a");
    let loaded_b = db.get_app(b, app_id).await.expect("load b").expect("app b");
    assert_eq!(loaded_a.name, "Alpha");
    assert_eq!(loaded_b.name, "Beta");
    assert_eq!(loaded_a.created_by, creator_a);
    assert_eq!(loaded_b.created_by, creator_b);
    assert!(db
        .list_apps(a)
        .await
        .expect("list a")
        .iter()
        .all(|row| row.id == app_id && row.name == "Alpha"));
    assert!(db
        .list_apps(b)
        .await
        .expect("list b")
        .iter()
        .all(|row| row.id == app_id && row.name == "Beta"));
}

#[tokio::test]
async fn create_update_clear_enable_disable_and_created_by_are_stable() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let relay = Keys::generate();
    let command = command_event(
        &admin,
        r#"{"action":"create","name":"Buildkite","description":"Build notifications","icon_url":"https://example.test/icon.png"}"#,
    );
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let created_by = pubkey_bytes(&admin);

    let mut tx = db.begin_transaction().await.expect("begin create");
    assert!(
        claim_app_command_event(&mut tx, community, &command)
            .await
            .expect("claim create"),
        "first claim must insert the command event"
    );
    let created = create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Buildkite",
            Some("Build notifications"),
            Some("https://example.test/icon.png"),
            hash.as_slice(),
            created_by.as_slice(),
        ),
    )
    .await
    .expect("create app");
    let created_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("create timestamp") as u64;
    let created_meta = metadata_event(
        &relay,
        app_id,
        "Buildkite",
        "active",
        "Build notifications",
        Some("https://example.test/icon.png"),
        created_at,
    );
    assert!(
        replace_app_metadata(&mut tx, community, &created_meta)
            .await
            .expect("replace create metadata")
            .1
    );
    tx.commit().await.expect("commit create");

    assert_eq!(created.name, "Buildkite");
    assert_eq!(created.description.as_deref(), Some("Build notifications"));
    assert_eq!(
        created.icon_url.as_deref(),
        Some("https://example.test/icon.png")
    );
    assert_eq!(created.status, AppStatus::Active);
    assert_eq!(created.created_by, created_by);
    assert_eq!(created.secret_hash, hash);

    let update_cmd = command_event(
        &admin,
        &format!(
            r#"{{"action":"update","app_id":"{app_id}","name":"CI","description":"","icon_url":""}}"#
        ),
    );
    let mut tx = db.begin_transaction().await.expect("begin update");
    assert!(claim_app_command_event(&mut tx, community, &update_cmd)
        .await
        .expect("claim update"));
    let updated = update_app(
        &mut tx,
        community,
        app_id,
        UpdateAppParams {
            name: Some("CI"),
            description: Some(""),
            icon_url: Some(""),
        },
    )
    .await
    .expect("update and clear");
    let update_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("update timestamp") as u64;
    let updated_meta = metadata_event(&relay, app_id, "CI", "active", "", None, update_at);
    assert!(
        replace_app_metadata(&mut tx, community, &updated_meta)
            .await
            .expect("replace update metadata")
            .1
    );
    tx.commit().await.expect("commit update");

    assert_eq!(updated.name, "CI");
    assert_eq!(updated.description, None);
    assert_eq!(updated.icon_url, None);
    assert_eq!(updated.created_by, created_by);
    assert!(updated.updated_at >= created.updated_at);

    let disable_cmd = command_event(
        &admin,
        &format!(r#"{{"action":"disable","app_id":"{app_id}"}}"#),
    );
    let mut tx = db.begin_transaction().await.expect("begin disable");
    assert!(claim_app_command_event(&mut tx, community, &disable_cmd)
        .await
        .expect("claim disable"));
    let disabled = set_app_status(&mut tx, community, app_id, AppStatus::Disabled)
        .await
        .expect("disable");
    let disable_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("disable timestamp") as u64;
    let disabled_meta = metadata_event(&relay, app_id, "CI", "disabled", "", None, disable_at);
    assert!(
        replace_app_metadata(&mut tx, community, &disabled_meta)
            .await
            .expect("replace disable metadata")
            .1
    );
    tx.commit().await.expect("commit disable");
    assert_eq!(disabled.status, AppStatus::Disabled);
    assert_eq!(disabled.created_by, created_by);

    let enable_cmd = command_event(
        &admin,
        &format!(r#"{{"action":"enable","app_id":"{app_id}"}}"#),
    );
    let mut tx = db.begin_transaction().await.expect("begin enable");
    assert!(claim_app_command_event(&mut tx, community, &enable_cmd)
        .await
        .expect("claim enable"));
    let enabled = set_app_status(&mut tx, community, app_id, AppStatus::Active)
        .await
        .expect("enable");
    let enable_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("enable timestamp") as u64;
    let enabled_meta = metadata_event(&relay, app_id, "CI", "active", "", None, enable_at);
    assert!(
        replace_app_metadata(&mut tx, community, &enabled_meta)
            .await
            .expect("replace enable metadata")
            .1
    );
    tx.commit().await.expect("commit enable");
    assert_eq!(enabled.status, AppStatus::Active);
    assert_eq!(enabled.created_by, created_by);
    assert_eq!(live_metadata_count(&pool, community, app_id).await, 1);
}

#[tokio::test]
async fn rotate_updates_hash_and_private_timestamp_without_metadata_replacement() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let relay = Keys::generate();
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let rotated = secret_hash(&ROTATED_SECRET);
    let created_by = pubkey_bytes(&admin);
    let create_cmd = command_event(&admin, r#"{"action":"create","name":"RotateMe"}"#);

    let mut tx = db.begin_transaction().await.expect("begin create");
    assert!(claim_app_command_event(&mut tx, community, &create_cmd)
        .await
        .expect("claim create"));
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "RotateMe",
            None,
            None,
            hash.as_slice(),
            created_by.as_slice(),
        ),
    )
    .await
    .expect("create");
    let created_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("ts") as u64;
    let meta = metadata_event(&relay, app_id, "RotateMe", "active", "", None, created_at);
    assert!(
        replace_app_metadata(&mut tx, community, &meta)
            .await
            .expect("metadata")
            .1
    );
    tx.commit().await.expect("commit create");
    let before = db
        .get_app(community, app_id)
        .await
        .expect("load")
        .expect("app");
    let head_before = live_metadata_content(&pool, community, app_id)
        .await
        .expect("head before rotate");

    let rotate_cmd = command_event(
        &admin,
        &format!(r#"{{"action":"rotate_secret","app_id":"{app_id}"}}"#),
    );
    let mut tx = db.begin_transaction().await.expect("begin rotate");
    assert!(claim_app_command_event(&mut tx, community, &rotate_cmd)
        .await
        .expect("claim rotate"));
    let after = rotate_app_secret(&mut tx, community, app_id, rotated.as_slice())
        .await
        .expect("rotate");
    tx.commit().await.expect("commit rotate");

    assert_eq!(after.secret_hash, rotated);
    assert_ne!(after.secret_hash, hash);
    assert!(after.updated_at >= before.updated_at);
    assert_eq!(after.created_by, created_by);
    assert_eq!(after.name, "RotateMe");
    let head_after = live_metadata_content(&pool, community, app_id)
        .await
        .expect("head after rotate");
    assert_eq!(
        head_after.1, head_before.1,
        "rotate must not replace kind 39008"
    );
    assert_eq!(live_metadata_count(&pool, community, app_id).await, 1);
}

#[tokio::test]
async fn secret_hash_only_storage_never_persists_raw_secret() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let created_by = [0xcdu8; 32];
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);

    let mut tx = db.begin_transaction().await.expect("begin");
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Secret",
            None,
            None,
            hash.as_slice(),
            created_by.as_slice(),
        ),
    )
    .await
    .expect("create");
    tx.commit().await.expect("commit");

    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM apps WHERE community_id = $1 AND id = $2")
            .bind(community.as_uuid())
            .bind(app_id)
            .fetch_one(&pool)
            .await
            .expect("read hash");
    assert_eq!(stored.as_slice(), hash.as_slice());
    assert_ne!(stored.as_slice(), RAW_SECRET.as_slice());

    let stored_hash: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM apps WHERE community_id = $1 AND id = $2")
            .bind(community.as_uuid())
            .bind(app_id)
            .fetch_one(&pool)
            .await
            .expect("re-read hash");
    assert_ne!(stored_hash.as_slice(), RAW_SECRET.as_slice());
}

#[tokio::test]
async fn missing_app_mutations_return_not_found() {
    let (db, _pool) = setup().await;
    let community = make_community(&_pool).await;
    let missing = Uuid::new_v4();
    assert!(db
        .get_app(community, missing)
        .await
        .expect("get missing")
        .is_none());

    let mut tx = db.begin_transaction().await.expect("begin");
    let err = update_app(
        &mut tx,
        community,
        missing,
        UpdateAppParams {
            name: Some("nope"),
            description: None,
            icon_url: None,
        },
    )
    .await
    .expect_err("update missing");
    assert!(matches!(err, DbError::NotFound(_)), "{err:?}");
    let err = rotate_app_secret(&mut tx, community, missing, &secret_hash(&RAW_SECRET))
        .await
        .expect_err("rotate missing");
    assert!(matches!(err, DbError::NotFound(_)), "{err:?}");
    let err = set_app_status(&mut tx, community, missing, AppStatus::Disabled)
        .await
        .expect_err("status missing");
    assert!(matches!(err, DbError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn duplicate_command_claim_is_a_no_op() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let command = command_event(&admin, r#"{"action":"create","name":"Once"}"#);
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);

    let mut tx = db.begin_transaction().await.expect("begin first");
    assert!(claim_app_command_event(&mut tx, community, &command)
        .await
        .expect("first claim"));
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Once",
            None,
            None,
            hash.as_slice(),
            pubkey_bytes(&admin).as_slice(),
        ),
    )
    .await
    .expect("create");
    tx.commit().await.expect("commit first");

    let mut tx = db.begin_transaction().await.expect("begin duplicate");
    assert!(
        !claim_app_command_event(&mut tx, community, &command)
            .await
            .expect("duplicate claim"),
        "duplicate command must not be claimed again"
    );
    tx.commit().await.expect("commit duplicate");
    assert_eq!(
        command_count(&pool, community, command.id.as_bytes().as_slice()).await,
        1
    );
    let loaded = db
        .get_app(community, app_id)
        .await
        .expect("load")
        .expect("app");
    assert_eq!(loaded.secret_hash, hash);
}

#[tokio::test]
async fn metadata_changing_transaction_rolls_back_command_app_and_head() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let relay = Keys::generate();
    let command = command_event(&admin, r#"{"action":"create","name":"Rollback"}"#);
    let app_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);

    let mut tx = db.begin_transaction().await.expect("begin");
    assert!(claim_app_command_event(&mut tx, community, &command)
        .await
        .expect("claim"));
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Rollback",
            None,
            None,
            hash.as_slice(),
            pubkey_bytes(&admin).as_slice(),
        ),
    )
    .await
    .expect("create");
    let created_at =
        next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), app_id)
            .await
            .expect("ts") as u64;
    let meta = metadata_event(&relay, app_id, "Rollback", "active", "", None, created_at);
    assert!(
        replace_app_metadata(&mut tx, community, &meta)
            .await
            .expect("metadata")
            .1
    );
    tx.rollback().await.expect("rollback");

    assert!(db.get_app(community, app_id).await.expect("get").is_none());
    assert_eq!(
        command_count(&pool, community, command.id.as_bytes().as_slice()).await,
        0
    );
    assert_eq!(live_metadata_count(&pool, community, app_id).await, 0);
}

#[tokio::test]
async fn independent_app_metadata_heads_and_same_second_updates_are_monotonic() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let admin = Keys::generate();
    let relay = Keys::generate();
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    let hash = secret_hash(&RAW_SECRET);
    let created_by = pubkey_bytes(&admin);

    let mut tx = db.begin_transaction().await.expect("begin");
    for (id, name) in [(first_id, "One"), (second_id, "Two")] {
        create_app(
            &mut tx,
            community,
            create_params(id, name, None, None, hash.as_slice(), created_by.as_slice()),
        )
        .await
        .expect("create app");
        let ts =
            next_app_metadata_created_at(&mut tx, community, pubkey_bytes(&relay).as_slice(), id)
                .await
                .expect("ts") as u64;
        let meta = metadata_event(&relay, id, name, "active", name, None, ts);
        assert!(
            replace_app_metadata(&mut tx, community, &meta)
                .await
                .expect("metadata")
                .1
        );
    }
    tx.commit().await.expect("commit two apps");
    assert_eq!(live_metadata_count(&pool, community, first_id).await, 1);
    assert_eq!(live_metadata_count(&pool, community, second_id).await, 1);

    let mut tx = db.begin_transaction().await.expect("begin rapid updates");
    let first_ts = next_app_metadata_created_at(
        &mut tx,
        community,
        pubkey_bytes(&relay).as_slice(),
        first_id,
    )
    .await
    .expect("first rapid ts");
    let first_meta = metadata_event(
        &relay,
        first_id,
        "One",
        "active",
        "first-update",
        None,
        first_ts as u64,
    );
    assert!(
        replace_app_metadata(&mut tx, community, &first_meta)
            .await
            .expect("first rapid")
            .1
    );
    let second_ts = next_app_metadata_created_at(
        &mut tx,
        community,
        pubkey_bytes(&relay).as_slice(),
        first_id,
    )
    .await
    .expect("second rapid ts");
    assert!(
        second_ts > first_ts,
        "same-second updates must be monotonic: {first_ts} then {second_ts}"
    );
    let second_meta = metadata_event(
        &relay,
        first_id,
        "One",
        "active",
        "second-update",
        None,
        second_ts as u64,
    );
    assert!(
        replace_app_metadata(&mut tx, community, &second_meta)
            .await
            .expect("second rapid")
            .1
    );
    tx.commit().await.expect("commit rapid");

    let head = live_metadata_content(&pool, community, first_id)
        .await
        .expect("live head");
    assert_eq!(head.2, "second-update");
    assert_eq!(live_metadata_count(&pool, community, first_id).await, 1);
    assert_eq!(live_metadata_count(&pool, community, second_id).await, 1);
}

#[tokio::test]
async fn same_second_nip33_tie_keeps_lexicographically_lower_event_id() {
    let (db, pool) = setup().await;
    let community = make_community(&pool).await;
    let relay = Keys::generate();
    let app_id = Uuid::new_v4();
    let created_by = [0xeeu8; 32];
    let hash = secret_hash(&RAW_SECRET);
    let created_at = Utc::now().timestamp().max(1) as u64;

    let mut tx = db.begin_transaction().await.expect("begin");
    create_app(
        &mut tx,
        community,
        create_params(
            app_id,
            "Tie",
            None,
            None,
            hash.as_slice(),
            created_by.as_slice(),
        ),
    )
    .await
    .expect("create");
    let mut events = vec![
        metadata_event(
            &relay,
            app_id,
            "Tie",
            "active",
            "candidate-a",
            None,
            created_at,
        ),
        metadata_event(
            &relay,
            app_id,
            "Tie",
            "active",
            "candidate-b",
            None,
            created_at,
        ),
    ];
    events.sort_by(|left, right| left.id.as_bytes().cmp(right.id.as_bytes()));
    events.dedup_by(|left, right| left.id == right.id);
    assert_eq!(
        events.len(),
        2,
        "same-second metadata events must have distinct ids"
    );
    let lower = &events[0];
    let higher = &events[1];
    assert!(
        replace_app_metadata(&mut tx, community, higher)
            .await
            .expect("store higher first")
            .1
    );
    let accepted = replace_app_metadata(&mut tx, community, lower)
        .await
        .expect("lower id wins on same-second tie");
    assert!(
        accepted.1,
        "lexicographically lower event id must become the live head"
    );
    tx.commit().await.expect("commit ordered");

    let head = live_metadata_content(&pool, community, app_id)
        .await
        .expect("tie head");
    assert_eq!(head.1.as_slice(), lower.id.as_bytes().as_slice());
}
