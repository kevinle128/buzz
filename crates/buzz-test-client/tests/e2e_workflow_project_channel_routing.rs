//! Relay-backed tests for webhook `project_channel_by_repository` routing.
//!
//! These tests require a running relay. They are `#[ignore]` so `cargo test`
//! does not fail in CI when the relay is not available.
//!
//! ```text
//! RELAY_URL=ws://localhost:3000 cargo test -p buzz-test-client \
//!   --test e2e_workflow_project_channel_routing -- --ignored
//! ```

use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use buzz_db::channel::{ChannelType, ChannelVisibility};
use buzz_db::{CreateCommunityWithOwnerResult, Db};
use buzz_test_client::BuzzTestClient;
use nostr::{EventBuilder, Keys, Kind, Tag};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_string())
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

/// A short unique suffix so concurrent runs never collide on a `d` tag.
fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8])
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn key_hash(raw: &str) -> Vec<u8> {
    Sha256::digest(raw.as_bytes()).to_vec()
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

/// NIP-98 `Authorization` header for an authenticated HTTP POST.
fn nip98_post_header(keys: &Keys, url: &str, body: &str) -> String {
    let event = EventBuilder::new(Kind::Custom(27_235), "")
        .tags(vec![
            Tag::parse(["u", url]).unwrap(),
            Tag::parse(["method", "POST"]).unwrap(),
            Tag::parse(["payload", &sha256_hex(body.as_bytes())]).unwrap(),
            Tag::parse(["nonce", &Uuid::new_v4().to_string()]).unwrap(),
        ])
        .sign_with_keys(keys)
        .expect("sign NIP-98 event");
    format!(
        "Nostr {}",
        BASE64.encode(serde_json::to_string(&event).expect("serialize NIP-98 event"))
    )
}

/// Submit a signed event via the HTTP bridge (`POST /events`).
async fn submit_event(keys: &Keys, event: nostr::Event) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/events", relay_http_url()))
        .header("X-Pubkey", keys.public_key().to_hex())
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&event).expect("serialize event"))
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST /events failed: {e}"));
    let status = resp.status();
    let body = resp.text().await.expect("read /events body");
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parse /events JSON: {e} (body: {body})"));
    if status.as_u16() == 200 {
        value
    } else {
        serde_json::json!({
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

/// Create an `open` stream channel; the signer is bootstrapped as owner-member.
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

async fn remove_member(owner: &Keys, channel_id: Uuid, member: &Keys) {
    let event = EventBuilder::new(Kind::Custom(9001), "")
        .tags(vec![
            Tag::parse(["h", &channel_id.to_string()]).unwrap(),
            Tag::parse(["p", &member.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    publish_accepted(owner, event, "remove-member").await;
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

fn static_webhook_yaml() -> &'static str {
    concat!(
        "name: 'Incident Alert'\n",
        "trigger:\n  on: webhook\n",
        "steps:\n  - id: notify\n    action: send_message\n    text: 'P1 alert'\n",
    )
}

fn dynamic_yaml(text: &str) -> String {
    format!(
        "name: Routed Notify\n\
         trigger:\n  on: webhook\n\
         routing:\n  mode: project_channel_by_repository\n\
         steps:\n  - id: notify\n    action: send_message\n    text: {text}\n"
    )
}

fn delay_then_send_yaml() -> &'static str {
    concat!(
        "name: Delayed Route\n",
        "trigger:\n  on: webhook\n",
        "routing:\n  mode: project_channel_by_repository\n",
        "steps:\n",
        "  - id: wait\n    action: delay\n    duration: 2s\n",
        "  - id: notify\n    action: send_message\n    text: hi\n",
    )
}

struct WorkflowSaveResult {
    accepted: bool,
    message: String,
    workflow_id: Option<Uuid>,
    webhook_secret: Option<String>,
}

/// Publish a client-UUID kind `30620` workflow definition over the authenticated
/// WebSocket. Response fields are populated only when the save is accepted.
async fn define_webhook_workflow_raw(
    keys: &Keys,
    home_channel: Uuid,
    yaml: &str,
) -> WorkflowSaveResult {
    let workflow_id = Uuid::new_v4();
    let event = EventBuilder::new(Kind::Custom(30620), yaml)
        .tags(vec![
            Tag::parse(["d", &workflow_id.to_string()]).unwrap(),
            Tag::parse(["h", &home_channel.to_string()]).unwrap(),
        ])
        .sign_with_keys(keys)
        .expect("sign workflow def");

    let mut client = BuzzTestClient::connect(&relay_url(), keys)
        .await
        .expect("connect");
    let ok = client
        .send_event(event)
        .await
        .expect("publish workflow def");
    client.disconnect().await.expect("disconnect");

    if !ok.accepted {
        return WorkflowSaveResult {
            accepted: false,
            message: ok.message,
            workflow_id: None,
            webhook_secret: None,
        };
    }

    let json_part = ok.message.strip_prefix("response:").unwrap_or_else(|| {
        panic!(
            "accepted workflow save missing `response:` prefix: {:?}",
            ok.message
        )
    });
    let resp: serde_json::Value = serde_json::from_str(json_part)
        .unwrap_or_else(|e| panic!("parse workflow save response json: {e} ({json_part:?})"));
    WorkflowSaveResult {
        accepted: true,
        message: ok.message,
        workflow_id: resp["workflow_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok()),
        webhook_secret: resp["webhook_secret"].as_str().map(str::to_string),
    }
}

async fn define_webhook_workflow(
    http: &str,
    keys: &Keys,
    home_channel: Uuid,
    yaml: &str,
) -> (Uuid, String) {
    let result = define_webhook_workflow_raw(keys, home_channel, yaml).await;
    assert!(
        result.accepted,
        "workflow save not accepted against {http}: {}",
        result.message
    );
    let workflow_id = result.workflow_id.unwrap_or_else(|| {
        panic!(
            "accepted workflow save missing workflow_id: {}",
            result.message
        )
    });
    let webhook_secret = result.webhook_secret.unwrap_or_else(|| {
        panic!(
            "accepted workflow save missing webhook_secret: {}",
            result.message
        )
    });
    (workflow_id, webhook_secret)
}

async fn post_hook_raw(
    http: &str,
    workflow_id: Uuid,
    secret: &str,
    body: &str,
) -> (StatusCode, Value) {
    let url = format!("{http}/hooks/{workflow_id}");
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(http))
        .header("X-Webhook-Secret", secret)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {url} failed: {e}"));
    let status = resp.status();
    let text = resp.text().await.expect("read /hooks body");
    let value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    (status, value)
}

async fn post_hook(
    http: &str,
    workflow_id: Uuid,
    secret: &str,
    body: &Value,
) -> (StatusCode, Value) {
    post_hook_raw(
        http,
        workflow_id,
        secret,
        &serde_json::to_string(body).expect("serialize hook body"),
    )
    .await
}

async fn query_kind9(http: &str, reader_keys: &Keys, channel_id: Uuid) -> Vec<Value> {
    let url = format!("{http}/query");
    let body = serde_json::to_string(&json!([{
        "kinds": [9],
        "#h": [channel_id.to_string()],
        "limit": 100,
    }]))
    .expect("serialize query");
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Host", request_host(http))
        .header("Authorization", nip98_post_header(reader_keys, &url, &body))
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

async fn create_foreign_channel(pool: &sqlx::PgPool, channel_id: Uuid, keys: &Keys) -> Uuid {
    let db = Db::from_pool(pool.clone());
    let host = format!("foreign-{}.example.test", Uuid::new_v4());
    let owner = Keys::generate();
    let created = db
        .create_community_with_owner(&host, &owner.public_key().to_hex(), &Keys::generate())
        .await
        .unwrap_or_else(|e| panic!("create foreign community: {e}"));
    let community_id = match created.outcome {
        CreateCommunityWithOwnerResult::Created(record) => record.id,
        other => panic!("expected Created foreign community, got {other:?}"),
    };
    db.ensure_user(community_id, &pubkey_bytes(keys))
        .await
        .unwrap_or_else(|e| panic!("ensure foreign user: {e}"));
    db.create_channel_with_id(
        community_id,
        channel_id,
        &unique("foreign"),
        ChannelType::Stream,
        ChannelVisibility::Open,
        None,
        &pubkey_bytes(keys),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("create foreign channel: {e}"));
    *community_id.as_uuid()
}

#[derive(Debug)]
struct RunInspection {
    id: Uuid,
    status: String,
    trigger_context: Option<Value>,
    route_snapshot: Option<Value>,
    execution_trace: Value,
    error_message: Option<String>,
    error_code: Option<String>,
}

fn row_to_run(row: sqlx::postgres::PgRow) -> RunInspection {
    RunInspection {
        id: row.get("id"),
        status: row.get("status"),
        trigger_context: row.get("trigger_context"),
        route_snapshot: row.get("route_snapshot"),
        execution_trace: row.get("execution_trace"),
        error_message: row.get("error_message"),
        error_code: row.get("error_code"),
    }
}

async fn workflow_run_for_key(
    pool: &sqlx::PgPool,
    community_id: Uuid,
    workflow_id: Uuid,
    raw_key: &str,
) -> RunInspection {
    let row = sqlx::query(
        "SELECT id, trigger_context, route_snapshot, execution_trace, \
         error_message, error_code, status::text AS status \
         FROM workflow_runs \
         WHERE community_id = $1 AND workflow_id = $2 AND idempotency_key_hash = $3",
    )
    .bind(community_id)
    .bind(workflow_id)
    .bind(key_hash(raw_key))
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("workflow run for key: {e}"));
    row_to_run(row)
}

async fn wait_for_run_status(
    pool: &sqlx::PgPool,
    community_id: Uuid,
    run_id: Uuid,
    expected: &str,
) -> RunInspection {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last: Option<RunInspection> = None;
    loop {
        let row = sqlx::query(
            "SELECT id, trigger_context, route_snapshot, execution_trace, \
             error_message, error_code, status::text AS status \
             FROM workflow_runs \
             WHERE community_id = $1 AND id = $2",
        )
        .bind(community_id)
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|e| panic!("load workflow run {run_id}: {e}"));
        if let Some(row) = row {
            let run = row_to_run(row);
            if run.status == expected {
                return run;
            }
            last = Some(run);
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for run {run_id} status {expected}; last={last:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn scoped_run_count(pool: &sqlx::PgPool, community_id: Uuid, workflow_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM workflow_runs WHERE community_id = $1 AND workflow_id = $2",
    )
    .bind(community_id)
    .bind(workflow_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("count workflow runs: {e}"))
}

async fn disable_workflow(pool: &sqlx::PgPool, community_id: Uuid, workflow_id: Uuid) {
    let updated =
        sqlx::query("UPDATE workflows SET enabled = FALSE WHERE community_id = $1 AND id = $2")
            .bind(community_id)
            .bind(workflow_id)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("disable workflow: {e}"));
    assert_eq!(
        updated.rows_affected(),
        1,
        "workflow {workflow_id} not updated"
    );
}

async fn sql_kind9_count(pool: &sqlx::PgPool, community_id: Uuid, channel_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM events \
         WHERE community_id = $1 AND channel_id = $2 AND kind = 9 AND deleted_at IS NULL",
    )
    .bind(community_id)
    .bind(channel_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("count kind 9: {e}"))
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

fn event_serialized(event: &Value) -> String {
    serde_json::to_string(event).unwrap_or_default()
}

fn run_serialized(run: &RunInspection) -> String {
    json!({
        "trigger_context": run.trigger_context,
        "route_snapshot": run.route_snapshot,
        "execution_trace": run.execution_trace,
        "error_message": run.error_message,
        "error_code": run.error_code,
    })
    .to_string()
}

fn send_message_event_id(run: &RunInspection) -> &str {
    run.execution_trace
        .as_array()
        .into_iter()
        .flatten()
        .find(|step| step["step_id"].as_str() == Some("notify"))
        .and_then(|step| step["output"]["event_id"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "send_message trace missing event_id: {}",
                run.execution_trace
            )
        })
}

struct Fixture {
    http: String,
    pool: sqlx::PgPool,
    community_id: Uuid,
    workflow_owner: Keys,
    repo_owner: Keys,
    home: Uuid,
}

async fn setup_fixture() -> Fixture {
    let http = relay_http_url();
    let pool = test_pool().await;
    let community_id = community_id_for_http(&pool, &http).await;
    let workflow_owner = Keys::generate();
    let repo_owner = Keys::generate();
    let home = create_channel(&workflow_owner, "wf-home").await;
    Fixture {
        http,
        pool,
        community_id,
        workflow_owner,
        repo_owner,
        home,
    }
}

fn hook_body(repo: &str, key: &str, title: &str) -> Value {
    json!({
        "repository_name": repo,
        "idempotency_key": key,
        "title": title,
    })
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn alias_target_must_be_live_at_save() {
    let keys = nostr::Keys::generate();
    let home_channel = create_channel(&keys, "alias-home").await;
    let alias_key = "hs";
    let missing_target = format!(
        "30617:{}:missing-harness-service",
        keys.public_key().to_hex()
    );
    let yaml = format!(
        "name: Alias Save Rejection\n\
         trigger:\n  on: webhook\n\
         routing:\n  mode: project_channel_by_repository\n\
         \x20 aliases:\n    {alias_key}: {missing_target}\n\
         steps:\n  - id: notify\n    action: send_message\n    text: hi\n"
    );
    let result = define_webhook_workflow_raw(&keys, home_channel, &yaml).await;
    assert!(!result.accepted);
    assert!(result.workflow_id.is_none());
    assert!(result.webhook_secret.is_none());
    assert!(result.message.contains(alias_key));
    assert!(!result.message.contains("30617:"));
    assert!(!result.message.contains("missing-harness-service"));
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn static_webhook_without_routing_still_posts_to_home_channel() {
    let fx = setup_fixture().await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, static_webhook_yaml()).await;
    let (status, body) = post_hook(&fx.http, workflow_id, &secret, &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "static hook: {body}");
    let events = wait_for_kind9_count(&fx.http, &fx.workflow_owner, fx.home, 1).await;
    assert!(has_tag(&events[0], "buzz:workflow", Some("true")));
    assert!(!has_tag(&events[0], "buzz:workflow-run", None));
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn one_workflow_routes_agentic_os_plan_and_harness_service() {
    let fx = setup_fixture().await;
    let channel_admin = Keys::generate();
    let dest = create_channel(&channel_admin, "gigo-harness").await;
    add_member(&channel_admin, dest, &fx.workflow_owner).await;

    let plan_d = unique("agentic-os-plan");
    let harness_d = unique("harness-service");
    let plan_coord = publish_repo(&fx.repo_owner, &plan_d).await;
    let harness_coord = publish_repo(&fx.repo_owner, &harness_d).await;
    let project_d = unique("gigo-harness");
    let project = publish_project(
        &fx.repo_owner,
        &project_d,
        "gigo-harness",
        &[plan_coord.clone(), harness_coord.clone()],
        Some(&dest.to_string()),
    )
    .await;

    let yaml = dynamic_yaml("repo={{trigger.repository_name}} key={{trigger.idempotency_key}}");
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &yaml).await;

    let plan_key = unique("plan-key");
    let harness_key = unique("harness-key");
    let (plan_status, plan_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&plan_d, &plan_key, "plan"),
    )
    .await;
    let (harness_status, harness_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&harness_d, &harness_key, "harness"),
    )
    .await;
    assert_eq!(plan_status, StatusCode::ACCEPTED, "{plan_body}");
    assert_eq!(harness_status, StatusCode::ACCEPTED, "{harness_body}");
    let plan_run_id = Uuid::parse_str(plan_body["run_id"].as_str().expect("plan run_id")).unwrap();
    let harness_run_id =
        Uuid::parse_str(harness_body["run_id"].as_str().expect("harness run_id")).unwrap();

    let dest_events = wait_for_kind9_count(&fx.http, &fx.workflow_owner, dest, 2).await;
    let home_events = query_kind9(&fx.http, &fx.workflow_owner, fx.home).await;
    assert!(
        home_events.is_empty(),
        "home must stay empty: {home_events:?}"
    );

    let assert_event = |event: &Value, repo: &str, run_id: Uuid| {
        assert!(!has_tag(event, "e", None), "no e tags: {event}");
        assert!(has_tag(event, "a", Some(repo)));
        assert!(has_tag(event, "a", Some(&project)));
        assert!(has_tag(
            event,
            "buzz:workflow-run",
            Some(&run_id.to_string())
        ));
        event["id"].as_str().expect("kind 9 event id").to_string()
    };

    let plan_event = dest_events
        .iter()
        .find(|e| has_tag(e, "a", Some(&plan_coord)))
        .expect("plan event");
    let harness_event = dest_events
        .iter()
        .find(|e| has_tag(e, "a", Some(&harness_coord)))
        .expect("harness event");
    let plan_event_id = assert_event(plan_event, &plan_coord, plan_run_id);
    let harness_event_id = assert_event(harness_event, &harness_coord, harness_run_id);

    let plan_run = wait_for_run_status(&fx.pool, fx.community_id, plan_run_id, "completed").await;
    let harness_run =
        wait_for_run_status(&fx.pool, fx.community_id, harness_run_id, "completed").await;
    let community_id = fx.community_id.to_string();
    let dest_id = dest.to_string();
    for (run, repo) in [(&plan_run, &plan_coord), (&harness_run, &harness_coord)] {
        let snap = run.route_snapshot.as_ref().expect("route snapshot");
        assert_eq!(snap["community_id"].as_str(), Some(community_id.as_str()));
        assert_eq!(snap["repository_coordinate"].as_str(), Some(repo.as_str()));
        assert_eq!(snap["project_coordinate"].as_str(), Some(project.as_str()));
        assert_eq!(snap["channel_id"].as_str(), Some(dest_id.as_str()));
        assert_eq!(snap["matched_identity_tier"].as_str(), Some("d_tag"));
    }
    assert_eq!(send_message_event_id(&plan_run), plan_event_id);
    assert_eq!(send_message_event_id(&harness_run), harness_event_id);
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn ambiguous_claim_valid_projects_return_422_and_no_message() {
    let fx = setup_fixture().await;
    let dest_a = create_channel(&fx.repo_owner, "proj-a").await;
    let dest_b = create_channel(&fx.repo_owner, "proj-b").await;
    add_member(&fx.repo_owner, dest_a, &fx.workflow_owner).await;
    add_member(&fx.repo_owner, dest_b, &fx.workflow_owner).await;
    let repo_d = unique("shared-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("proj-a"),
        "proj-a",
        std::slice::from_ref(&coord),
        Some(&dest_a.to_string()),
    )
    .await;
    publish_project(
        &fx.repo_owner,
        &unique("proj-b"),
        "proj-b",
        &[coord],
        Some(&dest_b.to_string()),
    )
    .await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let key = unique("amb-key");
    let (status, body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &key, "amb"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"].as_str(), Some("project_ambiguous"));
    let run = workflow_run_for_key(&fx.pool, fx.community_id, workflow_id, &key).await;
    assert_eq!(run.status, "failed");
    for channel in [fx.home, dest_a, dest_b] {
        assert_eq!(
            sql_kind9_count(&fx.pool, fx.community_id, channel).await,
            0,
            "kind 9 leaked into {channel}"
        );
    }
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn open_destination_without_membership_is_unauthorized() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "open-dest").await;
    let repo_d = unique("unauth-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("unauth-proj"),
        "unauth",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let key = unique("unauth-key");
    let (status, body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &key, "unauth"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"].as_str(), Some("route_unauthorized"));
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, fx.home).await, 0);
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, dest).await, 0);
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn same_key_same_payload_does_not_send_a_second_message() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "idem-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("idem-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("idem-proj"),
        "idem",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let key = unique("same-key");
    let payload = hook_body(&repo_d, &key, "same");
    let (left, right) = tokio::join!(
        post_hook(&fx.http, workflow_id, &secret, &payload),
        post_hook(&fx.http, workflow_id, &secret, &payload),
    );
    assert_eq!(left.0, StatusCode::ACCEPTED, "{}", left.1);
    assert_eq!(right.0, StatusCode::ACCEPTED, "{}", right.1);
    assert_eq!(left.1["run_id"], right.1["run_id"]);
    assert_eq!(
        scoped_run_count(&fx.pool, fx.community_id, workflow_id).await,
        1
    );
    wait_for_kind9_count(&fx.http, &fx.workflow_owner, dest, 1).await;
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn same_key_different_payload_is_409() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "conflict-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("conflict-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("conflict-proj"),
        "conflict",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let key = unique("conflict-key");
    let (first_status, first_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &key, "one"),
    )
    .await;
    assert_eq!(first_status, StatusCode::ACCEPTED, "{first_body}");
    let (second_status, second_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &key, "two"),
    )
    .await;
    assert_eq!(second_status, StatusCode::CONFLICT, "{second_body}");
    assert_eq!(second_body["code"].as_str(), Some("idempotency_conflict"));
    wait_for_kind9_count(&fx.http, &fx.workflow_owner, dest, 1).await;
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn idempotency_key_never_appears_on_the_event_or_run_context() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "redact-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("redact-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("redact-proj"),
        "redact",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let yaml = dynamic_yaml("repo={{trigger.repository_name}} key={{trigger.idempotency_key}}");
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &yaml).await;
    let raw_key = format!("raw-secret-{}", Uuid::new_v4());
    let (status, body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &raw_key, "redact"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run_id = Uuid::parse_str(body["run_id"].as_str().expect("run_id")).unwrap();
    let events = wait_for_kind9_count(&fx.http, &fx.workflow_owner, dest, 1).await;
    let content = events[0]["content"].as_str().unwrap_or("");
    assert!(
        content.contains(&repo_d),
        "missing repo substitution: {content}"
    );
    assert!(
        content.contains("{{trigger.idempotency_key}}"),
        "placeholder must stay literal: {content}"
    );
    assert!(!content.contains(&raw_key), "raw key leaked into content");
    assert!(
        !event_serialized(&events[0]).contains(&raw_key),
        "raw key leaked into event"
    );
    let run = wait_for_run_status(&fx.pool, fx.community_id, run_id, "completed").await;
    assert!(
        !run_serialized(&run).contains(&raw_key),
        "raw key leaked into run context"
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn destination_channel_failure_matrix_posts_nothing() {
    let fx = setup_fixture().await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;

    let mut cases: Vec<(String, Option<Uuid>, Option<Uuid>)> = Vec::new();

    let missing_d = unique("missing-ch");
    let missing_coord = publish_repo(&fx.repo_owner, &missing_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("missing-ch-proj"),
        "missing-ch",
        &[missing_coord],
        None,
    )
    .await;
    cases.push((missing_d, None, None));

    let bad_d = unique("bad-uuid");
    let bad_coord = publish_repo(&fx.repo_owner, &bad_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("bad-uuid-proj"),
        "bad-uuid",
        &[bad_coord],
        Some("not-a-uuid"),
    )
    .await;
    cases.push((bad_d, None, None));

    let archived = create_channel(&fx.repo_owner, "archived-dest").await;
    add_member(&fx.repo_owner, archived, &fx.workflow_owner).await;
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
    cases.push((archived_d, Some(archived), None));

    let foreign_channel = Uuid::new_v4();
    let foreign_community = create_foreign_channel(&fx.pool, foreign_channel, &fx.repo_owner).await;
    let foreign_d = unique("foreign-repo");
    let foreign_coord = publish_repo(&fx.repo_owner, &foreign_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("foreign-proj"),
        "foreign",
        &[foreign_coord],
        Some(&foreign_channel.to_string()),
    )
    .await;
    cases.push((foreign_d, Some(foreign_channel), Some(foreign_community)));

    for (repo_d, dest, foreign) in cases {
        let key = unique("matrix-key");
        let (status, body) = post_hook(
            &fx.http,
            workflow_id,
            &secret,
            &hook_body(&repo_d, &key, "matrix"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{repo_d}: {body}");
        assert_eq!(body["code"].as_str(), Some("project_channel_invalid"));
        let run = workflow_run_for_key(&fx.pool, fx.community_id, workflow_id, &key).await;
        assert_eq!(run.status, "failed");
        assert!(
            run.route_snapshot.is_none(),
            "{repo_d} snapshot {:?}",
            run.route_snapshot
        );
        assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, fx.home).await, 0);
        if let Some(dest) = dest {
            let community = foreign.unwrap_or(fx.community_id);
            assert_eq!(sql_kind9_count(&fx.pool, community, dest).await, 0);
        }
    }
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn deterministic_failure_replays_until_a_new_key_is_used() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "replay-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("replay-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let key = unique("replay-key");
    let payload = hook_body(&repo_d, &key, "replay");
    let (first_status, first_body) = post_hook(&fx.http, workflow_id, &secret, &payload).await;
    assert_eq!(
        first_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{first_body}"
    );
    assert_eq!(first_body["code"].as_str(), Some("project_missing"));
    let first_run = workflow_run_for_key(&fx.pool, fx.community_id, workflow_id, &key).await;
    assert_eq!(first_run.status, "failed");

    publish_project(
        &fx.repo_owner,
        &unique("replay-proj"),
        "replay",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (retry_status, retry_body) = post_hook(&fx.http, workflow_id, &secret, &payload).await;
    assert_eq!(
        retry_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{retry_body}"
    );
    assert_eq!(retry_body["code"].as_str(), Some("project_missing"));
    let retry_run = workflow_run_for_key(&fx.pool, fx.community_id, workflow_id, &key).await;
    assert_eq!(retry_run.id, first_run.id);
    assert_eq!(retry_run.status, "failed");
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, dest).await, 0);

    let new_key = unique("replay-new");
    let (ok_status, ok_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &hook_body(&repo_d, &new_key, "replay"),
    )
    .await;
    assert_eq!(ok_status, StatusCode::ACCEPTED, "{ok_body}");
    wait_for_kind9_count(&fx.http, &fx.workflow_owner, dest, 1).await;
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn membership_revoked_during_delay_marks_same_run_route_stale() {
    let fx = setup_fixture().await;
    let channel_admin = Keys::generate();
    let dest = create_channel(&channel_admin, "stale-dest").await;
    add_member(&channel_admin, dest, &fx.workflow_owner).await;
    let repo_d = unique("stale-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("stale-proj"),
        "stale",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (workflow_id, secret) = define_webhook_workflow(
        &fx.http,
        &fx.workflow_owner,
        fx.home,
        delay_then_send_yaml(),
    )
    .await;
    let key = unique("stale-key");
    let payload = hook_body(&repo_d, &key, "stale");
    let (status, body) = post_hook(&fx.http, workflow_id, &secret, &payload).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run_id = Uuid::parse_str(body["run_id"].as_str().expect("run_id")).unwrap();
    remove_member(&channel_admin, dest, &fx.workflow_owner).await;
    let failed = wait_for_run_status(&fx.pool, fx.community_id, run_id, "failed").await;
    assert_eq!(failed.error_code.as_deref(), Some("route_stale"));
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, dest).await, 0);
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, fx.home).await, 0);

    let (retry_status, retry_body) = post_hook(&fx.http, workflow_id, &secret, &payload).await;
    assert_eq!(retry_status, StatusCode::ACCEPTED, "{retry_body}");
    let run_id_str = run_id.to_string();
    assert_eq!(retry_body["run_id"].as_str(), Some(run_id_str.as_str()));
    assert_eq!(retry_body["status"].as_str(), Some("failed"));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let again = wait_for_run_status(&fx.pool, fx.community_id, run_id, "failed").await;
    assert_eq!(again.status, "failed");
    assert_eq!(again.error_code.as_deref(), Some("route_stale"));
    assert_eq!(sql_kind9_count(&fx.pool, fx.community_id, dest).await, 0);
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn pre_admission_failures_create_no_run() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "pre-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("pre-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("pre-proj"),
        "pre",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (workflow_id, secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    let before = scoped_run_count(&fx.pool, fx.community_id, workflow_id).await;

    let (bad_json_status, bad_json_body) =
        post_hook_raw(&fx.http, workflow_id, &secret, "{not json").await;
    assert_eq!(bad_json_status, StatusCode::BAD_REQUEST, "{bad_json_body}");

    let (missing_status, missing_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &json!({ "repository_name": repo_d }),
    )
    .await;
    assert_eq!(
        missing_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{missing_body}"
    );
    assert_eq!(
        missing_body["code"].as_str(),
        Some("invalid_control_fields")
    );

    let (empty_status, empty_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &json!({ "repository_name": repo_d, "idempotency_key": "" }),
    )
    .await;
    assert_eq!(
        empty_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{empty_body}"
    );
    assert_eq!(empty_body["code"].as_str(), Some("invalid_control_fields"));

    let (type_status, type_body) = post_hook(
        &fx.http,
        workflow_id,
        &secret,
        &json!({ "repository_name": repo_d, "idempotency_key": 1 }),
    )
    .await;
    assert_eq!(type_status, StatusCode::UNPROCESSABLE_ENTITY, "{type_body}");
    assert_eq!(type_body["code"].as_str(), Some("invalid_control_fields"));

    let (secret_status, secret_body) = post_hook(
        &fx.http,
        workflow_id,
        "definitely-not-the-secret",
        &hook_body(&repo_d, &unique("pre-key"), "pre"),
    )
    .await;
    assert_eq!(secret_status, StatusCode::UNAUTHORIZED, "{secret_body}");

    assert_eq!(
        scoped_run_count(&fx.pool, fx.community_id, workflow_id).await,
        before
    );
}

#[tokio::test]
#[ignore = "requires a running relay"]
async fn static_and_dynamic_lifecycle_parse_order_is_preserved() {
    let fx = setup_fixture().await;
    let dest = create_channel(&fx.repo_owner, "life-dest").await;
    add_member(&fx.repo_owner, dest, &fx.workflow_owner).await;
    let repo_d = unique("life-repo");
    let coord = publish_repo(&fx.repo_owner, &repo_d).await;
    publish_project(
        &fx.repo_owner,
        &unique("life-proj"),
        "life",
        &[coord],
        Some(&dest.to_string()),
    )
    .await;
    let (static_id, static_secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, static_webhook_yaml()).await;
    let (dynamic_id, dynamic_secret) =
        define_webhook_workflow(&fx.http, &fx.workflow_owner, fx.home, &dynamic_yaml("hi")).await;
    disable_workflow(&fx.pool, fx.community_id, static_id).await;
    disable_workflow(&fx.pool, fx.community_id, dynamic_id).await;

    let static_before = scoped_run_count(&fx.pool, fx.community_id, static_id).await;
    let dynamic_before = scoped_run_count(&fx.pool, fx.community_id, dynamic_id).await;
    let (static_status, static_body) =
        post_hook_raw(&fx.http, static_id, &static_secret, "{not json").await;
    assert_eq!(static_status, StatusCode::BAD_REQUEST, "{static_body}");
    let static_err = static_body["error"].as_str().unwrap_or("");
    assert!(
        static_err.contains("invalid JSON body"),
        "static parse-first: {static_body}"
    );

    let (dynamic_status, dynamic_body) =
        post_hook_raw(&fx.http, dynamic_id, &dynamic_secret, "{not json").await;
    assert_eq!(dynamic_status, StatusCode::NOT_FOUND, "{dynamic_body}");
    assert_eq!(dynamic_body["error"].as_str(), Some("workflow not found"));

    assert_eq!(
        scoped_run_count(&fx.pool, fx.community_id, static_id).await,
        static_before
    );
    assert_eq!(
        scoped_run_count(&fx.pool, fx.community_id, dynamic_id).await,
        dynamic_before
    );
}
