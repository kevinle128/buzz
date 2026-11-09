// Stub-agent tests for the stdio tasks of `AcpClient` and its `buzz.wake`
// metadata. Included into `acp::tests`, so the private client API,
// `spawn_script`, and the elicitation test helpers are in scope.

const WAKE_INIT_RESULT: &str = r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":2,"agentCapabilities":{"_meta":{"buzz.wake":{"version":1}}}}}"#;
const PLAIN_INIT_RESULT: &str =
    r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":2,"agentCapabilities":{}}}"#;

fn wake_test_thread() -> wake::WakeThread {
    wake::WakeThread {
        channel_id: uuid::Uuid::parse_str("2f0b8a52-6a9d-4c1e-9d3b-1a2b3c4d5e6f").expect("uuid"),
        root_event_id: "cd".repeat(32),
    }
}

/// Run `initialize`, `session/new`, and one prompt against a stub that
/// answers `initialize` with `init_result`. Returns the recorded
/// `session/new` and `session/prompt` frames and whether the client recorded
/// the wake capability.
async fn thread_meta_frames(
    init_result: &str,
    thread: Option<wake::WakeThread>,
) -> (serde_json::Value, serde_json::Value, bool) {
    let record = elicitation_record_path();
    let body = format!(
        "rec; printf '%s\\n' '{init_result}'\n\
         rec; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"sessionId\":\"s1\"}}}}'\n\
         rec; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n\
         sleep 5"
    );
    let mut client = spawn_script(&elicitation_script(&record, &body)).await;
    client.initialize().await.expect("initialize");
    let supported = client.wake_supported();
    client.set_turn_thread(thread);
    client
        .session_new_full("/tmp", vec![], None, Some("title"))
        .await
        .expect("session/new");
    client
        .session_prompt_with_idle_timeout(
            "s1",
            "hi",
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("prompt");
    let lines = recorded_lines(&record, 3).await;
    client.shutdown().await;
    assert_eq!(lines.len(), 3, "{lines:?}");
    (lines[1].clone(), lines[2].clone(), supported)
}

#[tokio::test]
async fn prompt_meta_thread_only_for_wake_child() {
    let thread = wake_test_thread();
    let expected = thread.meta_value();

    let (new, prompt, supported) =
        thread_meta_frames(WAKE_INIT_RESULT, Some(thread.clone())).await;
    assert!(supported);
    assert_eq!(new["params"]["_meta"]["buzz.thread"], expected);
    // Merged into `_meta`: the other keys stay.
    assert_eq!(new["params"]["_meta"]["sessionTitle"], "title");
    assert_eq!(prompt["params"]["_meta"]["buzz.thread"], expected);
    assert_eq!(prompt["method"], "session/prompt");

    // A wake child without a turn thread (a heartbeat) gets no thread.
    let (new, prompt, _) = thread_meta_frames(WAKE_INIT_RESULT, None).await;
    assert!(new["params"]["_meta"].get("buzz.thread").is_none());
    assert!(prompt["params"].get("_meta").is_none());

    // Another adapter gets nothing new, also with a turn thread.
    let (new, prompt, supported) = thread_meta_frames(PLAIN_INIT_RESULT, Some(thread)).await;
    assert!(!supported);
    assert!(new["params"]["_meta"].get("buzz.thread").is_none());
    assert_eq!(new["params"]["_meta"]["sessionTitle"], "title");
    assert!(prompt["params"].get("_meta").is_none());
}

#[tokio::test]
async fn writer_reports_broken_pipe() {
    let mut client = spawn_script("exit 0").await;
    // Stdout closes when the child exits.
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), client.reader.next())
        .await
        .expect("child exits");
    assert!(end.is_none());
    for attempt in ["first", "second"] {
        let result = client.session_cancel("s").await;
        assert!(
            matches!(&result, Err(AcpError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe),
            "{attempt} write after exit: {result:?}"
        );
    }
}

#[tokio::test]
async fn reader_does_not_block_idle_chatter() {
    let done = std::env::temp_dir().join(format!("buzz-acp-chatter-{}", uuid::Uuid::new_v4()));
    // About 420 KiB of notifications, far more than the pipe and the line
    // queue hold, then one line with an `id`.
    let script = format!(
        "for i in $(seq 1 3000); do printf '%s\\n' \
         '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"update\":{{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{{\"type\":\"text\",\"text\":\"chatter\"}}}}}}}}'; \
         done; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{\"kept\":true}}}}'; \
         touch '{}'; sleep 10",
        done.display()
    );
    let mut client = spawn_script(&script).await;
    // The child finishes writing although no read loop runs.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !done.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the idle child must not block on its stdout");
    assert!(client.dropped_idle_notifications() > 0);
    // The line with an `id` is kept.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.read_until_response(7),
    )
    .await
    .expect("response line kept")
    .expect("response");
    assert_eq!(result["kept"], true);
    client.shutdown().await;
    let _ = std::fs::remove_file(done);
}

/// A `_buzz/wake` request never reaches the read loop's method-not-found
/// arm: the agent runtime would mark the connection as not supporting wakes.
#[tokio::test]
async fn wake_request_inside_request_is_not_method_not_found() {
    let record = elicitation_record_path();
    let wake = r#"{"jsonrpc":"2.0","id":"goclaw-1","method":"_buzz/wake","params":{}}"#;
    let body = format!(
        "rec; printf '%s\\n' '{wake}'; rec; \
         printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{}}}}'; sleep 5"
    );
    let mut client = spawn_script(&elicitation_script(&record, &body)).await;
    // No pool route: the reader itself answers `busy`.
    client.initialize().await.expect("initialize completes");
    let lines = recorded_lines(&record, 2).await;
    assert_eq!(lines[1]["id"], "goclaw-1");
    assert_eq!(lines[1]["result"]["outcome"], "busy", "{lines:?}");
    client.shutdown().await;
}
