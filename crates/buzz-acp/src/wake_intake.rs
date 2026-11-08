//! Main-loop side of the `_buzz/wake` extension: check one wake request from
//! an agent child, answer it, and queue its turn in the scope of its thread.

use std::collections::HashSet;

use uuid::Uuid;

use crate::acp::child_io::WakeFrame;
use crate::acp::wake::{self, WakeReply, WakeThread};
use crate::queue::EventQueue;
use crate::scope::{SessionPolicy, SessionScope};

/// What the main loop knows to check a wake request.
pub(crate) struct WakeIntake<'a> {
    pub(crate) policy: SessionPolicy,
    /// Channels this agent is subscribed to. A wake opens no turn elsewhere.
    pub(crate) subscribed: &'a HashSet<Uuid>,
    pub(crate) channel_info: &'a crate::pool::ChannelInfoResolver,
}

/// The session scope of a turn in `thread`, as the listener derives it for a
/// reply in that thread.
pub(crate) fn wake_scope(policy: SessionPolicy, is_dm: bool, thread: &WakeThread) -> SessionScope {
    if policy == SessionPolicy::Thread && !is_dm {
        SessionScope::Thread {
            channel_id: thread.channel_id,
            root_event_id: thread.root_event_id.clone(),
        }
    } else {
        SessionScope::Conversation {
            channel_id: thread.channel_id,
        }
    }
}

/// Check one wake request, queue its turn when accepted, and answer it.
///
/// Returns the reply and the task that writes it. Returns `None`, with no
/// answer, only for a request from a child that was replaced: its stdin is
/// gone and its request must open no turn.
pub(crate) async fn handle_wake_frame(
    frame: WakeFrame,
    queue: &mut EventQueue,
    intake: &WakeIntake<'_>,
) -> Option<(WakeReply, tokio::task::JoinHandle<()>)> {
    let worker = frame.worker;
    let generation = frame.generation();
    let id = frame.id().to_string();
    if !frame.is_current() {
        tracing::info!(
            target: "acp::wake",
            worker, generation, id = %id,
            "wake request from a replaced agent child — ignored"
        );
        return None;
    }
    let params = frame.message.get("params");
    let wake_id = params
        .and_then(|params| params.get("wakeId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-")
        .to_owned();
    let reply = match wake::parse_request(frame.frame_len, frame.child_supports_wake(), params) {
        Err(message) => Err(message),
        Ok(request) if !intake.subscribed.contains(&request.thread.channel_id) => {
            Err("thread.channelId is not a channel of this agent")
        }
        Ok(request) => {
            // The agent's own session and task ids are logged, never put in
            // the prompt.
            tracing::info!(
                target: "acp::wake",
                worker, generation, id = %id, wake_id = %wake_id,
                reason = request.reason.as_str(),
                channel = %request.thread.channel_id,
                root = %request.thread.root_event_id,
                session = ?params.and_then(|params| params.get("sessionId")),
                task = ?params.and_then(|params| params.get("task")),
                "wake request"
            );
            let is_dm = crate::is_dm_channel(request.thread.channel_id, intake.channel_info).await;
            let scope = wake_scope(intake.policy, is_dm, &request.thread);
            Ok(queue.admit_wake(std::time::Instant::now(), request, scope))
        }
    };
    match &reply {
        Ok(outcome) => tracing::info!(
            target: "acp::wake",
            worker, id = %id, wake_id = %wake_id, outcome = outcome.as_str(),
            "wake request answered"
        ),
        Err(message) => tracing::warn!(
            target: "acp::wake",
            worker, id = %id, wake_id = %wake_id, message,
            "wake request refused"
        ),
    }
    let written = frame.respond(reply);
    Some((reply, written))
}
