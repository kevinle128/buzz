//! Relay-backed tests for `POST /hooks/apps/{app_id}`.
//!
//! These tests require a running relay. They are `#[ignore]` so `cargo test`
//! does not fail in CI when the relay is not available.
//!
//! ```text
//! RELAY_URL=ws://localhost:3010 cargo test -p buzz-test-client \
//!   --test e2e_app_callback_notifications -- --ignored --test-threads=1
//! ```

use std::time::{Duration, Instant};

use buzz_core::kind::KIND_APP_ADMIN_COMMAND;
use buzz_core::tenant::CommunityId;
use buzz_db::Db;
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3010".to_string())
}

fn relay_http_url() -> String {
    relay_url()
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches('/')
        .to_string()
}

fn request_host(http: &str) -> String {
    buzz_core::tenant::relay_url_authority(http)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8])
}

fn pubkey_bytes(keys: &Keys) -> [u8; 32] {
    keys.public_key().to_bytes()
}

fn repo_coord(owner: &Keys, d_tag: &str) -> String {
    format!("30617:{}:{d_tag}", owner.public_key().to_hex())
}

fn project_coord(owner: &Keys, d_tag: &str) -> String {
    format!("30621:{}:{d_tag}", owner.public_key().to_hex())
}

async fn submit_event(keys: &Keys, event: nostr::Event) -> Value {
    let client = reqwest::Client::new();
    let http = relay_http_url();
    let resp = client
        .post(format!("{http}/events"))
        .header("Host", request_host(&http))
        .header("X-Pubkey", keys.public_key().to_hex())
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&event).expect("serialize event"))
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST /events failed: {e}"));
    let status = resp.status();
    let body = resp.text().await.expect("read /events body");
    let value: Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parse /events JSON: {e} (body: {body})"));
    if status.as_u16() == 200 {
        value
    } else {
        json!({
            "accepted": false,
            "message": value["error"].as_str().unwrap_or(&body),
        })
    }
}

async fn publish_accepted(keys: &Keys, event: nostr::Event, what: &str) {
    let body = submit_event(keys, event).await;
    assert!(
        body["accepted"].as_bool().unwrap_or(false),
        "{what} not accepted: {body}"
    );
}

async fn create_channel(keys: &Keys, name_prefix: &str) -> Uuid {
    let channel_uuid = Uuid::new_v4();
    let channel_name = unique(name_prefix);
    let event = EventBuilder::new(Kind::Custom(9007), "")
        .tags(vec![
            Tag::parse(["h", &channel_uuid.to_string()]).unwrap(),
            Tag::parse(["name", &channel_name]).unwrap(),
            Tag::parse(["channel_type", "stream"]).unwrap(),
            Tag::parse(["visibility", "open"]).unwrap(),
        ])
        .sign_with_keys(keys)
        .unwrap();
    publish_accepted(keys, event, "create-channel").await;
    channel_uuid
}

