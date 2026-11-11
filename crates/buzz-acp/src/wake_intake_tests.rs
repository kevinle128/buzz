//! Main-loop tests for `_buzz/wake`: request checks and outcomes through the
//! production reader task, requests while idle and inside a turn, the wake
//! turn in its thread, serialization with the other turns of the scope, the
//! child-generation fence, retries, and form questions inside a wake turn.

use super::*;
use crate::acp::child_io::{self, WakeFrame, WakeRoute};
use crate::acp::elicitation::{ElicitationEvent, ElicitationReply};
use crate::acp::wake::{WakeOutcome, WakeReply};
use crate::wake_intake::{handle_wake_frame, wake_scope, WakeIntake};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WAKE_INIT: &str = r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":2,"agentCapabilities":{"_meta":{"buzz.wake":{"version":1}}}}}"#;
/// Text of the agent's task that buzz-acp must log but never prompt.
const TASK_SECRET: &str = "run-task-secret-42";
const SESSION_SECRET: &str = "sess-secret-17";

fn wake_params(wake_id: &str, channel_id: Uuid, root: &str) -> serde_json::Value {
    serde_json::json!({
        "wakeId": wake_id,
        "reason": "input_required",
        "thread": {"channelId": channel_id.to_string(), "rootEventId": root},
        "sessionId": SESSION_SECRET,
        "task": {"server": "archon", "taskId": TASK_SECRET, "status": "input_required",
                 "lastUpdatedAt": "2026-11-08T03:12:00Z"},
        "prompt": "[goclaw task update] The run needs approval. Call mcp_task now.",
        "createdAt": "2026-11-08T03:12:01Z"
    })
}

fn wake_line(id: &str, params: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "_buzz/wake", "params": params})
        .to_string()
}

fn channel_info(channel_id: Uuid, channel_type: &str) -> pool::ChannelInfoResolver {
    let ctx = pool::tests::make_prompt_context_no_owner();
    pool::ChannelInfoResolver::new(
        HashMap::from([(
            channel_id,
            relay::ChannelInfo {
                name: "resolver-test".into(),
                channel_type: channel_type.into(),
                description: None,
            },
        )]),
        ctx.rest_client.clone(),
    )
}

fn top_level(channel_id: Uuid, text: &str) -> nostr::Event {
    buzz_sdk::build_message(channel_id, text, None, &[], false, &[], &[])
        .expect("build message")
        .sign_with_keys(&nostr::Keys::generate())
        .expect("sign")
}

fn ingress_for(event: nostr::Event, channel_id: Uuid) -> NormalListenerIngress {
    NormalListenerIngress {
        buzz_event: relay::BuzzEvent {
            connection_generation: 0,
            channel_id,
            event,
        },
        effective_author: "author".into(),
        prompt_tag: "@mention".into(),
        edit: None,
    }
}

fn thread_scope(channel_id: Uuid, root: &str) -> scope::SessionScope {
    scope::SessionScope::Thread {
        channel_id,
        root_event_id: root.to_owned(),
    }
}

fn agent_with(acp: AcpClient, index: usize) -> OwnedAgent {
    OwnedAgent {
        index,
        acp,
        state: SessionState::default(),
        model_capabilities: None,
        desired_model: None,
        model_overridden: false,
        desired_model_request_id: None,
        desired_model_pending_ack: false,
        startup_effort: None,
        agent_name: "wake-test-agent".into(),
        goose_system_prompt_supported: None,
        protocol_version: 2,
    }
}

/// Task metadata of a running wake turn of worker 0.
fn wake_task_meta(
    scope: &scope::SessionScope,
    control_tx: Option<tokio::sync::oneshot::Sender<ControlSignal>>,
    steer_tx: Option<mpsc::Sender<pool::SteerRequest>>,
) -> pool::TaskMeta {
    pool::TaskMeta {
        agent_index: 0,
        channel_id: Some(scope.channel_id()),
        scope: Some(scope.clone()),
        turn_id: "wake-turn".into(),
        recoverable_batch: None,
        control_tx,
        steer_tx,
        permission_decision_tx: None,
        successful_steer_deliveries: HashSet::new(),
    }
}

fn record_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("buzz-acp-wake-{}.jsonl", Uuid::new_v4()))
}

/// Recorded stdin lines of a stub child, once there are `count`.
async fn recorded(path: &std::path::Path, count: usize) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("recorded JSON"))
            .collect();
        if lines.len() >= count || std::time::Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Spawn a stub child running `body` after the bash helper `rec`, which reads
