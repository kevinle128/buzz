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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::acp::elicitation::{
    self, CloseReason, ElicitationEvent, ElicitationReply, ElicitationSink, ElicitationTurn,
    FormSchema,
};
use crate::relay::{RelayEventPublisher, RestClient};
use crate::scope::SessionScope;

/// Longest wait for the agent's own kind:0 profile.
const AGENT_LABEL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of hex characters of a pubkey used as a short label.
const SHORT_PUBKEY_CHARS: usize = 8;

/// Kind of the stream message a thread answer must be.
const KIND_STREAM_MESSAGE: u16 = 9;

/// Most hint and "already answered" replies the harness posts for one
/// question. Later replies of that kind are consumed without a post, so a
/// chatty thread cannot make the agent flood it.
pub(crate) const MAX_THREAD_REPLIES_PER_QUESTION: u8 = 3;

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

/// An admitted listener event offered to the open question.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnswerCandidate<'a> {
    /// The event as signed.
    pub event: &'a nostr::Event,
    /// The author the listener gate admitted (for a relay-signed workflow
    /// message this is the workflow owner, not the signer).
    pub effective_author: &'a str,
    /// Whether the active relay identity signed the event.
    pub relay_signed: bool,
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

/// Result of recording a question the read loop opened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenOutcome {
    /// The question is the open question of its scope; post it.
    Opened,
    /// Another turn's question still holds the scope. The new question was
    /// answered `cancel`; `notice` is true for the first such refusal of the
    /// turn only.
    Refused { notice: bool },
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
    state: QuestionState,
    /// Hint and "already answered" replies posted for this question.
    replies_posted: u8,
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
    /// Turns that already got the "one question at a time" notice. Cleared
    /// with the turn, so it holds in-flight turns only.
    busy_noticed_turns: HashSet<String>,
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
    ///
    /// The first question of a scope keeps the scope until its turn ends: a
    /// question from another turn of the same scope (a forked session) is
    /// answered `cancel` at once, so that one thread never has two questions
    /// that the same replies could answer. A new question of the same turn
    /// replaces that turn's answered question.
    pub(crate) fn open(
        &mut self,
        turn: &ElicitationTurn,
        agent_index: usize,
        request_id: String,
        schema: FormSchema,
        reply_tx: oneshot::Sender<ElicitationReply>,
    ) -> OpenOutcome {
        if let Some(existing) = self.by_scope.get(&turn.scope) {
            if existing.turn_id != turn.turn_id {
                tracing::warn!(
                    scope = %turn.scope.telemetry_label(),
                    open_turn = %existing.turn_id,
                    "a question of another turn holds this scope — new question cancelled"
                );
                let _ = reply_tx.send(ElicitationReply::Cancel);
                let notice = self.busy_noticed_turns.insert(turn.turn_id.clone());
                return OpenOutcome::Refused { notice };
            }
        }
        let record = QuestionRecord {
            turn_id: turn.turn_id.clone(),
            agent_index,
            request_id,
            anchor_event_id: turn.anchor_event_id.clone(),
            schema,
            state: QuestionState::Pending { reply_tx },
            replies_posted: 0,
        };
        self.by_scope.insert(turn.scope.clone(), record);
        OpenOutcome::Opened
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

    /// Whether `scope` has a question record, pending or answered. The
    /// record lives until its turn ends.
    pub(crate) fn has_question(&self, scope: &SessionScope) -> bool {
        self.by_scope.contains_key(scope)
    }

    /// Remove the record of the turn `turn_id` (the turn ended or stopped).
    pub(crate) fn clear_turn(&mut self, turn_id: &str) {
        self.by_scope.retain(|_, r| r.turn_id != turn_id);
        self.busy_noticed_turns.remove(turn_id);
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

    /// Whether `candidate` may answer the question of `scope`: a kind 9
    /// reply in the question thread that `p`-tags the agent and is signed by
    /// a human who is its own author.
    ///
    /// The author already passed the listener author gate, which is the
    /// answer gate: anyone the agent responds to may answer. The checks here
    /// keep everything that is not that person out:
    /// - the agent itself, so its own posts never answer or loop;
    /// - events that do not `p`-tag the agent, so subscription modes that
    ///   admit plain chat (`--subscribe all`, no mention filter) cannot
    ///   answer;
    /// - relay-signed events and `buzz:workflow` messages, because the gate
    ///   attributes them to another principal (the workflow owner or the
    ///   relay key), not to a person who wrote the reply.
    pub(crate) fn accepts_candidate(
        &self,
        scope: &SessionScope,
        candidate: &AnswerCandidate<'_>,
        agent_hex: &str,
    ) -> bool {
        let Some(record) = self.by_scope.get(scope) else {
            return false;
        };
        let event = candidate.event;
        let signer = event.pubkey.to_hex();
        let is_workflow = event
            .tags
            .iter()
            .any(|tag| tag.as_slice().first().map(String::as_str) == Some("buzz:workflow"));
        let mentions_agent = event.tags.iter().any(|tag| {
            let parts = tag.as_slice();
            parts.first().map(String::as_str) == Some("p")
                && parts
                    .get(1)
                    .is_some_and(|key| key.eq_ignore_ascii_case(agent_hex))
        });
        let in_thread = crate::queue::parse_thread_tags(event)
            .root_event_id
            .is_some_and(|root| root.eq_ignore_ascii_case(&record.anchor_event_id));
        event.kind.as_u16() == KIND_STREAM_MESSAGE
            && !signer.eq_ignore_ascii_case(agent_hex)
            && mentions_agent
            && in_thread
            && !candidate.relay_signed
            && !is_workflow
            && signer.eq_ignore_ascii_case(candidate.effective_author)
    }

    /// Check an admitted event of `scope` against the open question.
    ///
    /// `known_names` holds the agent display name for the mention strip.
    pub(crate) fn intake(
        &mut self,
        scope: &SessionScope,
        candidate: AnswerCandidate<'_>,
        agent_hex: &str,
        known_names: &[&str],
    ) -> Intake {
        if !self.accepts_candidate(scope, &candidate, agent_hex) {
            return Intake::NotForQuestion;
        }
        let Some(record) = self.by_scope.get_mut(scope) else {
            return Intake::NotForQuestion;
        };
        let author_hex = candidate.effective_author;
        let parsed =
            elicitation::parse_reply(&candidate.event.content, &record.schema, known_names);
        let reply_text = match (&record.state, parsed) {
            (QuestionState::Answered { by, at }, Some(_)) => {
                tracing::info!(
                    scope = %scope.telemetry_label(),
                    answered_by = %by,
                    answered_at = ?at,
                    "second answer to a question — not sent"
                );
                elicitation::already_answered_text(&short_pubkey(by))
            }
            (QuestionState::Answered { .. }, None) => return Intake::NotForQuestion,
            (QuestionState::Pending { .. }, None) => {
                elicitation::hint_text(&record.schema, known_names.first().copied())
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
                    // The question is closed, so the turn is ending. The
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
                return Intake::Consumed { reply: None };
            }
        };
        if record.replies_posted >= MAX_THREAD_REPLIES_PER_QUESTION {
            tracing::debug!(
                scope = %scope.telemetry_label(),
                "reply limit of the question reached — reply consumed without a post"
            );
            return Intake::Consumed { reply: None };
        }
        record.replies_posted += 1;
        Intake::Consumed {
            reply: Some(ThreadReply {
                channel_id: scope.channel_id(),
                anchor_event_id: record.anchor_event_id.clone(),
                mention: Some(author_hex.to_string()),
                content: reply_text,
            }),
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

/// Posts question messages as the agent and knows the agent display name.
/// Built once per listener run; every post runs in its own task so that
/// relay back-pressure never blocks the main loop.
#[derive(Clone)]
pub(crate) struct ThreadPoster {
    keys: nostr::Keys,
    agent_hex: String,
    publisher: RelayEventPublisher,
    /// Used to read the agent display name.
    rest: Option<RestClient>,
    events_tx: mpsc::UnboundedSender<ElicitationEvent>,
    /// The agent display name, set by the first lookup that finds it.
    label: Arc<OnceLock<String>>,
}

impl ThreadPoster {
    pub(crate) fn new(
        keys: nostr::Keys,
        publisher: RelayEventPublisher,
        rest: Option<RestClient>,
        events_tx: mpsc::UnboundedSender<ElicitationEvent>,
    ) -> Self {
        Self {
            agent_hex: keys.public_key().to_hex(),
            keys,
            publisher,
            rest,
            events_tx,
            label: Arc::new(OnceLock::new()),
        }
    }

    /// The agent pubkey hex.
    pub(crate) fn agent_hex(&self) -> &str {
        &self.agent_hex
    }

    /// The agent display name, looked up once and kept. While no lookup has
    /// found it, every call looks it up again (bounded by
    /// [`AGENT_LABEL_LOOKUP_TIMEOUT`]).
    pub(crate) async fn agent_label(&self) -> Option<String> {
        if let Some(label) = self.label.get() {
            return Some(label.clone());
        }
        let rest = self.rest.as_ref()?;
        let label = lookup_agent_label(rest, &self.keys.public_key()).await?;
        Some(self.label.get_or_init(|| label).clone())
    }

    /// Start the display name lookup in the background.
    pub(crate) fn prefetch_agent_label(&self) {
        let poster = self.clone();
        tokio::spawn(async move {
            if poster.agent_label().await.is_none() {
                tracing::warn!("agent display name not found yet — next question retries");
            }
        });
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
    fn post_notice(&self, turn: &ElicitationTurn, owner: Option<&str>, text: &str) {
        self.post(ThreadReply {
            channel_id: turn.scope.channel_id(),
            anchor_event_id: turn.anchor_event_id.clone(),
            mention: owner.map(str::to_string),
            content: text.to_string(),
        });
    }

    /// Post the question. On failure the task reports `PostFailed`, which
    /// answers the question `cancel`.
    fn post_question(
        &self,
        turn: Arc<ElicitationTurn>,
        request_id: String,
        message: String,
        schema: FormSchema,
        owner: Option<String>,
    ) {
        let poster = self.clone();
        tokio::spawn(async move {
            let label = poster.agent_label().await;
            let reply = ThreadReply {
                channel_id: turn.scope.channel_id(),
                anchor_event_id: turn.anchor_event_id.clone(),
                mention: owner,
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
            if let Err(error) = result {
                tracing::error!(
                    scope = %turn.scope.telemetry_label(),
                    "could not post the question — answering cancel: {error}"
                );
                // A gone main loop has dropped every record already.
                let _ = poster.events_tx.send(ElicitationEvent::PostFailed {
                    turn_id: turn.turn_id.clone(),
                    request_id,
                });
            }
        });
    }
}

/// The agent display name from its newest kind:0 profile (`display_name`,
/// else `name`), the same field the relay matches `@name` mentions against.
async fn lookup_agent_label(rest: &RestClient, agent: &nostr::PublicKey) -> Option<String> {
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

/// Apply one [`ElicitationEvent`] in the main loop. `owner` is the owner
/// pubkey hex when known; the question and its notices notify the owner.
pub(crate) fn handle_event(
    pool: &mut crate::pool::AgentPool,
    poster: &ThreadPoster,
    event: ElicitationEvent,
    owner: Option<&str>,
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
            match pool.elicitations_mut().open(
                &turn,
                agent_index,
                request_id.clone(),
                schema.clone(),
                reply_tx,
            ) {
                OpenOutcome::Opened => poster.post_question(
                    turn,
                    request_id,
                    message,
                    schema,
                    owner.map(str::to_string),
                ),
                OpenOutcome::Refused { notice: true } => {
                    poster.post_notice(&turn, owner, elicitation::BUSY_NOTICE)
                }
                OpenOutcome::Refused { notice: false } => {}
            }
        }
        ElicitationEvent::Closed {
            turn,
            request_id,
            reason,
        } => {
            pool.elicitations_mut()
                .closed(&turn.turn_id, &request_id, reason);
            if reason == CloseReason::Expired {
                poster.post_notice(&turn, owner, elicitation::EXPIRED_NOTICE);
            }
        }
        ElicitationEvent::Refused { turn } => {
            poster.post_notice(&turn, owner, elicitation::UNSUPPORTED_NOTICE);
        }
        ElicitationEvent::PostFailed {
            turn_id,
            request_id,
        } => pool.elicitations_mut().post_failed(&turn_id, &request_id),
    }
}
