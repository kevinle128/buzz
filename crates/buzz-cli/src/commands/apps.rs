//! `buzz apps` — community App lifecycle via kind 9038 and verified kind 39008.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use buzz_core::app::{parse_app_admin_command, parse_canonical_app_id, AppAdminCommand};
use buzz_core::kind::{KIND_APP_ADMIN_COMMAND, KIND_APP_METADATA};
use nostr::{Event, EventBuilder, Kind};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::client::BuzzClient;
use crate::commands::parse_write_response;
use crate::error::CliError;
use crate::OutputFormat;

const APP_METADATA_QUERY_LIMIT: u32 = 500;
const DUPLICATE_NO_SECRET_MSG: &str =
    "duplicate create or rotate returned no secret; use `buzz apps rotate-secret --app <uuid>`";

/// Verified public App metadata folded from a kind 39008 head.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppListEntry {
    app_id: Uuid,
    name: String,
    status: String,
    description: String,
    icon_url: Option<String>,
    updated_at: u64,
}

pub async fn dispatch(
    cmd: crate::AppsCmd,
    client: &BuzzClient,
    format: &OutputFormat,
) -> Result<(), CliError> {
    match cmd {
        crate::AppsCmd::List => cmd_list_apps(client, format).await,
        crate::AppsCmd::Create {
            name,
            description,
            icon_url,
        } => cmd_create_app(client, name, description, icon_url).await,
        crate::AppsCmd::Update {
            app,
            name,
            description,
            icon_url,
            clear_description,
            clear_icon,
        } => {
            cmd_update_app(
                client,
                &app,
                name,
                description,
                icon_url,
                clear_description,
                clear_icon,
            )
            .await
        }
        crate::AppsCmd::RotateSecret { app } => cmd_rotate_secret(client, &app).await,
        crate::AppsCmd::Enable { app } => cmd_set_enabled(client, &app, true).await,
        crate::AppsCmd::Disable { app } => cmd_set_enabled(client, &app, false).await,
    }
}

async fn cmd_list_apps(client: &BuzzClient, format: &OutputFormat) -> Result<(), CliError> {
    let nip11_raw = client
        .get_public("/")
        .await
        .map_err(|err| CliError::Other(format!("failed to fetch relay info document: {err}")))?;
    let relay_self = parse_nip11_self(&nip11_raw)?;
    let raw = client.query(&app_metadata_filter(&relay_self)).await?;
    let events: Vec<Value> = serde_json::from_str(&raw)
        .map_err(|err| CliError::Other(format!("invalid query response: {err}")))?;
    let apps = fold_app_heads(&events, &relay_self);
    println!("{}", format_app_list(&apps, format, client.relay_url()));
    Ok(())
}

async fn cmd_create_app(
    client: &BuzzClient,
    name: String,
    description: Option<String>,
    icon_url: Option<String>,
) -> Result<(), CliError> {
    let command = build_create_command(name, description, icon_url)?;
    publish_secret_command(client, &command).await
}

async fn cmd_update_app(
    client: &BuzzClient,
    app: &str,
    name: Option<String>,
    description: Option<String>,
    icon_url: Option<String>,
    clear_description: bool,
    clear_icon: bool,
) -> Result<(), CliError> {
    let app_id = parse_app_id_flag(app)?;
    let command = build_update_command(
        app_id,
        name,
        description,
        icon_url,
        clear_description,
        clear_icon,
    )?;
    let raw = publish_admin_command(client, &command).await?;
    println!(
        "{}",
        parse_write_response(&raw, "app update was dominated; retry")?
    );
    Ok(())
}

async fn cmd_rotate_secret(client: &BuzzClient, app: &str) -> Result<(), CliError> {
    let app_id = parse_app_id_flag(app)?;
    publish_secret_command(client, &AppAdminCommand::RotateSecret { app_id }).await
}

async fn cmd_set_enabled(client: &BuzzClient, app: &str, enable: bool) -> Result<(), CliError> {
    let app_id = parse_app_id_flag(app)?;
    let command = if enable {
        AppAdminCommand::Enable { app_id }
    } else {
        AppAdminCommand::Disable { app_id }
    };
    let raw = publish_admin_command(client, &command).await?;
    let conflict = if enable {
        "app enable was dominated; retry"
    } else {
        "app disable was dominated; retry"
    };
    println!("{}", parse_write_response(&raw, conflict)?);
    Ok(())
}

