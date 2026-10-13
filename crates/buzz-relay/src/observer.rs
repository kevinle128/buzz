//! Current authorization for channel-scoped observer deliveries.
use crate::state::AppState;
use buzz_core::{observer::ObserverRoute, CommunityId};
use buzz_db::{event::EventQuery, DbError};
use buzz_sdk::observer_policy::{verified_observer_policy, verified_profile_owner};
use nostr::{Event, PublicKey};

async fn latest_profile(
    state: &AppState,
    community: CommunityId,
    key: &PublicKey,
) -> Result<Option<Event>, DbError> {
    Ok(state
        .db
        .get_latest_global_replaceable(community, 0, key.as_bytes())
        .await?
        .map(|stored| stored.event))
}

/// Read authoritative writer state for each admission; caches are not a revocation fence.
pub(crate) async fn channel_observer_allowed(
    state: &AppState,
    community: CommunityId,
    route: &ObserverRoute,
) -> Result<bool, DbError> {
    let Some(channel_id) = route.channel_id else {
        return Ok(false);
    };
    let channel = state.db.get_channel(community, channel_id).await?;
    for key in [&route.agent, &route.recipient] {
        if !state
            .db
            .is_member(community, channel_id, key.as_bytes())
            .await?
        {
            return Ok(false);
        }
        let restriction = state
            .db
            .moderation_restriction_state(community, key.as_bytes())
            .await?;
        if restriction.banned || (*key == route.agent && restriction.muted_until.is_some()) {
            return Ok(false);
        }
    }
    let Some(profile) = latest_profile(state, community, &route.agent).await? else {
        return Ok(false);
    };
    let Some(owner) = verified_profile_owner(&profile) else {
        return Ok(false);
    };
    let mut query = EventQuery::for_community(community);
    query.kinds = Some(vec![buzz_core::kind::KIND_MANAGED_AGENT as i32]);
    query.pubkey = Some(owner.to_bytes().to_vec());
    query.d_tag = Some(route.agent.to_hex());
    query.global_only = true;
    query.limit = Some(1);
    let policies: Vec<_> = state
        .db
        .query_events(&query)
        .await?
        .into_iter()
        .map(|stored| stored.event)
        .collect();
    let Some(policy) = verified_observer_policy(&route.agent, &[profile], &policies) else {
        return Ok(false);
    };
    if policy.allows(&route.recipient, None, channel.channel_type == "dm") {
        return Ok(true);
    }
    let recipient_owner = latest_profile(state, community, &route.recipient)
        .await?
        .as_ref()
        .and_then(verified_profile_owner);
    Ok(policy.allows(
        &route.recipient,
        recipient_owner.as_ref(),
        channel.channel_type == "dm",
    ))
}
