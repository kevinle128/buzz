//! The `_buzz/wake` ACP extension: wire names, request checks, the outcome
//! rules, and the wake prompt.
//!
//! An agent that advertised `buzz.wake` version 1 can send the request
//! `_buzz/wake` at any time, also between turns. Buzz answers when it has
//! queued or refused a turn (not after the turn):
//!
//! - `accepted`: a wake turn is queued in the thread of the request;
//! - `duplicate`: this `wakeId` was accepted before; no new turn;
//! - `busy`: the thread already has a queued wake turn, or the thread is
//!   inside its wake limits; the agent sends the request again later;
//! - JSON-RPC error `-32602`: the request is malformed or out of scope.
//!
//! This module holds only pure logic. The reader task and the main loop use
//! it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::scope::SessionScope;

/// JSON-RPC method of the wake request.
pub(crate) const WAKE_METHOD: &str = "_buzz/wake";
/// Key in `_meta` of the `initialize` capabilities on both sides.
pub(crate) const WAKE_CAPABILITY: &str = "buzz.wake";
/// Key in `_meta` of `session/new` and `session/prompt` that names the thread.
pub(crate) const THREAD_META: &str = "buzz.thread";
/// Largest wake request line, in bytes.
pub(crate) const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Largest `prompt` text of a wake request, in bytes.
pub(crate) const MAX_PROMPT_BYTES: usize = 2 * 1024;
/// Largest `wakeId`, in bytes.
pub(crate) const MAX_WAKE_ID_BYTES: usize = 128;
/// JSON-RPC error code for a malformed or out-of-scope request.
pub(crate) const INVALID_PARAMS: i64 = -32602;
/// How long an accepted `wakeId` is remembered.
pub(crate) const DEDUP_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Most accepted `wakeId`s that are remembered; the oldest is dropped first.
pub(crate) const DEDUP_CAPACITY: usize = 4096;
/// Smallest time between two accepted wakes of one scope.
pub(crate) const MIN_SPACING: Duration = Duration::from_secs(30);
/// Window of the per-scope wake budget.
pub(crate) const BUDGET_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Accepted wakes of one scope in one budget window.
pub(crate) const BUDGET_PER_WINDOW: usize = 20;
/// Queued wake turns over all scopes.
pub(crate) const MAX_PENDING_WAKES: usize = 256;
/// Times one accepted wake turn runs before a failure drops it.
pub(crate) const MAX_TURN_ATTEMPTS: u8 = 3;
/// Scopes whose budget is tracked; the least recent is dropped first.
const MAX_TRACKED_SCOPES: usize = 4096;

/// The capability value that both sides send in `initialize`.
pub(crate) fn capability_value() -> Value {
    serde_json::json!({ "version": 1 })
}

/// Whether an `initialize` result advertises `buzz.wake` version 1.
///
/// The value must be exactly `{"version": 1}`; any other value means "not
/// supported". The capability is read from `agentCapabilities._meta`, and
/// also from the result's own `_meta`, where other extensions put theirs.
pub(crate) fn advertised_v1(init_result: &Value) -> bool {
    let expected = capability_value();
    ["/agentCapabilities/_meta", "/_meta"].iter().any(|path| {
        init_result
            .pointer(path)
            .and_then(|meta| meta.get(WAKE_CAPABILITY))
            == Some(&expected)
    })
}

/// Parse `line` when it is a `_buzz/wake` request (it has a `method` and an
/// `id`). A `_buzz/wake` line without an `id` is not a request.
pub(crate) fn parse_wake_request(line: &str) -> Option<Value> {
    // Cheap filter first; "wake" also matches an escaped `\/` in the method.
    if !line.contains("wake") {
        return None;
    }
    let message: Value = serde_json::from_str(line).ok()?;
    let is_request = message.get("method").and_then(Value::as_str) == Some(WAKE_METHOD)
        && message.get("id").is_some();
    is_request.then_some(message)
}

/// Why the agent asked for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeReason {
    InputRequired,
    PrReady,
    Completed,
    Failed,
    Cancelled,
}

