//! Main-loop tests for ACP form questions: posting, answer intake in the
//! listener, the steer/interrupt and hold guards, and every path that removes
//! a question record.

use super::*;
use crate::acp::elicitation::{
    CloseReason, ElicitationEvent, ElicitationReply, ElicitationTurn, FormSchema,
};
use crate::elicitation_intake::{handle_event, AnswerCandidate, Intake, ThreadPoster};

const TURN_ID: &str = "turn-question";
const REQUEST_ID: &str = "\"e1\"";

fn decision_schema() -> FormSchema {
    FormSchema::parse(&serde_json::json!({
        "type": "object",
        "properties": {
            "decision": {"type": "string", "enum": ["approve", "reject"]},
            "comment": {"type": "string"}
        },
        "required": ["decision"]
    }))
    .expect("supported schema")
}

/// A top-level message that starts a thread, signed by `author`.
fn top_level(author: &nostr::Keys, channel_id: Uuid, mention: &str, text: &str) -> nostr::Event {
    buzz_sdk::build_message(channel_id, text, None, &[mention], false, &[], &[])
        .expect("build message")
        .sign_with_keys(author)
        .expect("sign")
}

/// A kind 9 reply in the thread of `root`, signed by `author`, `p`-tagging
/// `mention`.
fn thread_reply(
    author: &nostr::Keys,
    channel_id: Uuid,
    root: &nostr::Event,
    mention: &str,
    text: &str,
) -> nostr::Event {
    let thread = buzz_sdk::ThreadRef {
        root_event_id: root.id,
        parent_event_id: root.id,
    };
    buzz_sdk::build_message(channel_id, text, Some(&thread), &[mention], false, &[], &[])
        .expect("build reply")
        .sign_with_keys(author)
        .expect("sign")
}

/// The reply the agent gets, or `None` when the route closed or nothing came
/// within 5 s. Bounded so that a broken guard fails the test, not hangs it.
async fn reply_within(
    rx: &mut tokio::sync::oneshot::Receiver<ElicitationReply>,
) -> Option<ElicitationReply> {
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .ok()
        .and_then(Result::ok)
}

fn ingress_for(event: nostr::Event, channel_id: Uuid, author: &str) -> NormalListenerIngress {
    NormalListenerIngress {
        buzz_event: relay::BuzzEvent {
            connection_generation: 0,
            channel_id,
            event,
        },
        effective_author: author.to_string(),
        prompt_tag: "@mention".into(),
        edit: None,
    }
}

fn task_meta(
    agent_index: usize,
    scope: &scope::SessionScope,
    turn_id: &str,
    control_tx: Option<tokio::sync::oneshot::Sender<ControlSignal>>,
    steer_tx: Option<mpsc::Sender<pool::SteerRequest>>,
) -> pool::TaskMeta {
    pool::TaskMeta {
        agent_index,
        channel_id: Some(scope.channel_id()),
        scope: Some(scope.clone()),
        turn_id: turn_id.into(),
        recoverable_batch: None,
        control_tx,
        steer_tx,
        permission_decision_tx: None,
        successful_steer_deliveries: HashSet::new(),
    }
}

/// A pool with one in-flight turn that asked a question in the thread of
/// `root`, opened through the production main-loop handler.
struct QuestionFixture {
    pool: AgentPool,
    scope: scope::SessionScope,
    channel_id: Uuid,
    root: nostr::Event,
    agent: nostr::Keys,
    owner: nostr::Keys,
    reply_rx: tokio::sync::oneshot::Receiver<ElicitationReply>,
    control_rx: tokio::sync::oneshot::Receiver<ControlSignal>,
    published: mpsc::Receiver<nostr::Event>,
    poster: ThreadPoster,
    events_rx: mpsc::UnboundedReceiver<ElicitationEvent>,
}

impl QuestionFixture {
    async fn new(policy: scope::SessionPolicy) -> Self {
        Self::with_options(policy, None, None).await
    }

    async fn with_publisher(
        policy: scope::SessionPolicy,
        publisher: Option<relay::RelayEventPublisher>,
    ) -> Self {
        Self::with_options(policy, publisher, None).await
    }

    /// `rest` serves the agent profile lookup; `None` means no lookup.
    async fn with_options(
        policy: scope::SessionPolicy,
        publisher: Option<relay::RelayEventPublisher>,
        rest: Option<relay::RestClient>,
    ) -> Self {
        let channel_id = Uuid::new_v4();
        let agent = nostr::Keys::generate();
        let owner = nostr::Keys::generate();
        let root = top_level(
            &owner,
            channel_id,
            &agent.public_key().to_hex(),
            "@bot merge?",
        );
        let scope = scope::SessionScope::derive(policy, channel_id, false, &root);
        let mut pool = AgentPool::from_slots(vec![None]);
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();
        let task = pool.join_set.spawn(std::future::pending::<()>());
        pool.task_map_mut().insert(
            task.id(),
            task_meta(0, &scope, TURN_ID, Some(control_tx), None),
        );
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        pool.elicitations_mut().ensure_events_tx(&events_tx);
        let (test_publisher, published) = relay::RelayEventPublisher::test_pair();
        let poster = ThreadPoster::new(
            agent.clone(),
            publisher.unwrap_or(test_publisher),
            rest,
            events_tx,
        );
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let turn = ElicitationTurn {
            turn_id: TURN_ID.into(),
            scope: scope.clone(),
            anchor_event_id: root.id.to_hex(),
        };
        handle_event(
            &mut pool,
            &poster,
            ElicitationEvent::Opened {
                turn: std::sync::Arc::new(turn),
                request_id: REQUEST_ID.into(),
                message: "Approve the merge?".into(),
                schema: decision_schema(),
                reply_tx,
            },
            Some(&owner.public_key().to_hex()),
        );
        Self {
            pool,
            scope,
            channel_id,
            root,
            agent,
            owner,
            reply_rx,
            control_rx,
            published,
            poster,
            events_rx,
        }
    }

