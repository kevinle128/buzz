use buzz_core::observer::{
    encrypt_observer_payload, parse_observer_route, validate_observer_channel_payload,
};
use nostr::{EventBuilder, Keys, Kind, Tag};

#[test]
fn signed_channel_observer_envelope_binds_recipient_and_channel() {
    let agent = Keys::generate();
    let viewer = Keys::generate();
    let channel = uuid::Uuid::new_v4();
    let content = encrypt_observer_payload(
        &agent,
        &viewer.public_key(),
        &serde_json::json!({"kind":"turn_started","channelId":channel.to_string()}),
    )
    .unwrap();
    let event = EventBuilder::new(Kind::Custom(24200), content)
        .tags([
            Tag::public_key(viewer.public_key()),
            Tag::parse(["agent", &agent.public_key().to_hex()]).unwrap(),
            Tag::parse(["frame", "telemetry"]).unwrap(),
            Tag::parse(["h", &channel.to_string()]).unwrap(),
        ])
        .sign_with_keys(&agent)
        .unwrap();
    let route = parse_observer_route(&event).unwrap().unwrap();
    assert_eq!(route.recipient, viewer.public_key());
    assert_eq!(route.channel_id, Some(channel));
}

#[test]
fn shared_channel_payload_rejects_cross_channel_and_missing_batch_items() {
    let channel = uuid::Uuid::new_v4().to_string();
    let item = serde_json::json!({"kind":"turn_started","channelId":channel});
    assert!(validate_observer_channel_payload(&item, &channel).is_ok());
    let valid =
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":[item.clone()]}});
    assert!(validate_observer_channel_payload(&valid, &channel).is_ok());
    for invalid in [
        serde_json::json!({"kind":"turn_started"}),
        serde_json::json!({"channelId":channel}),
        serde_json::json!({"kind":42,"channelId":channel}),
        serde_json::json!({"kind":"","channelId":channel}),
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":[{"kind":"","channelId":channel}]}}),
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":[{"channelId":channel}]}}),
        serde_json::json!({"kind":"turn_started","channelId":"other"}),
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":[]}}),
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":[item.clone(),{"kind":"acp_read","channelId":null}]}}),
        serde_json::json!({"kind":"batch","channelId":channel,"payload":{"events":"invalid"}}),
    ] {
        assert!(validate_observer_channel_payload(&invalid, &channel).is_err());
    }
}