/// one stdin line (5 s max) and records it.
async fn stub_client(record: &std::path::Path, body: &str) -> AcpClient {
    let script = format!(
        "rec() {{ IFS= read -r -t 5 L && printf '%s\\n' \"$L\" >> '{}'; }}\n{body}",
        record.display()
    );
    AcpClient::spawn("bash", &["-c".into(), script], &[], false)
        .await
        .expect("spawn stub child")
}

/// A pool of one worker running `client`, with the production wake route.
fn pool_with(client: AcpClient) -> (AgentPool, mpsc::Receiver<WakeFrame>) {
    let pool = AgentPool::from_slots(vec![Some(agent_with(client, 0))]);
    let (tx, rx) = mpsc::channel(AGENT_WAKE_QUEUE);
    pool.attach_wake_routes(&tx);
    (pool, rx)
}

/// A stub child with the route of worker 0 attached before `initialize`.
///
/// The pool attaches its route only after `initialize`, and the reader
/// answers busy to a wake request that arrives first. A child that sends
/// its request right after the initialize response must have the route
/// before that response, or the test depends on task scheduling.
async fn routed_stub_client(
    record: &std::path::Path,
    body: &str,
    tx: &mpsc::Sender<WakeFrame>,
) -> AcpClient {
    let mut client = stub_client(record, body).await;
    assert!(client.wake_link().attach(WakeRoute::new(tx.clone(), 0)));
    client.initialize().await.expect("initialize");
    client
}

async fn next_frame(rx: &mut mpsc::Receiver<WakeFrame>) -> WakeFrame {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("wake request reaches the main loop")
        .expect("route open")
}

/// Child stdio through the production reader and writer tasks, over
/// in-memory pipes, for request tables.
struct PipeChild {
    _io: child_io::ChildIo,
    frames: mpsc::Receiver<WakeFrame>,
    agent_out: tokio::io::DuplexStream,
    agent_in: BufReader<tokio::io::DuplexStream>,
}

impl PipeChild {
    fn new(supports_wake: bool) -> Self {
        let (stdout, agent_out) = tokio::io::duplex(64 * 1024);
        let (stdin, agent_in) = tokio::io::duplex(64 * 1024);
        let io = child_io::start(stdout, stdin);
        io.link.set_supported(supports_wake);
        let (tx, frames) = mpsc::channel(4);
        assert!(io.link.attach(WakeRoute::new(tx, 0)));
        Self {
            _io: io,
            frames,
            agent_out,
            agent_in: BufReader::new(agent_in),
        }
    }

    /// Send one request line; handle it in the main loop; return the reply
    /// and the response the child read.
    async fn request(
        &mut self,
        line: &str,
        queue: &mut EventQueue,
        intake: &WakeIntake<'_>,
    ) -> (WakeReply, serde_json::Value) {
        self.agent_out
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("agent write");
        let frame = next_frame(&mut self.frames).await;
        let (reply, written) = handle_wake_frame(frame, queue, intake)
            .await
            .expect("current child is answered");
        written.await.expect("response task");
        let mut response = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            self.agent_in.read_line(&mut response),
        )
        .await
        .expect("response within 5 s")
        .expect("agent read");
        (
            reply,
            serde_json::from_str(&response).expect("response JSON"),
        )
    }
}