    fn turn(&self) -> std::sync::Arc<ElicitationTurn> {
        std::sync::Arc::new(ElicitationTurn {
            turn_id: TURN_ID.into(),
            scope: self.scope.clone(),
            anchor_event_id: self.root.id.to_hex(),
        })
    }

    /// Apply one event through the production main-loop handler.
    fn handle(&mut self, event: ElicitationEvent) {
        let owner = self.owner.public_key().to_hex();
        handle_event(&mut self.pool, &self.poster, event, Some(&owner));
    }

    fn agent_hex(&self) -> String {
        self.agent.public_key().to_hex()
    }

    /// Wait for the next event the harness published.
    async fn next_published(&mut self) -> nostr::Event {
        tokio::time::timeout(Duration::from_secs(5), self.published.recv())
            .await
            .expect("a post within 5 s")
            .expect("publisher open")
    }

    /// Drive one admitted reply through the listener intake and, when it is
    /// not consumed, through the queue push, in main-loop order.
    async fn admit(&mut self, event: nostr::Event, author: &str, queue: &mut EventQueue) -> bool {
        self.admit_signed(event, author, false, queue).await
    }

    /// Like [`admit`](Self::admit), with the gate's relay-signed flag.
    async fn admit_signed(
        &mut self,
        event: nostr::Event,
        author: &str,
        relay_signed: bool,
        queue: &mut EventQueue,
    ) -> bool {
        let ingress = ingress_for(event, self.channel_id, author);
        let scope = ingress.session_scope(
            if self.scope.is_thread() {
                scope::SessionPolicy::Thread
            } else {
                scope::SessionPolicy::Channel
            },
            false,
        );
        match ingress
            .answer_open_question(&scope, relay_signed, &mut self.pool, &self.poster)
            .await
        {
            Some(ingress) => {
                ingress.push(queue, scope, false);
                false
            }
            None => true,
        }
    }

    /// A reply in the question thread is still taken by the question.
    fn question_is_open(&mut self) -> bool {
        let author = nostr::Keys::generate();
        let probe = thread_reply(
            &author,
            self.channel_id,
            &self.root,
            &self.agent_hex(),
            "@bot what?",
        );
        let author_hex = author.public_key().to_hex();
        let agent_hex = self.agent_hex();
        let candidate = AnswerCandidate {
            event: &probe,
            effective_author: &author_hex,
            relay_signed: false,
        };
        !matches!(
            self.pool
                .elicitations_mut()
                .intake(&self.scope, candidate, &agent_hex, &[]),
            Intake::NotForQuestion
        )
    }
}

#[tokio::test]
async fn elicitation_question_event_shape() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let question = fx.next_published().await;
    assert_eq!(question.kind.as_u16(), 9);
    assert_eq!(
        question.pubkey,
        fx.agent.public_key(),
        "signed by the agent key"
    );
    assert!(question.verify().is_ok());
    let tags: Vec<Vec<String>> = question
        .tags
        .iter()
        .map(|t| t.as_slice().to_vec())
        .collect();
    assert!(tags.contains(&vec!["h".to_string(), fx.channel_id.to_string()]));
    let thread = queue::parse_thread_tags(&question);
    assert_eq!(
        thread.root_event_id.as_deref(),
        Some(fx.root.id.to_hex().as_str())
    );
    assert_eq!(
        thread.mentioned_pubkeys,
        vec![fx.owner.public_key().to_hex()],
        "the owner is notified"
    );
    assert!(question
        .content
        .starts_with("Input needed: Approve the merge?"));
    assert!(question.content.contains("one of: approve | reject"));
    assert!(
        fx.events_rx.try_recv().is_err(),
        "a posted question sends no event"
    );
}

#[tokio::test]
async fn elicitation_answer_consumed_not_queued() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let answer = thread_reply(
        &human,
        fx.channel_id,
        &fx.root,
        &fx.agent_hex(),
        "@bot approve looks fine",
    );
    assert!(
        fx.admit(answer, &human.public_key().to_hex(), &mut queue)
            .await
    );
    assert!(
        !queue.has_pending_scope(&fx.scope),
        "the answer is not queued"
    );
    let mut content = serde_json::Map::new();
    content.insert("decision".into(), serde_json::json!("approve"));
    content.insert("comment".into(), serde_json::json!("looks fine"));
    assert_eq!(
        reply_within(&mut fx.reply_rx)
            .await
            .expect("reply delivered"),
        ElicitationReply::Accept(content)
    );
    assert!(!fx.pool.elicitations().is_pending(&fx.scope));
}

#[tokio::test]
async fn elicitation_second_answer_gets_already_answered() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let _question = fx.next_published().await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let first = nostr::Keys::generate();
    let second = nostr::Keys::generate();
    let agent_hex = fx.agent_hex();
    let a1 = thread_reply(&first, fx.channel_id, &fx.root, &agent_hex, "@bot reject");
    assert!(fx.admit(a1, &first.public_key().to_hex(), &mut queue).await);
    assert_eq!(reply_within(&mut fx.reply_rx).await, Some(parse_reject()));

    // The read loop reports the delivered reply; the answered record stays.
    fx.handle(ElicitationEvent::Closed {
        turn: fx.turn(),
        request_id: REQUEST_ID.into(),
        reason: CloseReason::Replied,
    });

    let a2 = thread_reply(&second, fx.channel_id, &fx.root, &agent_hex, "@bot APPROVE");
    assert!(
        fx.admit(a2, &second.public_key().to_hex(), &mut queue)
            .await
    );
    assert!(!queue.has_pending_scope(&fx.scope));
    let notice = fx.next_published().await;
    let first_short: String = first.public_key().to_hex().chars().take(8).collect();
    assert_eq!(
        notice.content,
        format!("Already answered by {first_short}; this reply was not sent.")
    );
    assert_eq!(
        queue::parse_thread_tags(&notice).root_event_id.as_deref(),
        Some(fx.root.id.to_hex().as_str())
    );

    let chat = thread_reply(&second, fx.channel_id, &fx.root, &agent_hex, "@bot thanks!");
    assert!(
        !fx.admit(chat, &second.public_key().to_hex(), &mut queue)
            .await,
        "a reply that is not an answer takes the normal path"
    );
    assert!(queue.has_pending_scope(&fx.scope));
}

