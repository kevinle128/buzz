//! Main-loop side of ACP form elicitation: the record of open thread
//! questions, the answer intake for thread replies, and the posting of the
//! question and its notices.
//!
//! The read loop of a prompt task cannot publish to the relay, so it sends
//! [`ElicitationEvent`]s to the main loop. The main loop owns one record per
//! session scope. A thread reply that answers a pending question goes back to
//! the read loop through the question's own oneshot sender, which binds the
//! answer to the exact turn and JSON-RPC request that asked. Removing a
//! record drops that sender, and the read loop then answers `cancel`, so a
//! removed record can never leave the agent waiting.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::acp::elicitation::{
    self, CloseReason, ElicitationEvent, ElicitationReply, ElicitationSink, ElicitationTurn,
    FormSchema,
};
use crate::relay::{RelayEventPublisher, RestClient};
use crate::scope::SessionScope;

/// Longest wait for the agent's own kind:0 profile before the question is
/// posted without the display name.
const AGENT_LABEL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of hex characters of a pubkey used as a short label.
const SHORT_PUBKEY_CHARS: usize = 8;

/// Kind of the stream message a thread answer must be.
const KIND_STREAM_MESSAGE: u16 = 9;

/// A plain-text kind 9 reply that the harness posts in a question thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadReply {
    /// Channel of the thread.
    pub channel_id: Uuid,
    /// Lowercase hex id of the thread root.
    pub anchor_event_id: String,
    /// Pubkey hex to `p`-tag, if any.
    pub mention: Option<String>,
    /// Message text.
    pub content: String,
}

/// What the listener does with an admitted event after the intake check.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Intake {
    /// The event is not for an open question. It takes the normal path.
    NotForQuestion,
    /// The event was used by the question. It is not queued; the harness
    /// posts `reply` when there is one.
    Consumed { reply: Option<ThreadReply> },
}

#[derive(Debug)]
enum QuestionState {
    /// Posted (or being posted); the first valid answer wins.
    Pending {
        reply_tx: oneshot::Sender<ElicitationReply>,
    },
    /// Answered. Kept until the turn ends so that a second answer gets the
    /// "already answered" reply instead of a new prompt.
    Answered {
        by: String,
        at: std::time::SystemTime,
    },
}

#[derive(Debug)]
struct QuestionRecord {
    turn_id: String,
    agent_index: usize,
    request_id: String,
    anchor_event_id: String,
    schema: FormSchema,
    agent_label: Option<String>,
    state: QuestionState,
}

impl QuestionRecord {
    fn is_for(&self, turn_id: &str, request_id: &str) -> bool {
        self.turn_id == turn_id && self.request_id == request_id
    }
}

/// Open thread questions, one per session scope. Owned by
/// [`crate::pool::AgentPool`] so that dispatch decisions can see them.
#[derive(Debug, Default)]
pub(crate) struct ElicitationRegistry {
    events_tx: Option<mpsc::UnboundedSender<ElicitationEvent>>,
    by_scope: HashMap<SessionScope, QuestionRecord>,
}

impl ElicitationRegistry {
    /// Set the main-loop event route once. A replaced pool calls this again.
    pub(crate) fn ensure_events_tx(&mut self, tx: &mpsc::UnboundedSender<ElicitationEvent>) {
        if self.events_tx.is_none() {
            self.events_tx = Some(tx.clone());
        }
    }

    /// The per-turn route for a prompt task, or `None` before the main loop
    /// set the event route (the task then refuses every question).
    pub(crate) fn sink_for(&self, turn: ElicitationTurn) -> Option<ElicitationSink> {
        self.events_tx
            .as_ref()
            .map(|tx| ElicitationSink::new(tx.clone(), turn))
    }

    /// Record a question that the read loop opened.
    pub(crate) fn open(
        &mut self,
        turn: &ElicitationTurn,
        agent_index: usize,
        request_id: String,
        schema: FormSchema,
        reply_tx: oneshot::Sender<ElicitationReply>,
    ) {
        let record = QuestionRecord {
            turn_id: turn.turn_id.clone(),
            agent_index,
            request_id,
            anchor_event_id: turn.anchor_event_id.clone(),
            schema,
            agent_label: None,
            state: QuestionState::Pending { reply_tx },
        };
        if let Some(previous) = self.by_scope.insert(turn.scope.clone(), record) {
            tracing::info!(
                scope = %turn.scope.telemetry_label(),
                previous_request = %previous.request_id,
                "new question replaces the previous question record of this scope"
            );
        }
    }

