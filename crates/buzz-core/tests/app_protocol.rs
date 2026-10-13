//! Kind 9038 / 39008 App protocol contract.

use buzz_core::app::{
    parse_app_admin_command, parse_canonical_app_id, validate_app_icon_url, AppAdminCommand,
    AppStatus, APP_CALLBACK_BODY_MAX_BYTES, APP_CALLBACK_METADATA_MAX_BYTES,
    APP_CALLBACK_METADATA_MAX_DEPTH, APP_CALLBACK_METADATA_MAX_NODES, APP_SECRET_BYTES,
};
use buzz_core::kind::{
    is_command_kind, is_parameterized_replaceable, is_relay_admin_kind, is_relay_only_kind,
    KIND_APP_ADMIN_COMMAND, KIND_APP_METADATA,
};

const CANONICAL_APP_ID: &str = "6eb31227-8ed2-42ec-9024-863497cbeed2";

fn parse(json: &str) -> Result<AppAdminCommand, buzz_core::app::AppCommandError> {
    parse_app_admin_command(json)
}

#[test]
fn app_kind_constants_have_stable_numeric_values() {
    assert_eq!(KIND_APP_ADMIN_COMMAND, 9038);
    assert_eq!(KIND_APP_METADATA, 39008);
}

#[test]
fn callback_limit_constants_match_protocol() {
    assert_eq!(APP_SECRET_BYTES, 32);
    assert_eq!(APP_CALLBACK_BODY_MAX_BYTES, 65_536);
    assert_eq!(APP_CALLBACK_METADATA_MAX_BYTES, 32_768);
    assert_eq!(APP_CALLBACK_METADATA_MAX_DEPTH, 16);
    assert_eq!(APP_CALLBACK_METADATA_MAX_NODES, 1_024);
}

#[test]
fn app_admin_command_kind_is_command_and_relay_admin() {
    assert!(is_command_kind(KIND_APP_ADMIN_COMMAND));
    assert!(is_relay_admin_kind(KIND_APP_ADMIN_COMMAND));
    assert!(!is_relay_only_kind(KIND_APP_ADMIN_COMMAND));
    assert!(!is_parameterized_replaceable(KIND_APP_ADMIN_COMMAND));
}

#[test]
fn app_metadata_kind_is_relay_only() {
    assert!(is_relay_only_kind(KIND_APP_METADATA));
    assert!(is_parameterized_replaceable(KIND_APP_METADATA));
    assert!(!is_command_kind(KIND_APP_METADATA));
    assert!(!is_relay_admin_kind(KIND_APP_METADATA));
}

#[test]
fn app_status_wire_names_are_snake_case() {
    assert_eq!(
        serde_json::to_string(&AppStatus::Active).expect("active"),
        "\"active\""
    );
    assert_eq!(
        serde_json::to_string(&AppStatus::Disabled).expect("disabled"),
        "\"disabled\""
    );
    assert_eq!(
        serde_json::from_str::<AppStatus>("\"active\"").expect("parse active"),
        AppStatus::Active
    );
    assert_eq!(
        serde_json::from_str::<AppStatus>("\"disabled\"").expect("parse disabled"),
        AppStatus::Disabled
    );
}