fn parse_reject() -> ElicitationReply {
    let mut content = serde_json::Map::new();
    content.insert("decision".into(), serde_json::json!("reject"));
    ElicitationReply::Accept(content)
}

#[tokio::test]
async fn elicitation_unparseable_reply_gets_hint() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let _question = fx.next_published().await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let reply = thread_reply(
        &human,
        fx.channel_id,
        &fx.root,
        &fx.agent_hex(),
        "@bot don't approve yet",
    );
    assert!(
        fx.admit(reply, &human.public_key().to_hex(), &mut queue)
            .await
    );
    assert!(!queue.has_pending_scope(&fx.scope));
    let hint = fx.next_published().await;
    assert!(
        hint.content.contains("one of: approve | reject"),
        "{}",
        hint.content
    );
    assert_eq!(
        queue::parse_thread_tags(&hint).mentioned_pubkeys,
        vec![human.public_key().to_hex()],
        "the hint notifies the person who replied"
    );
    assert!(
        fx.pool.elicitations().is_pending(&fx.scope),
        "still waiting"
    );
    assert!(
        fx.reply_rx.try_recv().is_err(),
        "nothing was sent to the agent"
    );
}

/// Under the channel policy every thread shares one scope: only a reply in
/// the question thread answers; another thread or a new top-level mention
/// takes the normal path.
#[tokio::test]
async fn elicitation_answer_other_thread_ignored() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Channel).await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let agent_hex = fx.agent_hex();
    let other_root = top_level(&human, fx.channel_id, &agent_hex, "@bot other topic");
    let other_thread = thread_reply(
        &human,
        fx.channel_id,
        &other_root,
        &agent_hex,
        "@bot approve",
    );
    assert!(
        !fx.admit(other_thread, &human.public_key().to_hex(), &mut queue)
            .await
    );
    let new_top = top_level(&human, fx.channel_id, &agent_hex, "@bot approve");
    assert!(
        !fx.admit(new_top, &human.public_key().to_hex(), &mut queue)
            .await
    );
    assert!(queue.has_pending_scope(&fx.scope));
    assert!(fx.pool.elicitations().is_pending(&fx.scope));
    assert!(fx.reply_rx.try_recv().is_err());
}

/// Who wrote an author-table event, and how.
#[derive(Clone, Copy, Debug)]
enum AuthorShape {
    /// A thread reply signed by the owner.
    OwnerReply,
    /// A thread reply signed by the allowlisted human.
    AllowedReply,
    /// A thread reply signed by a stranger.
    StrangerReply,
    /// A relay-signed top-level App message.
    RelayTopLevel,
    /// A relay-signed thread reply without workflow tags; the gate
    /// attributes it to the relay key.
    RelayReply,
    /// A human-signed thread reply that carries a `buzz:workflow` tag.
    HumanWorkflowTagged,
    /// A relay-signed workflow thread reply that the gate attributes to the
    /// relay key (no workflow mention of this agent).
    RelayWorkflowReply,
    /// A relay-signed workflow thread reply that the gate attributes to the
    /// verified workflow owner.
    OwnerWorkflowReply,
}

struct AuthorCase {
    name: &'static str,
    respond_to: RespondTo,
    allow_relay: bool,
    shape: AuthorShape,
    admitted: bool,
    consumed: bool,
}

/// A relay-signed kind 9 workflow reply in the thread of `root`.
fn workflow_reply(
    relay_keys: &nostr::Keys,
    channel_id: Uuid,
    root: &nostr::Event,
    owner: &str,
    agent: &str,
    mention_agent: bool,
) -> nostr::Event {
    let root_hex = root.id.to_hex();
    let mut tags = vec![
        nostr::Tag::parse(["p", agent]).expect("p"),
        nostr::Tag::parse(["p", owner, "", "owner"]).expect("owner p"),
        nostr::Tag::parse(["h", &channel_id.to_string()]).expect("h"),
        nostr::Tag::parse(["buzz:workflow", "true"]).expect("workflow"),
        nostr::Tag::parse(["buzz:workflow-owner", owner]).expect("owner"),
        nostr::Tag::parse(["e", &root_hex, "", "reply"]).expect("e"),
    ];
    if mention_agent {
        tags.push(nostr::Tag::parse(["buzz:workflow-mention", agent]).expect("mention"));
    }
    nostr::EventBuilder::new(nostr::Kind::Custom(9), "@bot approve")
        .tags(tags)
        .sign_with_keys(relay_keys)
        .expect("signed workflow reply")
}

