// Stub-agent tests for `elicitation/create` in the ACP read loops. Included
// into `acp::tests`, so the private client API and `spawn_script` are in scope.

const ELICITATION_SESSION: &str = "sess-elicit";

fn elicitation_decision_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "decision": {"type": "string", "enum": ["approve", "reject"]},
            "comment": {"type": "string"}
        },
        "required": ["decision"]
    })
}

/// One `elicitation/create` request line, as goclaw sends it.
fn elicitation_request_line(
    id: &str,
    session: &str,
    mode: &str,
    schema: serde_json::Value,
) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "elicitation/create",
        "params": {
            "sessionId": session,
            "toolCallId": "tc-1",
            "mode": mode,
            "message": "Approve the merge of foo.ts?",
            "requestedSchema": schema,
        }
    })
    .to_string()
}

/// A temp file that the stub agent appends each stdin line to.
fn elicitation_record_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "buzz-acp-elicitation-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Bash prelude: `rec` reads one stdin line (5 s max) and records it.
fn elicitation_script(record: &std::path::Path, body: &str) -> String {
    format!(
        "rec() {{ IFS= read -r -t 5 L && printf '%s\\n' \"$L\" >> '{}'; }}\n{body}",
        record.display()
    )
}

/// Wait until the stub agent recorded `count` lines, then parse them.
async fn recorded_lines(record: &std::path::Path, count: usize) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(record).unwrap_or_default();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("recorded line is JSON"))
            .collect();
        if lines.len() >= count || std::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(record);
            return lines;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Install a turn route on `client` and return its event receiver.
fn install_test_elicitation_sink(
    client: &mut AcpClient,
) -> tokio::sync::mpsc::UnboundedReceiver<elicitation::ElicitationEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let root = "ab".repeat(32);
    client.install_elicitation_sink(elicitation::ElicitationSink::new(
        tx,
        elicitation::ElicitationTurn {
            turn_id: "turn-1".into(),
            scope: crate::scope::SessionScope::Thread {
                channel_id: uuid::Uuid::nil(),
                root_event_id: root.clone(),
            },
            anchor_event_id: root,
        },
    ));
    rx
}

fn elicitation_cancel_line(id: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"action":"cancel"}})
}

#[test]
fn initialize_declares_form_elicitation_only() {
    let params = build_initialize_params();
    let elicitation = &params["clientCapabilities"]["elicitation"];
    assert_eq!(elicitation["form"], serde_json::json!({}));
    assert!(
        elicitation.get("url").is_none(),
        "url mode must not be declared: {elicitation}"
    );
}

#[tokio::test]
async fn elicitation_in_turn_accept_writes_content() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "echo '{request}'\nrec\necho '{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);
    client.last_prompt_id = Some(7);

    let answerer = tokio::spawn(async move {
        match events.recv().await {
            Some(elicitation::ElicitationEvent::Opened {
                request_id,
                message,
                schema,
                reply_tx,
                ..
            }) => {
                assert_eq!(request_id, "\"e1\"");
                assert_eq!(message, "Approve the merge of foo.ts?");
                let reply = elicitation::parse_reply("@bot approve ok", &schema, &[])
                    .expect("answer parses");
                reply_tx.send(reply).expect("read loop waits for the reply");
            }
            other => panic!("expected Opened, got {other:?}"),
        }
        events
    });

    let max = std::time::Duration::from_secs(10);
    let result = client
        .read_until_response_with_idle_timeout(
            ELICITATION_SESSION,
            7,
            std::time::Duration::from_secs(5),
            tokio::time::Instant::now() + max,
            max,
        )
        .await;
    assert!(result.is_ok(), "turn must finish, got {result:?}");
    let mut events = answerer.await.expect("answerer task");
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Closed {
            reason: elicitation::CloseReason::Replied,
            ..
        })
    ));
    assert!(!client.has_pending_elicitation());
    assert_eq!(
        recorded_lines(&record, 1).await,
        vec![serde_json::json!({
            "jsonrpc": "2.0",
            "id": "e1",
            "result": {"action": "accept", "content": {"decision": "approve", "comment": "ok"}}
        })]
    );
}

/// Unsupported schema and wrong session both refuse at once: `cancel` on the
/// wire, one `Refused` event for the thread notice, and no question.
#[tokio::test]
async fn elicitation_unsupported_schema_and_session_mismatch_cancel_at_once() {
    let number_schema = serde_json::json!({
        "type": "object",
        "properties": {"n": {"type": "number"}}
    });
    let cases = [
        (
            "unsupported schema",
            elicitation_request_line("e1", ELICITATION_SESSION, "form", number_schema),
        ),
        (
            "session mismatch",
            elicitation_request_line("e1", "sess-other", "form", elicitation_decision_schema()),
        ),
    ];
    for (name, request) in cases {
        let record = elicitation_record_path();
        let script = elicitation_script(
            &record,
            &format!(
                "echo '{request}'\nrec\necho '{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{}}}}'\nsleep 1"
            ),
        );
        let mut client = spawn_script(&script).await;
        let mut events = install_test_elicitation_sink(&mut client);
        client.last_prompt_id = Some(7);
        let max = std::time::Duration::from_secs(10);
        let result = client
            .read_until_response_with_idle_timeout(
                ELICITATION_SESSION,
                7,
                std::time::Duration::from_secs(5),
                tokio::time::Instant::now() + max,
                max,
            )
            .await;
        assert!(result.is_ok(), "{name}: {result:?}");
        assert!(
            matches!(
                events.try_recv(),
                Ok(elicitation::ElicitationEvent::Refused { .. })
            ),
            "{name}: one notice"
        );
        assert!(events.try_recv().is_err(), "{name}: no question opened");
        assert!(!client.has_pending_elicitation(), "{name}");
        assert_eq!(
            recorded_lines(&record, 1).await,
            vec![elicitation_cancel_line("e1")],
            "{name}"
        );
    }
}

