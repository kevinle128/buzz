use buzz_core::kind::KIND_FILE_METADATA;
use nostr::{Event, EventBuilder, Kind, Tag};

use crate::client::{media_url_from_input, normalize_write_response, BuzzClient};
use crate::error::CliError;

const PUBLIC_SHARE_MARKER: &str = "buzz-public";

pub async fn dispatch(cmd: crate::UploadCmd, client: &BuzzClient) -> Result<(), CliError> {
    match cmd {
        crate::UploadCmd::File { file } => {
            let desc = client.upload_file(&file).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&desc).map_err(|e| CliError::Other(e.to_string()))?
            );
            Ok(())
        }
    }
}

pub async fn dispatch_media(cmd: crate::MediaCmd, client: &BuzzClient) -> Result<(), CliError> {
    match cmd {
        crate::MediaCmd::Get { input, output } => {
            let bytes = client.download_media(&input).await?;
            match output.as_deref() {
                Some(path) if path != "-" => {
                    std::fs::write(path, &bytes)
                        .map_err(|e| CliError::Other(format!("could not write {path}: {e}")))?;
                }
                _ => {
                    use std::io::Write;
                    std::io::stdout()
                        .write_all(&bytes)
                        .map_err(|e| CliError::Other(format!("could not write stdout: {e}")))?;
                }
            }
            Ok(())
        }
        crate::MediaCmd::Publish { input } => publish_media(client, &input).await,
        crate::MediaCmd::Unpublish { share_event } => unpublish_media(client, &share_event).await,
    }
}

fn build_public_share_event(media_url: &str, sha256: &str) -> Result<EventBuilder, CliError> {
    let tags = [
        Tag::parse(["url", media_url]).map_err(|e| CliError::Other(e.to_string()))?,
        Tag::parse(["x", sha256]).map_err(|e| CliError::Other(e.to_string()))?,
        Tag::parse(["t", PUBLIC_SHARE_MARKER]).map_err(|e| CliError::Other(e.to_string()))?,
    ];
    Ok(EventBuilder::new(
        Kind::Custom(KIND_FILE_METADATA as u16),
        "Public Buzz media share",
    )
    .tags(tags))
}

fn build_unpublish_event(share_event: &str) -> Result<EventBuilder, CliError> {
    if share_event.len() != 64
        || !share_event
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    {
        return Err(CliError::Usage(
            "share event ID must be 64 lowercase hex characters".into(),
        ));
    }
    let tag = Tag::parse(["e", share_event]).map_err(|e| CliError::Other(e.to_string()))?;
    Ok(EventBuilder::new(Kind::EventDeletion, "Unpublish Buzz media").tag(tag))
}

fn public_share_url(media_url: &str, share_event: &str) -> Result<String, CliError> {
    let mut url = url::Url::parse(media_url)
        .map_err(|e| CliError::Usage(format!("invalid media URL: {e}")))?;
    url.query_pairs_mut()
        .clear()
        .append_pair("share", share_event);
    Ok(url.to_string())
}

fn public_share_event_allows(event: &Event, sha256: &str) -> bool {
    event.kind == Kind::Custom(KIND_FILE_METADATA as u16)
        && event.tags.iter().any(|tag| tag.as_slice() == ["x", sha256])
        && event
            .tags
            .iter()
            .any(|tag| tag.as_slice() == ["t", PUBLIC_SHARE_MARKER])
}

fn canonical_media(client: &BuzzClient, input: &str) -> Result<(String, String), CliError> {
    let media_url = media_url_from_input(client.relay_url(), input)?;
    let mut parsed = url::Url::parse(&media_url)
        .map_err(|e| CliError::Usage(format!("invalid media URL: {e}")))?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    let path = parsed
        .path()
        .strip_prefix("/media/")
        .ok_or_else(|| CliError::Usage("media URL must point at a /media/ path".into()))?;
    let sha256 = path.split('.').next().unwrap_or_default().to_string();
    Ok((parsed.to_string(), sha256))
}

async fn publish_media(client: &BuzzClient, input: &str) -> Result<(), CliError> {
    let (media_url, sha256) = canonical_media(client, input)?;
    client.head_media(&media_url).await?;

    let event = client.sign_event(build_public_share_event(&media_url, &sha256)?).await?;
    let share_event = event.id.to_hex();
    let public_url = public_share_url(&media_url, &share_event)?;
    let raw = client.submit_event(event).await?;
    let mut response: serde_json::Value = serde_json::from_str(&normalize_write_response(&raw))
        .map_err(|e| CliError::Other(format!("relay response is not JSON: {e}")))?;
    if response
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        response["share_event"] = share_event.into();
        response["public_url"] = public_url.into();
    }
    println!("{response}");
    Ok(())
}

async fn unpublish_media(client: &BuzzClient, share_event: &str) -> Result<(), CliError> {
    let builder = build_unpublish_event(share_event)?;
    let raw = client
        .query(&serde_json::json!({
            "ids": [share_event],
            "kinds": [KIND_FILE_METADATA],
            "limit": 1,
        }))
        .await?;
    let shares: Vec<Event> = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("failed to parse share event: {e}")))?;
    let share = shares
        .first()
        .filter(|event| {
            event.pubkey == client.public_key()
                && event
                    .tags
                    .iter()
                    .find(|tag| tag.as_slice().first().map(String::as_str) == Some("x"))
                    .and_then(|tag| tag.as_slice().get(1))
                    .is_some_and(|sha256| public_share_event_allows(event, sha256))
        })
        .ok_or_else(|| {
            CliError::NotFound("public media share not found for this identity".into())
        })?;
    debug_assert_eq!(share.id.to_hex(), share_event);

    let event = client.sign_event(builder).await?;
    let raw = client.submit_event(event).await?;
    println!("{}", normalize_write_response(&raw));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{Keys, Kind};

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn public_share_event_is_nip94_and_hash_scoped() {
        let media_url = format!("https://relay.example/media/{HASH}.png");
        let event = build_public_share_event(&media_url, HASH)
            .expect("share builder")
            .sign_with_keys(&Keys::generate())
            .expect("sign share");

        assert_eq!(event.kind, Kind::from(1063));
        assert!(event.tags.iter().any(|tag| tag.as_slice() == ["x", HASH]));
        assert!(event
            .tags
            .iter()
            .any(|tag| tag.as_slice() == ["t", "buzz-public"]));
        assert!(event
            .tags
            .iter()
            .any(|tag| tag.as_slice() == ["url", media_url.as_str()]));
    }

    #[test]
    fn public_share_url_is_stable_and_carries_event_id() {
        let media_url = format!("https://relay.example/media/{HASH}.png");
        let share_id = "1".repeat(64);

        assert_eq!(
            public_share_url(&media_url, &share_id).expect("public URL"),
            format!("{media_url}?share={share_id}")
        );
    }

    #[test]
    fn unpublish_event_targets_only_the_share_event() {
        let share_id = "2".repeat(64);
        let event = build_unpublish_event(&share_id)
            .expect("unpublish builder")
            .sign_with_keys(&Keys::generate())
            .expect("sign deletion");

        assert_eq!(event.kind, Kind::EventDeletion);
        assert!(event
            .tags
            .iter()
            .any(|tag| tag.as_slice() == ["e", share_id.as_str()]));
        assert_eq!(
            event
                .tags
                .iter()
                .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("e"))
                .count(),
            1
        );
        assert!(event.verify().is_ok());
    }
}
