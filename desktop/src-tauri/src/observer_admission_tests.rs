use super::*;
use crate::archive::{archive_candidates, ArchiveCandidate, MatchedScope, ScopeType};
use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use nostr::{EventBuilder, JsonUtil, Kind, Tag, Timestamp};
use std::sync::{Arc, RwLock};
use tauri::Manager;
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Clone)]
struct QueryFixture {
    events: Arc<RwLock<Vec<Event>>>,
    pause_metadata: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
struct Fixture {
    query: QueryFixture,
    task: tokio::task::JoinHandle<()>,
    relay: String,
    signer: Keys,
    agent: Keys,
    owner: Keys,
    viewer: Keys,
    channel: Uuid,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn signed(keys: &Keys, kind: u16, tags: Vec<Tag>, content: &str, time: u64) -> Event {
    EventBuilder::new(Kind::Custom(kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(keys)
        .unwrap()
}
impl Fixture {
    async fn new() -> Self {
        let signer = Keys::generate();
        let query = QueryFixture {
            events: Arc::new(RwLock::new(Vec::new())),
            pause_metadata: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        async fn query_events(
            State(f): State<QueryFixture>,
            headers: HeaderMap,
            Json(filters): Json<Vec<Value>>,
        ) -> Json<Vec<Event>> {
            assert!(headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Nostr "));
            if filters[0]["kinds"][0] == 39000
                && f.pause_metadata
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                f.entered.notify_one();
                f.release.notified().await;
            }
            let mut events: Vec<_> =
                f.events
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|event| {
                        filters.iter().any(|filter| {
                            filter["kinds"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|kind| kind.as_u64() == Some(event.kind.as_u16() as u64))
                                && filter.get("authors").is_none_or(|authors| {
                                    authors.as_array().unwrap().iter().any(|pk| {
                                        pk.as_str() == Some(event.pubkey.to_hex().as_str())
                                    })
                                })
                                && filter.get("#d").is_none_or(|values| {
                                    values.as_array().unwrap().iter().any(|v| {
                                        event.tags.iter().any(|t| {
                                            t.as_slice().first().map(String::as_str) == Some("d")
                                                && t.content() == v.as_str()
                                        })
                                    })
                                })
                        })
                    })
                    .cloned()
                    .collect();
            events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
            if let Some(limit) = filters[0]["limit"].as_u64() {
                events.truncate(limit as usize);
            }
            Json(events)
        }
        let pk = signer.public_key().to_hex();
        let router = Router::new()
            .route(
                "/",
                get(move || {
                    let pk = pk.clone();
                    async move { Json(json!({"self":pk})) }
                }),
            )
            .route("/query", post(query_events))
            .with_state(query.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let f = Self {
            query,
            task,
            relay,
            signer,
            agent: Keys::generate(),
            owner: Keys::generate(),
            viewer: Keys::generate(),
            channel: Uuid::new_v4(),
        };
        let auth =
            buzz_sdk_pkg::nip_oa::compute_auth_tag(&f.owner, &f.agent.public_key(), "kind=0")
                .unwrap();
        f.push(signed(
            &f.agent,
            0,
            vec![buzz_sdk_pkg::nip_oa::parse_auth_tag(&auth).unwrap()],
            "{}",
            1,
        ));
        f.push(signed(
            &f.signer,
            39000,
            vec![
                Tag::parse(["d", &f.channel.to_string()]).unwrap(),
                Tag::parse(["t", "stream"]).unwrap(),
            ],
            "",
            1,
        ));
        f.roster(true, 1);
        f.policy("anyone", 1);
        f
    }
    fn push(&self, event: Event) {
        self.query.events.write().unwrap().push(event);
    }
    fn roster(&self, include_viewer: bool, time: u64) {
        let mut tags = vec![
            Tag::parse(["d", &self.channel.to_string()]).unwrap(),
            Tag::public_key(self.agent.public_key()),
        ];
        if include_viewer {
            tags.push(Tag::public_key(self.viewer.public_key()));
        }
        self.push(signed(&self.signer, 39002, tags, "", time));
    }
    fn policy(&self, mode: &str, time: u64) {
        self.push(signed(
            &self.owner,
            30177,
            vec![Tag::parse(["d", &self.agent.public_key().to_hex()]).unwrap()],
            &json!({"name":"test","parallelism":1,"respond_to":mode}).to_string(),
            time,
        ));
    }
    fn frame(&self, payload: Value) -> Event {
        let ciphertext = buzz_core_pkg::observer::encrypt_observer_payload(
            &self.agent,
            &self.viewer.public_key(),
            &payload,
        )
        .unwrap();
        buzz_sdk_pkg::build_channel_agent_observer_frame(
            &self.viewer.public_key().to_hex(),
            &self.agent.public_key().to_hex(),
            self.channel,
            &ciphertext,
        )
        .unwrap()
        .sign_with_keys(&self.agent)
        .unwrap()
    }
    fn payload(&self, text: &str) -> Value {
        json!({"kind":"message","channelId":self.channel.to_string(),"text":text})
    }
    async fn archive(&self, state: &AppState, event: &Event) -> crate::archive::ArchiveBatchResult {
        archive_candidates(
            state,
            vec![ArchiveCandidate {
                raw_event_json: event.as_json(),
                matched_scope: MatchedScope {
                    scope_type: ScopeType::OwnerP,
                    scope_value: self.viewer.public_key().to_hex(),
                },
            }],
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn display_and_archive_enforce_current_signed_membership_and_policy() {
    let _serial = crate::relay_admission::TEST_SERIAL.lock().await;
    crate::relay_admission::reset_rate_limit_gate();
    let f = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut state = crate::app_state::build_app_state();
    *state.keys.lock().unwrap() = f.viewer.clone();
    *state.relay_url_override.lock().unwrap() = Some(f.relay.clone());
    state.archive_db = crate::archive::ArchiveDb::with_test_path(dir.path().join("archive.db"));
    let identity = f.viewer.public_key().to_hex();
    let relay = f.relay.clone();
    state
        .archive_db
        .with_conn(move |conn| {
            crate::archive::store::upsert_save_subscription(
                conn, &identity, &relay, "owner_p", &identity, "[24200]", 0,
            )
        })
        .await
        .unwrap();
    let app = tauri::test::mock_builder()
        .manage(state)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let event = f.frame(f.payload("eligible"));
    assert!(
        crate::commands::decrypt_observer_event(event.as_json(), None, app.state())
            .await
            .is_ok()
    );
    assert_eq!(
        f.archive(&app.state::<AppState>(), &event).await.persisted,
        1
    );

    f.roster(false, 2);
    let event = f.frame(f.payload("removed"));
    assert!(
        crate::commands::decrypt_observer_event(event.as_json(), None, app.state())
            .await
            .is_err()
    );
    assert_eq!(f.archive(&app.state::<AppState>(), &event).await.dropped, 1);
    f.push(signed(
        &f.signer,
        39002,
        vec![
            Tag::parse(["d", &f.channel.to_string()]).unwrap(),
            Tag::public_key(f.viewer.public_key()),
        ],
        "",
        3,
    ));
    let event = f.frame(f.payload("agent removed"));
    assert!(
        crate::commands::decrypt_observer_event(event.as_json(), None, app.state())
            .await
            .is_err()
    );
    assert_eq!(f.archive(&app.state::<AppState>(), &event).await.dropped, 1);
    f.roster(true, 4);
    f.policy("owner-only", 2);
    let event = f.frame(f.payload("policy restricted"));
    assert!(
        crate::commands::decrypt_observer_event(event.as_json(), None, app.state())
            .await
            .is_err()
    );
    assert_eq!(f.archive(&app.state::<AppState>(), &event).await.dropped, 1);

    f.policy("anyone", 3);
    let valid_batch = f.frame(json!({"kind":"batch","channelId":f.channel.to_string(),"payload":{"events":[f.payload("batch member")]}}));
    assert!(
        crate::commands::decrypt_observer_event(valid_batch.as_json(), None, app.state())
            .await
            .is_ok()
    );
    assert_eq!(
        f.archive(&app.state::<AppState>(), &valid_batch)
            .await
            .persisted,
        1
    );
    let bad_batch = f.frame(json!({"kind":"batch","channelId":f.channel.to_string(),"payload":{"events":[f.payload("valid item"),{"kind":"message","channelId":Uuid::new_v4().to_string()}]}}));
    let mut bad_route = f.frame(f.payload("bad route"));
    let mut tags = bad_route.tags.to_vec();
    tags.push(Tag::parse(["h", &f.channel.to_string()]).unwrap());
    bad_route = signed(&f.agent, 24200, tags, &bad_route.content, 5);
    let mut invalid_signature = serde_json::to_value(f.frame(f.payload("bad signature"))).unwrap();
    invalid_signature["sig"] = json!("0".repeat(128));
    let invalid_signature = Event::from_json(invalid_signature.to_string()).unwrap();
    for event in [bad_batch, bad_route, invalid_signature] {
        assert!(
            crate::commands::decrypt_observer_event(event.as_json(), None, app.state())
                .await
                .is_err()
        );
        assert_eq!(f.archive(&app.state::<AppState>(), &event).await.dropped, 1);
    }

    f.query
        .pause_metadata
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let event = f.frame(f.payload("context changed"));
    let display = crate::commands::decrypt_observer_event(event.as_json(), None, app.state());
    tokio::pin!(display);
    tokio::select! { _ = f.query.entered.notified() => {}, result = &mut display => panic!("admission returned before context change: {result:?}") }
    *app.state::<AppState>().keys.lock().unwrap() = Keys::generate();
    f.query.release.notify_one();
    assert!(display.await.unwrap_err().contains("context changed"));

    *app.state::<AppState>().keys.lock().unwrap() = f.viewer.clone();
    f.query
        .pause_metadata
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let event = f.frame(f.payload("archive context changed"));
    let state = app.state::<AppState>();
    let archive = archive_candidates(
        &state,
        vec![ArchiveCandidate {
            raw_event_json: event.as_json(),
            matched_scope: MatchedScope {
                scope_type: ScopeType::OwnerP,
                scope_value: f.viewer.public_key().to_hex(),
            },
        }],
    );
    tokio::pin!(archive);
    tokio::select! { _ = f.query.entered.notified() => {}, result = &mut archive => panic!("archive returned before context change: {result:?}") }
    *state.relay_url_override.lock().unwrap() = Some("ws://127.0.0.1:1".into());
    f.query.release.notify_one();
    assert!(archive.await.unwrap_err().contains("context changed"));
}
