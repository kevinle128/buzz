//! The `buzz.turnAbort` ACP extension: an agent ends a turn on purpose and
//! says why.
//!
//! The agent puts the key in `_meta` of the `session/prompt` result:
//!
//! ```json
//! {"stopReason":"end_turn","_meta":{"buzz.turnAbort":{
//!   "reason":"context_over_budget",
//!   "message":"This conversation is too long to continue.",
//!   "resetSession":true,
//!   "usedTokens":208000,
//!   "budgetTokens":152000}}}
//! ```
//!
//! It also advertises `"buzz.turnAbort":{"version":1}` in the `_meta` of its
//! `initialize` capabilities. Buzz records and logs that; reading the result
//! key does not depend on it.
//!
//! This module holds only pure logic.

use serde_json::Value;

/// Key in `_meta` of the `initialize` capabilities and the prompt result.
pub(crate) const TURN_ABORT_KEY: &str = "buzz.turnAbort";
/// Largest notice text, in characters.
pub(crate) const MAX_MESSAGE_CHARS: usize = 500;
/// Reason whose session cannot continue; resets by default.
pub(crate) const REASON_CONTEXT_OVER_BUDGET: &str = "context_over_budget";

/// An agent's own end of a turn, read from a `session/prompt` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnAbort {
    /// Open set of reasons, for example `context_over_budget`.
    pub(crate) reason: String,
    /// Trimmed notice text, capped at [`MAX_MESSAGE_CHARS`]. `None` when the
    /// agent sent none or only blanks.
    pub(crate) message: Option<String>,
    /// What the agent asked for; `None` means the default of the reason.
    pub(crate) reset_session: Option<bool>,
    /// Tokens the session held, for display.
    pub(crate) used_tokens: Option<u64>,
    /// Token budget of the session, for display.
    pub(crate) budget_tokens: Option<u64>,
}

impl TurnAbort {
    /// Whether the session for the turn's scope is dropped. The agent's value
    /// wins; otherwise only `context_over_budget` resets.
    pub(crate) fn effective_reset_session(&self) -> bool {
        self.reset_session
            .unwrap_or(self.reason == REASON_CONTEXT_OVER_BUDGET)
    }
}

/// Whether an `initialize` result advertises `buzz.turnAbort` version 1.
/// Same places and same strict value as `buzz.wake`.
pub(crate) fn advertised_v1(init_result: &Value) -> bool {
    let expected = serde_json::json!({ "version": 1 });
    ["/agentCapabilities/_meta", "/_meta"].iter().any(|path| {
        init_result
            .pointer(path)
            .and_then(|meta| meta.get(TURN_ABORT_KEY))
            == Some(&expected)
    })
}

/// Read `_meta["buzz.turnAbort"]` from a `session/prompt` result.
///
/// No key gives `None`. A malformed value (not an object, or no usable
/// `reason`) is logged at warn level and gives `None`; the turn then ends as
/// it does today. Keys that are not used are ignored.
pub(crate) fn parse(result: &Value) -> Option<TurnAbort> {
    let raw = result.pointer("/_meta")?.get(TURN_ABORT_KEY)?;
    let Some(object) = raw.as_object() else {
        tracing::warn!(target: "acp::turn_abort", "ignoring malformed {TURN_ABORT_KEY}: not an object");
        return None;
    };
    let Some(reason) = object
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    else {
        tracing::warn!(target: "acp::turn_abort", "ignoring malformed {TURN_ABORT_KEY}: missing reason");
        return None;
    };
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .map(|text| {
            text.trim()
                .chars()
                .take(MAX_MESSAGE_CHARS)
                .collect::<String>()
        })
        .map(|text| text.trim_end().to_owned())
        .filter(|text| !text.is_empty());
    Some(TurnAbort {
        reason: reason.to_owned(),
        message,
        reset_session: object.get("resetSession").and_then(Value::as_bool),
        used_tokens: object.get("usedTokens").and_then(Value::as_u64),
        budget_tokens: object.get("budgetTokens").and_then(Value::as_u64),
    })
}

#[cfg(test)]
#[path = "turn_abort_tests.rs"]
mod tests;