impl WakeReason {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "input_required" => Some(Self::InputRequired),
            "pr_ready" => Some(Self::PrReady),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// The wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InputRequired => "input_required",
            Self::PrReady => "pr_ready",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The thread a turn works in, as sent in `buzz.thread` and echoed back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeThread {
    pub(crate) channel_id: Uuid,
    /// 64 lowercase hex characters.
    pub(crate) root_event_id: String,
}

impl WakeThread {
    /// The `buzz.thread` value for this thread.
    pub(crate) fn meta_value(&self) -> Value {
        serde_json::json!({
            "channelId": self.channel_id.to_string(),
            "rootEventId": self.root_event_id,
        })
    }
}

/// A checked wake request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeRequest {
    pub(crate) wake_id: String,
    pub(crate) reason: WakeReason,
    pub(crate) thread: WakeThread,
    /// Text from the agent's runtime. Untrusted.
    pub(crate) prompt: String,
}

/// The response to a wake request: an outcome, or a `-32602` message.
pub(crate) type WakeReply = Result<WakeOutcome, &'static str>;

/// The result of a valid wake request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeOutcome {
    Accepted,
    Duplicate,
    Busy,
}

impl WakeOutcome {
    /// The wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Busy => "busy",
        }
    }
}

/// The JSON-RPC response line for request `id`, without a newline.
pub(crate) fn response_line(id: &Value, reply: &WakeReply) -> String {
    let message = match reply {
        Ok(outcome) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "outcome": outcome.as_str() },
        }),
        Err(message) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": INVALID_PARAMS, "message": message },
        }),
    };
    message.to_string()
}

fn is_event_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Check a wake request. `frame_len` is the size of the request line in
/// bytes; `child_supports_wake` tells whether the child advertised
/// `buzz.wake` version 1. Keys that are not used are accepted and ignored.
pub(crate) fn parse_request(
    frame_len: usize,
    child_supports_wake: bool,
    params: Option<&Value>,
) -> Result<WakeRequest, &'static str> {
    if frame_len > MAX_FRAME_BYTES {
        return Err("wake request exceeds 16 KiB");
    }
    if !child_supports_wake {
        return Err("agent did not advertise buzz.wake version 1");
    }
    let params = params
        .and_then(Value::as_object)
        .ok_or("params must be an object")?;
    let wake_id = params
        .get("wakeId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= MAX_WAKE_ID_BYTES)
        .ok_or("wakeId must be a non-empty string of at most 128 bytes")?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .and_then(WakeReason::parse)
        .ok_or("reason must be input_required, pr_ready, completed, failed, or cancelled")?;
    let thread = params.get("thread").and_then(Value::as_object);
    let channel_id = thread
        .and_then(|thread| thread.get("channelId"))
        .and_then(Value::as_str)
        .and_then(|id| Uuid::try_parse(id).ok())
        .ok_or("thread.channelId must be a UUID")?;
    let root_event_id = thread
        .and_then(|thread| thread.get("rootEventId"))
        .and_then(Value::as_str)
        .filter(|id| is_event_id(id))
        .ok_or("thread.rootEventId must be 64 lowercase hex characters")?;
    let prompt = params
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| prompt.len() <= MAX_PROMPT_BYTES)
        .ok_or("prompt must be a string of at most 2048 bytes")?;
    Ok(WakeRequest {
        wake_id: wake_id.to_owned(),
        reason,
        thread: WakeThread {
            channel_id,
            root_event_id: root_event_id.to_owned(),
        },
        prompt: prompt.to_owned(),
    })
}

/// The `wakeId` dedup set and the per-scope wake budget.
#[derive(Debug, Default)]
pub(crate) struct WakeLimiter {
    accepted_ids: HashMap<String, Instant>,
    /// Accepted ids in acceptance order, for the TTL and the size bound.
    id_order: VecDeque<(String, Instant)>,
    /// Acceptance times of each scope inside the budget window, oldest first.
    scopes: HashMap<SessionScope, VecDeque<Instant>>,
}

