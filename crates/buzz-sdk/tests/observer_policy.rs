use buzz_sdk::observer_policy::{verified_observer_policy, verified_profile_owner};
use nostr::{Event, EventBuilder, Keys, Kind, Tag, Timestamp};

fn profile(agent: &Keys, owner: &Keys) -> Event {
    let auth = buzz_sdk::nip_oa::compute_auth_tag(owner, &agent.public_key(), "kind=0").unwrap();
    EventBuilder::new(Kind::Metadata, "{}")
        .tags([buzz_sdk::nip_oa::parse_auth_tag(&auth).unwrap()])
        .sign_with_keys(agent)
        .unwrap()
}
fn policy(agent: &Keys, owner: &Keys, content: &str, time: u64) -> Event {
    EventBuilder::new(Kind::Custom(30177), content)
        .tags([Tag::parse(["d", &agent.public_key().to_hex()]).unwrap()])
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(owner)
        .unwrap()
}
#[test]
fn verified_latest_policy_preserves_owner_sibling_and_dm_rules() {
    let agent = Keys::generate();
    let owner = Keys::generate();
    let viewer = Keys::generate();
    let sibling = Keys::generate();
    let profiles = [profile(&agent, &owner)];
    assert_eq!(
        verified_profile_owner(&profiles[0]),
        Some(owner.public_key())
    );
    for (mode, expected) in [
        ("anyone", true),
        ("allowlist", false),
        ("owner-only", false),
        ("nobody", false),
    ] {
        let events = [policy(
            &agent,
            &owner,
            &format!(r#"{{"name":"test","parallelism":1,"respond_to":"{mode}"}}"#),
            1,
        )];
        let p = verified_observer_policy(&agent.public_key(), &profiles, &events).unwrap();
        assert_eq!(p.allows(&viewer.public_key(), None, false), expected);
        assert!(!p.allows(&viewer.public_key(), None, true));
        assert_eq!(p.allows(&owner.public_key(), None, true), mode != "nobody");
        assert_eq!(
            p.allows(&sibling.public_key(), Some(&owner.public_key()), true),
            mode != "nobody"
        );
    }
}
#[test]
fn invalid_latest_and_forged_policy_cannot_revive_old_permissions() {
    let agent = Keys::generate();
    let owner = Keys::generate();
    let forged = Keys::generate();
    let profiles = [profile(&agent, &owner)];
    let old = policy(
        &agent,
        &owner,
        r#"{"name":"test","parallelism":1,"respond_to":"anyone"}"#,
        1,
    );
    let invalid = policy(
        &agent,
        &owner,
        r#"{"name":"test","parallelism":1,"respond_to":"unexpected"}"#,
        2,
    );
    assert!(
        verified_observer_policy(&agent.public_key(), &profiles, &[old.clone(), invalid]).is_none()
    );
    assert!(verified_observer_policy(
        &agent.public_key(),
        &profiles,
        &[policy(
            &agent,
            &forged,
            r#"{"name":"test","parallelism":1,"respond_to":"anyone"}"#,
            3
        )]
    )
    .is_none());
    assert!(verified_observer_policy(&agent.public_key(), &[], &[old]).is_none());
}

#[test]
fn malformed_saved_definition_cannot_grant_an_observer_audience() {
    let agent = Keys::generate();
    let owner = Keys::generate();
    let profiles = [profile(&agent, &owner)];
    for content in [
        r#"{"respond_to":"anyone"}"#,
        r#"{"name":"test","parallelism":1,"respond_to":"anyone","model":42}"#,
        r#"{"name":"test","parallelism":-1,"respond_to":"anyone"}"#,
    ] {
        assert!(verified_observer_policy(
            &agent.public_key(),
            &profiles,
            &[policy(&agent, &owner, content, 2)]
        )
        .is_none());
    }
}