async fn publish_secret_command(
    client: &BuzzClient,
    command: &AppAdminCommand,
) -> Result<(), CliError> {
    let raw = publish_admin_command(client, command).await?;
    let output = parse_secret_write_response(&raw, client.relay_url())?;
    println!("{output}");
    Ok(())
}

async fn publish_admin_command(
    client: &BuzzClient,
    command: &AppAdminCommand,
) -> Result<String, CliError> {
    let event = client.sign_event(app_admin_builder(command)?)?;
    client.submit_event(event).await
}

fn app_callback_url(relay_url: &str, app_id: &Uuid) -> String {
    format!("{}/hooks/apps/{app_id}", relay_url.trim_end_matches('/'))
}

fn parse_nip11_self(nip11_json: &str) -> Result<String, CliError> {
    let nip11: Value = serde_json::from_str(nip11_json)
        .map_err(|err| CliError::Other(format!("relay info document is not valid JSON: {err}")))?;
    let self_hex = nip11
        .get("self")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other("relay info document missing 'self' field".into()))?;
    if self_hex.len() != 64 || !self_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Other(format!(
            "relay 'self' field is not a valid 64-hex pubkey: {self_hex}"
        )));
    }
    Ok(self_hex.to_ascii_lowercase())
}

fn app_metadata_filter(relay_self: &str) -> Value {
    json!({
        "kinds": [KIND_APP_METADATA],
        "authors": [relay_self],
        "limit": APP_METADATA_QUERY_LIMIT
    })
}

fn serialize_admin_command(command: &AppAdminCommand) -> Result<String, CliError> {
    let mut value = serde_json::to_value(command)
        .map_err(|err| CliError::Other(format!("serialize app command: {err}")))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    serde_json::to_string(&value)
        .map_err(|err| CliError::Other(format!("serialize app command: {err}")))
}

fn app_admin_builder(command: &AppAdminCommand) -> Result<EventBuilder, CliError> {
    Ok(EventBuilder::new(
        Kind::Custom(KIND_APP_ADMIN_COMMAND as u16),
        serialize_admin_command(command)?,
    ))
}

fn parse_app_id_flag(raw: &str) -> Result<Uuid, CliError> {
    parse_canonical_app_id(raw).map_err(|err| CliError::Usage(err.to_string()))
}

fn command_from_json(body: Value) -> Result<AppAdminCommand, CliError> {
    parse_app_admin_command(&body.to_string()).map_err(|err| CliError::Usage(err.to_string()))
}

fn build_create_command(
    name: String,
    description: Option<String>,
    icon_url: Option<String>,
) -> Result<AppAdminCommand, CliError> {
    let mut body = json!({ "action": "create", "name": name });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    if let Some(icon_url) = icon_url {
        body["icon_url"] = json!(icon_url);
    }
    command_from_json(body)
}

fn build_update_command(
    app_id: Uuid,
    name: Option<String>,
    description: Option<String>,
    icon_url: Option<String>,
    clear_description: bool,
    clear_icon: bool,
) -> Result<AppAdminCommand, CliError> {
    let mut body = json!({ "action": "update", "app_id": app_id });
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    if clear_description {
        body["description"] = json!("");
    } else if let Some(description) = description {
        body["description"] = json!(description);
    }
    if clear_icon {
        body["icon_url"] = json!("");
    } else if let Some(icon_url) = icon_url {
        body["icon_url"] = json!(icon_url);
    }
    command_from_json(body)
}

