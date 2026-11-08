//! ACP form elicitation (`elicitation/create`) as a Buzz thread question.
//!
//! The agent asks a question through ACP. Buzz has no form UI, so the harness
//! posts the question as a plain thread reply and reads the human answer from
//! a later reply in the same thread. This module holds the parts that do not
//! touch I/O: the request check, the schema grammar, the reply grammar, the
//! JSON-RPC response shapes, the user-visible texts, and the per-turn route
//! from the read loop to the main loop.
//!
//! Supported schema: a flat object with at most one enum-or-boolean property
//! and at most one string property. Every other shape is refused with
//! `{"action":"cancel"}` so that the agent never waits for an answer that no
//! thread reply can give.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::scope::SessionScope;

/// ACP method name of a form or URL elicitation request.
pub(crate) const ELICITATION_METHOD: &str = "elicitation/create";

/// Most enum values a choice property may have. A longer list does not fit a
/// one-line thread prompt.
const MAX_ENUM_VALUES: usize = 16;

/// Longest enum value in characters. An answer word must be easy to type.
const MAX_ENUM_VALUE_CHARS: usize = 64;

/// Longest agent message in characters that the thread question quotes.
/// Longer messages are cut so that the kind 9 event stays far below the
/// relay content limit.
const MAX_MESSAGE_CHARS: usize = 4_000;

/// Text of the notice that replaces a question Buzz cannot show.
pub const UNSUPPORTED_NOTICE: &str = "I cannot show this question in Buzz.";

/// Text of the notice that follows a question that reached the turn deadline.
pub const EXPIRED_NOTICE: &str = "The question expired; mention me in this thread to ask again.";

/// Label used in examples when the agent display name is not known.
const UNKNOWN_AGENT_LABEL: &str = "<my name>";

/// The kind of value the choice property accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceKind {
    /// A string property with an `enum` list. Values keep the schema casing.
    Enum(Vec<String>),
    /// A boolean property. Replies use `yes`/`no`/`true`/`false`.
    Boolean,
}

/// The enum-or-boolean property of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceField {
    /// Property name in `requestedSchema.properties`.
    pub name: String,
    /// Accepted values.
    pub kind: ChoiceKind,
}

/// The free-text property of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextField {
    /// Property name in `requestedSchema.properties`.
    pub name: String,
    /// Whether `requestedSchema.required` lists this property.
    pub required: bool,
}

/// A `requestedSchema` that the thread reply grammar can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSchema {
    /// The single enum-or-boolean property, if any.
    pub choice: Option<ChoiceField>,
    /// The single string property, if any.
    pub text: Option<TextField>,
}

/// Why a `requestedSchema` is outside the supported grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSchema(pub String);

impl FormSchema {
    /// Check `schema` against the supported grammar.
    pub fn parse(schema: &serde_json::Value) -> Result<Self, UnsupportedSchema> {
        let unsupported = |why: &str| Err(UnsupportedSchema(why.to_string()));
        let Some(object) = schema.as_object() else {
            return unsupported("requestedSchema is not an object");
        };
        if object.get("type").and_then(|v| v.as_str()) != Some("object") {
            return unsupported("requestedSchema type is not \"object\"");
        }
        let Some(properties) = object.get("properties").and_then(|v| v.as_object()) else {
            return unsupported("requestedSchema has no properties object");
        };
        if properties.is_empty() {
            return unsupported("requestedSchema has no properties");
        }
        let required: Vec<&str> = match object.get("required") {
            None => Vec::new(),
            Some(serde_json::Value::Array(names)) => {
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    match name.as_str() {
                        Some(name) => out.push(name),
                        None => return unsupported("required lists a non-string"),
                    }
                }
                out
            }
            Some(_) => return unsupported("required is not an array"),
        };