#[tokio::test]
async fn wake_request_outcome_journey() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let other_root = "cd".repeat(32);
    let subscribed = HashSet::from([channel_id]);
    let info = channel_info(channel_id, "stream");
    let intake = WakeIntake {
        policy: scope::SessionPolicy::Thread,
        subscribed: &subscribed,
        channel_info: &info,
    };
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let mut child = PipeChild::new(true);
    let scope = thread_scope(channel_id, &root);

    let (reply, response) = child
        .request(
            &wake_line("g-1", wake_params("w1", channel_id, &root)),
            &mut queue,
            &intake,
        )
        .await;
    assert_eq!(reply, Ok(WakeOutcome::Accepted));
    assert_eq!(
        response,
        serde_json::json!({"jsonrpc": "2.0", "id": "g-1", "result": {"outcome": "accepted"}})
    );
    assert_eq!(queue.ready_wake_scopes(), vec![scope.clone()]);

    // The same wake again: delivered before, no new turn.
    let (reply, response) = child
        .request(
            &wake_line("g-2", wake_params("w1", channel_id, &root)),
            &mut queue,
            &intake,
        )
        .await;
    assert_eq!(reply, Ok(WakeOutcome::Duplicate));
    assert_eq!(response["result"]["outcome"], "duplicate");

    // Another wake for the thread with a queued wake turn: busy.
    let (reply, response) = child
        .request(
            &wake_line("g-3", wake_params("w2", channel_id, &root)),
            &mut queue,
            &intake,
        )
        .await;
    assert_eq!(reply, Ok(WakeOutcome::Busy));
    assert_eq!(response["result"]["outcome"], "busy");

    // Another thread has its own budget.
    let (reply, _) = child
        .request(
            &wake_line("g-4", wake_params("w3", channel_id, &other_root)),
            &mut queue,
            &intake,
        )
        .await;
    assert_eq!(reply, Ok(WakeOutcome::Accepted));

    // Malformed and out of scope: -32602, no turn.
    let mut bad_reason = wake_params("w4", channel_id, &root);
    bad_reason["reason"] = serde_json::json!("finished");
    let unsubscribed = wake_params("w5", Uuid::new_v4(), &root);
    for (name, params, word) in [
        ("bad reason", bad_reason, "reason"),
        ("unsubscribed channel", unsubscribed, "not a channel"),
        ("params not an object", serde_json::json!("x"), "params"),
    ] {
        let (reply, response) = child
            .request(&wake_line("g-bad", params), &mut queue, &intake)
            .await;
        assert!(reply.is_err(), "{name}");
        assert_eq!(response["id"], "g-bad", "{name}");
        assert_eq!(response["error"]["code"], -32602, "{name}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(word)),
            "{name}: {response}"
        );
    }
    let mut oversized = wake_params("w6", channel_id, &other_root);
    oversized["task"]["pad"] = serde_json::json!("p".repeat(acp::wake::MAX_FRAME_BYTES));
    let (reply, _) = child
        .request(&wake_line("g-big", oversized), &mut queue, &intake)
        .await;
    assert!(reply.is_err());

    // A child that did not advertise the capability.
    let mut plain = PipeChild::new(false);
    let (reply, response) = plain
        .request(
            &wake_line("g-5", wake_params("w7", channel_id, &"ef".repeat(32))),
            &mut queue,
            &intake,
        )
        .await;
    assert!(reply.is_err());
    assert_eq!(response["error"]["code"], -32602);

    assert_eq!(
        queue.ready_wake_scopes().len(),
        2,
        "only accepted wakes queue a turn"
    );
}

#[test]
fn wake_scope_follows_session_policy() {
    let channel_id = Uuid::new_v4();
    let thread = acp::wake::WakeThread {
        channel_id,
        root_event_id: "ab".repeat(32),
    };
    let conversation = scope::SessionScope::Conversation { channel_id };
    for (policy, is_dm, expected) in [
        (
            scope::SessionPolicy::Thread,
            false,
            thread_scope(channel_id, &"ab".repeat(32)),
        ),
        (scope::SessionPolicy::Thread, true, conversation.clone()),
        (scope::SessionPolicy::Channel, false, conversation.clone()),
        (scope::SessionPolicy::Channel, true, conversation),
    ] {
        assert_eq!(
            wake_scope(policy, is_dm, &thread),
            expected,
            "{policy:?} dm={is_dm}"
        );
    }
}

#[tokio::test]
async fn wake_request_answered_while_idle() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let record = record_path();
    let wake = wake_line("goclaw-17", wake_params("w-idle", channel_id, &root));
    let body = format!(
        "rec; printf '%s\\n' '{WAKE_INIT}'\n\
         rec; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n\
         printf '%s\\n' '{wake}'\n\
         rec; sleep 5"
    );
    let mut client = stub_client(&record, &body).await;
    client.initialize().await.expect("initialize");
    let (mut pool, mut frames) = pool_with(client);
    let mut agent = pool.try_claim(None).expect("idle worker");
    agent
        .acp
        .session_prompt_with_idle_timeout(
            "s1",
            "hello",
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await
        .expect("prompt");
    // The turn is over; the request arrives while the worker is idle.
    pool.return_agent(agent);

    let subscribed = HashSet::from([channel_id]);
    let info = channel_info(channel_id, "stream");
    let intake = WakeIntake {
        policy: scope::SessionPolicy::Thread,
        subscribed: &subscribed,
        channel_info: &info,
    };
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let frame = next_frame(&mut frames).await;
    assert_eq!(frame.worker, 0);
    let (reply, written) = handle_wake_frame(frame, &mut queue, &intake)
        .await
        .expect("answered");
    written.await.expect("response task");
    assert_eq!(reply, Ok(WakeOutcome::Accepted));
    let lines = recorded(&record, 3).await;
    assert_eq!(
        lines[2],
        serde_json::json!({"jsonrpc": "2.0", "id": "goclaw-17", "result": {"outcome": "accepted"}})
    );
    assert_eq!(
        queue.ready_wake_scopes(),
        vec![thread_scope(channel_id, &root)]
    );
    shutdown_agent_pool(&mut pool).await;
    let _ = std::fs::remove_file(record);
}