/// The answer gate is the listener author gate, and only a person's own
/// signed reply answers. Run the production gate, rule match, and intake in
/// main-loop order.
#[tokio::test]
async fn elicitation_answer_author_table() {
    let relay_keys = nostr::Keys::generate();
    let relay_hex = relay_keys.public_key().to_hex();
    let allowed_human = nostr::Keys::generate();
    let stranger = nostr::Keys::generate();
    let mut base = build_mcp_servers_tests::test_config();
    base.subscribe_mode = config::SubscribeMode::Mentions;
    let rules = startup_subscription_rules(&base).expect("rules");

    let row = |name, respond_to, allow_relay, shape, admitted, consumed| AuthorCase {
        name,
        respond_to,
        allow_relay,
        shape,
        admitted,
        consumed,
    };
    let cases = [
        row(
            "owner",
            RespondTo::OwnerOnly,
            false,
            AuthorShape::OwnerReply,
            true,
            true,
        ),
        row(
            "allowlisted human",
            RespondTo::Allowlist,
            false,
            AuthorShape::AllowedReply,
            true,
            true,
        ),
        row(
            "anyone mode",
            RespondTo::Anyone,
            false,
            AuthorShape::StrangerReply,
            true,
            true,
        ),
        row(
            "dropped by owner-only",
            RespondTo::OwnerOnly,
            false,
            AuthorShape::StrangerReply,
            false,
            false,
        ),
        row(
            "relay-signed top-level App message",
            RespondTo::Allowlist,
            true,
            AuthorShape::RelayTopLevel,
            true,
            false,
        ),
        row(
            "relay-signed thread reply",
            RespondTo::Allowlist,
            true,
            AuthorShape::RelayReply,
            true,
            false,
        ),
        row(
            "human reply with a workflow tag",
            RespondTo::Allowlist,
            false,
            AuthorShape::HumanWorkflowTagged,
            true,
            false,
        ),
        row(
            "relay-signed threaded workflow reply",
            RespondTo::Allowlist,
            true,
            AuthorShape::RelayWorkflowReply,
            true,
            false,
        ),
        row(
            "verified-owner workflow reply",
            RespondTo::OwnerOnly,
            false,
            AuthorShape::OwnerWorkflowReply,
            true,
            false,
        ),
    ];
    for case in cases {
        let name = case.name;
        let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
        let agent_hex = fx.agent_hex();
        let owner_hex = fx.owner.public_key().to_hex();
        let event = match case.shape {
            AuthorShape::OwnerReply => thread_reply(
                &fx.owner,
                fx.channel_id,
                &fx.root,
                &agent_hex,
                "@bot approve",
            ),
            AuthorShape::AllowedReply => thread_reply(
                &allowed_human,
                fx.channel_id,
                &fx.root,
                &agent_hex,
                "@bot approve",
            ),
            AuthorShape::StrangerReply => thread_reply(
                &stranger,
                fx.channel_id,
                &fx.root,
                &agent_hex,
                "@bot approve",
            ),
            AuthorShape::RelayTopLevel => {
                top_level(&relay_keys, fx.channel_id, &agent_hex, "@bot approve")
            }
            AuthorShape::RelayReply => thread_reply(
                &relay_keys,
                fx.channel_id,
                &fx.root,
                &agent_hex,
                "@bot approve",
            ),
            AuthorShape::HumanWorkflowTagged => {
                let thread = buzz_sdk::ThreadRef {
                    root_event_id: fx.root.id,
                    parent_event_id: fx.root.id,
                };
                buzz_sdk::build_message(
                    fx.channel_id,
                    "@bot approve",
                    Some(&thread),
                    &[agent_hex.as_str()],
                    false,
                    &[],
                    &[],
                )
                .expect("build")
                .tags([nostr::Tag::parse(["buzz:workflow", "true"]).expect("tag")])
                .sign_with_keys(&allowed_human)
                .expect("sign")
            }
            AuthorShape::RelayWorkflowReply => workflow_reply(
                &relay_keys,
                fx.channel_id,
                &fx.root,
                &owner_hex,
                &agent_hex,
                false,
            ),
            AuthorShape::OwnerWorkflowReply => workflow_reply(
                &relay_keys,
                fx.channel_id,
                &fx.root,
                &owner_hex,
                &agent_hex,
                true,
            ),
        };
        let (rest_client, server) =
            author_gate_tests::nip11_server(serde_json::json!({ "self": relay_hex })).await;
        let mut gate = InboundAuthorGate::connect(&rest_client, &agent_hex, "test").await;
        let owner_cache = OwnerCache::new(Some(owner_hex.clone()));
        for key in [
            relay_hex.clone(),
            allowed_human.public_key().to_hex(),
            stranger.public_key().to_hex(),
        ] {
            owner_cache.cache_sibling(key, false);
        }
        let channel_info = pool::ChannelInfoResolver::new(
            HashMap::from([(
                fx.channel_id,
                relay::ChannelInfo {
                    name: "paseo".into(),
                    channel_type: "stream".into(),
                    description: None,
                },
            )]),
            rest_client.clone(),
        );
        let mut allowlist: HashSet<String> = HashSet::from([allowed_human.public_key().to_hex()]);
        if case.allow_relay {
            allowlist.insert(relay_hex.clone());
        }
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        let authorized = authorize_normal_listener_event(
            &mut gate,
            relay::BuzzEvent {
                connection_generation: 0,
                channel_id: fx.channel_id,
                event,
            },
            &case.respond_to,
            &allowlist,
            &owner_cache,
            &channel_info,
            &rest_client,
        )
        .await;
        assert_eq!(authorized.is_some(), case.admitted, "{name}: gate verdict");
        let consumed = match authorized {
            None => false,
            Some(authorized) => {
                let relay_signed = authorized.relay_signed();
                let ingress = AuthorizedNormalListenerEvent(authorized)
                    .match_subscription(&rules, &agent_hex)
                    .await
                    .expect("a mention matches the mention rule");
                let scope = ingress.session_scope(scope::SessionPolicy::Thread, false);
                match ingress
                    .answer_open_question(&scope, relay_signed, &mut fx.pool, &fx.poster)
                    .await
                {
                    Some(ingress) => {
                        ingress.push(&mut queue, scope, false);
                        false
                    }
                    None => true,
                }
            }
        };
        server.abort();
        assert_eq!(consumed, case.consumed, "{name}");
        assert_eq!(
            fx.reply_rx.try_recv().is_ok(),
            case.consumed,
            "{name}: only a consumed answer reaches the agent"
        );
        if !case.consumed {
            assert!(fx.pool.elicitations().is_pending(&fx.scope), "{name}");
        }
    }
}