impl WakeLimiter {
    /// Decide the outcome of a valid request for `scope` and record an
    /// acceptance. `blocked` is true when the scope already has a queued
    /// wake turn (or no wake turn can be queued at all).
    pub(crate) fn decide(
        &mut self,
        now: Instant,
        wake_id: &str,
        scope: &SessionScope,
        blocked: bool,
    ) -> WakeOutcome {
        self.prune(now);
        if self.accepted_ids.contains_key(wake_id) {
            return WakeOutcome::Duplicate;
        }
        if blocked {
            return WakeOutcome::Busy;
        }
        if let Some(times) = self.scopes.get(scope) {
            let too_soon = times
                .back()
                .is_some_and(|last| now.saturating_duration_since(*last) < MIN_SPACING);
            if too_soon || times.len() >= BUDGET_PER_WINDOW {
                return WakeOutcome::Busy;
            }
        }
        self.accepted_ids.insert(wake_id.to_owned(), now);
        self.id_order.push_back((wake_id.to_owned(), now));
        self.scopes.entry(scope.clone()).or_default().push_back(now);
        self.prune(now);
        WakeOutcome::Accepted
    }

    /// Forget an accepted id, so that the same id is accepted again. Used
    /// when a queued wake turn is dropped before it ran.
    pub(crate) fn forget(&mut self, wake_id: &str) {
        self.accepted_ids.remove(wake_id);
    }

    fn prune(&mut self, now: Instant) {
        while let Some((id, at)) = self.id_order.front() {
            let expired = now.saturating_duration_since(*at) >= DEDUP_TTL;
            if !expired && self.id_order.len() <= DEDUP_CAPACITY {
                break;
            }
            // An id that was forgotten and accepted again has a newer time.
            if self.accepted_ids.get(id) == Some(at) {
                self.accepted_ids.remove(id);
            }
            self.id_order.pop_front();
        }
        self.scopes.retain(|_, times| {
            while times
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= BUDGET_WINDOW)
            {
                times.pop_front();
            }
            !times.is_empty()
        });
        while self.scopes.len() > MAX_TRACKED_SCOPES {
            let least_recent = self
                .scopes
                .iter()
                .min_by_key(|(_, times)| times.back().copied())
                .map(|(scope, _)| scope.clone());
            match least_recent {
                Some(scope) => self.scopes.remove(&scope),
                None => break,
            };
        }
    }

    #[cfg(test)]
    pub(crate) fn remembered_ids(&self) -> usize {
        self.accepted_ids.len()
    }
}

/// An accepted wake whose turn has not finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingWake {
    pub(crate) request: WakeRequest,
    pub(crate) accepted_at: Instant,
    /// Turns that already ran for this wake and failed.
    pub(crate) failed_attempts: u8,
}

/// Wake admission and the wake turns of each scope: at most one queued and
/// one running per scope. Owned by the event queue, which serializes them
/// with the other turns of the scope.
#[derive(Debug, Default)]
pub(crate) struct WakeBoard {
    limiter: WakeLimiter,
    pending: HashMap<SessionScope, PendingWake>,
    running: HashMap<SessionScope, PendingWake>,
}

impl WakeBoard {
    /// Decide a valid request for `scope`; an accepted one becomes the
    /// scope's queued wake turn.
    pub(crate) fn admit(
        &mut self,
        now: Instant,
        request: WakeRequest,
        scope: SessionScope,
    ) -> WakeOutcome {
        let blocked = self.pending.contains_key(&scope) || self.pending.len() >= MAX_PENDING_WAKES;
        let outcome = self.limiter.decide(now, &request.wake_id, &scope, blocked);
        if outcome == WakeOutcome::Accepted {
            self.pending.insert(
                scope,
                PendingWake {
                    request,
                    accepted_at: now,
                    failed_attempts: 0,
                },
            );
        }
        outcome
    }

    /// Whether `scope` has a queued wake turn.
    pub(crate) fn has_pending(&self, scope: &SessionScope) -> bool {
        self.pending.contains_key(scope)
    }