        let mut choice: Option<ChoiceField> = None;
        let mut text: Option<TextField> = None;
        for (name, property) in properties {
            let Some(property) = property.as_object() else {
                return unsupported("a property is not an object");
            };
            if property.contains_key("oneOf") || property.contains_key("anyOf") {
                return unsupported("titled or union values are not supported");
            }
            let kind = property.get("type").and_then(|v| v.as_str());
            match (kind, property.get("enum")) {
                (Some("string"), Some(values)) => {
                    let values = parse_enum_values(values)?;
                    if choice.is_some() {
                        return unsupported("more than one choice property");
                    }
                    choice = Some(ChoiceField {
                        name: name.clone(),
                        kind: ChoiceKind::Enum(values),
                    });
                }
                (Some("string"), None) => {
                    if text.is_some() {
                        return unsupported("more than one string property");
                    }
                    text = Some(TextField {
                        name: name.clone(),
                        required: required.contains(&name.as_str()),
                    });
                }
                (Some("boolean"), None) => {
                    if choice.is_some() {
                        return unsupported("more than one choice property");
                    }
                    choice = Some(ChoiceField {
                        name: name.clone(),
                        kind: ChoiceKind::Boolean,
                    });
                }
                _ => return unsupported("a property type is not string, enum, or boolean"),
            }
        }
        Ok(Self { choice, text })
    }

    /// The first words a reply may start with to give an answer, in schema
    /// order. Empty for a text-only form, where any text is the answer.
    pub fn answer_words(&self) -> Vec<String> {
        match self.choice.as_ref().map(|c| &c.kind) {
            Some(ChoiceKind::Enum(values)) => values.clone(),
            Some(ChoiceKind::Boolean) => vec!["yes".to_string(), "no".to_string()],
            None => Vec::new(),
        }
    }

    /// Whether `word` is an answer value of the choice property. An enum value
    /// shadows the `decline` and `cancel` keywords.
    fn shadows_keyword(&self, word: &str) -> bool {
        match self.choice.as_ref().map(|c| &c.kind) {
            Some(ChoiceKind::Enum(values)) => values.iter().any(|v| v.eq_ignore_ascii_case(word)),
            _ => false,
        }
    }
}

fn parse_enum_values(values: &serde_json::Value) -> Result<Vec<String>, UnsupportedSchema> {
    let unsupported = |why: &str| Err(UnsupportedSchema(why.to_string()));
    let Some(values) = values.as_array() else {
        return unsupported("enum is not an array");
    };
    if values.is_empty() || values.len() > MAX_ENUM_VALUES {
        return unsupported("enum has no values or too many values");
    }
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = value.as_str() else {
            return unsupported("enum lists a non-string value");
        };
        if value.is_empty()
            || value.chars().count() > MAX_ENUM_VALUE_CHARS
            || value.chars().any(char::is_whitespace)
        {
            return unsupported("an enum value is empty, too long, or has spaces");
        }
        if out.iter().any(|v| v.eq_ignore_ascii_case(value)) {
            return unsupported("enum values are not unique");
        }
        out.push(value.to_string());
    }
    Ok(out)
}

/// The answer that goes back to the agent as the `elicitation/create` result.
#[derive(Debug, Clone, PartialEq)]
pub enum ElicitationReply {
    /// `{"action":"accept","content":{...}}`.
    Accept(serde_json::Map<String, serde_json::Value>),
    /// `{"action":"decline"}`.
    Decline,
    /// `{"action":"cancel"}`.
    Cancel,
}

/// Parse a thread reply against `schema`.
///
/// The leading `@<agent_label>` mention and any other leading `@name` or
/// `nostr:` tokens are removed first. Then only the **first** remaining word
/// decides the action: a choice value gives `accept`, `decline` gives
/// `decline`, `cancel` gives `cancel`. The parser never searches later words,
/// so "don't approve" can never become an approval. Text after a choice value
/// fills the string property. Returns `None` when the reply is not an answer.
pub fn parse_reply(
    text: &str,
    schema: &FormSchema,
    agent_label: Option<&str>,
) -> Option<ElicitationReply> {
    let rest = strip_leading_mentions(text, agent_label);
    let (first, remainder) = match rest.split_once(char::is_whitespace) {
        Some((first, remainder)) => (first, remainder.trim()),
        None => (rest, ""),
    };
    let word = first.trim_end_matches(['.', ',', '!', ':', ';']);
    if word.is_empty() {
        return None;
    }

    if let Some(choice) = &schema.choice {
        let value = match &choice.kind {
            ChoiceKind::Enum(values) => values
                .iter()
                .find(|v| v.eq_ignore_ascii_case(word))
                .map(|v| serde_json::Value::String(v.clone())),
            ChoiceKind::Boolean => match word.to_ascii_lowercase().as_str() {
                "yes" | "true" => Some(serde_json::Value::Bool(true)),
                "no" | "false" => Some(serde_json::Value::Bool(false)),
                _ => None,
            },
        };
        if let Some(value) = value {
            let mut content = serde_json::Map::new();
            content.insert(choice.name.clone(), value);
            if let Some(field) = &schema.text {
                if !remainder.is_empty() {
                    content.insert(
                        field.name.clone(),
                        serde_json::Value::String(remainder.to_string()),
                    );
                } else if field.required {
                    return None;
                }
            }
            return Some(ElicitationReply::Accept(content));
        }
    }

    if !schema.shadows_keyword(word) {
        if word.eq_ignore_ascii_case("decline") {
            return Some(ElicitationReply::Decline);
        }
        if word.eq_ignore_ascii_case("cancel") {
            return Some(ElicitationReply::Cancel);
        }
    }

    match (&schema.choice, &schema.text) {
        (None, Some(field)) => {
            let mut content = serde_json::Map::new();
            content.insert(
                field.name.clone(),
                serde_json::Value::String(rest.to_string()),
            );
            Some(ElicitationReply::Accept(content))
        }
        _ => None,
    }
}