#[tokio::test]
async fn wake_request_inside_turn() {
    let channel_id = Uuid::new_v4();
    let running = top_level(channel_id, "@bot resolve the conflict");
    let root = running.id.to_hex();
    let scope = thread_scope(channel_id, &root);
    let record = record_path();
    let wake = wake_line("goclaw-18", wake_params("w-turn", channel_id, &root));
    // The child answers the prompt only after it read the wake response.
    let body = format!(
        "rec; printf '%s\\n' '{WAKE_INIT}'\n\
         rec; printf '%s\\n' '{wake}'; rec\n\
         printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n\
         sleep 5"
    );
    let mut client = stub_client(&record, &body).await;
    client.initialize().await.expect("initialize");
    let (mut pool, mut frames) = pool_with(client);
    let mut agent = pool.try_claim(None).expect("idle worker");

    // A channel turn runs in the thread.
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    ingress_for(running, channel_id).push(&mut queue, scope.clone(), false);
    queue.flush_next().expect("running turn");
    let turn = tokio::spawn(async move {
        let result = agent
            .acp
            .session_prompt_with_idle_timeout(
                "s1",
                "resolve",
                Duration::from_secs(10),
                Duration::from_secs(10),
            )
            .await;
        (agent, result)
    });

    let subscribed = HashSet::from([channel_id]);
    let info = channel_info(channel_id, "stream");
    let intake = WakeIntake {
        policy: scope::SessionPolicy::Thread,
        subscribed: &subscribed,
        channel_info: &info,
    };
    let frame = next_frame(&mut frames).await;
    let (reply, written) = handle_wake_frame(frame, &mut queue, &intake)
        .await
        .expect("answered");
    written.await.expect("response task");
    assert_eq!(reply, Ok(WakeOutcome::Accepted));

    // The running turn is not disturbed: it ends normally after the answer.
    let (agent, result) = tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .expect("turn ends")
        .expect("turn task");
    assert_eq!(result.expect("prompt"), acp::StopReason::EndTurn);
    let lines = recorded(&record, 3).await;
    assert_eq!(lines[1]["method"], "session/prompt");
    assert_eq!(lines[2]["result"]["outcome"], "accepted");

    // The wake turn waits for the running turn, then runs after it, and no
    // other turn of the scope runs at the same time as the wake turn.
    assert!(queue.ready_wake_scopes().is_empty());
    assert!(
        queue.start_wake(&scope).is_none(),
        "no wake turn while a turn runs"
    );
    queue.mark_complete(scope.clone());
    assert_eq!(queue.ready_wake_scopes(), vec![scope.clone()]);
    assert!(queue.start_wake(&scope).is_some());
    assert!(queue.is_scope_in_flight(&scope));
    let follow_up = top_level(channel_id, "@bot status?");
    ingress_for(follow_up, channel_id).push(&mut queue, scope.clone(), false);
    assert!(
        queue.flush_next().is_none(),
        "no second turn while the wake turn runs"
    );
    queue.mark_complete(scope.clone());
    assert!(
        queue.flush_next().is_some(),
        "the event runs after the wake turn"
    );

    pool.return_agent(agent);
    shutdown_agent_pool(&mut pool).await;
    let _ = std::fs::remove_file(record);
}

#[tokio::test]
async fn wake_from_old_child_generation_ignored() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let record = record_path();
    let wake = wake_line("goclaw-19", wake_params("w-old", channel_id, &root));
    let body = format!("rec; printf '%s\\n' '{WAKE_INIT}'; printf '%s\\n' '{wake}'; sleep 5");
    let (tx, mut frames) = mpsc::channel(AGENT_WAKE_QUEUE);
    let old = routed_stub_client(&record, &body, &tx).await;
    let mut pool = AgentPool::from_slots(vec![Some(agent_with(old, 0))]);
    let frame = next_frame(&mut frames).await;
    assert!(frame.is_current());

    // The worker is respawned before the main loop reads the request.
    let mut old = pool.try_claim(None).expect("idle worker");
    old.acp.shutdown().await;
    let new_record = record_path();
    let (tx, mut new_frames) = mpsc::channel(4);
    let new = routed_stub_client(&new_record, &body, &tx).await;
    assert_ne!(
        new.wake_link().generation(),
        old.acp.wake_link().generation()
    );
    pool.return_agent(agent_with(new, 0));

    let subscribed = HashSet::from([channel_id]);
    let info = channel_info(channel_id, "stream");
    let intake = WakeIntake {
        policy: scope::SessionPolicy::Thread,
        subscribed: &subscribed,
        channel_info: &info,
    };
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    assert!(handle_wake_frame(frame, &mut queue, &intake)
        .await
        .is_none());
    assert!(
        queue.ready_wake_scopes().is_empty(),
        "no turn from the old child"
    );

    // The same request from the new child is a first delivery.
    let frame = next_frame(&mut new_frames).await;
    let (reply, written) = handle_wake_frame(frame, &mut queue, &intake)
        .await
        .expect("answered");
    written.await.expect("response task");
    assert_eq!(reply, Ok(WakeOutcome::Accepted));
    shutdown_agent_pool(&mut pool).await;
    let _ = std::fs::remove_file(record);
    let _ = std::fs::remove_file(new_record);
}

