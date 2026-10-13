//! Production writer regressions with authoritative database state.
use super::*;
use buzz_core::{
    channel::{ChannelType, ChannelVisibility, MemberRole},
    event::StoredEvent,
};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, Tag, Timestamp};
use std::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

struct Fixture {
    state: Arc<AppState>,
    tenant: TenantContext,
    owner: Keys,
    agent: Keys,
    viewer: Keys,
    channel: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let state = crate::state::tests::test_state().await;
        let host = format!("observer-writer-{}.test", Uuid::new_v4().simple());
        let community = state
            .db
            .ensure_configured_community(&host)
            .await
            .unwrap()
            .id;
        let tenant = TenantContext::resolved(community, host);
        let owner = Keys::generate();
        let agent = Keys::generate();
        let viewer = Keys::generate();
        for keys in [&owner, &agent, &viewer] {
            state
                .db
                .ensure_user(community, keys.public_key().as_bytes())
                .await
                .unwrap();
        }
        state
            .db
            .set_agent_owner(
                community,
                agent.public_key().as_bytes(),
                owner.public_key().as_bytes(),
            )
            .await
            .unwrap();
        let channel = state
            .db
            .create_channel(
                community,
                "observer-writer",
                ChannelType::Stream,
                ChannelVisibility::Open,
                None,
                owner.public_key().as_bytes(),
                None,
            )
            .await
            .unwrap()
            .id;
        for keys in [&agent, &viewer] {
            state
                .db
                .add_member(
                    community,
                    channel,
                    keys.public_key().as_bytes(),
                    MemberRole::Member,
                    Some(owner.public_key().as_bytes()),
                )
                .await
                .unwrap();
        }
        let auth =
            buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=0").unwrap();
        let profile = EventBuilder::new(Kind::Metadata, "{}")
            .tags([buzz_sdk::nip_oa::parse_auth_tag(&auth).unwrap()])
            .sign_with_keys(&agent)
            .unwrap();
        state
            .db
            .insert_event(community, &profile, None)
            .await
            .unwrap();
        let fixture = Self {
            state,
            tenant,
            owner,
            agent,
            viewer,
            channel,
        };
        fixture.set_policy("anyone", 1).await;
        fixture
    }
    async fn set_policy(&self, mode: &str, timestamp: u64) {
        let policy = EventBuilder::new(
            Kind::Custom(30177),
            serde_json::json!({"name":"test","parallelism":1,"respond_to":mode}).to_string(),
        )
        .tags([Tag::parse(["d", &self.agent.public_key().to_hex()]).unwrap()])
        .custom_created_at(Timestamp::from(timestamp))
        .sign_with_keys(&self.owner)
        .unwrap();
        self.state
            .db
            .insert_event(self.tenant.community(), &policy, None)
            .await
            .unwrap();
    }
    fn event(&self) -> Event {
        let content = buzz_core::observer::encrypt_observer_payload(
            &self.agent,
            &self.viewer.public_key(),
            &serde_json::json!({"kind":"turn_started","channelId":self.channel.to_string()}),
        )
        .unwrap();
        EventBuilder::new(Kind::Custom(24200), content)
            .tags([
                Tag::public_key(self.viewer.public_key()),
                Tag::parse(["agent", &self.agent.public_key().to_hex()]).unwrap(),
                Tag::parse(["frame", "telemetry"]).unwrap(),
                Tag::parse(["h", &self.channel.to_string()]).unwrap(),
            ])
            .sign_with_keys(&self.agent)
            .unwrap()
    }
    fn connection(&self) -> (Arc<ConnectionState>, mpsc::Receiver<WsMessage>) {
        let mut test = tests::test_conn(
            AuthState::Authenticated(buzz_auth::AuthContext {
                pubkey: self.viewer.public_key(),
                scopes: vec![],
                channel_ids: None,
                auth_method: buzz_auth::AuthMethod::Nip42,
                agent_owner_pubkey: None,
            }),
            None,
        );
        Arc::get_mut(&mut test.conn).unwrap().tenant = self.tenant.clone();
        self.state.conn_manager.register(
            test.conn.conn_id,
            test.conn.send_tx.clone(),
            test.conn.ctrl_tx.clone(),
            test.conn.terminal_ctrl_tx.clone(),
            None,
            test.conn.cancel.clone(),
            self.tenant.community(),
            test.conn.backpressure_count.clone(),
            test.conn.subscriptions.clone(),
            3,
            test.conn.community_control.clone(),
        );
        self.state.conn_manager.set_authenticated_pubkey(
            test.conn.conn_id,
            self.viewer.public_key().to_bytes().to_vec(),
        );
        self.state.sub_registry.register_scoped(
            self.tenant.community(),
            test.conn.conn_id,
            "observer".into(),
            vec![Filter::new()
                .kind(Kind::Custom(24200))
                .pubkey(self.viewer.public_key())],
            None,
        );
        (test.conn, test.send_rx)
    }
}
#[derive(Default)]
struct BlockState {
    ready: AtomicBool,
    waker: StdMutex<Option<Waker>>,
    messages: StdMutex<Vec<WsMessage>>,
    polled: tokio::sync::Notify,
    delivered: tokio::sync::Notify,
}
struct BlockedSink(Arc<BlockState>);
impl Sink<WsMessage> for BlockedSink {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.polled.notify_one();
        if self.0.ready.load(Ordering::SeqCst) {
            Poll::Ready(Ok(()))
        } else {
            *self.0.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
    fn start_send(self: Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
        self.0.messages.lock().unwrap().push(item);
        self.0.delivered.notify_one();
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

mod observer_postgres_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn queued_shared_observer_rechecks_revocation_after_socket_readiness_local_and_redis() {
        for redis in [false, true] {
            for revoke_policy in [false, true] {
                let f = Fixture::new().await;
                let (conn, rx) = f.connection();
                let event = f.event();
                if redis {
                    crate::handlers::event::fan_out_pubsub_event(
                        &f.state,
                        buzz_pubsub::ChannelEvent {
                            community_id: f.tenant.community(),
                            topic: buzz_pubsub::EventTopic::Global,
                            event,
                        },
                    )
                    .await;
                } else {
                    crate::handlers::event::fan_out_event_to_local_subscribers(
                        &f.state,
                        f.tenant.community(),
                        &StoredEvent::new(event, None),
                    )
                    .await;
                }
                assert_eq!(
                    rx.len(),
                    1,
                    "eligible frame must reach real local/Redis outbound queue"
                );
                let blocked = Arc::new(BlockState::default());
                let (_ctrl_tx, ctrl_rx) = mpsc::channel(8);
                let (_terminal_tx, terminal_rx) = mpsc::channel(1);
                let (_restart_tx, restart_rx) = mpsc::channel(1);
                let (_, disconnect) = watch::channel(None);
                let cancel = conn.cancel.clone();
                let data_tx = conn.send_tx.clone();
                let task = tokio::spawn(send_loop_inner(
                    BlockedSink(blocked.clone()),
                    rx,
                    ctrl_rx,
                    terminal_rx,
                    restart_rx,
                    WriterContext {
                        cancel: cancel.clone(),
                        disconnect_reason: disconnect,
                        observer_context: Some((f.state.clone(), conn)),
                    },
                ));
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    blocked.polled.notified(),
                )
                .await
                .unwrap();
                if revoke_policy {
                    f.set_policy("nobody", 2).await;
                } else {
                    f.state
                        .db
                        .remove_member(
                            f.tenant.community(),
                            f.channel,
                            f.viewer.public_key().as_bytes(),
                            f.owner.public_key().as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                blocked.ready.store(true, Ordering::SeqCst);
                if let Some(waker) = blocked.waker.lock().unwrap().take() {
                    waker.wake();
                }
                // A normal data frame proves the writer has processed the revoked frame.
                data_tx
                    .send(WsMessage::Text(r#"["NOTICE","after-revoke"]"#.into()))
                    .await
                    .unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(10),async {
                    loop {
                        if blocked.messages.lock().unwrap().iter().any(|message|matches!(message,WsMessage::Text(text) if text.contains("after-revoke"))) { break; }
                        blocked.delivered.notified().await;
                    }
                }).await.unwrap();
                cancel.cancel();
                tokio::time::timeout(std::time::Duration::from_secs(10), task)
                    .await
                    .unwrap()
                    .unwrap();
                let frames = blocked.messages.lock().unwrap();
                assert!(
                    frames.iter().all(
                        |msg| !matches!(msg,WsMessage::Text(text) if text.contains("\"EVENT\""))
                    ),
                    "revoked queued observer reached sink (redis={redis},policy={revoke_policy})"
                );
            }
        }
    }
}

mod external_infra_tests {
    use super::*;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message},
        MaybeTlsStream, WebSocketStream,
    };
    type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    struct LocalServer(tokio::task::JoinHandle<()>);
    impl Drop for LocalServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn live_state() -> Arc<AppState> {
        let mut config = crate::config::Config::for_test();
        config.require_relay_membership = false;
        config.require_auth_token = false;
        config.read_database_url = None;
        config.redis_url = std::env::var("REDIS_URL").expect("isolated REDIS_URL required");
        config.relay_url = "ws://127.0.0.1:5844".into();
        let pool = sqlx::PgPool::connect(&config.database_url).await.unwrap();
        let db = buzz_db::Db::from_pool(pool.clone());
        let redis_pool = deadpool_redis::Config::from_url(&config.redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let pubsub = Arc::new(
            buzz_pubsub::PubSubManager::new(&config.redis_url, redis_pool.clone())
                .await
                .unwrap(),
        );
        let audit = buzz_audit::AuditService::new(pool.clone());
        let auth = buzz_auth::AuthService::new(config.auth.clone());
        let search = buzz_search::SearchService::new(pool.clone());
        let workflow = Arc::new(buzz_workflow::WorkflowEngine::new(
            db.clone(),
            buzz_workflow::WorkflowConfig::default(),
        ));
        let media = buzz_media::MediaStorage::new(&config.media).unwrap();
        let (state, _audit_shutdown) = AppState::new(
            config,
            db,
            redis_pool,
            audit,
            pubsub,
            auth,
            search,
            workflow,
            Keys::generate(),
            media,
        );
        Arc::new(state)
    }

    struct Client {
        socket: Socket,
        keys: Keys,
    }
    impl Client {
        async fn connect(f: &Fixture, keys: &Keys) -> Self {
            let mut request = "ws://127.0.0.1:5844/".into_client_request().unwrap();
            request
                .headers_mut()
                .insert("host", f.tenant.host().parse().unwrap());
            let (socket, _) = connect_async(request).await.unwrap();
            let mut client = Self {
                socket,
                keys: keys.clone(),
            };
            let challenge = client.until(|value| value[0] == "AUTH").await;
            let relay_url: nostr::RelayUrl =
                crate::api::bridge::nip42_expected_relay_url(&f.state.config.relay_url, &f.tenant)
                    .parse()
                    .unwrap();
            let auth = EventBuilder::auth(challenge[1].as_str().unwrap(), relay_url)
                .sign_with_keys(keys)
                .unwrap();
            let id = auth.id.to_hex();
            client.send(serde_json::json!(["AUTH", auth])).await;
            let ack = client
                .until(|value| value[0] == "OK" && value[1] == id)
                .await;
            assert_eq!(ack[2], true, "fresh test identity must authenticate: {ack}");
            client
        }
        async fn send(&mut self, value: serde_json::Value) {
            self.socket
                .send(Message::Text(value.to_string().into()))
                .await
                .unwrap();
        }
        async fn packet(&mut self) -> serde_json::Value {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    match self.socket.next().await.unwrap().unwrap() {
                        Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                        Message::Ping(bytes) => {
                            self.socket.send(Message::Pong(bytes)).await.unwrap()
                        }
                        Message::Pong(_) => {}
                        other => panic!("unexpected local WebSocket frame: {other:?}"),
                    }
                }
            })
            .await
            .expect("local relay response deadline")
        }
        async fn until(
            &mut self,
            matches: impl Fn(&serde_json::Value) -> bool,
        ) -> serde_json::Value {
            loop {
                let value = self.packet().await;
                if matches(&value) {
                    return value;
                }
            }
        }
        async fn subscribe(&mut self) {
            self.send(serde_json::json!(["REQ", "observer", {"kinds":[24200],"#p":[self.keys.public_key().to_hex()]}])).await;
            self.until(|value| value[0] == "EOSE" && value[1] == "observer")
                .await;
        }
        async fn publish(&mut self, event: Event) -> bool {
            let id = event.id.to_hex();
            self.send(serde_json::json!(["EVENT", event])).await;
            let ack = self.until(|value| value[0] == "OK" && value[1] == id).await;
            ack[2].as_bool().unwrap()
        }
        async fn activity(&mut self) -> Event {
            let packet = self
                .until(|value| value[0] == "EVENT" && value[1] == "observer")
                .await;
            serde_json::from_value(packet[2].clone()).unwrap()
        }
        async fn no_activity_barrier(&mut self) {
            let marker = Uuid::new_v4().to_string();
            self.send(serde_json::json!(["REQ",marker,{"kinds":[0],"authors":[self.keys.public_key().to_hex()]}])).await;
            loop {
                let value = self.packet().await;
                assert!(
                    !(value[0] == "EVENT" && value[1] == "observer"),
                    "denied activity reached the actual connection writer"
                );
                if value[0] == "EOSE" && value[1] == marker {
                    break;
                }
            }
        }
    }
    fn telemetry(
        f: &Fixture,
        recipient: &Keys,
        payload: &serde_json::Value,
        shared: bool,
    ) -> Event {
        let encrypted = buzz_core::observer::encrypt_observer_payload(
            &f.agent,
            &recipient.public_key(),
            payload,
        )
        .unwrap();
        let builder = if shared {
            buzz_sdk::build_channel_agent_observer_frame(
                &recipient.public_key().to_hex(),
                &f.agent.public_key().to_hex(),
                f.channel,
                &encrypted,
            )
            .unwrap()
        } else {
            buzz_sdk::build_agent_observer_frame(
                &recipient.public_key().to_hex(),
                &f.agent.public_key().to_hex(),
                "telemetry",
                &encrypted,
            )
            .unwrap()
        };
        builder.sign_with_keys(&f.agent).unwrap()
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL, Redis, and local network"]
    async fn shared_observer_websocket_e2e() {
        let mut f = Fixture::new().await;
        f.state = live_state().await;
        let viewer_two = Keys::generate();
        let denied = Keys::generate();
        for keys in [&viewer_two, &denied] {
            f.state
                .db
                .ensure_user(f.tenant.community(), keys.public_key().as_bytes())
                .await
                .unwrap();
        }
        f.state
            .db
            .add_member(
                f.tenant.community(),
                f.channel,
                viewer_two.public_key().as_bytes(),
                MemberRole::Member,
                Some(f.owner.public_key().as_bytes()),
            )
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:5844")
            .await
            .expect("port 5844 must be free; do not select a different port");
        let router = crate::router::build_router(f.state.clone());
        let _server = LocalServer(tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        }));
        let mut agent = Client::connect(&f, &f.agent).await;
        let mut owner = Client::connect(&f, &f.owner).await;
        let mut one = Client::connect(&f, &f.viewer).await;
        let mut two = Client::connect(&f, &viewer_two).await;
        let mut stranger = Client::connect(&f, &denied).await;
        for client in [&mut agent, &mut owner, &mut one, &mut two, &mut stranger] {
            client.subscribe().await;
        }
        let timestamp = chrono::Utc::now().to_rfc3339();
        let envelope = |seq: u64, kind: &str, payload: serde_json::Value| {
            serde_json::json!({
                "seq":seq,"timestamp":timestamp,"kind":kind,"agentIndex":0,
                "channelId":f.channel.to_string(),"sessionId":"shared-session",
                "turnId":"shared-turn","startedAt":timestamp,"payload":payload
            })
        };
        let payload = envelope(
            4,
            "batch",
            serde_json::json!({"events":[
                envelope(1,"turn_started",serde_json::json!({"prompt":"shared Activity fixture"})),
                envelope(2,"acp_read",serde_json::json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"shared-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Thinking about the shared turn"}}}})),
                envelope(3,"acp_read",serde_json::json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"shared-session","update":{"sessionUpdate":"tool_call","toolCallId":"shared-tool","title":"Fixture tool","kind":"execute","status":"completed","content":[]}}})),
                envelope(4,"turn_completed",serde_json::json!({"stopReason":"end_turn"}))
            ]}),
        );
        for (client, keys) in [(&mut one, &f.viewer), (&mut two, &viewer_two)] {
            assert!(agent.publish(telemetry(&f, keys, &payload, true)).await);
            let event = client.activity().await;
            event.verify().unwrap();
            assert_eq!(
                buzz_core::observer::parse_observer_route(&event)
                    .unwrap()
                    .unwrap()
                    .channel_id,
                Some(f.channel)
            );
            assert_eq!(
                buzz_core::observer::decrypt_observer_payload::<serde_json::Value>(keys, &event)
                    .unwrap(),
                payload
            );
        }
        assert!(!agent.publish(telemetry(&f, &denied, &payload, true)).await);
        stranger.no_activity_barrier().await;
        f.state
            .db
            .remove_member(
                f.tenant.community(),
                f.channel,
                f.viewer.public_key().as_bytes(),
                f.owner.public_key().as_bytes(),
            )
            .await
            .unwrap();
        assert!(
            !agent
                .publish(telemetry(&f, &f.viewer, &payload, true))
                .await
        );
        one.no_activity_barrier().await;
        assert!(
            agent
                .publish(telemetry(&f, &viewer_two, &payload, true))
                .await
        );
        assert_eq!(
            buzz_core::observer::decrypt_observer_payload::<serde_json::Value>(
                &viewer_two,
                &two.activity().await
            )
            .unwrap(),
            payload
        );
        f.set_policy("nobody", 2).await;
        assert!(
            !agent
                .publish(telemetry(&f, &viewer_two, &payload, true))
                .await
        );
        two.no_activity_barrier().await;
        // Legacy owner telemetry and controls remain operational after shared revocation.
        assert!(
            agent
                .publish(telemetry(&f, &f.owner, &payload, false))
                .await
        );
        assert_eq!(
            buzz_core::observer::decrypt_observer_payload::<serde_json::Value>(
                &f.owner,
                &owner.activity().await
            )
            .unwrap(),
            payload
        );
        let encrypted = buzz_core::observer::encrypt_observer_payload(
            &f.owner,
            &f.agent.public_key(),
            &serde_json::json!({"type":"test-control"}),
        )
        .unwrap();
        let builder = buzz_sdk::build_agent_observer_frame(
            &f.agent.public_key().to_hex(),
            &f.agent.public_key().to_hex(),
            "control",
            &encrypted,
        )
        .unwrap();
        let channel_control = builder
            .clone()
            .tag(Tag::parse(["h", &f.channel.to_string()]).unwrap())
            .sign_with_keys(&f.owner)
            .unwrap();
        assert!(!owner.publish(channel_control).await);
        agent.no_activity_barrier().await;
        assert!(
            owner
                .publish(builder.sign_with_keys(&f.owner).unwrap())
                .await
        );
        let control = agent.activity().await;
        assert_eq!(
            buzz_core::observer::parse_observer_route(&control)
                .unwrap()
                .unwrap()
                .channel_id,
            None
        );
        assert_eq!(
            buzz_core::observer::decrypt_observer_payload::<serde_json::Value>(&f.agent, &control)
                .unwrap()["type"],
            "test-control"
        );
        for client in [&mut agent, &mut owner, &mut one, &mut two, &mut stranger] {
            client.socket.close(None).await.unwrap();
        }
    }
}