/// While a question waits, no handling mode sends a stop signal or a native
/// steer for that scope; a scope without a question is unchanged.
#[tokio::test]
async fn elicitation_pending_scope_does_not_interrupt() {
    for handling in [
        MultipleEventHandling::Steer,
        MultipleEventHandling::Interrupt,
        MultipleEventHandling::OwnerInterrupt,
    ] {
        let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
        let owner_hex = fx.owner.public_key().to_hex();
        let agent_hex = fx.agent_hex();
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        // The waiting turn and a turn in another thread are both in flight.
        let running = ingress_for(fx.root.clone(), fx.channel_id, &owner_hex);
        running.push(&mut queue, fx.scope.clone(), false);
        let other_root = top_level(&fx.owner, fx.channel_id, &agent_hex, "@bot other");
        let other_scope = scope::SessionScope::derive(
            scope::SessionPolicy::Thread,
            fx.channel_id,
            false,
            &other_root,
        );
        ingress_for(other_root.clone(), fx.channel_id, &owner_hex).push(
            &mut queue,
            other_scope.clone(),
            false,
        );
        queue.flush_next().expect("question turn");
        queue.flush_next().expect("other turn");
        let (other_control_tx, mut other_control_rx) = tokio::sync::oneshot::channel();
        let (steer_tx, mut steer_rx) = mpsc::channel(1);
        let other_task = fx.pool.join_set.spawn(std::future::pending::<()>());
        fx.pool.task_map_mut().insert(
            other_task.id(),
            task_meta(
                1,
                &other_scope,
                "turn-other",
                Some(other_control_tx),
                Some(steer_tx),
            ),
        );
        let (ack_tx, _ack_rx) = mpsc::unbounded_channel();

        // `steer_or_interrupt` runs for every event the intake did not
        // consume; drive it directly with an event of the waiting scope.
        let chat = thread_reply(
            &fx.owner,
            fx.channel_id,
            &fx.root,
            &agent_hex,
            "@bot status?",
        );
        ingress_for(chat, fx.channel_id, &owner_hex)
            .push(&mut queue, fx.scope.clone(), false)
            .steer_or_interrupt(
                handling,
                Some(&owner_hex),
                &mut fx.pool,
                &mut queue,
                &ack_tx,
            );
        assert!(
            fx.control_rx.try_recv().is_err(),
            "{handling:?}: the waiting turn gets no stop signal"
        );
        assert!(fx.pool.elicitations().is_pending(&fx.scope), "{handling:?}");

        let other_chat = thread_reply(
            &fx.owner,
            fx.channel_id,
            &other_root,
            &agent_hex,
            "@bot more",
        );
        ingress_for(other_chat, fx.channel_id, &owner_hex)
            .push(&mut queue, other_scope.clone(), false)
            .steer_or_interrupt(
                handling,
                Some(&owner_hex),
                &mut fx.pool,
                &mut queue,
                &ack_tx,
            );
        let other_signalled = other_control_rx.try_recv().is_ok() || steer_rx.try_recv().is_ok();
        assert!(other_signalled, "{handling:?}: another scope is unchanged");
    }
}

/// A failed native steer falls back to cancel+merge, except for a turn whose
/// question waits: that turn keeps running.
#[tokio::test]
async fn elicitation_pending_scope_skips_steer_fallback() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let scope = fx.scope.clone();
    assert!(!signal_steer_fallback(&mut fx.pool, &scope));
    assert!(
        fx.control_rx.try_recv().is_err(),
        "the waiting turn is not cancelled"
    );

    fx.pool.elicitations_mut().clear_turn(TURN_ID);
    assert!(signal_steer_fallback(&mut fx.pool, &scope));
    assert!(matches!(fx.control_rx.try_recv(), Ok(ControlSignal::Steer)));
}

/// A busy thread owner with a waiting question is held, never forked, and the
/// hold arms no hold-deadline wake (that would spin).
#[tokio::test]
async fn elicitation_pending_scope_holds_without_fork() {
    let channel_id = Uuid::new_v4();
    let scope = scope::SessionScope::Thread {
        channel_id,
        root_event_id: "ab".repeat(32),
    };
    // Worker 0 runs the waiting turn; worker 1 is idle.
    let mut pool = AgentPool::from_slots(vec![None, None]);
    pool.return_agent(error_outcome_emission_tests::dummy_agent(1).await);
    let task = pool.join_set.spawn(std::future::pending::<()>());
    pool.task_map_mut()
        .insert(task.id(), task_meta(0, &scope, TURN_ID, None, None));
    pool.record_scope_owner(scope.clone(), 0);
    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let turn = ElicitationTurn {
        turn_id: TURN_ID.into(),
        scope: scope.clone(),
        anchor_event_id: "ab".repeat(32),
    };
    pool.elicitations_mut()
        .open(&turn, 0, REQUEST_ID.into(), decision_schema(), reply_tx);

    let start = tokio::time::Instant::now();
    let timeout = pool::HOLD_BUSY_OWNER_TIMEOUT;
    let late = start + timeout + Duration::from_secs(1);
    assert!(matches!(
        pool.hold_decision(&scope, start, timeout),
        pool::HoldDecision::Hold { .. }
    ));
    assert!(matches!(
        pool.hold_decision(&scope, late, timeout),
        pool::HoldDecision::Hold { .. }
    ));
    assert_eq!(
        pool.next_hold_deadline(timeout),
        None,
        "a question hold arms no deadline wake"
    );

    // Without the question the bounded hold forks as before.
    pool.elicitations_mut().clear_turn(TURN_ID);
    assert!(matches!(
        pool.hold_decision(&scope, start, timeout),
        pool::HoldDecision::Hold { .. }
    ));
    assert!(matches!(
        pool.hold_decision(&scope, late, timeout),
        pool::HoldDecision::ForkAfterHold { .. }
    ));
}