/// A relay fixture that answers every HTTP query with an empty list.
async fn empty_relay() -> (String, tokio::task::JoinHandle<()>) {
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind relay fixture");
    let base_url = format!("http://{}", relay.local_addr().expect("addr"));
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        loop {
            let Ok((mut socket, _)) = relay.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = [0; 16384];
                let _ = socket.read(&mut request).await;
                let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]";
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (base_url, task)
}

/// A stub child that answers `initialize` with the wake capability,
/// `session/new` with `wake-session`, and `session/prompt` with `end_turn`
/// after the shell code `on_prompt` ran. It records every stdin line in the
/// file `$REC`.
async fn responder_client(record: &std::path::Path, on_prompt: &str) -> AcpClient {
    let script = format!(
        r#"REC='{record}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$REC"
  id=$(printf '%s' "$line" | sed -n 's/^{{[^{{]*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":2,"agentCapabilities":{{"_meta":{{"buzz.wake":{{"version":1}}}}}}}}}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"wake-session"}}}}\n' "$id" ;;
    *'"method":"session/prompt"'*) {on_prompt}
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$id" ;;
    *'"method":'*) if [ -n "$id" ]; then printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"; fi ;;
  esac
done"#,
        record = record.display()
    );
    let mut client = AcpClient::spawn("bash", &["-c".into(), script], &[], false)
        .await
        .expect("spawn responder");
    client.initialize().await.expect("initialize");
    assert!(client.wake_supported());
    client
}

/// One wake turn dispatched through `dispatch_pending` on a stub child.
struct WakeTurnFixture {
    pool: AgentPool,
    queue: EventQueue,
    ctx: Arc<PromptContext>,
    scope: scope::SessionScope,
    channel_id: Uuid,
    root: String,
    record: std::path::PathBuf,
    _relay: tokio::task::JoinHandle<()>,
}

impl WakeTurnFixture {
    async fn new(on_prompt: &str) -> Self {
        let channel_id = Uuid::new_v4();
        let root = "ab".repeat(32);
        let record = record_path();
        let client = responder_client(&record, on_prompt).await;
        let (base_url, relay) = empty_relay().await;
        let mut ctx = pool::tests::make_prompt_context_no_owner();
        ctx.rest_client.base_url = base_url;
        ctx.channel_info = pool::ChannelInfoResolver::new(
            HashMap::from([(
                channel_id,
                relay::ChannelInfo {
                    name: "resolver-test".into(),
                    channel_type: "stream".into(),
                    description: None,
                },
            )]),
            ctx.rest_client.clone(),
        );
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        let request =
            acp::wake::parse_request(100, true, Some(&wake_params("w-turn", channel_id, &root)))
                .expect("valid request");
        let scope = thread_scope(channel_id, &root);
        assert_eq!(
            queue.admit_wake(std::time::Instant::now(), request, scope.clone()),
            WakeOutcome::Accepted
        );
        Self {
            pool: AgentPool::from_slots(vec![Some(agent_with(client, 0))]),
            queue,
            ctx: Arc::new(ctx),
            scope,
            channel_id,
            root,
            record,
            _relay: relay,
        }
    }

    fn dispatch(&mut self) -> Vec<(scope::SessionScope, ThreadTags)> {
        let mut last_activity = tokio::time::Instant::now();
        dispatch_pending(
            &mut self.pool,
            &mut self.queue,
            &self.ctx,
            &mut last_activity,
            None,
        )
    }

    async fn result(&mut self) -> PromptResult {
        let (result_rx, _) = self.pool.rx_and_join_set();
        tokio::time::timeout(Duration::from_secs(20), result_rx.recv())
            .await
            .expect("wake turn ends")
            .expect("result channel open")
    }

