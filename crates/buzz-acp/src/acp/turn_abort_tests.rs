use super::*;
use serde_json::json;

fn result_with(abort: Value) -> Value {
    json!({"stopReason": "end_turn", "_meta": {"buzz.turnAbort": abort}})
}

#[test]
fn parses_all_fields() {
    let abort = parse(&result_with(json!({
        "reason": "context_over_budget",
        "message": "  Too long.  ",
        "resetSession": false,
        "usedTokens": 208000,
        "budgetTokens": 152000,
        "futureField": 1
    })))
    .expect("abort");
    assert_eq!(abort.reason, "context_over_budget");
    assert_eq!(abort.message.as_deref(), Some("Too long."));
    assert_eq!(abort.reset_session, Some(false));
    assert_eq!(abort.used_tokens, Some(208000));
    assert_eq!(abort.budget_tokens, Some(152000));
}

#[test]
fn no_key_is_no_abort() {
    assert!(parse(&json!({"stopReason": "end_turn"})).is_none());
    assert!(parse(&json!({"stopReason": "end_turn", "_meta": {"other": 1}})).is_none());
}

#[test]
fn malformed_meta_is_ignored() {
    for bad in [
        json!("text"),
        json!(7),
        json!([]),
        json!({}),
        json!({"reason": ""}),
        json!({"reason": 5}),
    ] {
        assert!(parse(&result_with(bad.clone())).is_none(), "{bad}");
    }
}

#[test]
fn empty_or_blank_message_is_none() {
    for message in [json!(""), json!("   \n"), json!(null), json!(3)] {
        let abort = parse(&result_with(json!({"reason": "r", "message": message}))).expect("abort");
        assert_eq!(abort.message, None);
    }
    let abort = parse(&result_with(json!({"reason": "r"}))).expect("abort");
    assert_eq!(abort.message, None);
}

#[test]
fn message_is_capped_in_characters() {
    let long = "é".repeat(MAX_MESSAGE_CHARS + 50);
    let abort = parse(&result_with(json!({"reason": "r", "message": long}))).expect("abort");
    assert_eq!(
        abort.message.expect("message").chars().count(),
        MAX_MESSAGE_CHARS
    );
}

#[test]
fn reset_defaults_follow_the_reason() {
    let reset = |reason: &str, flag: Option<bool>| {
        let mut object = json!({"reason": reason});
        if let Some(flag) = flag {
            object["resetSession"] = json!(flag);
        }
        parse(&result_with(object))
            .expect("abort")
            .effective_reset_session()
    };
    assert!(reset("context_over_budget", None));
    assert!(!reset("context_over_budget", Some(false)));
    assert!(!reset("output_truncated", None));
    assert!(reset("output_truncated", Some(true)));
    assert!(!reset("something_new", None));
}

#[test]
fn capability_needs_exact_version_one() {
    let cases = [
        (
            json!({"agentCapabilities": {"_meta": {"buzz.turnAbort": {"version": 1}}}}),
            true,
        ),
        (json!({"_meta": {"buzz.turnAbort": {"version": 1}}}), true),
        (
            json!({"agentCapabilities": {"_meta": {"buzz.turnAbort": {"version": 2}}}}),
            false,
        ),
        (
            json!({"agentCapabilities": {"_meta": {"buzz.turnAbort": true}}}),
            false,
        ),
        (json!({"agentCapabilities": {}}), false),
    ];
    for (init, expected) in cases {
        assert_eq!(advertised_v1(&init), expected, "{init}");
    }
}