    /// Store the agent label that the posted question used, so that replies
    /// are parsed with the same mention text.
    pub(crate) fn posted(&mut self, turn_id: &str, request_id: &str, label: Option<String>) {
        if let Some(record) = self
            .by_scope
            .values_mut()
            .find(|r| r.is_for(turn_id, request_id))
        {
            record.agent_label = label;
        }
    }

    /// The question could not be posted: answer `cancel` at once.
    pub(crate) fn post_failed(&mut self, turn_id: &str, request_id: &str) {
        let scope = self
            .by_scope
            .iter()
            .find(|(_, r)| r.is_for(turn_id, request_id))
            .map(|(scope, _)| scope.clone());
        let Some(record) = scope.and_then(|scope| self.by_scope.remove(&scope)) else {
            return;
        };
        if let QuestionState::Pending { reply_tx } = record.state {
            let _ = reply_tx.send(ElicitationReply::Cancel);
        }
    }

    /// The read loop closed a question. A `Replied` close keeps an
    /// `Answered` record; every other close removes the record.
    pub(crate) fn closed(&mut self, turn_id: &str, request_id: &str, reason: CloseReason) {
        self.by_scope.retain(|_, record| {
            !record.is_for(turn_id, request_id)
                || (reason == CloseReason::Replied
                    && matches!(record.state, QuestionState::Answered { .. }))
        });
    }

    /// Whether `scope` has a question that waits for its first answer.
    pub(crate) fn is_pending(&self, scope: &SessionScope) -> bool {
        self.by_scope
            .get(scope)
            .is_some_and(|r| matches!(r.state, QuestionState::Pending { .. }))
    }

    /// Remove the record of the turn `turn_id` (the turn ended or stopped).
    pub(crate) fn clear_turn(&mut self, turn_id: &str) {
        self.by_scope.retain(|_, r| r.turn_id != turn_id);
    }

    /// Remove the record of one scope (its session was invalidated).
    pub(crate) fn clear_scope(&mut self, scope: &SessionScope) {
        self.by_scope.remove(scope);
    }

    /// Remove every record of one channel (its sessions were invalidated).
    pub(crate) fn clear_channel(&mut self, channel_id: Uuid) {
        self.by_scope
            .retain(|scope, _| scope.channel_id() != channel_id);
    }

    /// Remove every record of the worker `agent_index` (it was replaced).
    pub(crate) fn clear_agent(&mut self, agent_index: usize) {
        self.by_scope.retain(|_, r| r.agent_index != agent_index);
    }

    /// Check an admitted event of `scope` against the open question.
    ///
    /// Only a kind 9 reply whose NIP-10 root is the question thread can
    /// answer. The author already passed the listener author gate, which is
    /// the answer gate: anyone the agent responds to may answer.
    pub(crate) fn intake(
        &mut self,
        scope: &SessionScope,
        event: &nostr::Event,
        author_hex: &str,
    ) -> Intake {
        let Some(record) = self.by_scope.get_mut(scope) else {
            return Intake::NotForQuestion;
        };
        if event.kind.as_u16() != KIND_STREAM_MESSAGE {
            return Intake::NotForQuestion;
        }
        let in_thread = crate::queue::parse_thread_tags(event)
            .root_event_id
            .is_some_and(|root| root.eq_ignore_ascii_case(&record.anchor_event_id));
        if !in_thread {
            return Intake::NotForQuestion;
        }
        let parsed = elicitation::parse_reply(
            &event.content,
            &record.schema,
            record.agent_label.as_deref(),
        );
        let reply_to = |content: String| ThreadReply {
            channel_id: scope.channel_id(),
            anchor_event_id: record.anchor_event_id.clone(),
            mention: Some(author_hex.to_string()),
            content,
        };
        match (&record.state, parsed) {
            (QuestionState::Answered { by, at }, Some(_)) => {
                tracing::info!(
                    scope = %scope.telemetry_label(),
                    answered_by = %by,
                    answered_at = ?at,
                    "second answer to a question — not sent"
                );
                let reply = reply_to(elicitation::already_answered_text(&short_pubkey(by)));
                Intake::Consumed { reply: Some(reply) }
            }
            (QuestionState::Answered { .. }, None) => Intake::NotForQuestion,
            (QuestionState::Pending { .. }, None) => {
                let hint = elicitation::hint_text(&record.schema, record.agent_label.as_deref());
                Intake::Consumed {
                    reply: Some(reply_to(hint)),
                }
            }
            (QuestionState::Pending { .. }, Some(answer)) => {
                let answered = QuestionState::Answered {
                    by: author_hex.to_string(),
                    at: std::time::SystemTime::now(),
                };
                let QuestionState::Pending { reply_tx } =
                    std::mem::replace(&mut record.state, answered)
                else {
                    return Intake::NotForQuestion;
                };
                if reply_tx.send(answer).is_err() {
                    // The read loop is gone, so the turn is ending. The
                    // message takes the normal path and is not lost.
                    tracing::warn!(
                        scope = %scope.telemetry_label(),
                        "answer arrived after the question closed — queueing it"
                    );
                    self.by_scope.remove(scope);
                    return Intake::NotForQuestion;
                }
                tracing::info!(
                    scope = %scope.telemetry_label(),
                    answered_by = %author_hex,
                    "question answered from the thread"
                );
                Intake::Consumed { reply: None }
            }
        }
    }
}