    fn handle(&mut self, result: PromptResult) {
        let config = build_mcp_servers_tests::test_config();
        let mut crash_history = vec![SlotCircuit {
            crash_times: Vec::new(),
            open_until: None,
            respawn_in_flight: false,
        }];
        let (respawn_tx, _respawn_rx) = mpsc::channel(8);
        let mut respawn_tasks = tokio::task::JoinSet::new();
        let mut heartbeat_in_flight = false;
        let mut last_activity = tokio::time::Instant::now();
        handle_prompt_result(
            &mut self.pool,
            &mut self.queue,
            &config,
            result,
            &mut heartbeat_in_flight,
            &HashSet::new(),
            &mut crash_history,
            &respawn_tx,
            &mut respawn_tasks,
            None,
            None,
            &mut last_activity,
        );
        respawn_tasks.abort_all();
    }
}

#[tokio::test]
async fn wake_opens_turn_in_thread() {
    let mut fx = WakeTurnFixture::new(":").await;
    let dispatched = fx.dispatch();
    assert_eq!(dispatched.len(), 1, "one wake turn starts");
    assert_eq!(dispatched[0].0, fx.scope);
    assert_eq!(
        dispatched[0].1.root_event_id.as_deref(),
        Some(fx.root.as_str())
    );
    assert!(fx.queue.is_wake_in_flight(&fx.scope));

    let result = fx.result().await;
    assert!(matches!(&result.source, PromptSource::Wake(scope) if *scope == fx.scope));
    assert!(matches!(
        result.outcome,
        PromptOutcome::Ok(acp::StopReason::EndTurn)
    ));
    fx.handle(result);
    assert!(!fx.queue.is_scope_in_flight(&fx.scope));
    assert!(
        fx.queue.ready_wake_scopes().is_empty(),
        "a finished wake turn is not run again"
    );

    let lines = recorded(&fx.record, 3).await;
    let prompts: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|line| line["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 1, "one session/prompt");
    let new = lines
        .iter()
        .find(|line| line["method"] == "session/new")
        .expect("session/new");
    let thread =
        serde_json::json!({"channelId": fx.channel_id.to_string(), "rootEventId": fx.root});
    assert_eq!(new["params"]["_meta"]["buzz.thread"], thread);
    let prompt = prompts[0];
    assert_eq!(prompt["params"]["sessionId"], "wake-session");
    assert_eq!(prompt["params"]["_meta"]["buzz.thread"], thread);
    let text = prompt["params"]["prompt"][0]["text"]
        .as_str()
        .expect("prompt text");
    assert!(text.starts_with(
        "[Buzz wake] Your runtime asked for a turn in this thread (reason: input_required).\n<context>"
    ));
    assert!(text.contains(&fx.channel_id.to_string()), "{text}");
    assert!(
        text.contains(&format!("Thread root: {}", fx.root)),
        "{text}"
    );
    assert!(text.contains(&format!("--reply-to {}", fx.root)), "{text}");
    assert!(text.contains(
        "<runtime-text>\n[goclaw task update] The run needs approval. Call mcp_task now.\n</runtime-text>"
    ));
    assert!(
        !text.contains(TASK_SECRET) && !text.contains(SESSION_SECRET),
        "{text}"
    );
    shutdown_agent_pool(&mut fx.pool).await;
    let _ = std::fs::remove_file(&fx.record);
}

#[tokio::test]
async fn wake_turn_failure_runs_again_within_its_attempts() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let scope = thread_scope(channel_id, &root);
    let outcomes: Vec<(&str, PromptOutcome, bool)> = vec![
        ("ok", PromptOutcome::Ok(acp::StopReason::EndTurn), false),
        ("cancelled", PromptOutcome::Cancelled, false),
        ("agent exited", PromptOutcome::AgentExited, true),
        (
            "error",
            PromptOutcome::Error(acp::AcpError::IdleTimeout(Duration::from_secs(1))),
            true,
        ),
        (
            "hard cap, silent",
            PromptOutcome::Timeout(pool::TimeoutKind::Hard {
                recently_active: false,
            }),
            false,
        ),
    ];
    for (name, outcome, retried) in outcomes {
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        let request =
            acp::wake::parse_request(100, true, Some(&wake_params("w-fail", channel_id, &root)))
                .expect("valid");
        queue.admit_wake(std::time::Instant::now(), request, scope.clone());
        queue.start_wake(&scope).expect("started");
        let mut pool = AgentPool::from_slots(vec![None]);
        // The dispatched wake turn, as `spawn_scope_turn` records it.
        let task = pool.join_set.spawn(std::future::pending::<()>());
        pool.task_map_mut()
            .insert(task.id(), wake_task_meta(&scope, None, None));
        let config = build_mcp_servers_tests::test_config();
        let mut crash_history = vec![SlotCircuit {
            crash_times: Vec::new(),
            open_until: None,
            respawn_in_flight: false,
        }];
        let (respawn_tx, _respawn_rx) = mpsc::channel(8);
        let mut respawn_tasks = tokio::task::JoinSet::new();
        let mut heartbeat_in_flight = false;
        let mut last_activity = tokio::time::Instant::now();
        handle_prompt_result(
            &mut pool,
            &mut queue,
            &config,
            PromptResult {
                agent: error_outcome_emission_tests::dummy_agent(0).await,
                source: PromptSource::Wake(scope.clone()),
                turn_id: "wake-turn".into(),
                outcome,
                batch: None,
            },
            &mut heartbeat_in_flight,
            &HashSet::new(),
            &mut crash_history,
            &respawn_tx,
            &mut respawn_tasks,
            None,
            None,
            &mut last_activity,
        );
        respawn_tasks.abort_all();
        pool.join_set.abort_all();
        assert!(!queue.is_scope_in_flight(&scope), "{name}");
        assert_eq!(!queue.ready_wake_scopes().is_empty(), retried, "{name}");
        shutdown_agent_pool(&mut pool).await;
    }
}