#[tokio::test]
async fn elicitation_publish_failure_cancels() {
    let mut fx = QuestionFixture::with_publisher(
        scope::SessionPolicy::Thread,
        Some(relay::RelayEventPublisher::closed_for_test()),
    )
    .await;
    let event = tokio::time::timeout(Duration::from_secs(5), fx.events_rx.recv())
        .await
        .expect("post result within 5 s")
        .expect("event");
    assert!(
        matches!(event, ElicitationEvent::PostFailed { .. }),
        "{event:?}"
    );
    fx.handle(event);
    assert_eq!(
        reply_within(&mut fx.reply_rx).await,
        Some(ElicitationReply::Cancel)
    );
    assert!(!fx.question_is_open());
}

#[tokio::test]
async fn elicitation_expired_and_refused_post_notices() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let _question = fx.next_published().await;
    fx.handle(ElicitationEvent::Closed {
        turn: fx.turn(),
        request_id: REQUEST_ID.into(),
        reason: CloseReason::Expired,
    });
    let expired = fx.next_published().await;
    assert_eq!(
        expired.content,
        acp::elicitation::EXPIRED_NOTICE,
        "expiry notice in the thread"
    );
    assert!(!fx.question_is_open());

    fx.handle(ElicitationEvent::Refused { turn: fx.turn() });
    let refused = fx.next_published().await;
    assert_eq!(refused.content, acp::elicitation::UNSUPPORTED_NOTICE);
    assert_eq!(
        queue::parse_thread_tags(&refused).root_event_id.as_deref(),
        Some(fx.root.id.to_hex().as_str())
    );
}

// ── Every removal path clears the question record ───────────────────────

#[tokio::test]
async fn elicitation_pending_cleared_on_close() {
    for reason in [
        CloseReason::Ended,
        CloseReason::Expired,
        CloseReason::Replied,
    ] {
        let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
        assert!(fx.question_is_open());
        fx.handle(ElicitationEvent::Closed {
            turn: fx.turn(),
            request_id: REQUEST_ID.into(),
            reason,
        });
        assert!(!fx.question_is_open(), "{reason:?}");
    }
}

/// The turn result clears the question. The fatal outcomes matter most: they
/// respawn the worker instead of returning it to the pool, so only the
/// result path can clear the record of the dead turn.
#[tokio::test]
async fn elicitation_pending_cleared_on_prompt_result() {
    let outcomes = [
        (
            "end turn",
            PromptOutcome::Ok(crate::acp::StopReason::EndTurn),
        ),
        ("agent exited", PromptOutcome::AgentExited),
    ];
    for (name, outcome) in outcomes {
        let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
        let agent = error_outcome_emission_tests::dummy_agent(0).await;
        let mut queue = EventQueue::new(config::DedupMode::Queue);
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
            &mut fx.pool,
            &mut queue,
            &config,
            PromptResult {
                agent,
                source: PromptSource::Channel(fx.scope.clone()),
                turn_id: TURN_ID.into(),
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
        respawn_tasks.shutdown().await;
        assert!(!fx.question_is_open(), "{name}");
        assert!(!fx.pool.elicitations().is_pending(&fx.scope), "{name}");
    }
}

#[tokio::test]
async fn elicitation_pending_cleared_on_task_panic() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    // Replace the in-flight task by one that panics, with the same turn.
    fx.pool.task_map_mut().clear();
    let task = fx.pool.join_set.spawn(async { panic!("turn task panics") });
    fx.pool
        .task_map_mut()
        .insert(task.id(), task_meta(0, &fx.scope, TURN_ID, None, None));
    let join_error = loop {
        match fx.pool.join_set.join_next().await {
            Some(Err(error)) if error.is_panic() => break error,
            Some(_) => continue,
            None => panic!("panicking task must report"),
        }
    };
    let mut queue = EventQueue::new(config::DedupMode::Queue);
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
        &mut fx.pool,
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
    assert!(!fx.question_is_open());

    // A later reply in the thread now goes to the queue.
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let agent_hex = fx.agent_hex();
    let reply = thread_reply(&human, fx.channel_id, &fx.root, &agent_hex, "@bot approve");
    assert!(
        !fx.admit(reply, &human.public_key().to_hex(), &mut queue)
            .await
    );
    assert!(queue.has_pending_scope(&fx.scope));
}

#[tokio::test]
async fn elicitation_pending_cleared_on_owner_cancel() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    // `!cancel` from the owner sends this signal to the scope's turn.
    assert!(signal_in_flight_task_for_scope(
        &mut fx.pool,
        &fx.scope.clone(),
        ControlSignal::Cancel
    ));
    assert!(!fx.question_is_open());
    assert_eq!(
        reply_within(&mut fx.reply_rx).await,
        None,
        "dropping the record closes the reply route, which the read loop answers cancel"
    );
}

#[tokio::test]
async fn elicitation_pending_cleared_on_desktop_cancel() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Channel).await;
    assert!(signal_in_flight_task(
        &mut fx.pool,
        fx.channel_id,
        ControlSignal::Cancel
    ));
    assert!(!fx.question_is_open());
}

#[tokio::test]
async fn elicitation_pending_cleared_on_rotate_invalidation() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    // The idle `!rotate` path invalidates the scope session.
    fx.pool.invalidate_scope_session(&fx.scope.clone());
    assert!(!fx.question_is_open());

    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    // Leaving the channel invalidates every session of the channel.
    fx.pool.invalidate_channel_sessions(fx.channel_id);
    assert!(!fx.question_is_open());
}