#[tokio::test]
async fn elicitation_url_mode_is_invalid_params() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "url", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "echo '{request}'\nrec\necho '{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);
    client.last_prompt_id = Some(7);
    let max = std::time::Duration::from_secs(10);
    let result = client
        .read_until_response_with_idle_timeout(
            ELICITATION_SESSION,
            7,
            std::time::Duration::from_secs(5),
            tokio::time::Instant::now() + max,
            max,
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(events.try_recv().is_err(), "nothing is posted for url mode");
    let lines = recorded_lines(&record, 1).await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["id"], "e1");
    assert_eq!(lines[0]["error"]["code"], -32602);
}

/// While the question waits, agent silence is not idle time. After the reply
/// the idle clock starts again from the reply, not from the request.
#[tokio::test]
async fn elicitation_pending_suspends_idle_clock() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(&record, &format!("echo '{request}'\nrec\nsleep 10"));
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);
    client.last_prompt_id = Some(7);

    let answer_delay = std::time::Duration::from_millis(1_200);
    let idle = std::time::Duration::from_millis(400);
    tokio::spawn(async move {
        if let Some(elicitation::ElicitationEvent::Opened { reply_tx, .. }) = events.recv().await
        {
            tokio::time::sleep(answer_delay).await;
            let _ = reply_tx.send(elicitation::ElicitationReply::Decline);
        }
        // Keep the route open until the test ends.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        drop(events);
    });

    let started = std::time::Instant::now();
    let max = std::time::Duration::from_secs(20);
    let result = client
        .read_until_response_with_idle_timeout(
            ELICITATION_SESSION,
            7,
            idle,
            tokio::time::Instant::now() + max,
            max,
        )
        .await;
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(AcpError::IdleTimeout(_))),
        "silence after the reply is idle time, got {result:?}"
    );
    assert!(
        elapsed >= answer_delay + idle - std::time::Duration::from_millis(50),
        "idle must not fire while the question waits or right at the reply: {elapsed:?}"
    );
    assert_eq!(
        recorded_lines(&record, 1).await,
        vec![serde_json::json!({"jsonrpc":"2.0","id":"e1","result":{"action":"decline"}})]
    );
}

/// No reply before the hard deadline: `cancel` goes to the agent before the
/// turn returns `HardTimeout`, and the close reason asks for the expiry notice.
#[tokio::test]
async fn elicitation_hard_deadline_answers_cancel() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script =
        elicitation_script(&record, &format!("read -t 5 _prompt\necho '{request}'\nrec\nsleep 10"));
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);

    let result = client
        .session_prompt_blocks_with_idle_timeout(
            ELICITATION_SESSION,
            &["hello"],
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(1_200),
        )
        .await;
    assert!(
        matches!(result, Err(AcpError::HardTimeout { .. })),
        "the question keeps idle off, so the hard deadline ends the turn: {result:?}"
    );
    assert!(!client.has_pending_elicitation());
    let Some(elicitation::ElicitationEvent::Opened { reply_tx, .. }) = events.recv().await else {
        panic!("question must open");
    };
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Closed {
            reason: elicitation::CloseReason::Expired,
            ..
        })
    ));
    drop(reply_tx);
    assert_eq!(
        recorded_lines(&record, 1).await,
        vec![elicitation_cancel_line("e1")]
    );
}

/// The prompt response arrives while a question waits: the turn end answers
/// it `cancel`. A second question while one is open is cancelled at once.
#[tokio::test]
async fn elicitation_turn_end_drains_pending() {
    let record = elicitation_record_path();
    let first =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let second =
        elicitation_request_line("e2", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "read -t 5 _prompt\necho '{first}'\necho '{second}'\nrec\n\
             echo '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\nrec\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);

    let result = client
        .session_prompt_blocks_with_idle_timeout(
            ELICITATION_SESSION,
            &["hello"],
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
        )
        .await;
    assert!(matches!(result, Ok(StopReason::EndTurn)), "{result:?}");
    assert!(!client.has_pending_elicitation(), "no question survives a turn");
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Opened { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Closed {
            reason: elicitation::CloseReason::Ended,
            ..
        })
    ));
    assert_eq!(
        recorded_lines(&record, 2).await,
        vec![elicitation_cancel_line("e2"), elicitation_cancel_line("e1")]
    );
}

