use super::*;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use nostr::{Event, EventBuilder, Kind, Tag, Timestamp};
use std::sync::{Arc, RwLock};

struct Fixture {
    rest: relay::RestClient,
    events: Arc<RwLock<Vec<Event>>>,
    task: tokio::task::JoinHandle<()>,
    signer: nostr::Keys,
    channel: Uuid,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn signed(keys: &nostr::Keys, kind: u16, tags: Vec<Tag>, content: &str, time: u64) -> Event {
    EventBuilder::new(Kind::Custom(kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(keys)
        .unwrap()
}
fn profile(agent: &nostr::Keys, owner: &nostr::Keys) -> Event {
    let auth = buzz_sdk::nip_oa::compute_auth_tag(owner, &agent.public_key(), "kind=0").unwrap();
    signed(
        agent,
        0,
        vec![buzz_sdk::nip_oa::parse_auth_tag(&auth).unwrap()],
        "{}",
        1,
    )
}
impl Fixture {
    async fn new(agent: &nostr::Keys, owner: &nostr::Keys, viewers: &[PublicKey]) -> Self {
        let signer = nostr::Keys::generate();
        let channel = Uuid::new_v4();
        let events = Arc::new(RwLock::new(vec![profile(agent, owner)]));
        let relay_key = signer.public_key().to_hex();
        async fn query(
            State(events): State<Arc<RwLock<Vec<Event>>>>,
            Json(filters): Json<Vec<serde_json::Value>>,
        ) -> Json<Vec<Event>> {
            let mut found: Vec<_> = events
                .read()
                .unwrap()
                .iter()
                .filter(|event| {
                    filters.iter().any(|filter| {
                        let kinds = filter["kinds"].as_array().unwrap();
                        kinds
                            .iter()
                            .any(|kind| kind.as_u64() == Some(event.kind.as_u16() as u64))
                            && filter.get("authors").is_none_or(|authors| {
                                authors.as_array().unwrap().iter().any(|author| {
                                    author.as_str() == Some(event.pubkey.to_hex().as_str())
                                })
                            })
                            && filter.get("#d").is_none_or(|values| {
                                values.as_array().unwrap().iter().any(|v| {
                                    event.tags.iter().any(|tag| {
                                        tag.as_slice().first().map(String::as_str) == Some("d")
                                            && tag.content() == v.as_str()
                                    })
                                })
                            })
                    })
                })
                .cloned()
                .collect();
            found.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
            Json(found)
        }
        let app = Router::new()
            .route(
                "/",
                get(move || {
                    let relay_key = relay_key.clone();
                    async move { Json(serde_json::json!({"self":relay_key})) }
                }),
            )
            .route("/query", post(query))
            .with_state(events.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let fixture = Self {
            rest: relay::RestClient {
                http: reqwest::Client::new(),
                base_url: format!("http://{addr}"),
                keys: agent.clone(),
                auth_tag_json: None,
            },
            events,
            task,
            signer,
            channel,
        };
        fixture.roster(agent, viewers, 1);
        fixture.policy(agent, owner, "anyone", 1);
        fixture.metadata("stream", 1);
        fixture
    }
    fn roster(&self, agent: &nostr::Keys, viewers: &[PublicKey], time: u64) {
        let mut tags = vec![
            Tag::parse(["d", &self.channel.to_string()]).unwrap(),
            Tag::public_key(agent.public_key()),
        ];
        tags.extend(viewers.iter().copied().map(Tag::public_key));
        self.events
            .write()
            .unwrap()
            .push(signed(&self.signer, 39002, tags, "", time));
    }
    fn policy(&self, agent: &nostr::Keys, owner: &nostr::Keys, mode: &str, time: u64) {
        self.events.write().unwrap().push(signed(
            owner,
            30177,
            vec![Tag::parse(["d", &agent.public_key().to_hex()]).unwrap()],
            &format!(r#"{{"name":"test","parallelism":1,"respond_to":"{mode}"}}"#),
            time,
        ));
    }
    fn metadata(&self, kind: &str, time: u64) {
        self.events.write().unwrap().push(signed(
            &self.signer,
            39000,
            vec![
                Tag::parse(["d", &self.channel.to_string()]).unwrap(),
                Tag::parse(["t", kind]).unwrap(),
            ],
            "",
            time,
        ));
    }
}
fn emit(observer: &observer::ObserverHandle, channel: Uuid, kind: &str) {
    observer.emit(
        kind,
        None,
        &observer::context_for(Some(channel), None, None),
        serde_json::json!({"marker":"shared"}),
    );
}

#[tokio::test]
async fn publisher_encrypts_each_member_copy_and_keeps_owner_controls_legacy() {
    let agent = nostr::Keys::generate();
    let owner = nostr::Keys::generate();
    let viewer_a = nostr::Keys::generate();
    let viewer_b = nostr::Keys::generate();
    let fixture = Fixture::new(
        &agent,
        &owner,
        &[viewer_a.public_key(), viewer_b.public_key()],
    )
    .await;
    let observer = observer::ObserverHandle::in_process();
    let (publisher, mut rx) = RelayEventPublisher::test_pair();
    emit(&observer, fixture.channel, "acp_read");
    emit(&observer, fixture.channel, "control_result");
    let task = tokio::spawn(run_relay_observer_publisher(
        observer.snapshot(),
        observer.subscribe(),
        publisher,
        agent.clone(),
        owner.public_key(),
        fixture.rest.clone(),
    ));
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(4), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let route = buzz_core::observer::parse_observer_route(&frame)
            .unwrap()
            .unwrap();
        assert_eq!(
            route.channel_id,
            Some(fixture.channel),
            "channel telemetry must never use a legacy owner copy"
        );
        let viewer = if route.recipient == viewer_a.public_key() {
            &viewer_a
        } else {
            assert_eq!(route.recipient, viewer_b.public_key());
            &viewer_b
        };
        let value: serde_json::Value = decrypt_observer_payload(viewer, &frame).unwrap();
        buzz_core::observer::validate_observer_channel_payload(
            &value,
            &fixture.channel.to_string(),
        )
        .unwrap();
        assert_eq!(value["kind"], "acp_read");
        assert!(decrypt_observer_payload::<serde_json::Value>(&owner, &frame).is_err());
    }
    let control = tokio::time::timeout(Duration::from_secs(4), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        buzz_core::observer::parse_observer_route(&control)
            .unwrap()
            .unwrap()
            .channel_id,
        None
    );
    assert_eq!(
        decrypt_observer_payload::<serde_json::Value>(&owner, &control).unwrap()["kind"],
        "control_result"
    );
    task.abort();
}

#[tokio::test]
async fn publisher_rechecks_pending_recipient_and_never_falls_back_when_revoked() {
    let agent = nostr::Keys::generate();
    let owner = nostr::Keys::generate();
    let a = nostr::Keys::generate();
    let b = nostr::Keys::generate();
    let fixture = Fixture::new(&agent, &owner, &[a.public_key(), b.public_key()]).await;
    let observer = observer::ObserverHandle::in_process();
    let (publisher, mut rx) = RelayEventPublisher::test_pair();
    emit(&observer, fixture.channel, "acp_read");
    let task = tokio::spawn(run_relay_observer_publisher(
        observer.snapshot(),
        observer.subscribe(),
        publisher,
        agent.clone(),
        owner.public_key(),
        fixture.rest.clone(),
    ));
    let first = tokio::time::timeout(Duration::from_secs(4), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(buzz_core::observer::parse_observer_route(&first)
        .unwrap()
        .unwrap()
        .channel_id
        .is_some());
    fixture.policy(&agent, &owner, "nobody", 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .is_err(),
        "pending recipient must be rechecked after policy revocation"
    );
    fixture.policy(&agent, &owner, "anyone", 3);
    fixture.metadata("dm", 2);
    emit(&observer, fixture.channel, "acp_read");
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .is_err(),
        "DM does not share with unrelated members even under anyone"
    );
    task.abort();
}

#[tokio::test]
async fn publisher_rechecks_roster_and_rejects_invalid_latest_signed_heads() {
    let agent = nostr::Keys::generate();
    let owner = nostr::Keys::generate();
    let a = nostr::Keys::generate();
    let b = nostr::Keys::generate();
    let fixture = Fixture::new(&agent, &owner, &[a.public_key(), b.public_key()]).await;
    let observer = observer::ObserverHandle::in_process();
    let (publisher, mut rx) = RelayEventPublisher::test_pair();
    emit(&observer, fixture.channel, "acp_read");
    let task = tokio::spawn(run_relay_observer_publisher(
        observer.snapshot(),
        observer.subscribe(),
        publisher,
        agent.clone(),
        owner.public_key(),
        fixture.rest.clone(),
    ));
    let first = tokio::time::timeout(Duration::from_secs(4), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let recipient = buzz_core::observer::parse_observer_route(&first)
        .unwrap()
        .unwrap()
        .recipient;
    fixture.roster(&agent, &[recipient], 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .is_err(),
        "removed pending recipient must receive no copy"
    );
    fixture.roster(&agent, &[a.public_key(), b.public_key()], 3);
    // An invalid latest head must not restore an older eligible roster.
    let mut forged = signed(
        &fixture.signer,
        39002,
        vec![
            Tag::parse(["d", &fixture.channel.to_string()]).unwrap(),
            Tag::public_key(agent.public_key()),
            Tag::public_key(a.public_key()),
        ],
        "",
        4,
    );
    forged.content = "changed after signature".into();
    fixture.events.write().unwrap().push(forged);
    emit(&observer, fixture.channel, "acp_read");
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .is_err(),
        "invalid latest roster must fail closed without owner fallback"
    );
    task.abort();
}