#[tokio::test]
async fn wake_turn_in_flight_gets_no_steer_or_interrupt() {
    for handling in [
        MultipleEventHandling::Steer,
        MultipleEventHandling::Interrupt,
    ] {
        let channel_id = Uuid::new_v4();
        let root = top_level(channel_id, "@bot start");
        let scope = thread_scope(channel_id, &root.id.to_hex());
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        let request = acp::wake::parse_request(
            100,
            true,
            Some(&wake_params("w-steer", channel_id, &root.id.to_hex())),
        )
        .expect("valid");
        queue.admit_wake(std::time::Instant::now(), request, scope.clone());
        queue.start_wake(&scope).expect("started");
        let mut pool = AgentPool::from_slots(vec![None]);
        let (control_tx, mut control_rx) = tokio::sync::oneshot::channel();
        let (steer_tx, mut steer_rx) = mpsc::channel(1);
        let task = pool.join_set.spawn(std::future::pending::<()>());
        pool.task_map_mut().insert(
            task.id(),
            wake_task_meta(&scope, Some(control_tx), Some(steer_tx)),
        );
        let (ack_tx, _ack_rx) = mpsc::unbounded_channel();
        let chat = top_level(channel_id, "@bot status?");
        ingress_for(chat, channel_id)
            .push(&mut queue, scope.clone(), false)
            .steer_or_interrupt(handling, None, &mut pool, &mut queue, &ack_tx);
        assert!(
            control_rx.try_recv().is_err(),
            "{handling:?}: no stop signal"
        );
        assert!(steer_rx.try_recv().is_err(), "{handling:?}: no steer");
        assert_eq!(
            queue.queued_event_count(scope.clone()),
            1,
            "{handling:?}: event waits"
        );
        pool.join_set.abort_all();
    }
}

#[tokio::test]
async fn elicitation_inside_wake_turn_posts_in_the_wake_thread() {
    // During the prompt the child asks one form question and waits for the
    // answer before it ends the turn.
    let question = r#"{"jsonrpc":"2.0","id":"e1","method":"elicitation/create","params":{"sessionId":"wake-session","toolCallId":"tc-1","mode":"form","message":"Approve the merge?","requestedSchema":{"type":"object","properties":{"decision":{"type":"string","enum":["approve","reject"]}},"required":["decision"]}}}"#;
    let on_prompt = format!(
        "printf '%s\\n' '{question}'; IFS= read -r answer; printf '%s\\n' \"$answer\" >> \"$REC\";"
    );
    let mut fx = WakeTurnFixture::new(&on_prompt).await;
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    fx.pool.elicitations_mut().ensure_events_tx(&events_tx);
    assert_eq!(fx.dispatch().len(), 1);

    let event = tokio::time::timeout(Duration::from_secs(20), events_rx.recv())
        .await
        .expect("question reaches the main loop")
        .expect("open");
    let ElicitationEvent::Opened { turn, reply_tx, .. } = event else {
        panic!("expected an opened question, got {event:?}");
    };
    assert_eq!(turn.scope, fx.scope);
    assert_eq!(
        turn.anchor_event_id, fx.root,
        "the question posts in the wake thread"
    );
    let _ = reply_tx.send(ElicitationReply::Cancel);

    let result = fx.result().await;
    assert!(matches!(result.outcome, PromptOutcome::Ok(_)));
    fx.handle(result);
    let lines = recorded(&fx.record, 4).await;
    let answer = lines
        .iter()
        .find(|line| line["id"] == "e1")
        .expect("the question was answered");
    assert_eq!(answer["result"]["action"], "cancel");
    shutdown_agent_pool(&mut fx.pool).await;
    let _ = std::fs::remove_file(&fx.record);
}