fn short_pubkey(hex: &str) -> String {
    hex.chars().take(SHORT_PUBKEY_CHARS).collect()
}

/// Build the signed kind 9 thread reply for `reply`.
pub(crate) fn build_thread_reply(
    keys: &nostr::Keys,
    reply: &ThreadReply,
) -> anyhow::Result<nostr::Event> {
    let anchor = nostr::EventId::from_hex(&reply.anchor_event_id)
        .map_err(|e| anyhow::anyhow!("invalid question thread id: {e}"))?;
    let thread_ref = buzz_sdk::ThreadRef {
        root_event_id: anchor,
        parent_event_id: anchor,
    };
    let mentions: Vec<&str> = reply.mention.as_deref().into_iter().collect();
    buzz_sdk::build_message(
        reply.channel_id,
        &reply.content,
        Some(&thread_ref),
        &mentions,
        false,
        &[],
        &[],
    )
    .map_err(|e| anyhow::anyhow!("failed to build question thread reply: {e}"))?
    .sign_with_keys(keys)
    .map_err(|e| anyhow::anyhow!("failed to sign question thread reply: {e}"))
}

/// Posts question messages as the agent. Every post runs in its own task so
/// that relay back-pressure never blocks the main loop.
#[derive(Clone)]
pub(crate) struct ThreadPoster {
    keys: nostr::Keys,
    publisher: RelayEventPublisher,
    /// Owner pubkey hex; the question and its notices notify the owner.
    owner: Option<String>,
    /// Used to read the agent display name for the mention examples.
    rest: Option<RestClient>,
    events_tx: mpsc::UnboundedSender<ElicitationEvent>,
}

impl ThreadPoster {
    pub(crate) fn new(
        keys: nostr::Keys,
        publisher: RelayEventPublisher,
        owner: Option<String>,
        rest: Option<RestClient>,
        events_tx: mpsc::UnboundedSender<ElicitationEvent>,
    ) -> Self {
        Self {
            keys,
            publisher,
            owner,
            rest,
            events_tx,
        }
    }

