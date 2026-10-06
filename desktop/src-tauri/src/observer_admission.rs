//! Current channel-observer admission shared by display and local archiving.
use crate::{
    app_state::AppState,
    relay::{query_relay_at_with_keys, relay_http_base_url, relay_ws_url_with_override},
};
use buzz_core_pkg::observer::{
    parse_observer_route, validate_observer_channel_payload, ObserverDirection,
};
use buzz_sdk_pkg::observer_policy::{verified_observer_policy, verified_profile_owner};
use nostr::{Event, Keys};
use serde_json::{json, Value};

#[cfg(test)]
#[path = "observer_admission_tests.rs"]
mod tests;

pub(crate) async fn authorize_shared_observer(
    state: &AppState,
    event: &Event,
    keys: &Keys,
    relay_url: &str,
) -> Result<(), String> {
    let route = parse_observer_route(event)?.ok_or("unknown observer frame")?;
    let Some(channel) = route.channel_id else {
        return Ok(());
    };
    if route.direction != ObserverDirection::Telemetry || route.recipient != keys.public_key() {
        return Err("observer recipient mismatch".into());
    }
    let signer = crate::commands::fetch_relay_self_at(state, relay_url)
        .await?
        .ok_or("relay signer unavailable")?;
    let base = relay_http_base_url(relay_url);
    let roster = query_relay_at_with_keys(
        state,
        &base,
        &[json!({"kinds":[39002],"authors":[signer],"#d":[channel.to_string()],"limit":1})],
        keys,
        None,
    )
    .await?;
    let roster = roster
        .first()
        .ok_or("observer channel roster unavailable")?;
    if roster.verify().is_err()
        || roster.pubkey.to_hex() != signer
        || roster.kind.as_u16() != 39002
        || !roster
            .tags
            .iter()
            .any(|t| t.as_slice() == ["d", channel.to_string().as_str()])
    {
        return Err("invalid observer channel roster".into());
    }
    for member in [route.agent, route.recipient] {
        if !roster.tags.iter().any(|t| {
            t.as_slice().first().is_some_and(|v| v == "p")
                && t.content() == Some(member.to_hex().as_str())
        }) {
            return Err("observer channel membership required".into());
        }
    }
    let profiles = query_relay_at_with_keys(
        state,
        &base,
        &[json!({"kinds":[0],"authors":[route.agent.to_hex(),route.recipient.to_hex()]})],
        keys,
        None,
    )
    .await?;
    let profile = profiles
        .iter()
        .filter(|e| e.pubkey == route.agent)
        .max_by_key(|e| (e.created_at, std::cmp::Reverse(e.id)))
        .ok_or("observer agent profile unavailable")?;
    let owner = verified_profile_owner(profile).ok_or("observer agent owner unavailable")?;
    let policies = query_relay_at_with_keys(state, &base, &[json!({"kinds":[30177],"authors":[owner.to_hex()],"#d":[route.agent.to_hex()],"limit":1})], keys, None).await?;
    let policy = verified_observer_policy(&route.agent, &profiles, &policies)
        .ok_or("observer policy unavailable")?;
    let metadata = query_relay_at_with_keys(
        state,
        &base,
        &[json!({"kinds":[39000],"authors":[signer],"#d":[channel.to_string()],"limit":1})],
        keys,
        None,
    )
    .await?;
    let metadata = metadata
        .first()
        .filter(|e| {
            e.verify().is_ok()
                && e.pubkey.to_hex() == signer
                && e.kind.as_u16() == 39000
                && e.tags
                    .iter()
                    .any(|t| t.as_slice() == ["d", channel.to_string().as_str()])
        })
        .ok_or("observer channel metadata unavailable")?;
    let is_dm = metadata.tags.iter().any(|t| {
        t.as_slice().first().is_some_and(|v| v == "hidden")
            || (t.as_slice().first().is_some_and(|v| v == "t") && t.content() == Some("dm"))
    });
    let recipient_owner = profiles
        .iter()
        .filter(|e| e.pubkey == route.recipient)
        .max_by_key(|e| (e.created_at, std::cmp::Reverse(e.id)))
        .and_then(verified_profile_owner);
    if !policy.allows(&route.recipient, recipient_owner.as_ref(), is_dm) {
        return Err("observer agent permission required".into());
    }
    if state.signing_keys()?.public_key() != keys.public_key()
        || relay_ws_url_with_override(state) != relay_url
    {
        return Err("observer context changed".into());
    }
    Ok(())
}

pub(crate) fn decrypt_bound_observer(keys: &Keys, event: &Event) -> Result<Value, String> {
    event
        .verify()
        .map_err(|e| format!("invalid observer event: {e}"))?;
    let payload = buzz_core_pkg::observer::decrypt_observer_payload(keys, event)
        .map_err(|e| e.to_string())?;
    if let Some(route) = parse_observer_route(event)? {
        if route.recipient != keys.public_key() {
            return Err("observer recipient mismatch".into());
        }
        if let Some(channel) = route.channel_id {
            validate_observer_channel_payload(&payload, &channel.to_string())?;
        }
    }
    Ok(payload)
}
