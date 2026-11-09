use super::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Child stdio over in-memory pipes. `agent_out` is what the agent writes
/// to its stdout; `agent_in` is what the client writes to the agent.
struct Pipes {
    io: ChildIo,
    agent_out: tokio::io::DuplexStream,
    agent_in: BufReader<tokio::io::DuplexStream>,
}

fn pipes() -> Pipes {
    let (stdout, agent_out) = tokio::io::duplex(64 * 1024);
    let (stdin, agent_in) = tokio::io::duplex(64 * 1024);
    Pipes {
        io: start(stdout, stdin),
        agent_out,
        agent_in: BufReader::new(agent_in),
    }
}

impl Pipes {
    async fn agent_writes(&mut self, line: &str) {
        self.agent_out
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("agent write");
    }

    async fn agent_reads(&mut self) -> serde_json::Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.agent_in.read_line(&mut line))
            .await
            .expect("client answers within 5 s")
            .expect("agent read");
        serde_json::from_str(&line).expect("json line")
    }
}

fn wake_line(id: &str) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": wake::WAKE_METHOD, "params": {}})
        .to_string()
}

#[tokio::test]
async fn wake_request_goes_to_the_pool_and_other_lines_to_the_client() {
    let mut p = pipes();
    let (tx, mut rx) = mpsc::channel(4);
    assert!(p.io.link.attach(WakeRoute::new(tx, 3)));
    p.agent_writes(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#)
        .await;
    p.agent_writes(&wake_line("goclaw-1")).await;
    p.agent_writes(r#"{"jsonrpc":"2.0","id":5,"result":{}}"#)
        .await;

    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("wake routed")
        .expect("open");
    assert_eq!(frame.worker, 3);
    assert_eq!(frame.id(), &serde_json::json!("goclaw-1"));
    assert_eq!(frame.generation(), p.io.link.generation());
    assert!(frame.is_current());

    // The client sees the two other lines in order and never the wake.
    let first = p.io.lines.next().await.expect("line").expect("ok");
    assert!(first.contains("session/update"));
    let second = p.io.lines.next().await.expect("line").expect("ok");
    assert!(second.contains("\"id\":5"));

    drop(frame.respond(Ok(wake::WakeOutcome::Accepted)));
    let response = p.agent_reads().await;
    assert_eq!(response["id"], "goclaw-1");
    assert_eq!(response["result"]["outcome"], "accepted");
}

#[tokio::test]
async fn wake_before_the_pool_route_is_answered_busy() {
    let mut p = pipes();
    p.agent_writes(&wake_line("early")).await;
    let response = p.agent_reads().await;
    assert_eq!(response["id"], "early");
    assert_eq!(response["result"]["outcome"], "busy");
}

#[tokio::test]
async fn wake_over_a_full_main_loop_queue_is_answered_busy() {
    let mut p = pipes();
    let (tx, _rx) = mpsc::channel(1);
    p.io.link.attach(WakeRoute::new(tx, 0));
    p.agent_writes(&wake_line("first")).await;
    p.agent_writes(&wake_line("second")).await;
    let response = p.agent_reads().await;
    assert_eq!(response["id"], "second");
    assert_eq!(response["result"]["outcome"], "busy");
}

#[tokio::test]
async fn retired_child_frame_is_not_current() {
    let p = pipes();
    let (tx, mut rx) = mpsc::channel(1);
    p.io.link.attach(WakeRoute::new(tx, 0));
    let mut p = p;
    p.agent_writes(&wake_line("w")).await;
    let frame = rx.recv().await.expect("frame");
    p.io.link.retire();
    assert!(!frame.is_current());
}

#[tokio::test]
async fn dropping_the_guard_retires_the_child() {
    let p = pipes();
    let link = Arc::clone(&p.io.link);
    drop(p.io.guard);
    assert!(link.retired.load(Ordering::Acquire));
}

#[tokio::test]
async fn reader_keeps_id_lines_and_drops_only_notifications_while_idle() {
    let mut p = pipes();
    let notification = r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#;
    let total = LINE_QUEUE_LINES * 2;
    for _ in 0..total {
        p.agent_writes(notification).await;
    }
    p.agent_writes(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)
        .await;
    // A wake still reaches the pool: the reader did not stop.
    let (tx, mut rx) = mpsc::channel(1);
    p.io.link.attach(WakeRoute::new(tx, 0));
    p.agent_writes(&wake_line("after-chatter")).await;
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("reader still reads while idle")
        .expect("frame");

    assert!(p.io.lines.dropped_notifications() > 0);
    let mut kept = 0;
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), p.io.lines.next())
            .await
            .expect("the line with an id is kept")
            .expect("line")
            .expect("ok");
        if line.contains("\"id\":9") {
            break;
        }
        kept += 1;
    }
    assert_eq!(
        u64::try_from(kept).expect("small") + p.io.lines.dropped_notifications(),
        u64::try_from(total).expect("small")
    );
}