#[test]
fn accepts_every_command_shape() {
    let create = parse(
        r#"{"action":"create","name":"Buildkite","description":"Build notifications","icon_url":"https://example.test/icon.png"}"#,
    )
    .expect("create");
    match create {
        AppAdminCommand::Create {
            name,
            description,
            icon_url,
        } => {
            assert_eq!(name, "Buildkite");
            assert_eq!(description.as_deref(), Some("Build notifications"));
            assert_eq!(icon_url.as_deref(), Some("https://example.test/icon.png"));
        }
        other => panic!("expected create, got {other:?}"),
    }

    let update = parse(&format!(
        r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}","name":"CI","description":"","icon_url":""}}"#
    ))
    .expect("update");
    match update {
        AppAdminCommand::Update {
            app_id,
            name,
            description,
            icon_url,
        } => {
            assert_eq!(app_id.to_string(), CANONICAL_APP_ID);
            assert_eq!(name.as_deref(), Some("CI"));
            assert_eq!(description.as_deref(), Some(""));
            assert_eq!(icon_url.as_deref(), Some(""));
        }
        other => panic!("expected update, got {other:?}"),
    }

    match parse(&format!(
        r#"{{"action":"rotate_secret","app_id":"{CANONICAL_APP_ID}"}}"#
    ))
    .expect("rotate")
    {
        AppAdminCommand::RotateSecret { app_id } => {
            assert_eq!(app_id.to_string(), CANONICAL_APP_ID);
        }
        other => panic!("expected rotate_secret, got {other:?}"),
    }

    match parse(&format!(
        r#"{{"action":"enable","app_id":"{CANONICAL_APP_ID}"}}"#
    ))
    .expect("enable")
    {
        AppAdminCommand::Enable { app_id } => {
            assert_eq!(app_id.to_string(), CANONICAL_APP_ID);
        }
        other => panic!("expected enable, got {other:?}"),
    }

    match parse(&format!(
        r#"{{"action":"disable","app_id":"{CANONICAL_APP_ID}"}}"#
    ))
    .expect("disable")
    {
        AppAdminCommand::Disable { app_id } => {
            assert_eq!(app_id.to_string(), CANONICAL_APP_ID);
        }
        other => panic!("expected disable, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_fields_and_unknown_actions() {
    for json in [
        r#"{"action":"create","name":"Buildkite","extra":true}"#,
        &format!(r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}","name":"CI","unknown":1}}"#),
        &format!(r#"{{"action":"rotate_secret","app_id":"{CANONICAL_APP_ID}","name":"x"}}"#),
        &format!(r#"{{"action":"enable","app_id":"{CANONICAL_APP_ID}","status":"active"}}"#),
        &format!(r#"{{"action":"disable","app_id":"{CANONICAL_APP_ID}","force":true}}"#),
        r#"{"action":"delete","app_id":"6eb31227-8ed2-42ec-9024-863497cbeed2"}"#,
        r#"{"action":"create"}"#,
        r#"{}"#,
        "[]",
        "null",
    ] {
        assert!(
            parse(json).is_err(),
            "accepted invalid command payload: {json}"
        );
    }
}

#[test]
fn update_without_mutable_fields_is_rejected() {
    assert!(parse(&format!(
        r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}"}}"#
    ))
    .is_err());
}

#[test]
fn name_is_trimmed_and_bounded_by_unicode_scalars() {
    let trimmed = parse(r#"{"action":"create","name":"  Buildkite  "}"#).expect("trim");
    match trimmed {
        AppAdminCommand::Create { name, .. } => assert_eq!(name, "Buildkite"),
        other => panic!("expected create, got {other:?}"),
    }

    let max = "名".repeat(128);
    parse(&format!(r#"{{"action":"create","name":"{max}"}}"#)).expect("128 scalars");

    let too_long = "名".repeat(129);
    assert!(parse(&format!(r#"{{"action":"create","name":"{too_long}"}}"#)).is_err());

    assert!(parse(r#"{"action":"create","name":""}"#).is_err());
    assert!(parse(r#"{"action":"create","name":"   "}"#).is_err());

    assert!(parse(&format!(
        r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}","name":"   "}}"#
    ))
    .is_err());

    let padded_max = format!(" {max} ");
    match parse(&format!(r#"{{"action":"create","name":"{padded_max}"}}"#))
        .expect("trim then bound")
    {
        AppAdminCommand::Create { name, .. } => assert_eq!(name, max),
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn description_is_bounded_by_unicode_scalars() {
    let max = "é".repeat(2048);
    parse(&format!(
        r#"{{"action":"create","name":"Buildkite","description":"{max}"}}"#
    ))
    .expect("2048 scalars");

    let too_long = "é".repeat(2049);
    assert!(parse(&format!(
        r#"{{"action":"create","name":"Buildkite","description":"{too_long}"}}"#
    ))
    .is_err());

    assert!(parse(&format!(
        r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}","description":"{too_long}"}}"#
    ))
    .is_err());
}

#[test]
fn create_normalizes_empty_optionals_to_absent() {
    match parse(r#"{"action":"create","name":"Buildkite","description":"","icon_url":""}"#)
        .expect("create empty optionals")
    {
        AppAdminCommand::Create {
            description,
            icon_url,
            ..
        } => {
            assert_eq!(description, None);
            assert_eq!(icon_url, None);
        }
        other => panic!("expected create, got {other:?}"),
    }

    match parse(r#"{"action":"create","name":"Buildkite"}"#).expect("create omitted optionals") {
        AppAdminCommand::Create {
            description,
            icon_url,
            ..
        } => {
            assert_eq!(description, None);
            assert_eq!(icon_url, None);
        }
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn update_empty_optionals_clear_fields() {
    match parse(&format!(
        r#"{{"action":"update","app_id":"{CANONICAL_APP_ID}","description":"","icon_url":""}}"#
    ))
    .expect("clear")
    {
        AppAdminCommand::Update {
            name,
            description,
            icon_url,
            ..
        } => {
            assert_eq!(name, None);
            assert_eq!(description.as_deref(), Some(""));
            assert_eq!(icon_url.as_deref(), Some(""));
        }
        other => panic!("expected update, got {other:?}"),
    }
}

#[test]
fn app_id_must_be_canonical_lowercase_hyphenated_uuid() {
    parse_canonical_app_id(CANONICAL_APP_ID).expect("canonical");

    for invalid in [
        "6EB31227-8ED2-42EC-9024-863497CBEED2",
        "6eb312278ed242ec9024863497cbeed2",
        "{6eb31227-8ed2-42ec-9024-863497cbeed2}",
        "urn:uuid:6eb31227-8ed2-42ec-9024-863497cbeed2",
        "6eb31227-8ed2-42ec-9024-863497cbeed",
        "not-a-uuid",
        "",
    ] {
        assert!(
            parse_canonical_app_id(invalid).is_err(),
            "accepted non-canonical app_id: {invalid}"
        );
        assert!(
            parse(&format!(r#"{{"action":"enable","app_id":"{invalid}"}}"#)).is_err(),
            "accepted non-canonical command app_id: {invalid}"
        );
    }
}

#[test]
fn icon_url_accepts_only_safe_http_https_or_data_image() {
    validate_app_icon_url("https://example.test/icon.png").expect("https");
    validate_app_icon_url("http://example.test/icon.png").expect("http");
    validate_app_icon_url("data:image/webp;base64,UklGRg==").expect("data image");
    validate_app_icon_url("").expect("empty clears");

    parse(r#"{"action":"create","name":"Buildkite","icon_url":"https://example.test/icon.png"}"#)
        .expect("https create");
    parse(r#"{"action":"create","name":"Buildkite","icon_url":"data:image/png;base64,AAAA"}"#)
        .expect("data create");

    for unsafe_icon in [
        "javascript:alert(1)",
        "data:text/html;base64,PGI+",
        "ftp://example.test/icon.png",
        "HTTPS://example.test/icon.png",
        "https://example.test/a b.png",
        "https://example.test/a\nb.png",
        "https://example.test/a\tp.png",
    ] {
        assert!(
            validate_app_icon_url(unsafe_icon).is_err(),
            "accepted unsafe icon: {unsafe_icon:?}"
        );
        let json = serde_json::json!({
            "action": "create",
            "name": "Buildkite",
            "icon_url": unsafe_icon,
        });
        assert!(
            parse(&json.to_string()).is_err(),
            "accepted unsafe create icon: {unsafe_icon:?}"
        );
    }

    let https_prefix = "https://example.test/";
    let max_https = format!("{https_prefix}{}", "a".repeat(4096 - https_prefix.len()));
    assert_eq!(max_https.len(), 4096);
    validate_app_icon_url(&max_https).expect("4096-byte https");
    parse(
        &serde_json::json!({"action":"create","name":"Buildkite","icon_url": max_https})
            .to_string(),
    )
    .expect("4096-byte create icon");

    let too_long = format!("{https_prefix}{}", "a".repeat(4097 - https_prefix.len()));
    assert_eq!(too_long.len(), 4097);
    assert!(validate_app_icon_url(&too_long).is_err());
    assert!(parse(
        &serde_json::json!({"action":"create","name":"Buildkite","icon_url": too_long}).to_string()
    )
    .is_err());
}