/// Remove the leading mention run: `@<agent_label>` (case-insensitive, also
/// for a label with spaces), then single `@name` and `nostr:` tokens.
fn strip_leading_mentions<'a>(text: &'a str, agent_label: Option<&str>) -> &'a str {
    let separators = |c: char| c.is_whitespace() || c == ',' || c == ':';
    let mut rest = text.trim_start_matches(separators);
    if let Some(label) = agent_label.filter(|l| !l.trim().is_empty()) {
        let mention = format!("@{}", label.trim());
        if let Some(head) = rest.get(..mention.len()) {
            let tail = &rest[mention.len()..];
            let at_boundary = tail.chars().next().is_none_or(|c| !c.is_alphanumeric());
            if head.to_lowercase() == mention.to_lowercase() && at_boundary {
                rest = tail.trim_start_matches(separators);
            }
        }
    }
    loop {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        if token.starts_with('@') || token.starts_with("nostr:") {
            rest = rest[token_end..].trim_start_matches(separators);
        } else {
            return rest;
        }
    }
}

/// The result of checking an `elicitation/create` request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RequestCheck {
    /// Answer with JSON-RPC error `-32602`: a mode the client did not declare.
    InvalidParams(String),
    /// Answer `{"action":"cancel"}` at once and post one notice in the thread.
    Refuse(String),
    /// A form question Buzz can show.
    Open(OpenRequest),
}

/// A form question that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenRequest {
    /// `toolCallId`, kept for logs.
    pub tool_call_id: Option<String>,
    /// The agent message, cut to [`MAX_MESSAGE_CHARS`].
    pub message: String,
    /// The supported schema.
    pub schema: FormSchema,
}

/// Check the `params` of an `elicitation/create` request sent during the turn
/// of `turn_session_id`.
///
/// Buzz declares only the form mode, so any other mode is invalid params. A
/// form for another session would post the question in the wrong thread, so
/// it is refused like an unsupported schema.
pub(crate) fn check_request(params: &serde_json::Value, turn_session_id: &str) -> RequestCheck {
    let mode = params.get("mode").and_then(|v| v.as_str());
    if mode != Some("form") {
        return RequestCheck::InvalidParams(format!(
            "unsupported elicitation mode {}: the client declares only \"form\"",
            mode.map_or_else(|| "<absent>".to_string(), |m| format!("{m:?}"))
        ));
    }
    if params.get("sessionId").and_then(|v| v.as_str()) != Some(turn_session_id) {
        return RequestCheck::Refuse("sessionId is not the session of the running turn".into());
    }
    let message = match params.get("message").and_then(|v| v.as_str()) {
        Some(message) if !message.trim().is_empty() => message.trim(),
        _ => return RequestCheck::Refuse("message is absent or empty".into()),
    };
    let schema = match params.get("requestedSchema") {
        Some(schema) => match FormSchema::parse(schema) {
            Ok(schema) => schema,
            Err(UnsupportedSchema(why)) => return RequestCheck::Refuse(why),
        },
        None => return RequestCheck::Refuse("requestedSchema is absent".into()),
    };
    RequestCheck::Open(OpenRequest {
        tool_call_id: params
            .get("toolCallId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        message: cut_chars(message, MAX_MESSAGE_CHARS),
        schema,
    })
}

fn cut_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// JSON-RPC response that carries `reply` for request `id`.
pub(crate) fn reply_response(
    id: &serde_json::Value,
    reply: &ElicitationReply,
) -> serde_json::Value {
    let result = match reply {
        ElicitationReply::Accept(content) => {
            serde_json::json!({ "action": "accept", "content": content })
        }
        ElicitationReply::Decline => serde_json::json!({ "action": "decline" }),
        ElicitationReply::Cancel => serde_json::json!({ "action": "cancel" }),
    };
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// JSON-RPC `-32602` error response for request `id`.
pub(crate) fn invalid_params_response(id: &serde_json::Value, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32602, "message": message },
    })
}