#[tokio::test]
async fn panicked_wake_turn_runs_again_and_leaves_flight() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let scope = thread_scope(channel_id, &root);
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let request =
        acp::wake::parse_request(100, true, Some(&wake_params("w-panic", channel_id, &root)))
            .expect("valid");
    queue.admit_wake(std::time::Instant::now(), request, scope.clone());
    queue.start_wake(&scope).expect("started");
    let mut pool = AgentPool::from_slots(vec![None]);
    let task = pool
        .join_set
        .spawn(async { panic!("wake turn task panics") });
    pool.task_map_mut()
        .insert(task.id(), wake_task_meta(&scope, None, None));
    let join_error = loop {
        match pool.join_set.join_next().await {
            Some(Err(error)) if error.is_panic() => break error,
            Some(_) => continue,
            None => panic!("panicking task must report"),
        }
    };
    let config = build_mcp_servers_tests::test_config();
    let mut crash_history = vec![SlotCircuit {
        crash_times: Vec::new(),
        open_until: None,
        respawn_in_flight: false,
    }];
    let (respawn_tx, _respawn_rx) = mpsc::channel(8);
    let mut respawn_tasks = tokio::task::JoinSet::new();
    let mut heartbeat_in_flight = false;
    let mut typing = HashMap::new();
    recover_panicked_agent(
        &mut pool,
        &mut queue,
        &config,
        join_error,
        &mut heartbeat_in_flight,
        &HashSet::new(),
        &mut typing,
        &mut crash_history,
        &respawn_tx,
        &mut respawn_tasks,
        None,
    );
    respawn_tasks.shutdown().await;
    assert!(!queue.is_wake_in_flight(&scope));
    assert!(!queue.is_scope_in_flight(&scope));
    assert_eq!(queue.ready_wake_scopes(), vec![scope.clone()]);

    // Leaving the channel drops the queued wake turn.
    queue.drain_channel(channel_id);
    assert!(queue.ready_wake_scopes().is_empty());
    assert!(!queue.has_pending_scope(&scope));
}

#[tokio::test]
async fn held_wake_turn_waits_for_a_busy_thread_owner_then_forks() {
    let channel_id = Uuid::new_v4();
    let root = "ab".repeat(32);
    let scope = thread_scope(channel_id, &root);
    // Worker 0 owns the thread session and is busy; worker 1 is idle.
    let mut pool = AgentPool::from_slots(vec![
        None,
        Some(error_outcome_emission_tests::dummy_agent(1).await),
    ]);
    pool.record_scope_owner(scope.clone(), 0);
    let task = pool.join_set.spawn(std::future::pending::<()>());
    let mut meta = wake_task_meta(&scope, None, None);
    meta.scope = Some(thread_scope(channel_id, &"cd".repeat(32)));
    pool.task_map_mut().insert(task.id(), meta);
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let request =
        acp::wake::parse_request(100, true, Some(&wake_params("w-hold", channel_id, &root)))
            .expect("valid");
    queue.admit_wake(std::time::Instant::now(), request, scope.clone());
    let ctx = Arc::new(pool::tests::make_prompt_context_no_owner());

    let now = tokio::time::Instant::now();
    assert!(dispatch_wake_turns(&mut pool, &mut queue, &ctx, now).is_empty());
    assert_eq!(
        queue.ready_wake_scopes(),
        vec![scope.clone()],
        "held, still queued"
    );
    // The hold stays armed for the queued wake, so its deadline wakes the loop.
    pool.retain_held_scopes(|held| queue.has_pending_scope(held));
    assert!(pool
        .next_hold_deadline(pool::HOLD_BUSY_OWNER_TIMEOUT)
        .is_some());

    let later = now + pool::HOLD_BUSY_OWNER_TIMEOUT;
    let started = dispatch_wake_turns(&mut pool, &mut queue, &ctx, later);
    assert_eq!(started.len(), 1, "forked to the idle worker after the hold");
    assert!(queue.is_wake_in_flight(&scope));
    pool.join_set.abort_all();
}
