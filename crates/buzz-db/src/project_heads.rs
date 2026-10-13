//! Unlimited latest live global parameterized heads (kinds 30617/30621).

use sqlx::{PgConnection, PgPool};

use buzz_core::{CommunityId, StoredEvent};

use buzz_datastore_tracing::datastore_span;

use crate::error::Result;
use crate::event::row_to_stored_event;
use crate::Db;

const LIST_LATEST_PARAMETERIZED_HEADS: &str = r#"
SELECT DISTINCT ON (pubkey, d_tag)
    id, pubkey, created_at, kind, tags, content, sig, received_at, channel_id
FROM events
WHERE community_id = $1
  AND kind = $2
  AND deleted_at IS NULL
  AND channel_id IS NULL
  AND d_tag IS NOT NULL
ORDER BY pubkey, d_tag, created_at DESC, id ASC
"#;

const GET_LATEST_PARAMETERIZED_HEAD: &str = r#"
SELECT id, pubkey, created_at, kind, tags, content, sig, received_at, channel_id
FROM events
WHERE community_id = $1
  AND kind = $2
  AND pubkey = $3
  AND d_tag = $4
  AND deleted_at IS NULL
  AND channel_id IS NULL
ORDER BY created_at DESC, id ASC
LIMIT 1
"#;

/// List latest live global parameterized-replaceable heads of `kind`.
///
/// Unbounded: there is no `LIMIT`. Live means `deleted_at IS NULL`. These
/// kinds are global-only, so rows with a `channel_id` are excluded.
pub async fn list_latest_parameterized_heads(
    pool: &PgPool,
    community_id: CommunityId,
    kind: i32,
) -> Result<Vec<StoredEvent>> {
    let mut conn = pool.acquire().await?;
    list_latest_parameterized_heads_on(&mut conn, community_id, kind).await
}

/// [`list_latest_parameterized_heads`] on a specific connection or transaction.
pub(crate) async fn list_latest_parameterized_heads_on(
    conn: &mut PgConnection,
    community_id: CommunityId,
    kind: i32,
) -> Result<Vec<StoredEvent>> {
    let rows = sqlx::query(LIST_LATEST_PARAMETERIZED_HEADS)
        .bind(community_id.as_uuid())
        .bind(kind)
        .fetch_all(&mut *conn)
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(ev) = row_to_stored_event(row)? {
            out.push(ev);
        }
    }
    Ok(out)
}

/// Fetch the latest live global head for `(kind, pubkey, d_tag)`.
///
/// `d_tag` comparison is case-sensitive. Live means `deleted_at IS NULL`.
pub async fn get_latest_parameterized_head(
    pool: &PgPool,
    community_id: CommunityId,
    kind: i32,
    pubkey: &[u8],
    d_tag: &str,
) -> Result<Option<StoredEvent>> {
    let mut conn = pool.acquire().await?;
    get_latest_parameterized_head_on(&mut conn, community_id, kind, pubkey, d_tag).await
}

/// [`get_latest_parameterized_head`] on a specific connection or transaction.
pub(crate) async fn get_latest_parameterized_head_on(
    conn: &mut PgConnection,
    community_id: CommunityId,
    kind: i32,
    pubkey: &[u8],
    d_tag: &str,
) -> Result<Option<StoredEvent>> {
    let row = sqlx::query(GET_LATEST_PARAMETERIZED_HEAD)
        .bind(community_id.as_uuid())
        .bind(kind)
        .bind(pubkey)
        .bind(d_tag)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(row_to_stored_event).transpose()?.flatten())
}

impl Db {
    /// List latest live global parameterized-replaceable heads of `kind`.
    ///
    /// Unbounded: does not apply [`crate::event::DEFAULT_MAX_PAGE_LIMIT`].
    #[datastore_span(name = "list_latest_parameterized_heads", system = "postgresql")]
    pub async fn list_latest_parameterized_heads(
        &self,
        community_id: CommunityId,
        kind: i32,
    ) -> Result<Vec<StoredEvent>> {
        list_latest_parameterized_heads(&self.pool, community_id, kind).await
    }

    /// Fetch the latest live global head for `(kind, pubkey, d_tag)`.
    #[datastore_span(name = "get_latest_parameterized_head", system = "postgresql")]
    pub async fn get_latest_parameterized_head(
        &self,
        community_id: CommunityId,
        kind: i32,
        pubkey: &[u8],
        d_tag: &str,
    ) -> Result<Option<StoredEvent>> {
        get_latest_parameterized_head(&self.pool, community_id, kind, pubkey, d_tag).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::insert_event;
    use nostr::{EventBuilder, Keys, Kind, Tag};
    use sqlx::PgPool;
    use uuid::Uuid;

    const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz"; // sadscan:disable np.postgres.1

    async fn setup_pool() -> PgPool {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| TEST_DB_URL.to_owned());
        PgPool::connect(&database_url)
            .await
            .expect("connect to test DB")
    }

    async fn make_community(pool: &PgPool) -> CommunityId {
        let id = Uuid::new_v4();
        let host = format!("heads-{}.example", id.simple());
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(id)
            .bind(&host)
            .execute(pool)
            .await
            .expect("insert community");
        CommunityId::from_uuid(id)
    }