fn mention_label(agent_label: Option<&str>) -> String {
    format!(
        "@{}",
        agent_label
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .unwrap_or(UNKNOWN_AGENT_LABEL)
    )
}

/// The lines that tell a human how to answer, shared by the question and the
/// hint. They list the schema's own answer words.
fn answer_instructions(schema: &FormSchema, agent_label: Option<&str>) -> String {
    let me = mention_label(agent_label);
    let words = schema.answer_words();
    let mut out = String::new();
    if words.is_empty() {
        out.push_str("Answer in this thread. Mention me, then write your answer. Example:\n");
        out.push_str(&format!("  {me} <your answer>\n"));
    } else {
        let comment = match &schema.text {
            Some(field) if field.required => " (a comment after it is required)",
            Some(_) => " (optional comment after it)",
            None => "",
        };
        out.push_str(&format!(
            "Answer in this thread. Mention me, then one of: {}{comment}. Examples:\n",
            words.join(" | ")
        ));
        for (index, word) in words.iter().take(2).enumerate() {
            if index == 1 && schema.text.is_some() {
                out.push_str(&format!("  {me} {word} <comment>\n"));
            } else {
                out.push_str(&format!("  {me} {word}\n"));
            }
        }
    }
    let keywords: Vec<&str> = ["decline", "cancel"]
        .into_iter()
        .filter(|k| !schema.shadows_keyword(k))
        .collect();
    match keywords.as_slice() {
        ["decline", "cancel"] => out.push_str(&format!(
            "\"{me} decline\" refuses; \"{me} cancel\" dismisses the question."
        )),
        ["cancel"] => out.push_str(&format!("\"{me} cancel\" dismisses the question.")),
        ["decline"] => out.push_str(&format!("\"{me} decline\" refuses the question.")),
        _ => {}
    }
    out
}

/// Plain-text body of the thread question.
pub fn question_text(message: &str, schema: &FormSchema, agent_label: Option<&str>) -> String {
    format!(
        "Input needed: {message}\n\n{}",
        answer_instructions(schema, agent_label)
    )
}

/// Plain-text body of the reply to an unparseable answer.
pub fn hint_text(schema: &FormSchema, agent_label: Option<&str>) -> String {
    format!(
        "I did not understand that answer, so I did not send it.\n{}",
        answer_instructions(schema, agent_label)
    )
}

/// Plain-text body of the reply to a second answer after the first one won.
pub fn already_answered_text(answered_by: &str) -> String {
    format!("Already answered by {answered_by}; this reply was not sent.")
}

/// The turn that owns a question, set by the dispatcher for each prompt task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElicitationTurn {
    /// The turn id of the prompt task (`TaskMeta::turn_id`).
    pub turn_id: String,
    /// Session scope of the turn.
    pub scope: SessionScope,
    /// Lowercase hex id of the thread root the question replies to: the
    /// thread root for a thread scope, otherwise the thread of the event that
    /// started the turn.
    pub anchor_event_id: String,
}

/// Why a question left the read loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The main loop delivered a reply (a human answer, or a cancel after the
    /// question could not be posted).
    Replied,
    /// The turn hard deadline passed before a reply came.
    Expired,
    /// The turn ended, or the main loop dropped its record, before a reply.
    Ended,
}