fn parse_secret_write_response(raw: &str, relay_url: &str) -> Result<Value, CliError> {
    let response: Value = serde_json::from_str(raw)
        .map_err(|err| CliError::Other(format!("relay response is not JSON: {err} ({raw})")))?;
    let accepted = response
        .get("accepted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !accepted {
        return Err(CliError::Other(format!("relay rejected event: {message}")));
    }
    if message == "duplicate" || message.starts_with("duplicate:") {
        return Err(CliError::Conflict(DUPLICATE_NO_SECRET_MSG.into()));
    }
    let payload = message
        .strip_prefix("response:")
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .ok_or_else(|| {
            CliError::Other(format!(
                "relay response missing response: payload: {message}"
            ))
        })?;
    let app_id = payload
        .get("app_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other("relay response missing app_id".into()))?;
    let app_id = parse_canonical_app_id(app_id).map_err(|err| CliError::Other(err.to_string()))?;
    let secret = payload
        .get("webhook_secret")
        .and_then(Value::as_str)
        .filter(|secret| !secret.is_empty());
    let Some(secret) = secret else {
        return Err(CliError::Conflict(format!(
            "create or rotate returned no secret; use `buzz apps rotate-secret --app {app_id}`"
        )));
    };
    Ok(json!({
        "app_id": app_id,
        "callback_url": app_callback_url(relay_url, &app_id),
        "webhook_secret": secret,
    }))
}

fn fold_app_heads(events: &[Value], relay_self: &str) -> Vec<AppListEntry> {
    let mut heads: BTreeMap<Uuid, (Event, AppListEntry)> = BTreeMap::new();
    for raw in events {
        let Ok(event) = serde_json::from_value::<Event>(raw.clone()) else {
            continue;
        };
        let Some(row) = parse_verified_app_metadata(&event, relay_self) else {
            continue;
        };
        match heads.get(&row.app_id) {
            Some((current, _)) if !is_newer_head(&event, current) => {}
            _ => {
                heads.insert(row.app_id, (event, row));
            }
        }
    }
    heads.into_values().map(|(_, row)| row).collect()
}

fn is_newer_head(candidate: &Event, current: &Event) -> bool {
    match candidate
        .created_at
        .as_secs()
        .cmp(&current.created_at.as_secs())
    {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => candidate.id.as_bytes() < current.id.as_bytes(),
    }
}

fn parse_verified_app_metadata(event: &Event, relay_self: &str) -> Option<AppListEntry> {
    if event.kind.as_u16() != KIND_APP_METADATA as u16 {
        return None;
    }
    if event.pubkey.to_hex() != relay_self {
        return None;
    }
    if event.verify().is_err() {
        return None;
    }
    let app_id = parse_canonical_app_id(single_tag(event, "d")?).ok()?;
    let name = single_tag(event, "name")?.to_string();
    if name.is_empty() {
        return None;
    }
    let status = single_tag(event, "status")?;
    if status != "active" && status != "disabled" {
        return None;
    }
    Some(AppListEntry {
        app_id,
        name,
        status: status.to_string(),
        description: event.content.clone(),
        icon_url: optional_tag(event, "picture").ok()?.map(str::to_string),
        updated_at: event.created_at.as_secs(),
    })
}

fn single_tag<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    optional_tag(event, name)
        .ok()?
        .filter(|value| !value.is_empty())
}

fn optional_tag<'a>(event: &'a Event, name: &str) -> Result<Option<&'a str>, ()> {
    let mut found = None;
    for tag in event.tags.iter() {
        let slice = tag.as_slice();
        if slice.first().map(String::as_str) != Some(name) {
            continue;
        }
        if slice.len() != 2 {
            return Err(());
        }
        if found.replace(slice[1].as_str()).is_some() {
            return Err(());
        }
    }
    Ok(found)
}