async fn add_member(owner: &Keys, channel_id: Uuid, member: &Keys) {
    let event = EventBuilder::new(Kind::Custom(9000), "")
        .allow_self_tagging()
        .tags(vec![
            Tag::parse(["h", &channel_id.to_string()]).unwrap(),
            Tag::parse(["p", &member.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    publish_accepted(owner, event, "add-member").await;
}

async fn archive_channel(owner: &Keys, channel_id: Uuid) {
    let event = EventBuilder::new(Kind::Custom(9002), "")
        .tags([
            Tag::parse(["h", &channel_id.to_string()]).unwrap(),
            Tag::parse(["archived", "true"]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    publish_accepted(owner, event, "archive-channel").await;
}

async fn publish_repo(owner: &Keys, d_tag: &str) -> String {
    let event = EventBuilder::new(Kind::Custom(30617), "")
        .tags(vec![
            Tag::parse(["d", d_tag]).unwrap(),
            Tag::parse(["name", d_tag]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    publish_accepted(owner, event, "repo announcement").await;
    repo_coord(owner, d_tag)
}

async fn publish_project(
    owner: &Keys,
    d_tag: &str,
    name: &str,
    members: &[String],
    buzz_channel: Option<&str>,
) -> String {
    let mut tags = vec![
        Tag::parse(["d", d_tag]).unwrap(),
        Tag::parse(["name", name]).unwrap(),
    ];
    tags.extend(
        members
            .iter()
            .map(|m| Tag::parse(["a", m.as_str()]).unwrap()),
    );
    if let Some(channel) = buzz_channel {
        tags.push(Tag::parse(["buzz-channel", channel]).unwrap());
    }
    let event = EventBuilder::new(Kind::Custom(30621), "")
        .tags(tags)
        .sign_with_keys(owner)
        .unwrap();
    publish_accepted(owner, event, "project").await;
    project_coord(owner, d_tag)
}

async fn test_pool() -> sqlx::PgPool {
    let url = std::env::var("BUZZ_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string());
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap_or_else(|e| panic!("connect test postgres: {e}"))
}

async fn community_id_for_http(pool: &sqlx::PgPool, http: &str) -> Uuid {
    let host = buzz_core::tenant::relay_url_authority(http);
    sqlx::query_scalar("SELECT id FROM communities WHERE lower(host) = lower($1)")
        .bind(&host)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("community for host {host}: {e}"))
}

async fn seed_owner(pool: &sqlx::PgPool, community_id: Uuid, keys: &Keys) {
    let db = Db::from_pool(pool.clone());
    let community = CommunityId::from_uuid(community_id);
    db.ensure_user(community, &pubkey_bytes(keys))
        .await
        .unwrap_or_else(|e| panic!("ensure owner user: {e}"));
    db.add_relay_member(
        community,
        &keys.public_key().to_hex(),
        "owner",
        None,
        &Keys::generate(),
    )
    .await
    .map(|write| write.outcome)
    .unwrap_or_else(|e| panic!("add owner member: {e}"));
}

fn parse_command_response(body: &Value) -> Value {
    assert!(
        body["accepted"].as_bool().unwrap_or(false),
        "app command not accepted: {body}"
    );
    let message = body["message"].as_str().unwrap_or("");
    let json_part = message
        .strip_prefix("response:")
        .unwrap_or_else(|| panic!("accepted app command missing `response:` prefix: {message}"));
    serde_json::from_str(json_part)
        .unwrap_or_else(|e| panic!("parse app command response json: {e} ({json_part})"))
}

async fn create_app(owner: &Keys, name: &str) -> (Uuid, String) {
    let event = EventBuilder::new(
        Kind::Custom(KIND_APP_ADMIN_COMMAND as u16),
        json!({"action":"create","name": name}).to_string(),
    )
    .sign_with_keys(owner)
    .expect("sign create app");
    let body = submit_event(owner, event).await;
    let resp = parse_command_response(&body);
    let app_id = resp["app_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(|| panic!("create missing app_id: {resp}"));
    let secret = resp["webhook_secret"]
        .as_str()
        .unwrap_or_else(|| panic!("create missing webhook_secret: {resp}"))
        .to_string();
    (app_id, secret)
}

async fn post_callback_raw(
    http: &str,
    app_id: &str,
    secret: Option<&str>,
    content_type: Option<&str>,
    body: Vec<u8>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, Value, reqwest::header::HeaderMap) {
    let url = format!("{http}/hooks/apps/{app_id}");
    let mut req = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(http));
    if let Some(secret) = secret {
        req = req.header("X-Webhook-Secret", secret);
    }
    if let Some(ct) = content_type {
        req = req.header("Content-Type", ct);
    }
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    let resp = req
        .body(body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {url} failed: {e}"));
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp.text().await.expect("read callback body");
    let value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    (status, value, headers)
}

async fn post_callback(
    http: &str,
    app_id: Uuid,
    secret: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let (status, value, _) = post_callback_raw(
        http,
        &app_id.to_string(),
        Some(secret),
        Some("application/json"),
        serde_json::to_vec(body).expect("serialize callback"),
        &[],
    )
    .await;
    (status, value)
}

fn callback_body(repo: &str, key: &str, content: &str) -> Value {
    json!({
        "idempotency_key": key,
        "repository_name": repo,
        "event_type": "workflow.run.completed",
        "content": content,
    })
}

async fn query_kind9(http: &str, reader_keys: &Keys, channel_id: Uuid) -> Vec<Value> {
    let url = format!("{http}/query");
    let body = serde_json::to_string(&json!([{
        "kinds": [9],
        "#h": [channel_id.to_string()],
        "limit": 100,
    }]))
    .expect("serialize query");
    let payload_hash = hex::encode(Sha256::digest(body.as_bytes()));
    let nip98 = EventBuilder::new(Kind::Custom(27_235), "")
        .tags(vec![
            Tag::parse(["u", &url]).unwrap(),
            Tag::parse(["method", "POST"]).unwrap(),
            Tag::parse(["payload", &payload_hash]).unwrap(),
            Tag::parse(["nonce", &Uuid::new_v4().to_string()]).unwrap(),
        ])
        .sign_with_keys(reader_keys)
        .expect("sign NIP-98");
    use base64::Engine;
    let auth = format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&nip98).expect("serialize NIP-98"))
    );
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(http))
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST /query failed: {e}"));
    let status = resp.status();
    let text = resp.text().await.expect("read /query body");
    assert!(status.is_success(), "POST /query failed: {status} {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse /query JSON: {e} ({text})"))
}

async fn wait_for_kind9_count(
    http: &str,
    reader_keys: &Keys,
    channel_id: Uuid,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let last = query_kind9(http, reader_keys, channel_id).await;
        if last.len() == expected {
            return last;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {expected} kind 9 events in {channel_id}; last={}",
                last.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn relay_self(http: &str) -> String {
    let resp = reqwest::Client::new()
        .get(http)
        .header("Host", request_host(http))
        .header("Accept", "application/nostr+json")
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET NIP-11 failed: {e}"));
    let text = resp.text().await.expect("read NIP-11");
    let doc: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse NIP-11: {e}"));
    if let Some(self_hex) = doc["self"].as_str() {
        return self_hex.to_string();
    }
    doc["push"]["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|key| key["current"].as_bool() == Some(true))
        .and_then(|key| key["pubkey"].as_str())
        .or_else(|| doc["pubkey"].as_str())
        .unwrap_or_else(|| panic!("NIP-11 missing relay pubkey: {doc}"))
        .to_string()
}

async fn delivery_rows(
    pool: &sqlx::PgPool,
    community_id: Uuid,
    app_id: Uuid,
) -> Vec<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT id, status::text AS status, event_id, failure_code \
         FROM app_callback_deliveries \
         WHERE community_id = $1 AND app_id = $2 \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(community_id)
    .bind(app_id)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| panic!("list deliveries: {e}"))
}

async fn workflow_run_count_for_event(
    pool: &sqlx::PgPool,
    community_id: Uuid,
    event_id: &[u8],
) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM workflow_runs \
         WHERE community_id = $1 AND trigger_event_id = $2",
    )
    .bind(community_id)
    .bind(event_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("count workflow runs: {e}"))
}

fn event_tags(event: &Value) -> Vec<Vec<String>> {
    event["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    tag.as_array().map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn has_tag(event: &Value, name: &str, value: Option<&str>) -> bool {
    event_tags(event).iter().any(|tag| {
        tag.first().map(String::as_str) == Some(name)
            && match value {
                Some(expected) => tag.get(1).map(String::as_str) == Some(expected),
                None => true,
            }
    })
}

fn has_a_marker(event: &Value, coordinate: &str, marker: &str) -> bool {
    event_tags(event).iter().any(|tag| {
        tag.first().map(String::as_str) == Some("a")
            && tag.get(1).map(String::as_str) == Some(coordinate)
            && tag.get(2).map(String::as_str) == Some(marker)
    })
}

struct RoutedApp {
    http: String,
    pool: sqlx::PgPool,
    community_id: Uuid,
    owner: Keys,
    repo_owner: Keys,
    channel: Uuid,
    repo_d: String,
    repo_coord: String,
    project_coord: String,
    app_id: Uuid,
    secret: String,
}

async fn setup_routed_app() -> RoutedApp {
    let http = relay_http_url();
    let pool = test_pool().await;
    let community_id = community_id_for_http(&pool, &http).await;
    let owner = Keys::generate();
    let repo_owner = Keys::generate();
    seed_owner(&pool, community_id, &owner).await;
    let channel = create_channel(&repo_owner, "app-dest").await;
    let repo_d = unique("harness-service");
    let repo_coord = publish_repo(&repo_owner, &repo_d).await;
    let project_coord = publish_project(
        &repo_owner,
        &unique("app-proj"),
        "app-proj",
        std::slice::from_ref(&repo_coord),
        Some(&channel.to_string()),
    )
    .await;
    let (app_id, secret) = create_app(&owner, "Buildkite").await;
    RoutedApp {
        http,
        pool,
        community_id,
        owner,
        repo_owner,
        channel,
        repo_d,
        repo_coord,
        project_coord,
        app_id,
        secret,
    }
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn callback_delivers_one_relay_signed_project_message() {
    let fx = setup_routed_app().await;
    let content = "✅ Archon completed speckit-feature for harness-service";
    let key = unique("delivery-key");
    let (status, body) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &key, content),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "callback: {body}");
    assert_eq!(body["status"].as_str(), Some("delivered"));
    assert_eq!(body["replayed"].as_bool(), Some(false));
    let delivery_id = body["delivery_id"]
        .as_str()
        .expect("delivery_id")
        .to_string();
    let event_id = body["event_id"].as_str().expect("event_id").to_string();

    let events = wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 1).await;
    let event = &events[0];
    assert_eq!(event["id"].as_str(), Some(event_id.as_str()));
    assert_eq!(event["kind"].as_u64(), Some(9));
    assert_eq!(event["content"].as_str(), Some(content));
    let self_hex = relay_self(&fx.http).await;
    assert_eq!(event["pubkey"].as_str(), Some(self_hex.as_str()));
    let parsed: Event =
        serde_json::from_value(event.clone()).unwrap_or_else(|e| panic!("parse event: {e}"));
    parsed.verify().expect("relay signature must verify");
    assert!(has_tag(event, "h", Some(&fx.channel.to_string())));
    assert!(has_tag(event, "buzz:app", Some(&fx.app_id.to_string())));
    assert!(has_a_marker(event, &fx.repo_coord, "repository"));
    assert!(has_a_marker(event, &fx.project_coord, "project"));
    assert!(has_tag(event, "buzz:app-delivery", Some(&delivery_id)));
    assert!(has_tag(
        event,
        "buzz:app-event",
        Some("workflow.run.completed")
    ));
    assert!(!has_tag(event, "buzz:workflow", None));

    let rows = delivery_rows(&fx.pool, fx.community_id, fx.app_id).await;
    assert_eq!(rows.len(), 1, "exactly one delivery row");
    let row = &rows[0];
    let row_id: Uuid = row.get("id");
    assert_eq!(row_id.to_string(), delivery_id);
    let status: String = row.get("status");
    assert_eq!(status, "delivered");
    let stored_event_id: Vec<u8> = row.get("event_id");
    assert_eq!(hex::encode(&stored_event_id), event_id);
    assert_eq!(
        workflow_run_count_for_event(&fx.pool, fx.community_id, &stored_event_id).await,
        0,
        "app messages must not start workflows"
    );
    wait_event_created_audit(&fx.pool, fx.community_id, &event_id).await;
}

async fn app_command(owner: &Keys, content: Value) -> Value {
    let event = EventBuilder::new(
        Kind::Custom(KIND_APP_ADMIN_COMMAND as u16),
        content.to_string(),
    )
    .sign_with_keys(owner)
    .expect("sign app command");
    parse_command_response(&submit_event(owner, event).await)
}

async fn wait_event_created_audit(pool: &sqlx::PgPool, community_id: Uuid, event_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let action: Option<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE community_id = $1 AND object_id = $2 LIMIT 1",
        )
        .bind(community_id)
        .bind(event_id)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|e| panic!("audit lookup: {e}"));
        if action.as_deref() == Some("event_created") {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for EventCreated audit for {event_id}; last={action:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_code(status: StatusCode, body: &Value, expected_status: StatusCode, code: &str) {
    assert_eq!(status, expected_status, "status for {code}: {body}");
    assert_eq!(body["code"].as_str(), Some(code), "code body: {body}");
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn same_payload_replays_without_second_event() {
    let fx = setup_routed_app().await;
    let key = unique("replay-key");
    let body = callback_body(&fx.repo_d, &key, "hello once");
    let (first_status, first) = post_callback(&fx.http, fx.app_id, &fx.secret, &body).await;
    let (second_status, second) = post_callback(&fx.http, fx.app_id, &fx.secret, &body).await;
    assert_eq!(first_status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(second_status, StatusCode::ACCEPTED, "{second}");
    assert_eq!(second["replayed"].as_bool(), Some(true));
    assert_eq!(first["delivery_id"], second["delivery_id"]);
    assert_eq!(first["event_id"], second["event_id"]);
    wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 1).await;
    assert_eq!(
        delivery_rows(&fx.pool, fx.community_id, fx.app_id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn changed_payload_conflicts() {
    let fx = setup_routed_app().await;
    let key = unique("conflict-key");
    let (ok_status, ok) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &key, "first"),
    )
    .await;
    assert_eq!(ok_status, StatusCode::ACCEPTED, "{ok}");
    let (status, body) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &key, "second"),
    )
    .await;
    assert_code(status, &body, StatusCode::CONFLICT, "idempotency_conflict");
    assert_eq!(
        delivery_rows(&fx.pool, fx.community_id, fx.app_id)
            .await
            .len(),
        1
    );
    wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 1).await;
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn concurrent_identical_callbacks_create_one_event() {
    let fx = setup_routed_app().await;
    let key = unique("concurrent-key");
    let payload = callback_body(&fx.repo_d, &key, "same payload");
    let a = post_callback(&fx.http, fx.app_id, &fx.secret, &payload);
    let b = post_callback(&fx.http, fx.app_id, &fx.secret, &payload);
    let ((sa, ba), (sb, bb)) = tokio::join!(a, b);
    assert_eq!(sa, StatusCode::ACCEPTED, "{ba}");
    assert_eq!(sb, StatusCode::ACCEPTED, "{bb}");
    assert_eq!(ba["delivery_id"], bb["delivery_id"]);
    assert_eq!(ba["event_id"], bb["event_id"]);
    let replayed = [ba["replayed"].as_bool(), bb["replayed"].as_bool()];
    assert!(
        replayed.contains(&Some(true)) && replayed.contains(&Some(false)),
        "one fresh and one replay: {ba} {bb}"
    );
    wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 1).await;
    assert_eq!(
        delivery_rows(&fx.pool, fx.community_id, fx.app_id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn deterministic_route_rejection_replays() {
    let fx = setup_routed_app().await;
    let key = unique("missing-repo");
    let body = callback_body("no-such-repo", &key, "hello");
    let (first_status, first) = post_callback(&fx.http, fx.app_id, &fx.secret, &body).await;
    let (second_status, second) = post_callback(&fx.http, fx.app_id, &fx.secret, &body).await;
    assert_code(
        first_status,
        &first,
        StatusCode::UNPROCESSABLE_ENTITY,
        "repository_missing",
    );
    assert_code(
        second_status,
        &second,
        StatusCode::UNPROCESSABLE_ENTITY,
        "repository_missing",
    );
    let rows = delivery_rows(&fx.pool, fx.community_id, fx.app_id).await;
    assert_eq!(rows.len(), 1);
    let status: String = rows[0].get("status");
    assert_eq!(status, "rejected");
    let event_id: Option<Vec<u8>> = rows[0].get("event_id");
    assert!(event_id.is_none());
    assert_eq!(
        query_kind9(&fx.http, &fx.repo_owner, fx.channel)
            .await
            .len(),
        0
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn auth_failures_do_not_create_deliveries_or_inspect_body() {
    let fx = setup_routed_app().await;
    let malformed = br#"{not json"#.to_vec();
    let (wrong, wrong_body, _) = post_callback_raw(
        &fx.http,
        &fx.app_id.to_string(),
        Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        Some("text/plain"),
        malformed.clone(),
        &[],
    )
    .await;
    assert_code(wrong, &wrong_body, StatusCode::UNAUTHORIZED, "unauthorized");

    let (missing, missing_body, _) = post_callback_raw(
        &fx.http,
        &fx.app_id.to_string(),
        None,
        Some("application/json"),
        malformed.clone(),
        &[],
    )
    .await;
    assert_code(
        missing,
        &missing_body,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );

    let url = format!("{}/hooks/apps/{}?secret={}", fx.http, fx.app_id, fx.secret);
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(&fx.http))
        .header("Content-Type", "application/json")
        .body(malformed.clone())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    assert_code(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");

    let (hmac, hmac_body, _) = post_callback_raw(
        &fx.http,
        &fx.app_id.to_string(),
        None,
        Some("application/json"),
        malformed,
        &[
            ("X-Webhook-Signature-V2", "sha256=deadbeef"),
            ("X-Hub-Signature-256", "sha256=deadbeef"),
        ],
    )
    .await;
    assert_code(hmac, &hmac_body, StatusCode::UNAUTHORIZED, "unauthorized");
    assert!(delivery_rows(&fx.pool, fx.community_id, fx.app_id)
        .await
        .is_empty());
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn malformed_uuid_and_disabled_and_foreign_apps_are_not_found() {
    let fx = setup_routed_app().await;
    let (bad, bad_body, _) = post_callback_raw(
        &fx.http,
        "not-a-uuid",
        Some(&fx.secret),
        Some("application/json"),
        b"{}".to_vec(),
        &[],
    )
    .await;
    assert_code(bad, &bad_body, StatusCode::BAD_REQUEST, "invalid_app_id");

    app_command(&fx.owner, json!({"action":"disable","app_id": fx.app_id})).await;
    let (disabled, disabled_body) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("disabled"), "x"),
    )
    .await;
    assert_code(
        disabled,
        &disabled_body,
        StatusCode::NOT_FOUND,
        "app_not_found",
    );

    let db = Db::from_pool(fx.pool.clone());
    let host = format!("foreign-{}.example.test", Uuid::new_v4());
    let foreign_owner = Keys::generate();
    let created = db
        .create_community_with_owner(
            &host,
            &foreign_owner.public_key().to_hex(),
            &Keys::generate(),
        )
        .await
        .expect("foreign community");
    let foreign_community = match created.outcome {
        buzz_db::CreateCommunityWithOwnerResult::Created(record) => record.id,
        other => panic!("expected Created, got {other:?}"),
    };
    let foreign_app = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO apps (community_id, id, name, status, secret_hash, created_by) \
         VALUES ($1, $2, 'foreign', 'active', $3, $4)",
    )
    .bind(foreign_community.as_uuid())
    .bind(foreign_app)
    .bind(&[3u8; 32][..])
    .bind(&pubkey_bytes(&foreign_owner)[..])
    .execute(&fx.pool)
    .await
    .expect("insert foreign app");
    let (cross, cross_body) = post_callback(
        &fx.http,
        foreign_app,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("cross"), "x"),
    )
    .await;
    assert_code(cross, &cross_body, StatusCode::NOT_FOUND, "app_not_found");
    assert!(delivery_rows(&fx.pool, fx.community_id, fx.app_id)
        .await
        .is_empty());
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn body_diagnostics_after_auth() {
    let fx = setup_routed_app().await;
    let app = fx.app_id.to_string();
    let secret = fx.secret.as_str();
    let cases: Vec<(&str, Option<&str>, Vec<u8>, &str)> = vec![
        ("bad-ct", Some("text/plain"), br#"{"idempotency_key":"k","repository_name":"r","event_type":"push","content":"hi"}"#.to_vec(), "invalid_callback"),
        ("bad-json", Some("application/json"), br#"{nope"#.to_vec(), "invalid_callback"),
        ("array", Some("application/json"), b"[1]".to_vec(), "invalid_callback"),
        ("reserved", Some("application/json"), serde_json::to_vec(&json!({
            "idempotency_key":"k","repository_name":"r","event_type":"push","content":"hi","channel_id":"x"
        })).unwrap(), "invalid_callback"),
        ("missing-content", Some("application/json"), serde_json::to_vec(&json!({
            "idempotency_key":"k","repository_name":"r","event_type":"push"
        })).unwrap(), "invalid_control_fields"),
        ("bad-event-type", Some("application/json"), serde_json::to_vec(&json!({
            "idempotency_key":"k","repository_name":"r","event_type":"Push","content":"hi"
        })).unwrap(), "invalid_control_fields"),
    ];
    for (name, ct, bytes, code) in cases {
        let (status, body, _) =
            post_callback_raw(&fx.http, &app, Some(secret), ct, bytes, &[]).await;
        let expected = if code == "invalid_callback" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        assert_code(status, &body, expected, code);
        assert!(
            delivery_rows(&fx.pool, fx.community_id, fx.app_id)
                .await
                .is_empty(),
            "{name} created a delivery"
        );
    }

    let huge = vec![b'x'; APP_CALLBACK_BODY_MAX_BYTES + 1];
    let (over, over_body, _) = post_callback_raw(
        &fx.http,
        &app,
        Some(secret),
        Some("application/json"),
        huge,
        &[],
    )
    .await;
    assert_code(
        over,
        &over_body,
        StatusCode::BAD_REQUEST,
        "invalid_callback",
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn metadata_bounds_and_canonical_hashing() {
    let fx = setup_routed_app().await;
    let mut nested = json!({});
    for _ in 0..17 {
        nested = json!({ "c": nested });
    }
    let (deep_status, deep) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &json!({
            "idempotency_key": unique("deep"),
            "repository_name": fx.repo_d,
            "event_type": "push",
            "content": "hi",
            "metadata": nested,
        }),
    )
    .await;
    assert_code(
        deep_status,
        &deep,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_control_fields",
    );

    let items: Vec<u32> = (0..1025).collect();
    let (nodes_status, nodes) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &json!({
            "idempotency_key": unique("nodes"),
            "repository_name": fx.repo_d,
            "event_type": "push",
            "content": "hi",
            "metadata": { "items": items },
        }),
    )
    .await;
    assert_code(
        nodes_status,
        &nodes,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_control_fields",
    );

    let key = unique("canon");
    let first = json!({
        "content": "hi",
        "event_type": "push",
        "idempotency_key": key,
        "repository_name": fx.repo_d,
    });
    let second = json!({
        "repository_name": fx.repo_d,
        "idempotency_key": key,
        "event_type": "push",
        "content": "hi",
    });
    let (a_status, a) = post_callback(&fx.http, fx.app_id, &fx.secret, &first).await;
    let (b_status, b) = post_callback(&fx.http, fx.app_id, &fx.secret, &second).await;
    assert_eq!(a_status, StatusCode::ACCEPTED, "{a}");
    assert_eq!(b_status, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["replayed"].as_bool(), Some(true));

    let present_key = unique("presence");
    let (c_status, c) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &json!({
            "idempotency_key": present_key,
            "repository_name": fx.repo_d,
            "event_type": "push",
            "content": "hi",
            "metadata": { "items": [1, 2] },
        }),
    )
    .await;
    assert_eq!(c_status, StatusCode::ACCEPTED, "{c}");
    let (d_status, d) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &json!({
            "idempotency_key": present_key,
            "repository_name": fx.repo_d,
            "event_type": "push",
            "content": "hi",
            "metadata": { "items": [2, 1] },
        }),
    )
    .await;
    assert_code(d_status, &d, StatusCode::CONFLICT, "idempotency_conflict");
    let (e_status, e) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &json!({
            "idempotency_key": present_key,
            "repository_name": fx.repo_d,
            "event_type": "push",
            "content": "hi",
        }),
    )
    .await;
    assert_code(e_status, &e, StatusCode::CONFLICT, "idempotency_conflict");
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn rotation_rejects_old_secret_and_hmac_does_not_grant_access() {
    let fx = setup_routed_app().await;
    let rotated = app_command(
        &fx.owner,
        json!({"action":"rotate_secret","app_id": fx.app_id}),
    )
    .await;
    let new_secret = rotated["webhook_secret"].as_str().expect("new secret");
    let (old_status, old_body) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("old-secret"), "x"),
    )
    .await;
    assert_code(
        old_status,
        &old_body,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let (ok_status, ok, _) = post_callback_raw(
        &fx.http,
        &fx.app_id.to_string(),
        Some(new_secret),
        Some("application/json"),
        serde_json::to_vec(&callback_body(&fx.repo_d, &unique("new-secret"), "ok")).unwrap(),
        &[("X-Webhook-Signature-V2", "sha256=deadbeef")],
    )
    .await;
    assert_eq!(ok_status, StatusCode::ACCEPTED, "{ok}");
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn mention_p_tags_and_two_repos_share_project_channel() {
    let fx = setup_routed_app().await;
    let mentioned = Keys::generate();
    let profile = EventBuilder::new(
        Kind::from(0),
        json!({"name":"Robby","display_name":"Robby"}).to_string(),
    )
    .sign_with_keys(&mentioned)
    .unwrap();
    publish_accepted(&mentioned, profile, "kind 0").await;
    add_member(&fx.repo_owner, fx.channel, &mentioned).await;
    sqlx::query("UPDATE users SET display_name = 'Robby' WHERE community_id = $1 AND pubkey = $2")
        .bind(fx.community_id)
        .bind(&pubkey_bytes(&mentioned)[..])
        .execute(&fx.pool)
        .await
        .expect("set display name");
    let (status, body) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("mention"), "hey @Robby look"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let events = wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 1).await;
    assert!(has_tag(
        &events[0],
        "p",
        Some(&mentioned.public_key().to_hex())
    ));

    let second_d = unique("agentic-os-plan");
    let second_coord = publish_repo(&fx.repo_owner, &second_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("shared-proj"),
        "shared",
        &[fx.repo_coord.clone(), second_coord],
        Some(&fx.channel.to_string()),
    )
    .await;
    // Two claim-valid projects would be ambiguous for the first repo; publish
    // a replacement of the original project is not needed — the second repo is
    // claimed only by this new project. Route the second repo.
    let (second_status, second) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&second_d, &unique("second-repo"), "from second repo"),
    )
    .await;
    assert_eq!(second_status, StatusCode::ACCEPTED, "{second}");
    wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 2).await;
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn route_failures_are_deterministic_and_app_need_not_be_member() {
    let fx = setup_routed_app().await;
    let owner_b = Keys::generate();
    let extra = unique("ambiguous");
    publish_repo(&owner_b, &extra).await;
    publish_repo(&fx.repo_owner, &extra).await;
    let (amb_status, amb) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&extra, &unique("amb-repo"), "x"),
    )
    .await;
    assert_code(
        amb_status,
        &amb,
        StatusCode::UNPROCESSABLE_ENTITY,
        "repository_ambiguous",
    );

    let no_project = unique("orphan-repo");
    publish_repo(&fx.repo_owner, &no_project).await;
    let (missing_proj_status, missing_proj) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&no_project, &unique("no-proj"), "x"),
    )
    .await;
    assert_code(
        missing_proj_status,
        &missing_proj,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_missing",
    );

    let dest_a = create_channel(&fx.repo_owner, "proj-a").await;
    let dest_b = create_channel(&fx.repo_owner, "proj-b").await;
    let shared = unique("two-projects");
    let coord = publish_repo(&fx.repo_owner, &shared).await;
    publish_project(
        &fx.repo_owner,
        &unique("pa"),
        "pa",
        std::slice::from_ref(&coord),
        Some(&dest_a.to_string()),
    )
    .await;
    publish_project(
        &fx.repo_owner,
        &unique("pb"),
        "pb",
        &[coord],
        Some(&dest_b.to_string()),
    )
    .await;
    let (amb_proj_status, amb_proj) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&shared, &unique("amb-proj"), "x"),
    )
    .await;
    assert_code(
        amb_proj_status,
        &amb_proj,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_ambiguous",
    );

    let bad_channel_repo = unique("bad-chan");
    let bad_coord = publish_repo(&fx.repo_owner, &bad_channel_repo).await;
    publish_project(
        &fx.repo_owner,
        &unique("bad-chan-proj"),
        "bad",
        &[bad_coord],
        Some("not-a-uuid"),
    )
    .await;
    let (bad_ch_status, bad_ch) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&bad_channel_repo, &unique("bad-chan"), "x"),
    )
    .await;
    assert_code(
        bad_ch_status,
        &bad_ch,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_channel_invalid",
    );

    let archived = create_channel(&fx.repo_owner, "archived-dest").await;
    archive_channel(&fx.repo_owner, archived).await;
    let archived_d = unique("archived-repo");
    let archived_coord = publish_repo(&fx.repo_owner, &archived_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("archived-proj"),
        "archived",
        &[archived_coord],
        Some(&archived.to_string()),
    )
    .await;
    let (arch_status, arch) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&archived_d, &unique("archived"), "x"),
    )
    .await;
    assert_code(
        arch_status,
        &arch,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_channel_invalid",
    );

    sqlx::query("UPDATE channels SET deleted_at = NOW() WHERE id = $1")
        .bind(fx.channel)
        .execute(&fx.pool)
        .await
        .expect("delete channel");
    let (del_status, del) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("deleted"), "x"),
    )
    .await;
    assert_code(
        del_status,
        &del,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_channel_invalid",
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn two_app_heads_and_workflow_webhook_regression() {
    let fx = setup_routed_app().await;
    let (second_id, second_secret) = create_app(&fx.owner, "PagerDuty").await;
    let (first_status, first) = post_callback(
        &fx.http,
        fx.app_id,
        &fx.secret,
        &callback_body(&fx.repo_d, &unique("app-one"), "from first"),
    )
    .await;
    let (second_status, second) = post_callback(
        &fx.http,
        fx.app_id,
        &second_secret,
        &callback_body(&fx.repo_d, &unique("app-two-wrong"), "nope"),
    )
    .await;
    assert_eq!(first_status, StatusCode::ACCEPTED, "{first}");
    assert_code(
        second_status,
        &second,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let (ok2_status, ok2) = post_callback(
        &fx.http,
        second_id,
        &second_secret,
        &callback_body(&fx.repo_d, &unique("app-two"), "from second"),
    )
    .await;
    assert_eq!(ok2_status, StatusCode::ACCEPTED, "{ok2}");
    assert_ne!(first["delivery_id"], ok2["delivery_id"]);
    wait_for_kind9_count(&fx.http, &fx.repo_owner, fx.channel, 2).await;

    let yaml = concat!(
        "name: 'Incident Alert'\n",
        "trigger:\n  on: webhook\n",
        "steps:\n  - id: notify\n    action: send_message\n    text: 'P1 alert'\n",
    );
    let workflow_id = Uuid::new_v4();
    let event = EventBuilder::new(Kind::Custom(30620), yaml)
        .tags(vec![
            Tag::parse(["d", &workflow_id.to_string()]).unwrap(),
            Tag::parse(["h", &fx.channel.to_string()]).unwrap(),
        ])
        .sign_with_keys(&fx.repo_owner)
        .unwrap();
    let save = submit_event(&fx.repo_owner, event).await;
    assert!(
        save["accepted"].as_bool().unwrap_or(false),
        "workflow save: {save}"
    );
    let json_part = save["message"]
        .as_str()
        .and_then(|m| m.strip_prefix("response:"))
        .expect("workflow response");
    let resp: Value = serde_json::from_str(json_part).unwrap();
    let wf_id = Uuid::parse_str(resp["workflow_id"].as_str().unwrap()).unwrap();
    let wf_secret = resp["webhook_secret"].as_str().unwrap();
    let url = format!("{}/hooks/{wf_id}", fx.http);
    let hook = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(&fx.http))
        .header("X-Webhook-Secret", wf_secret)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(hook.status(), StatusCode::ACCEPTED, "static webhook");
}

const APP_CALLBACK_BODY_MAX_BYTES: usize = 65_536;

#[tokio::test]
#[ignore = "requires a running relay"]
async fn rate_limit_covers_valid_and_invalid_secrets() {
    let fx = setup_routed_app().await;
    let mut saw_429 = false;
    for i in 0..61 {
        let secret = if i % 2 == 0 {
            fx.secret.as_str()
        } else {
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        };
        let (status, body, headers) = post_callback_raw(
            &fx.http,
            &fx.app_id.to_string(),
            Some(secret),
            Some("application/json"),
            serde_json::to_vec(&callback_body(&fx.repo_d, &unique(&format!("rl-{i}")), "x"))
                .unwrap(),
            &[],
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(body["code"].as_str(), Some("rate_limited"));
            assert!(headers.get("retry-after").is_some());
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected 429 within 61 mixed secret attempts");
}