/// Events from the read loop and from the harness posting tasks to the main
/// loop. The main loop is the only owner of the scope-to-question record.
#[derive(Debug)]
pub enum ElicitationEvent {
    /// A question opened. The main loop posts it and keeps `reply_tx` until a
    /// human answers. Dropping `reply_tx` cancels the question.
    Opened {
        /// Turn that asked.
        turn: Arc<ElicitationTurn>,
        /// JSON-RPC id of the request, serialized.
        request_id: String,
        /// Agent message.
        message: String,
        /// Supported schema.
        schema: FormSchema,
        /// Route of the reply back into the read loop.
        reply_tx: oneshot::Sender<ElicitationReply>,
    },
    /// A question left the read loop.
    Closed {
        /// Turn that asked.
        turn: Arc<ElicitationTurn>,
        /// JSON-RPC id of the request, serialized.
        request_id: String,
        /// Why it closed.
        reason: CloseReason,
    },
    /// A request was answered `cancel` at once; the thread gets one notice.
    Refused {
        /// Turn that asked.
        turn: Arc<ElicitationTurn>,
    },
    /// The harness posted the question. Sent by the posting task.
    Posted {
        /// Turn that asked.
        turn_id: String,
        /// JSON-RPC id of the request, serialized.
        request_id: String,
        /// The agent display name the question used, when it was found.
        agent_label: Option<String>,
    },
    /// The harness could not post the question. Sent by the posting task.
    PostFailed {
        /// Turn that asked.
        turn_id: String,
        /// JSON-RPC id of the request, serialized.
        request_id: String,
    },
}

/// Per-turn route from the read loop to the main loop.
#[derive(Debug, Clone)]
pub struct ElicitationSink {
    tx: mpsc::UnboundedSender<ElicitationEvent>,
    turn: Arc<ElicitationTurn>,
}

impl ElicitationSink {
    /// Bind `tx` to the identity of one turn.
    pub fn new(tx: mpsc::UnboundedSender<ElicitationEvent>, turn: ElicitationTurn) -> Self {
        Self {
            tx,
            turn: Arc::new(turn),
        }
    }

    /// Send `Opened`. Returns `false` when the main loop is gone.
    pub(crate) fn opened(
        &self,
        request_id: &str,
        message: String,
        schema: FormSchema,
        reply_tx: oneshot::Sender<ElicitationReply>,
    ) -> bool {
        self.tx
            .send(ElicitationEvent::Opened {
                turn: Arc::clone(&self.turn),
                request_id: request_id.to_string(),
                message,
                schema,
                reply_tx,
            })
            .is_ok()
    }

    /// Send `Closed`. A gone main loop has no record left to clear.
    pub(crate) fn closed(&self, request_id: &str, reason: CloseReason) {
        let _ = self.tx.send(ElicitationEvent::Closed {
            turn: Arc::clone(&self.turn),
            request_id: request_id.to_string(),
            reason,
        });
    }