fn format_app_list(apps: &[AppListEntry], format: &OutputFormat, relay_url: &str) -> String {
    let rows: Vec<Value> = apps
        .iter()
        .map(|app| match format {
            OutputFormat::Compact => json!({
                "app_id": app.app_id,
                "name": app.name,
                "status": app.status,
                "callback_url": app_callback_url(relay_url, &app.app_id),
            }),
            OutputFormat::Json => json!({
                "app_id": app.app_id,
                "name": app.name,
                "status": app.status,
                "callback_url": app_callback_url(relay_url, &app.app_id),
                "description": app.description,
                "icon_url": app.icon_url,
                "updated_at": app.updated_at,
            }),
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error;
    use nostr::{Keys, Tag, Timestamp};
    use serde_json::json;

    const APP_ID: &str = "6eb31227-8ed2-42ec-9024-863497cbeed2";
    const RELAY: &str = "http://localhost:3000";

    fn app_uuid() -> Uuid {
        Uuid::parse_str(APP_ID).expect("canonical fixture")
    }

    fn json_eq(actual: &str, expected: Value) {
        let parsed: Value = serde_json::from_str(actual).expect("json");
        assert_eq!(parsed, expected);
    }

    struct Meta<'a> {
        app_id: &'a str,
        name: &'a str,
        status: &'a str,
        description: &'a str,
        picture: Option<&'a str>,
        created_at: u64,
        extra_tags: &'a [Tag],
    }

    impl<'a> Meta<'a> {
        fn new(app_id: &'a str, name: &'a str, created_at: u64) -> Self {
            Self {
                app_id,
                name,
                status: "active",
                description: "",
                picture: None,
                created_at,
                extra_tags: &[],
            }
        }
    }

    fn metadata_event(keys: &Keys, spec: Meta<'_>) -> Event {
        let mut tags = vec![
            Tag::parse(["d", spec.app_id]).expect("d"),
            Tag::parse(["name", spec.name]).expect("name"),
            Tag::parse(["status", spec.status]).expect("status"),
        ];
        if let Some(picture) = spec.picture {
            tags.push(Tag::parse(["picture", picture]).expect("picture"));
        }
        tags.extend(spec.extra_tags.iter().cloned());
        EventBuilder::new(Kind::Custom(KIND_APP_METADATA as u16), spec.description)
            .tags(tags)
            .custom_created_at(Timestamp::from(spec.created_at))
            .sign_with_keys(keys)
            .expect("sign metadata")
    }

    fn event_json(event: &Event) -> Value {
        serde_json::to_value(event).expect("event json")
    }

    fn flip_hex_nibble(hex: &str) -> String {
        let mut bytes: Vec<char> = hex.chars().collect();
        let last = bytes.last_mut().expect("non-empty hex");
        *last = if *last == '0' { '1' } else { '0' };
        bytes.into_iter().collect()
    }

    #[test]
    fn callback_url_uses_relay_http_base_and_app_id() {
        assert_eq!(
            app_callback_url(RELAY, &app_uuid()),
            format!("{RELAY}/hooks/apps/{APP_ID}")
        );
        assert_eq!(
            app_callback_url("https://relay.example.com/", &app_uuid()),
            format!("https://relay.example.com/hooks/apps/{APP_ID}")
        );
    }

    #[test]
    fn nip11_self_normalizes_lowercase_64_hex() {
        let upper = "A".repeat(64);
        let parsed = parse_nip11_self(&json!({"self": upper}).to_string()).expect("self");
        assert_eq!(parsed, "a".repeat(64));
    }

    #[test]
    fn nip11_self_rejects_missing_invalid_and_short() {
        assert!(parse_nip11_self("{}").is_err());
        assert!(parse_nip11_self(&json!({"self": "zz"}).to_string()).is_err());
        assert!(parse_nip11_self(&json!({"self": "a".repeat(63)}).to_string()).is_err());
        assert!(parse_nip11_self(&json!({"self": "z".repeat(64)}).to_string()).is_err());
    }

    #[test]
    fn metadata_query_filter_is_kind_39008_self_limit_500() {
        let self_hex = "ab".repeat(32);
        assert_eq!(
            app_metadata_filter(&self_hex),
            json!({
                "kinds": [39008],
                "authors": [self_hex],
                "limit": 500
            })
        );
    }

    #[test]
    fn create_command_json_is_exact_kind_9038_body() {
        let command = build_create_command(
            "Buildkite".into(),
            Some("Build notifications".into()),
            Some("https://example.test/icon.png".into()),
        )
        .expect("create");
        json_eq(
            &serialize_admin_command(&command).expect("json"),
            json!({
                "action": "create",
                "name": "Buildkite",
                "description": "Build notifications",
                "icon_url": "https://example.test/icon.png"
            }),
        );
        let keys = Keys::generate();
        let event = app_admin_builder(&command)
            .expect("builder")
            .sign_with_keys(&keys)
            .expect("sign");
        assert_eq!(event.kind.as_u16(), 9038);
        assert_eq!(event.kind.as_u16(), KIND_APP_ADMIN_COMMAND as u16);
        parse_app_admin_command(&event.content).expect("protocol accepts CLI json");
    }

    #[test]
    fn create_omits_null_optional_fields() {
        let command = build_create_command("Pager".into(), None, None).expect("create");
        json_eq(
            &serialize_admin_command(&command).expect("json"),
            json!({"action": "create", "name": "Pager"}),
        );
    }

    #[test]
    fn update_rotate_enable_disable_json_is_exact() {
        let id = app_uuid();
        json_eq(
            &serialize_admin_command(
                &build_update_command(id, Some("New".into()), None, None, false, false)
                    .expect("update name"),
            )
            .expect("json"),
            json!({"action": "update", "app_id": APP_ID, "name": "New"}),
        );
        json_eq(
            &serialize_admin_command(
                &build_update_command(id, None, None, None, true, false).expect("clear desc"),
            )
            .expect("json"),
            json!({"action": "update", "app_id": APP_ID, "description": ""}),
        );
        json_eq(
            &serialize_admin_command(
                &build_update_command(id, None, None, None, false, true).expect("clear icon"),
            )
            .expect("json"),
            json!({"action": "update", "app_id": APP_ID, "icon_url": ""}),
        );
        json_eq(
            &serialize_admin_command(&AppAdminCommand::RotateSecret { app_id: id }).expect("json"),
            json!({"action": "rotate_secret", "app_id": APP_ID}),
        );
        json_eq(
            &serialize_admin_command(&AppAdminCommand::Enable { app_id: id }).expect("json"),
            json!({"action": "enable", "app_id": APP_ID}),
        );
        json_eq(
            &serialize_admin_command(&AppAdminCommand::Disable { app_id: id }).expect("json"),
            json!({"action": "disable", "app_id": APP_ID}),
        );
    }

    #[test]
    fn update_without_mutation_is_rejected_by_core_validation() {
        let err = build_update_command(app_uuid(), None, None, None, false, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("at least one of name, description, or icon_url"),
            "{err}"
        );
    }

    #[test]
    fn app_flag_requires_canonical_lowercase_uuid() {
        assert_eq!(parse_app_id_flag(APP_ID).expect("canonical"), app_uuid());
        assert!(parse_app_id_flag(&APP_ID.to_uppercase()).is_err());
        assert!(parse_app_id_flag("6eb312278ed242ec9024863497cbeed2").is_err());
        assert!(parse_app_id_flag("not-a-uuid").is_err());
    }

    #[test]
    fn successful_create_prints_secret_and_callback_only() {
        let raw = json!({
            "event_id": "ab".repeat(32),
            "accepted": true,
            "message": format!(
                "response:{}",
                json!({"app_id": APP_ID, "webhook_secret": "one-time-secret"})
            )
        })
        .to_string();
        let out = parse_secret_write_response(&raw, RELAY).expect("secret response");
        assert_eq!(
            out,
            json!({
                "app_id": APP_ID,
                "callback_url": format!("{RELAY}/hooks/apps/{APP_ID}"),
                "webhook_secret": "one-time-secret"
            })
        );
        assert!(out.get("event_id").is_none());
    }

    #[test]
    fn duplicate_create_or_rotate_without_secret_is_exit_5() {
        let duplicate = json!({
            "event_id": "cd".repeat(32),
            "accepted": true,
            "message": "duplicate: already processed"
        })
        .to_string();
        let err = parse_secret_write_response(&duplicate, RELAY).unwrap_err();
        assert_eq!(error::exit_code(&err), 5);
        assert!(err.to_string().contains("rotate"), "{err}");

        let no_secret = json!({
            "event_id": "ef".repeat(32),
            "accepted": true,
            "message": format!("response:{}", json!({"app_id": APP_ID}))
        })
        .to_string();
        let err = parse_secret_write_response(&no_secret, RELAY).unwrap_err();
        assert_eq!(error::exit_code(&err), 5);
        assert!(err.to_string().contains("rotate-secret"), "{err}");
        assert!(err.to_string().contains(APP_ID), "{err}");
    }

    #[test]
    fn fold_keeps_latest_head_per_d_and_skips_invalid() {
        let relay = Keys::generate();
        let other = Keys::generate();
        let self_hex = relay.public_key().to_hex();
        let other_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

        let older = metadata_event(
            &relay,
            Meta {
                description: "v1",
                ..Meta::new(APP_ID, "Old", 100)
            },
        );
        let newer = metadata_event(
            &relay,
            Meta {
                status: "disabled",
                description: "v2",
                picture: Some("https://example.test/icon.png"),
                ..Meta::new(APP_ID, "New", 200)
            },
        );
        let other_app = metadata_event(&relay, Meta::new(other_id, "Other", 150));
        let wrong_relay = metadata_event(&other, Meta::new(APP_ID, "Spoof", 300));
        let missing_name = EventBuilder::new(Kind::Custom(KIND_APP_METADATA as u16), "")
            .tags([
                Tag::parse(["d", APP_ID]).unwrap(),
                Tag::parse(["status", "active"]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(400))
            .sign_with_keys(&relay)
            .unwrap();
        let bad_uuid = metadata_event(&relay, Meta::new("NOT-A-UUID", "Bad", 410));
        let extra_d = [Tag::parse(["d", other_id]).unwrap()];
        let dup_d = metadata_event(
            &relay,
            Meta {
                extra_tags: &extra_d,
                ..Meta::new(APP_ID, "Dup", 420)
            },
        );
        let mut bad_sig = event_json(&newer);
        bad_sig["sig"] = json!(flip_hex_nibble(bad_sig["sig"].as_str().unwrap()));
        let mut bad_id = event_json(&newer);
        bad_id["id"] = json!(flip_hex_nibble(bad_id["id"].as_str().unwrap()));

        let folded = fold_app_heads(
            &[
                event_json(&older),
                event_json(&newer),
                event_json(&other_app),
                event_json(&wrong_relay),
                event_json(&missing_name),
                event_json(&bad_uuid),
                event_json(&dup_d),
                bad_sig,
                bad_id,
            ],
            &self_hex,
        );
        assert_eq!(folded.len(), 2, "{folded:?}");
        assert_eq!(folded[0].app_id, app_uuid());
        assert_eq!(folded[0].name, "New");
        assert_eq!(folded[0].status, "disabled");
        assert_eq!(folded[0].description, "v2");
        assert_eq!(
            folded[0].icon_url.as_deref(),
            Some("https://example.test/icon.png")
        );
        assert_eq!(folded[0].updated_at, 200);
        assert_eq!(folded[1].app_id.to_string(), other_id);
        assert_eq!(folded[1].name, "Other");
    }

    #[test]
    fn fold_tie_breaks_equal_created_at_by_lower_event_id() {
        let relay = Keys::generate();
        let self_hex = relay.public_key().to_hex();
        let left = metadata_event(&relay, Meta::new(APP_ID, "Left", 50));
        let right = metadata_event(&relay, Meta::new(APP_ID, "Right", 50));
        let winner = if left.id.as_bytes() < right.id.as_bytes() {
            "Left"
        } else {
            "Right"
        };
        let folded = fold_app_heads(&[event_json(&left), event_json(&right)], &self_hex);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].name, winner);
    }

    #[test]
    fn compact_omits_description_icon_and_secret_full_includes_public_fields() {
        let apps = vec![AppListEntry {
            app_id: app_uuid(),
            name: "Buildkite".into(),
            status: "active".into(),
            description: "Build notifications".into(),
            icon_url: Some("https://example.test/icon.png".into()),
            updated_at: 1_700_000_000,
        }];
        let compact = format_app_list(&apps, &OutputFormat::Compact, RELAY);
        json_eq(
            &compact,
            json!([{
                "app_id": APP_ID,
                "name": "Buildkite",
                "status": "active",
                "callback_url": format!("{RELAY}/hooks/apps/{APP_ID}")
            }]),
        );
        assert!(!compact.contains("webhook_secret"));
        assert!(!compact.contains("description"));
        assert!(!compact.contains("icon_url"));
        assert!(!compact.contains("updated_at"));

        json_eq(
            &format_app_list(&apps, &OutputFormat::Json, RELAY),
            json!([{
                "app_id": APP_ID,
                "name": "Buildkite",
                "status": "active",
                "callback_url": format!("{RELAY}/hooks/apps/{APP_ID}"),
                "description": "Build notifications",
                "icon_url": "https://example.test/icon.png",
                "updated_at": 1_700_000_000u64
            }]),
        );
    }
}