    fn parameterized_event(
        kind: u16,
        keys: &Keys,
        d: &str,
        name: &str,
        created_at: u64,
    ) -> nostr::Event {
        EventBuilder::new(Kind::from(kind), "")
            .tags(vec![
                Tag::parse(["d", d]).unwrap(),
                Tag::parse(["name", name]).unwrap(),
            ])
            .custom_created_at(nostr::Timestamp::from(created_at))
            .sign_with_keys(keys)
            .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn list_heads_returns_latest_per_coordinate_without_limit() {
        let pool = setup_pool().await;
        let community = make_community(&pool).await;
        let keys_a = Keys::generate();
        let keys_b = Keys::generate();
        let keys_c = Keys::generate();
        let base = chrono::Utc::now().timestamp() as u64;
        let older = parameterized_event(30617, &keys_a, "agentic-os-plan", "old", base);
        let newer = parameterized_event(30617, &keys_a, "agentic-os-plan", "new", base + 5);
        let harness = parameterized_event(
            30617,
            &keys_b,
            "harness-service",
            "harness-service",
            base + 1,
        );
        let extra = parameterized_event(30617, &keys_c, "extra-repo", "extra-repo", base + 2);
        insert_event(&pool, community, &older, None)
            .await
            .expect("older");
        insert_event(&pool, community, &newer, None)
            .await
            .expect("newer");
        insert_event(&pool, community, &harness, None)
            .await
            .expect("harness");
        insert_event(&pool, community, &extra, None)
            .await
            .expect("extra");

        let heads = list_latest_parameterized_heads(&pool, community, 30617)
            .await
            .expect("list heads");
        let named: Vec<String> = heads
            .iter()
            .filter_map(|h| {
                h.event.tags.iter().find_map(|tag| {
                    let parts = tag.as_slice();
                    (parts.first().map(String::as_str) == Some("name"))
                        .then(|| parts.get(1).cloned())
                        .flatten()
                })
            })
            .collect();
        assert_eq!(heads.len(), 3, "three live coordinates, no page limit");
        assert!(named.contains(&"new".to_string()));
        assert!(!named.contains(&"old".to_string()));
        assert!(named.contains(&"harness-service".to_string()));
        assert!(named.contains(&"extra-repo".to_string()));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn list_heads_returns_more_than_default_query_page_limit() {
        let pool = setup_pool().await;
        for kind in [30617_u16, 30621_u16] {
            let community = make_community(&pool).await;
            let keys = Keys::generate();
            for index in 0..=crate::event::DEFAULT_MAX_PAGE_LIMIT {
                let d = format!("coordinate-{kind}-{index}");
                let event =
                    parameterized_event(kind, &keys, &d, &d, chrono::Utc::now().timestamp() as u64);
                insert_event(&pool, community, &event, None)
                    .await
                    .expect("insert parameterized head");
            }
            let heads = list_latest_parameterized_heads(&pool, community, i32::from(kind))
                .await
                .expect("list all heads");
            assert_eq!(
                heads.len(),
                crate::event::DEFAULT_MAX_PAGE_LIMIT as usize + 1,
                "kind {kind} must not be truncated",
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn list_heads_excludes_deleted_rows() {
        let pool = setup_pool().await;
        let community = make_community(&pool).await;
        let keys = Keys::generate();
        let event = parameterized_event(
            30617,
            &keys,
            "agentic-os-plan",
            "agentic-os-plan",
            chrono::Utc::now().timestamp() as u64,
        );
        insert_event(&pool, community, &event, None)
            .await
            .expect("insert");
        sqlx::query("UPDATE events SET deleted_at = NOW() WHERE community_id = $1 AND id = $2")
            .bind(community.as_uuid())
            .bind(event.id.as_bytes().as_slice())
            .execute(&pool)
            .await
            .expect("soft-delete");
        let heads = list_latest_parameterized_heads(&pool, community, 30617)
            .await
            .expect("list heads");
        assert!(heads.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn get_head_is_case_sensitive_on_d_tag() {
        let pool = setup_pool().await;
        let community = make_community(&pool).await;
        let keys = Keys::generate();
        let event = parameterized_event(
            30617,
            &keys,
            "agentic-os-plan",
            "agentic-os-plan",
            chrono::Utc::now().timestamp() as u64,
        );
        insert_event(&pool, community, &event, None)
            .await
            .expect("insert");
        let pubkey = keys.public_key().to_bytes();
        let found =
            get_latest_parameterized_head(&pool, community, 30617, &pubkey, "agentic-os-plan")
                .await
                .expect("lookup");
        assert!(found.is_some());
        let missing =
            get_latest_parameterized_head(&pool, community, 30617, &pubkey, "Agentic-os-plan")
                .await
                .expect("case mismatch");
        assert!(missing.is_none());
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn admission_destination_reads_do_not_cross_community() {
        let pool = setup_pool().await;
        let db = crate::Db::from_pool(pool.clone());
        let community_a = make_community(&pool).await;
        let community_b = make_community(&pool).await;
        let (workflow_id, _) = crate::workflow::tests::make_workflow_in(&pool, community_a).await;
        let owner = Keys::generate();
        let owner_bytes = owner.public_key().to_bytes();
        db.ensure_user(community_b, &owner_bytes)
            .await
            .expect("ensure owner in community B");
        let shared_channel_id = Uuid::new_v4();
        db.create_channel_with_id(
            community_b,
            shared_channel_id,
            "only-in-b",
            crate::channel::ChannelType::Stream,
            crate::channel::ChannelVisibility::Open,
            None,
            &owner_bytes,
            None,
        )
        .await
        .expect("create channel in community B");

        let begin = crate::workflow_admission::begin_workflow_admission(
            &pool,
            community_a,
            workflow_id,
            &[1_u8; 32],
            &[2_u8; 32],
        )
        .await
        .expect("begin admission in community A");
        let crate::workflow_admission::BeginWorkflowAdmission::Vacant(mut guard) = begin else {
            panic!("fresh admission must be vacant");
        };
        assert!(guard
            .load_destination_channel(shared_channel_id)
            .await
            .expect("load destination")
            .is_none());
        assert!(guard
            .destination_member_role(shared_channel_id, &owner_bytes)
            .await
            .expect("load destination role")
            .is_none());
    }
}