    /// Send `Refused`. A gone main loop cannot post the notice anyway.
    pub(crate) fn refused(&self) {
        let _ = self.tx.send(ElicitationEvent::Refused {
            turn: Arc::clone(&self.turn),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "decision": {"type": "string", "enum": ["approve", "reject"]},
                "comment": {"type": "string"}
            },
            "required": ["decision"]
        })
    }

    fn parsed(schema: serde_json::Value) -> FormSchema {
        FormSchema::parse(&schema).expect("supported schema")
    }

    fn accept(pairs: &[(&str, serde_json::Value)]) -> Option<ElicitationReply> {
        let mut content = serde_json::Map::new();
        for (key, value) in pairs {
            content.insert((*key).to_string(), value.clone());
        }
        Some(ElicitationReply::Accept(content))
    }

    #[test]
    fn elicitation_schema_support_table() {
        let cases: Vec<(&str, serde_json::Value, bool)> = vec![
            ("decision enum with comment", decision_schema(), true),
            (
                "boolean only",
                json!({"type":"object","properties":{"ok":{"type":"boolean"}}}),
                true,
            ),
            (
                "text only",
                json!({"type":"object","properties":{"why":{"type":"string"}}}),
                true,
            ),
            (
                "two enums",
                json!({"type":"object","properties":{
                    "a":{"type":"string","enum":["x"]},
                    "b":{"type":"string","enum":["y"]}}}),
                false,
            ),
            (
                "enum and boolean",
                json!({"type":"object","properties":{
                    "a":{"type":"string","enum":["x"]},
                    "b":{"type":"boolean"}}}),
                false,
            ),
            (
                "two strings",
                json!({"type":"object","properties":{
                    "a":{"type":"string"},"b":{"type":"string"}}}),
                false,
            ),
            (
                "number field",
                json!({"type":"object","properties":{"n":{"type":"number"}}}),
                false,
            ),
            (
                "nested object",
                json!({"type":"object","properties":{"o":{"type":"object","properties":{}}}}),
                false,
            ),
            (
                "titled enum",
                json!({"type":"object","properties":{"a":{"type":"string","oneOf":[{"const":"x","title":"X"}]}}}),
                false,
            ),
            (
                "enum value with a space",
                json!({"type":"object","properties":{"a":{"type":"string","enum":["looks good"]}}}),
                false,
            ),
            (
                "duplicate enum values",
                json!({"type":"object","properties":{"a":{"type":"string","enum":["Yes","yes"]}}}),
                false,
            ),
            ("not an object type", json!({"type":"string"}), false),
            (
                "no properties",
                json!({"type":"object","properties":{}}),
                false,
            ),
            ("not an object", json!("form"), false),
        ];
        for (name, schema, supported) in cases {
            assert_eq!(
                FormSchema::parse(&schema).is_ok(),
                supported,
                "schema case {name:?}"
            );
        }
    }

    #[test]
    fn elicitation_reply_grammar_table() {
        let decision = parsed(decision_schema());
        let boolean = parsed(json!({"type":"object","properties":{"ok":{"type":"boolean"}}}));
        let cancel_enum = parsed(json!({"type":"object","properties":{
            "choice":{"type":"string","enum":["keep","cancel"]}}}));
        let required_comment = parsed(json!({"type":"object","properties":{
            "decision":{"type":"string","enum":["approve","reject"]},
            "comment":{"type":"string"}},"required":["decision","comment"]}));
        let text_only = parsed(json!({"type":"object","properties":{"why":{"type":"string"}}}));
        let label = Some("Merge Bot");

        let cases: Vec<(&str, &FormSchema, &str, Option<ElicitationReply>)> = vec![
            (
                "approve",
                &decision,
                "@Merge Bot approve",
                accept(&[("decision", json!("approve"))]),
            ),
            (
                "upper case value keeps schema casing",
                &decision,
                "@Merge Bot APPROVE",
                accept(&[("decision", json!("approve"))]),
            ),
            (
                "reject with comment",
                &decision,
                "@merge bot reject keep upstream foo.ts",
                accept(&[
                    ("decision", json!("reject")),
                    ("comment", json!("keep upstream foo.ts")),
                ]),
            ),
            (
                "punctuation after the mention and the word",
                &decision,
                "@Merge Bot, approve. ok",
                accept(&[("decision", json!("approve")), ("comment", json!("ok"))]),
            ),
            (
                "other mentions before the word",
                &decision,
                "@Merge Bot @alice approve",
                accept(&[("decision", json!("approve"))]),
            ),
            (
                "unknown label strips one token",
                &decision,
                "@bot approve",
                accept(&[("decision", json!("approve"))]),
            ),
            (
                "decline",
                &decision,
                "@Merge Bot decline",
                Some(ElicitationReply::Decline),
            ),
            (
                "cancel",
                &decision,
                "@Merge Bot Cancel please",
                Some(ElicitationReply::Cancel),
            ),
            ("empty", &decision, "@Merge Bot", None),
            ("unknown word", &decision, "@Merge Bot looks fine", None),
            (
                "a later answer word never counts",
                &decision,
                "@Merge Bot don't approve",
                None,
            ),
            (
                "boolean yes",
                &boolean,
                "@Merge Bot yes",
                accept(&[("ok", json!(true))]),
            ),
            (
                "boolean false",
                &boolean,
                "@Merge Bot false",
                accept(&[("ok", json!(false))]),
            ),
            ("boolean unknown", &boolean, "@Merge Bot maybe", None),
            (
                "enum value cancel wins over the keyword",
                &cancel_enum,
                "@Merge Bot cancel",
                accept(&[("choice", json!("cancel"))]),
            ),
            (
                "required comment missing",
                &required_comment,
                "@Merge Bot approve",
                None,
            ),
            (
                "required comment given",
                &required_comment,
                "@Merge Bot approve fine",
                accept(&[("decision", json!("approve")), ("comment", json!("fine"))]),
            ),
            (
                "text only takes the whole reply",
                &text_only,
                "@Merge Bot use the upstream file",
                accept(&[("why", json!("use the upstream file"))]),
            ),
            (
                "text only still knows decline",
                &text_only,
                "@Merge Bot decline",
                Some(ElicitationReply::Decline),
            ),
        ];
        for (name, schema, text, expected) in cases {
            assert_eq!(
                parse_reply(text, schema, label),
                expected,
                "reply case {name:?}"
            );
        }
    }

    #[test]
    fn elicitation_request_check_table() {
        let form = |session: &str| {
            json!({
                "sessionId": session,
                "toolCallId": "tc-1",
                "mode": "form",
                "message": "Approve the merge?",
                "requestedSchema": decision_schema(),
            })
        };
        assert!(matches!(
            check_request(&form("s-1"), "s-1"),
            RequestCheck::Open(OpenRequest { ref tool_call_id, .. }) if tool_call_id.as_deref() == Some("tc-1")
        ));
        let mut url = form("s-1");
        url["mode"] = json!("url");
        assert!(matches!(
            check_request(&url, "s-1"),
            RequestCheck::InvalidParams(_)
        ));
        let mut no_mode = form("s-1");
        if let Some(object) = no_mode.as_object_mut() {
            object.remove("mode");
        }
        assert!(matches!(
            check_request(&no_mode, "s-1"),
            RequestCheck::InvalidParams(_)
        ));
        assert!(matches!(
            check_request(&form("s-other"), "s-1"),
            RequestCheck::Refuse(_)
        ));
        let mut bad_schema = form("s-1");
        bad_schema["requestedSchema"] =
            json!({"type":"object","properties":{"n":{"type":"number"}}});
        assert!(matches!(
            check_request(&bad_schema, "s-1"),
            RequestCheck::Refuse(_)
        ));
        let mut long = form("s-1");
        long["message"] = json!("x".repeat(MAX_MESSAGE_CHARS + 10));
        match check_request(&long, "s-1") {
            RequestCheck::Open(open) => {
                assert_eq!(open.message.chars().count(), MAX_MESSAGE_CHARS + 1);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn elicitation_response_shapes() {
        let id = json!(7);
        let mut content = serde_json::Map::new();
        content.insert("decision".into(), json!("approve"));
        assert_eq!(
            reply_response(&id, &ElicitationReply::Accept(content)),
            json!({"jsonrpc":"2.0","id":7,"result":{"action":"accept","content":{"decision":"approve"}}})
        );
        assert_eq!(
            reply_response(&json!("r"), &ElicitationReply::Decline),
            json!({"jsonrpc":"2.0","id":"r","result":{"action":"decline"}})
        );
        assert_eq!(
            reply_response(&id, &ElicitationReply::Cancel),
            json!({"jsonrpc":"2.0","id":7,"result":{"action":"cancel"}})
        );
        assert_eq!(
            invalid_params_response(&id, "no")["error"]["code"],
            json!(-32602)
        );
    }

    #[test]
    fn elicitation_texts_list_the_schema_words() {
        let decision = parsed(decision_schema());
        let question = question_text("Merge foo?", &decision, Some("Merge Bot"));
        assert!(question.starts_with("Input needed: Merge foo?\n\n"));
        assert!(question.contains("one of: approve | reject (optional comment after it)"));
        assert!(question.contains("  @Merge Bot approve\n"));
        assert!(question.contains("  @Merge Bot reject <comment>\n"));
        assert!(question.contains("\"@Merge Bot cancel\" dismisses the question."));
        assert!(!question.contains("Only "));

        let cancel_enum = parsed(json!({"type":"object","properties":{
            "choice":{"type":"string","enum":["keep","cancel"]}}}));
        let hint = hint_text(&cancel_enum, None);
        assert!(hint.contains("one of: keep | cancel."));
        assert!(hint.contains("\"@<my name> decline\" refuses the question."));
        assert!(!hint.contains("dismisses"));

        assert_eq!(
            already_answered_text("ab12cd34"),
            "Already answered by ab12cd34; this reply was not sent."
        );
    }
}