    /// Scopes with a queued wake turn, in no order.
    pub(crate) fn pending_keys(&self) -> impl Iterator<Item = &SessionScope> {
        self.pending.keys()
    }

    /// Scopes with a queued wake turn, oldest acceptance first.
    pub(crate) fn pending_scopes(&self) -> Vec<SessionScope> {
        let mut scopes: Vec<(&SessionScope, Instant)> = self
            .pending
            .iter()
            .map(|(scope, wake)| (scope, wake.accepted_at))
            .collect();
        scopes.sort_by_key(|(_, at)| *at);
        scopes.into_iter().map(|(scope, _)| scope.clone()).collect()
    }

    /// Move the queued wake turn of `scope` to running and return it.
    pub(crate) fn start(&mut self, scope: &SessionScope) -> Option<PendingWake> {
        let wake = self.pending.remove(scope)?;
        self.running.insert(scope.clone(), wake.clone());
        Some(wake)
    }

    /// Whether a wake turn runs in `scope`.
    pub(crate) fn is_running(&self, scope: &SessionScope) -> bool {
        self.running.contains_key(scope)
    }

    /// The running wake turn of `scope` ended.
    pub(crate) fn finish(&mut self, scope: &SessionScope) {
        self.running.remove(scope);
    }

    /// The running wake turn of `scope` failed. Queue it again unless it
    /// used all its attempts or a newer wake is already queued for the scope.
    /// Returns whether it was queued again. The running record stays until
    /// [`finish`](Self::finish).
    pub(crate) fn retry(&mut self, scope: &SessionScope) -> bool {
        let Some(wake) = self.running.get(scope) else {
            return false;
        };
        let failed_attempts = wake.failed_attempts.saturating_add(1);
        if failed_attempts >= MAX_TURN_ATTEMPTS {
            tracing::warn!(
                target: "acp::wake",
                wake_id = %wake.request.wake_id,
                scope = %scope.telemetry_label(),
                failed_attempts,
                "wake turn failed on every attempt — dropped"
            );
            return false;
        }
        if self.pending.contains_key(scope) {
            tracing::info!(
                target: "acp::wake",
                wake_id = %wake.request.wake_id,
                scope = %scope.telemetry_label(),
                "wake turn failed; a newer wake is queued for the scope"
            );
            return false;
        }
        let mut wake = wake.clone();
        wake.failed_attempts = failed_attempts;
        tracing::warn!(
            target: "acp::wake",
            wake_id = %wake.request.wake_id,
            scope = %scope.telemetry_label(),
            failed_attempts,
            "wake turn failed — queued again"
        );
        self.pending.insert(scope.clone(), wake);
        true
    }

    /// Drop the queued wake turns of `channel_id` and forget their ids, so
    /// that a resend is accepted again.
    pub(crate) fn drop_channel(&mut self, channel_id: Uuid) {
        let dropped: HashSet<String> = self
            .pending
            .iter()
            .filter(|(scope, _)| scope.channel_id() == channel_id)
            .map(|(_, wake)| wake.request.wake_id.clone())
            .collect();
        self.pending
            .retain(|scope, _| scope.channel_id() != channel_id);
        for wake_id in &dropped {
            self.limiter.forget(wake_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn limiter(&self) -> &WakeLimiter {
        &self.limiter
    }
}

/// The prompt of a wake turn. `context` is the rendered `<context>` section
/// of the thread; `runtime_text` is the agent runtime's untrusted text, which
/// is escaped so that it cannot open or close a prompt section.
pub(crate) fn frame_prompt(reason: WakeReason, context: &str, runtime_text: &str) -> String {
    let runtime = crate::prompt_framing::semantic_section(
        "runtime-text",
        &crate::prompt_framing::escape_semantic_text(runtime_text),
    );
    format!(
        "[Buzz wake] Your runtime asked for a turn in this thread (reason: {}).\n{context}\n{runtime}",
        reason.as_str()
    )
}

#[cfg(test)]
#[path = "wake_tests.rs"]
mod tests;