#[tokio::test]
async fn reader_waits_instead_of_dropping_while_a_read_loop_runs() {
    let mut p = pipes();
    let reading = p.io.lines.reading();
    let notification = r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#;
    // More lines than the queue holds; all fit into the in-memory pipe.
    let total = LINE_QUEUE_LINES + 8;
    for _ in 0..total {
        p.agent_writes(notification).await;
    }
    // The queue fills and the reader waits for the read loop.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(p.io.lines.dropped_notifications(), 0);
    for _ in 0..total {
        p.io.lines.next().await.expect("line").expect("ok");
    }
    assert_eq!(p.io.lines.dropped_notifications(), 0);
    drop(reading);
}

#[tokio::test]
async fn closed_stdout_ends_the_line_stream_after_errors() {
    let mut p = pipes();
    let long = "x".repeat(super::super::MAX_LINE_SIZE + 1);
    p.agent_writes(&long).await;
    drop(p.agent_out);
    assert!(matches!(
        p.io.lines.next().await,
        Some(Err(LinesCodecError::MaxLineLengthExceeded))
    ));
    assert!(p.io.lines.next().await.is_none());
}

#[tokio::test]
async fn writer_closes_stdin_after_a_frame_that_the_caller_dropped() {
    // Room for less than one frame, so the write blocks mid-frame.
    let (stdout, _agent_out) = tokio::io::duplex(1024);
    let (stdin, mut agent_in) = tokio::io::duplex(8);
    let io = start(stdout, stdin);
    let blocked = tokio::time::timeout(
        Duration::from_millis(50),
        io.writer.write_line("x".repeat(64)),
    )
    .await;
    assert!(blocked.is_err(), "the frame must block");
    let later = tokio::time::timeout(Duration::from_secs(5), io.writer.write_line("{}".into()))
        .await
        .expect("a later write fails at once");
    assert_eq!(
        later.expect_err("stdin closed").kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert!(!io.writer.is_open());
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut agent_in, &mut bytes)
        .await
        .expect("read");
    assert!(
        !bytes.contains(&b'\n'),
        "no frame may follow a partial frame"
    );
}

#[tokio::test]
async fn writer_frames_never_interleave() {
    let mut p = pipes();
    let writers: Vec<_> = (0..16)
        .map(|n| {
            let writer = p.io.writer.clone();
            tokio::spawn(async move {
                let line = serde_json::json!({"n": n, "pad": "y".repeat(10_000)}).to_string();
                writer.write_line(line).await
            })
        })
        .collect();
    // Read while they write: the frames are larger than the pipe.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..16 {
        let frame = p.agent_reads().await;
        seen.insert(frame["n"].as_u64().expect("n"));
    }
    for writer in writers {
        writer.await.expect("task").expect("written");
    }
    assert_eq!(seen.len(), 16);
}
