//! Current signed channel membership and saved policy for observer publication.
use crate::relay::{RelayError, RestClient};
use buzz_sdk::observer_policy::{verified_observer_policy, verified_profile_owner};
use nostr::{Event, PublicKey};
use serde_json::json;
use uuid::Uuid;

pub(crate) struct ObserverAudience {
    rest: RestClient,
    signer: Option<PublicKey>,
}
impl ObserverAudience {
    pub(crate) async fn new(rest: RestClient) -> Self {
        let signer = rest
            .relay_self()
            .await
            .ok()
            .flatten()
            .and_then(|key| PublicKey::from_hex(&key).ok());
        Self { rest, signer }
    }
    async fn query(&self, filter: serde_json::Value) -> Result<Vec<Event>, RelayError> {
        let value = self.rest.query_raw(&[filter]).await?;
        serde_json::from_value(value).map_err(|error| {
            RelayError::Http(format!("invalid observer authorization response: {error}"))
        })
    }
    pub(crate) async fn recipients(
        &mut self,
        agent: PublicKey,
        channel: Uuid,
    ) -> Result<Vec<PublicKey>, RelayError> {
        if self.signer.is_none() {
            self.signer = self
                .rest
                .relay_self()
                .await?
                .and_then(|key| PublicKey::from_hex(&key).ok());
        }
        let Some(signer) = self.signer else {
            return Ok(Vec::new());
        };
        let id = channel.to_string();
        let mut heads = self
            .query(json!({"kinds":[39002],"authors":[signer.to_hex()],"#d":[id],"limit":1}))
            .await?;
        heads.extend(
            self.query(json!({"kinds":[39000],"authors":[signer.to_hex()],"#d":[id],"limit":1}))
                .await?,
        );
        let Some(roster) = latest(heads.iter().filter(|event| event.kind.as_u16() == 39002)) else {
            return Ok(Vec::new());
        };
        let Some(metadata) = latest(heads.iter().filter(|event| event.kind.as_u16() == 39000))
        else {
            return Ok(Vec::new());
        };
        if !valid_head(roster, signer, &id) || !valid_head(metadata, signer, &id) {
            return Ok(Vec::new());
        }
        if metadata
            .tags
            .iter()
            .any(|tag| tag.as_slice() == ["archived", "true"])
        {
            return Ok(Vec::new());
        }
        let mut types = metadata
            .tags
            .iter()
            .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("t"));
        let Some(channel_type) = types.next().and_then(|tag| tag.content()) else {
            return Ok(Vec::new());
        };
        if channel_type.is_empty() || types.next().is_some() {
            return Ok(Vec::new());
        }
        let mut members = Vec::new();
        for tag in roster
            .tags
            .iter()
            .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("p"))
        {
            let Some(key) = tag.content().and_then(|key| PublicKey::from_hex(key).ok()) else {
                return Ok(Vec::new());
            };
            members.push(key);
        }
        members.sort();
        members.dedup();
        if !members.contains(&agent) {
            return Ok(Vec::new());
        }
        // The signed roster event bounds this author list. Profiles are current
        // replaceable heads; fetching them together avoids a request per member.
        let profiles=self.query(json!({"kinds":[0],"authors":members.iter().map(PublicKey::to_hex).collect::<Vec<_>>(),"limit":members.len()})).await?;
        let Some(profile) = latest(profiles.iter().filter(|event| event.pubkey == agent)) else {
            return Ok(Vec::new());
        };
        let Some(owner) = verified_profile_owner(profile) else {
            return Ok(Vec::new());
        };
        let policies = self
            .query(
                json!({"kinds":[30177],"authors":[owner.to_hex()],"#d":[agent.to_hex()],"limit":1}),
            )
            .await?;
        let Some(policy) = verified_observer_policy(
            &agent,
            &profiles
                .iter()
                .filter(|event| event.pubkey == agent)
                .cloned()
                .collect::<Vec<_>>(),
            &policies,
        ) else {
            return Ok(Vec::new());
        };
        Ok(members
            .into_iter()
            .filter(|recipient| {
                if *recipient == agent {
                    return false;
                }
                let recipient_owner =
                    latest(profiles.iter().filter(|event| event.pubkey == *recipient))
                        .and_then(verified_profile_owner);
                policy.allows(recipient, recipient_owner.as_ref(), channel_type == "dm")
            })
            .collect())
    }
}
fn latest<'a>(events: impl Iterator<Item = &'a Event>) -> Option<&'a Event> {
    events.max_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| b.id.cmp(&a.id))
    })
}
fn valid_head(event: &Event, signer: PublicKey, channel: &str) -> bool {
    if event.pubkey != signer || event.verify().is_err() {
        return false;
    }
    let mut tags = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("d"));
    tags.next()
        .is_some_and(|tag| tag.content() == Some(channel))
        && tags.next().is_none()
}