    /// Post `reply`. A failure is logged; it has no question to cancel.
    pub(crate) fn post(&self, reply: ThreadReply) {
        let keys = self.keys.clone();
        let publisher = self.publisher.clone();
        tokio::spawn(async move {
            let result = match build_thread_reply(&keys, &reply) {
                Ok(event) => publisher
                    .publish_event(event)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}")),
                Err(e) => Err(e),
            };
            if let Err(error) = result {
                tracing::warn!(
                    channel = %reply.channel_id,
                    "could not post question thread reply: {error}"
                );
            }
        });
    }

    /// Post a notice to the owner in the thread of `turn`.
    fn post_notice(&self, turn: &ElicitationTurn, text: &str) {
        self.post(ThreadReply {
            channel_id: turn.scope.channel_id(),
            anchor_event_id: turn.anchor_event_id.clone(),
            mention: self.owner.clone(),
            content: text.to_string(),
        });
    }

    /// Post the question. The task reports `Posted` with the agent label, or
    /// `PostFailed`, which answers the question `cancel`.
    fn post_question(
        &self,
        turn: Arc<ElicitationTurn>,
        request_id: String,
        message: String,
        schema: FormSchema,
    ) {
        let poster = self.clone();
        tokio::spawn(async move {
            let label = match &poster.rest {
                Some(rest) => agent_label(rest, &poster.keys.public_key()).await,
                None => None,
            };
            let reply = ThreadReply {
                channel_id: turn.scope.channel_id(),
                anchor_event_id: turn.anchor_event_id.clone(),
                mention: poster.owner.clone(),
                content: elicitation::question_text(&message, &schema, label.as_deref()),
            };
            let result = match build_thread_reply(&poster.keys, &reply) {
                Ok(event) => poster
                    .publisher
                    .publish_event(event)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}")),
                Err(e) => Err(e),
            };
            let event = match result {
                Ok(()) => ElicitationEvent::Posted {
                    turn_id: turn.turn_id.clone(),
                    request_id,
                    agent_label: label,
                },
                Err(error) => {
                    tracing::error!(
                        scope = %turn.scope.telemetry_label(),
                        "could not post the question — answering cancel: {error}"
                    );
                    ElicitationEvent::PostFailed {
                        turn_id: turn.turn_id.clone(),
                        request_id,
                    }
                }
            };
            // A gone main loop has dropped every record already.
            let _ = poster.events_tx.send(event);
        });
    }
}

/// The agent display name from its newest kind:0 profile (`display_name`,
/// else `name`), the same field the relay matches `@name` mentions against.
async fn agent_label(rest: &RestClient, agent: &nostr::PublicKey) -> Option<String> {
    let filter = nostr::Filter::new()
        .kind(nostr::Kind::Metadata)
        .author(*agent)
        .limit(1);
    let response = tokio::time::timeout(
        AGENT_LABEL_LOOKUP_TIMEOUT,
        rest.query(std::slice::from_ref(&filter)),
    )
    .await;
    let events = match response {
        Ok(Ok(events)) => events,
        Ok(Err(error)) => {
            tracing::debug!("agent profile lookup failed: {error}");
            return None;
        }
        Err(_) => {
            tracing::debug!("agent profile lookup timed out");
            return None;
        }
    };
    let content = events.as_array()?.first()?.get("content")?.as_str()?;
    let profile: serde_json::Value = serde_json::from_str(content).ok()?;
    profile
        .get("display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| profile.get("name").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Apply one [`ElicitationEvent`] in the main loop.
pub(crate) fn handle_event(
    pool: &mut crate::pool::AgentPool,
    poster: &ThreadPoster,
    event: ElicitationEvent,
) {
    match event {
        ElicitationEvent::Opened {
            turn,
            request_id,
            message,
            schema,
            reply_tx,
        } => {
            // Fence by turn: a question of a task that already returned is
            // dropped here, and dropping `reply_tx` answers it `cancel`.
            let Some(agent_index) = pool
                .task_map()
                .values()
                .find(|meta| meta.turn_id == turn.turn_id)
                .map(|meta| meta.agent_index)
            else {
                tracing::warn!(
                    scope = %turn.scope.telemetry_label(),
                    "question from a turn that is no longer in flight — cancelled"
                );
                return;
            };
            pool.elicitations_mut().open(
                &turn,
                agent_index,
                request_id.clone(),
                schema.clone(),
                reply_tx,
            );
            poster.post_question(turn, request_id, message, schema);
        }
        ElicitationEvent::Closed {
            turn,
            request_id,
            reason,
        } => {
            pool.elicitations_mut()
                .closed(&turn.turn_id, &request_id, reason);
            if reason == CloseReason::Expired {
                poster.post_notice(&turn, elicitation::EXPIRED_NOTICE);
            }
        }
        ElicitationEvent::Refused { turn } => {
            poster.post_notice(&turn, elicitation::UNSUPPORTED_NOTICE);
        }
        ElicitationEvent::Posted {
            turn_id,
            request_id,
            agent_label,
        } => pool
            .elicitations_mut()
            .posted(&turn_id, &request_id, agent_label),
        ElicitationEvent::PostFailed {
            turn_id,
            request_id,
        } => pool.elicitations_mut().post_failed(&turn_id, &request_id),
    }
}