/// A stop signal drops the turn future while the question waits; the cancel
/// path answers the question `cancel` exactly once before `session/cancel`.
#[tokio::test]
async fn elicitation_session_cancel_answers_cancel() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "read -t 5 _prompt\necho '{request}'\nrec\nrec\n\
             echo '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{\"stopReason\":\"cancelled\"}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);

    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(600),
        client.session_prompt_blocks_with_idle_timeout(
            ELICITATION_SESSION,
            &["hello"],
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
        ),
    )
    .await;
    assert!(dropped.is_err(), "the turn waits for the answer");
    assert!(client.has_pending_elicitation());

    let stop = client
        .cancel_with_cleanup_grace(ELICITATION_SESSION, std::time::Duration::from_secs(5))
        .await;
    assert!(matches!(stop, Ok(StopReason::Cancelled)), "{stop:?}");
    assert!(!client.has_pending_elicitation());
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Opened { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Closed {
            reason: elicitation::CloseReason::Ended,
            ..
        })
    ));
    let lines = recorded_lines(&record, 2).await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], elicitation_cancel_line("e1"));
    assert_eq!(lines[1]["method"], "session/cancel");
}

/// Outside a turn there is no thread to ask in: `cancel`, never `-32601`.
#[tokio::test]
async fn elicitation_before_turn_answers_cancel() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("pre", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "echo '{request}'\nrec\necho '{{\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{{\"sessionId\":\"s\"}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let result = client.read_until_response(5).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        recorded_lines(&record, 1).await,
        vec![elicitation_cancel_line("pre")]
    );
}

/// However many requests a turn refuses, the thread gets one notice.
#[tokio::test]
async fn elicitation_refusal_notice_once_per_turn() {
    let record = elicitation_record_path();
    let bad = |id: &str| {
        elicitation_request_line(
            id,
            ELICITATION_SESSION,
            "form",
            serde_json::json!({"type": "object", "properties": {"n": {"type": "number"}}}),
        )
    };
    let (first, second, third) = (bad("e1"), bad("e2"), bad("e3"));
    let script = elicitation_script(
        &record,
        &format!(
            "echo '{first}'\nrec\necho '{second}'\nrec\necho '{third}'\nrec\n\
             echo '{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);
    client.last_prompt_id = Some(7);
    let max = std::time::Duration::from_secs(10);
    let result = client
        .read_until_response_with_idle_timeout(
            ELICITATION_SESSION,
            7,
            std::time::Duration::from_secs(5),
            tokio::time::Instant::now() + max,
            max,
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(matches!(
        events.try_recv(),
        Ok(elicitation::ElicitationEvent::Refused { .. })
    ));
    assert!(events.try_recv().is_err(), "one notice for the turn");
    assert_eq!(
        recorded_lines(&record, 3).await,
        vec![
            elicitation_cancel_line("e1"),
            elicitation_cancel_line("e2"),
            elicitation_cancel_line("e3"),
        ]
    );
}

/// An answer that reached the turn after its read loop stopped is written by
/// the drain, not replaced by `cancel`: the main loop already counted it as
/// delivered.
#[tokio::test]
async fn elicitation_drain_writes_an_arrived_answer() {
    let record = elicitation_record_path();
    let request =
        elicitation_request_line("e1", ELICITATION_SESSION, "form", elicitation_decision_schema());
    let script = elicitation_script(
        &record,
        &format!(
            "read -t 5 _prompt\necho '{request}'\nrec\nrec\n\
             echo '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{\"stopReason\":\"cancelled\"}}}}'\nsleep 1"
        ),
    );
    let mut client = spawn_script(&script).await;
    let mut events = install_test_elicitation_sink(&mut client);

    // The turn waits for the answer; a stop signal drops its read loop.
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(600),
        client.session_prompt_blocks_with_idle_timeout(
            ELICITATION_SESSION,
            &["hello"],
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
        ),
    )
    .await;
    assert!(dropped.is_err(), "the turn waits for the answer");
    let Some(elicitation::ElicitationEvent::Opened { reply_tx, .. }) = events.recv().await else {
        panic!("question must open");
    };
    // The intake delivers the answer while no read loop runs.
    reply_tx
        .send(elicitation::ElicitationReply::Decline)
        .expect("the reply route stays open after the read loop stops");

    let stop = client
        .cancel_with_cleanup_grace(ELICITATION_SESSION, std::time::Duration::from_secs(5))
        .await;
    assert!(matches!(stop, Ok(StopReason::Cancelled)), "{stop:?}");
    assert!(matches!(
        events.recv().await,
        Some(elicitation::ElicitationEvent::Closed {
            reason: elicitation::CloseReason::Replied,
            ..
        })
    ));
    let lines = recorded_lines(&record, 2).await;
    assert_eq!(
        lines[0],
        serde_json::json!({"jsonrpc":"2.0","id":"e1","result":{"action":"decline"}})
    );
    assert_eq!(lines[1]["method"], "session/cancel");
}