#[tokio::test]
async fn elicitation_pending_cleared_on_agent_respawn() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    // A respawned worker takes over slot 0 through `return_agent`.
    let respawned = error_outcome_emission_tests::dummy_agent(0).await;
    fx.pool.return_agent(respawned);
    assert!(!fx.question_is_open());
}

/// The dispatcher posts a question where the turn replies: the thread root
/// for a thread scope, else the thread of the newest event of the batch.
#[test]
fn elicitation_anchor_follows_the_turn_thread() {
    let channel_id = Uuid::new_v4();
    let author = nostr::Keys::generate();
    let agent = nostr::Keys::generate().public_key().to_hex();
    let root = top_level(&author, channel_id, &agent, "@bot start");
    let reply = thread_reply(&author, channel_id, &root, &agent, "@bot more");
    let batch = |scope: scope::SessionScope, event: nostr::Event| FlushBatch {
        channel_id,
        scope,
        events: vec![queue::BatchEvent {
            event,
            prompt_tag: "@mention".into(),
            received_at: std::time::Instant::now(),
            edit: None,
        }],
        cancelled_events: Vec::new(),
        cancel_reason: None,
    };
    let conversation = scope::SessionScope::Conversation { channel_id };
    let thread =
        scope::SessionScope::derive(scope::SessionPolicy::Thread, channel_id, false, &reply);
    let root_hex = root.id.to_hex();
    for (name, batch) in [
        ("thread scope", batch(thread, reply.clone())),
        (
            "channel scope, top-level event",
            batch(conversation.clone(), root.clone()),
        ),
        ("channel scope, thread reply", batch(conversation, reply)),
    ] {
        assert_eq!(
            elicitation_anchor(&batch).as_deref(),
            Some(root_hex.as_str()),
            "{name}"
        );
    }
}

/// No question route exists before the main loop sets its event sender; the
/// read loop then refuses every question with `cancel`.
#[test]
fn elicitation_route_needs_the_main_loop_sender() {
    let channel_id = Uuid::new_v4();
    let mut pool = AgentPool::from_slots(vec![None]);
    let turn = ElicitationTurn {
        turn_id: "t".into(),
        scope: scope::SessionScope::Conversation { channel_id },
        anchor_event_id: "ab".repeat(32),
    };
    assert!(pool.elicitations().sink_for(turn.clone()).is_none());
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    pool.elicitations_mut().ensure_events_tx(&events_tx);
    assert!(pool.elicitations().sink_for(turn).is_some());
}

// ── Hardening of the answer intake ──────────────────────────────────────

/// A kind:0 profile list as the relay `/query` bridge returns it.
fn agent_profile(display_name: &str) -> serde_json::Value {
    serde_json::json!([{
        "kind": 0,
        "content": serde_json::json!({ "display_name": display_name }).to_string(),
    }])
}

/// A display name with a space is stripped whole, so its second word is never
/// read as the answer. When the name lookup fails, the reply gets a hint (not
/// a wrong answer) and the next reply retries the lookup and answers.
#[tokio::test]
async fn elicitation_multi_word_agent_name_answers() {
    let human = nostr::Keys::generate();
    let human_hex = human.public_key().to_hex();

    let (rest, server) = author_gate_tests::nip11_scripted_server(
        std::collections::VecDeque::from([Ok(agent_profile("Merge Bot"))]),
    )
    .await;
    let mut fx =
        QuestionFixture::with_options(scope::SessionPolicy::Thread, None, Some(rest)).await;
    let question = fx.next_published().await;
    assert!(
        question.content.contains("  @Merge Bot approve\n"),
        "{}",
        question.content
    );
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let agent_hex = fx.agent_hex();
    let answer = thread_reply(
        &human,
        fx.channel_id,
        &fx.root,
        &agent_hex,
        "@Merge Bot reject",
    );
    assert!(fx.admit(answer, &human_hex, &mut queue).await);
    assert_eq!(reply_within(&mut fx.reply_rx).await, Some(parse_reject()));
    server.abort();

    // The lookup fails for the question and for the first reply.
    let (rest, server) =
        author_gate_tests::nip11_scripted_server(std::collections::VecDeque::from([
            Err(()),
            Err(()),
            Ok(agent_profile("Merge Bot")),
        ]))
        .await;
    let mut fx =
        QuestionFixture::with_options(scope::SessionPolicy::Thread, None, Some(rest)).await;
    let question = fx.next_published().await;
    assert!(
        question.content.contains("@<my name> approve"),
        "{}",
        question.content
    );
    let agent_hex = fx.agent_hex();
    let first = thread_reply(
        &human,
        fx.channel_id,
        &fx.root,
        &agent_hex,
        "@Merge Bot reject",
    );
    assert!(
        fx.admit(first, &human_hex, &mut queue).await,
        "consumed with a hint"
    );
    let hint = fx.next_published().await;
    assert!(
        hint.content.starts_with("I did not understand"),
        "{}",
        hint.content
    );
    assert!(fx.reply_rx.try_recv().is_err(), "no answer from a cut name");
    let second = thread_reply(
        &human,
        fx.channel_id,
        &fx.root,
        &agent_hex,
        "@Merge Bot reject",
    );
    assert!(fx.admit(second, &human_hex, &mut queue).await);
    assert_eq!(reply_within(&mut fx.reply_rx).await, Some(parse_reject()));
    server.abort();
}

