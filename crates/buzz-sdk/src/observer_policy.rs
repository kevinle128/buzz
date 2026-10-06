//! Owner-authenticated saved policy for channel observer audiences.
use nostr::{Event, Kind, PublicKey};
use serde::Deserialize;

/// Audience selected by a verified owner-signed managed-agent policy.
#[derive(Debug, Clone)]
pub struct ObserverPolicy {
    /// Owner proven by the latest agent NIP-OA profile.
    pub owner: PublicKey,
    content: PolicyContent,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RespondTo {
    OwnerOnly,
    Allowlist,
    Anyone,
    Nobody,
}
#[derive(Debug, Clone, Deserialize)]
struct PolicyContent {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "parallelism")]
    _parallelism: u32,
    #[serde(rename = "persona_id")]
    _persona_id: Option<String>,
    #[serde(rename = "system_prompt")]
    _system_prompt: Option<String>,
    #[serde(rename = "model")]
    _model: Option<String>,
    #[serde(rename = "provider")]
    _provider: Option<String>,
    #[serde(rename = "persona_source_version")]
    _persona_source_version: Option<String>,
    respond_to: RespondTo,
    #[serde(default)]
    respond_to_allowlist: Vec<PublicKey>,
}
impl ObserverPolicy {
    /// Apply the existing owner/sibling exception and stricter DM rule.
    /// `recipient_owner` must come from a verified latest NIP-OA profile.
    pub fn allows(
        &self,
        recipient: &PublicKey,
        recipient_owner: Option<&PublicKey>,
        is_dm: bool,
    ) -> bool {
        if matches!(self.content.respond_to, RespondTo::Nobody) {
            return false;
        }
        let owner_or_sibling = *recipient == self.owner || recipient_owner == Some(&self.owner);
        if is_dm {
            return owner_or_sibling;
        }
        match self.content.respond_to {
            RespondTo::Anyone => true,
            RespondTo::Nobody => false,
            RespondTo::OwnerOnly => owner_or_sibling,
            RespondTo::Allowlist => {
                owner_or_sibling || self.content.respond_to_allowlist.contains(recipient)
            }
        }
    }
}

/// Verify a profile's sole NIP-OA tag, including all signed event conditions.
/// Conditions bind to the profile timestamp, not the verifier's clock.
pub fn verified_profile_owner(event: &Event) -> Option<PublicKey> {
    if event.kind != Kind::Metadata || event.verify().is_err() {
        return None;
    }
    let mut tags = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("auth"));
    let tag = tags.next()?;
    if tags.next().is_some() {
        return None;
    }
    let json = serde_json::to_string(tag.as_slice()).ok()?;
    crate::nip_oa::parse_auth_tag(&json).ok()?;
    let owner = crate::nip_oa::verify_auth_tag(&json, &event.pubkey).ok()?;
    let conditions = tag.as_slice().get(2)?;
    let applies = conditions.is_empty()
        || conditions.split('&').all(|clause| {
            if let Some(value) = clause.strip_prefix("kind=") {
                value.parse::<u16>() == Ok(event.kind.as_u16())
            } else if let Some(value) = clause.strip_prefix("created_at<") {
                value
                    .parse::<u64>()
                    .is_ok_and(|bound| event.created_at.as_secs() < bound)
            } else if let Some(value) = clause.strip_prefix("created_at>") {
                value
                    .parse::<u64>()
                    .is_ok_and(|bound| event.created_at.as_secs() > bound)
            } else {
                false
            }
        });
    applies.then_some(owner)
}

fn latest<'a>(events: impl Iterator<Item = &'a Event>) -> Option<&'a Event> {
    events.max_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| right.id.cmp(&left.id))
    })
}

/// Resolve the latest owner-authenticated managed-agent audience.
/// Invalid latest content or signatures deny rather than reviving older policy.
pub fn verified_observer_policy(
    agent: &PublicKey,
    profiles: &[Event],
    policies: &[Event],
) -> Option<ObserverPolicy> {
    let profile = latest(profiles.iter().filter(|event| event.pubkey == *agent))?;
    let owner = verified_profile_owner(profile)?;
    let agent_hex = agent.to_hex();
    let event = latest(policies.iter().filter(|event| {
        event.pubkey == owner
            && event.tags.iter().any(|tag| {
                tag.as_slice().first().map(String::as_str) == Some("d")
                    && tag.content() == Some(agent_hex.as_str())
            })
    }))?;
    if event.kind.as_u16() as u32 != buzz_core::kind::KIND_MANAGED_AGENT || event.verify().is_err()
    {
        return None;
    }
    let mut tags = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("d"));
    tags.next()?;
    if tags.next().is_some() {
        return None;
    }
    let content = serde_json::from_str(&event.content).ok()?;
    Some(ObserverPolicy { owner, content })
}