/// After the answer the turn keeps its question record until it ends, so a
/// later chat mention in the thread neither stops nor steers the turn.
#[tokio::test]
async fn elicitation_answered_scope_does_not_interrupt() {
    for handling in [
        MultipleEventHandling::Steer,
        MultipleEventHandling::Interrupt,
        MultipleEventHandling::OwnerInterrupt,
    ] {
        let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
        let owner_hex = fx.owner.public_key().to_hex();
        let agent_hex = fx.agent_hex();
        let mut queue = EventQueue::new(config::DedupMode::Queue);
        ingress_for(fx.root.clone(), fx.channel_id, &owner_hex).push(
            &mut queue,
            fx.scope.clone(),
            false,
        );
        queue.flush_next().expect("question turn");
        let answer = thread_reply(
            &fx.owner,
            fx.channel_id,
            &fx.root,
            &agent_hex,
            "@bot approve",
        );
        assert!(fx.admit(answer, &owner_hex, &mut queue).await);
        assert!(!fx.pool.elicitations().is_pending(&fx.scope));
        assert!(fx.pool.elicitations().has_question(&fx.scope));

        let chat = thread_reply(
            &fx.owner,
            fx.channel_id,
            &fx.root,
            &agent_hex,
            "@bot thanks",
        );
        assert!(
            !fx.admit(chat.clone(), &owner_hex, &mut queue).await,
            "{handling:?}: queued"
        );
        let (ack_tx, _ack_rx) = mpsc::unbounded_channel();
        ingress_for(chat, fx.channel_id, &owner_hex)
            .push(&mut queue, fx.scope.clone(), false)
            .steer_or_interrupt(
                handling,
                Some(&owner_hex),
                &mut fx.pool,
                &mut queue,
                &ack_tx,
            );
        let scope = fx.scope.clone();
        assert!(!signal_steer_fallback(&mut fx.pool, &scope), "{handling:?}");
        assert!(
            fx.control_rx.try_recv().is_err(),
            "{handling:?}: the answered turn is not stopped"
        );
    }
}

/// Only a reply that `p`-tags the agent and is not the agent's own post can
/// answer, whatever the subscription mode admits.
#[tokio::test]
async fn elicitation_answer_requires_agent_mention_and_another_author() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let human_hex = human.public_key().to_hex();
    let agent_hex = fx.agent_hex();
    let somebody = nostr::Keys::generate().public_key().to_hex();
    let no_mention = thread_reply(&human, fx.channel_id, &fx.root, &somebody, "@bot approve");
    assert!(
        !fx.admit(no_mention, &human_hex, &mut queue).await,
        "no agent p tag"
    );
    let agent_keys = fx.agent.clone();
    let own_post = thread_reply(
        &agent_keys,
        fx.channel_id,
        &fx.root,
        &agent_hex,
        "@bot approve",
    );
    assert!(
        !fx.admit(own_post, &agent_hex, &mut queue).await,
        "the agent's own post"
    );
    let forged = thread_reply(&human, fx.channel_id, &fx.root, &agent_hex, "@bot approve");
    assert!(
        !fx.admit(forged, &fx.owner.public_key().to_hex(), &mut queue)
            .await,
        "the signer is not the admitted author"
    );
    assert!(fx.pool.elicitations().is_pending(&fx.scope));
    assert!(fx.reply_rx.try_recv().is_err());
}

/// Hints and "already answered" replies are bounded per question; later
/// ones are consumed without a post.
#[tokio::test]
async fn elicitation_thread_replies_are_bounded() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let _question = fx.next_published().await;
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let human_hex = human.public_key().to_hex();
    let agent_hex = fx.agent_hex();
    let limit = usize::from(elicitation_intake::MAX_THREAD_REPLIES_PER_QUESTION);
    for _ in 0..limit + 2 {
        let reply = thread_reply(&human, fx.channel_id, &fx.root, &agent_hex, "@bot what?");
        assert!(fx.admit(reply, &human_hex, &mut queue).await, "consumed");
    }
    for _ in 0..limit {
        let _hint = fx.next_published().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fx.published.try_recv().is_err(), "no post after the limit");
    assert!(!queue.has_pending_scope(&fx.scope));
    assert!(fx.pool.elicitations().is_pending(&fx.scope));
}

/// A second turn of the same scope (a forked session) cannot open a question
/// while the first one holds the thread: it gets `cancel` and one notice.
#[tokio::test]
async fn elicitation_second_question_in_scope_is_refused() {
    let mut fx = QuestionFixture::new(scope::SessionPolicy::Thread).await;
    let _question = fx.next_published().await;
    let fork = fx.pool.join_set.spawn(std::future::pending::<()>());
    fx.pool
        .task_map_mut()
        .insert(fork.id(), task_meta(0, &fx.scope, "turn-fork", None, None));
    let fork_turn = std::sync::Arc::new(ElicitationTurn {
        turn_id: "turn-fork".into(),
        scope: fx.scope.clone(),
        anchor_event_id: fx.root.id.to_hex(),
    });
    for attempt in 0..2 {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        fx.handle(ElicitationEvent::Opened {
            turn: std::sync::Arc::clone(&fork_turn),
            request_id: "\"f1\"".into(),
            message: "Another question?".into(),
            schema: decision_schema(),
            reply_tx,
        });
        assert_eq!(
            reply_within(&mut { reply_rx }).await,
            Some(ElicitationReply::Cancel),
            "attempt {attempt}"
        );
    }
    let notice = fx.next_published().await;
    assert_eq!(notice.content, acp::elicitation::BUSY_NOTICE);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fx.published.try_recv().is_err(), "one notice per turn");

    // The first question still takes the answer.
    let mut queue = EventQueue::new(config::DedupMode::Queue);
    let human = nostr::Keys::generate();
    let agent_hex = fx.agent_hex();
    let answer = thread_reply(&human, fx.channel_id, &fx.root, &agent_hex, "@bot reject");
    assert!(
        fx.admit(answer, &human.public_key().to_hex(), &mut queue)
            .await
    );
    assert_eq!(reply_within(&mut fx.reply_rx).await, Some(parse_reject()));
}
