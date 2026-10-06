use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::Response,
    Router,
};
use buzz_core::tenant::{CommunityId, TenantContext};
use buzz_media::storage::BlobMeta;
use buzz_media::{MediaConfig, MediaStorage, ObjectVersionRef};
use futures_util::StreamExt;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

type Requests = Vec<(Method, String, String, String)>;
type Responses = VecDeque<(StatusCode, String)>;

#[derive(Clone)]
struct Peer(Arc<Mutex<(Requests, Responses)>>);

async fn respond(State(peer): State<Peer>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let range = req
        .headers()
        .get("range")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = String::from_utf8(
        to_bytes(req.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let mut state = peer.0.lock().unwrap();
    state.0.push((method, uri, range, body));
    let (status, body) = state.1.pop_front().expect("unexpected S3 request");
    Response::builder()
        .status(status)
        .header("content-length", body.len())
        .body(Body::from(body))
        .unwrap()
}

fn config(endpoint: &str, prefix: &str) -> MediaConfig {
    serde_json::from_value(serde_json::json!({
        "s3_endpoint": endpoint, "s3_access_key": "test", "s3_secret_key": "test",
        "s3_bucket": "shared", "s3_prefix": prefix, "max_image_bytes": 10,
        "max_gif_bytes": 10, "public_base_url": "http://localhost/media"
    }))
    .unwrap()
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn server(responses: Vec<(StatusCode, String)>) -> (String, Peer, Server) {
    let peer = Peer(Arc::new(Mutex::new((Vec::new(), responses.into()))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(respond).with_state(peer.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, peer, Server(task))
}
fn ok(body: &str) -> (StatusCode, String) {
    (StatusCode::OK, body.to_string())
}

#[tokio::test]
async fn root_prefix_scopes_all_object_operations_and_sidecars() {
    let (endpoint, peer, _server) = server(vec![
        ok(""),
        ok("blob"),
        (StatusCode::PARTIAL_CONTENT, "lo".into()),
        ok("blob"),
        ok("blob"),
        ok("blob"),
        ok(""),
        ok(""),
        ok(&serde_json::to_string(&BlobMeta::default()).unwrap()),
        ok(""),
        (StatusCode::NOT_FOUND, "".into()),
        (StatusCode::NOT_FOUND, "".into()),
        (StatusCode::NOT_FOUND, "".into()),
        (StatusCode::NOT_FOUND, "".into()),
    ])
    .await;
    let storage = MediaStorage::new(&config(&endpoint, "buzz-media")).unwrap();
    storage.put("hash.png", b"blob", "image/png").await.unwrap();
    assert_eq!(storage.get("hash.png").await.unwrap(), b"blob");
    assert_eq!(storage.get_range("hash.png", 1, 2).await.unwrap(), b"lo");
    let mut stream = storage.get_stream("hash.png").await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap().as_ref(), b"blob");
    assert!(storage.head("hash.png").await.unwrap());
    assert_eq!(
        storage
            .head_with_metadata("hash.png")
            .await
            .unwrap()
            .unwrap()
            .size,
        4
    );
    storage.delete("hash.png").await.unwrap();
    let ctx = TenantContext::resolved(
        CommunityId::from_uuid(uuid::Uuid::from_u128(1)),
        "test.invalid",
    );
    storage
        .put_sidecar(&ctx, "hash", &BlobMeta::default())
        .await
        .unwrap();
    storage.get_sidecar(&ctx, "hash").await.unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"blob").unwrap();
    storage
        .put_file("hash.png", file.path(), "image/png")
        .await
        .unwrap();
    assert!(storage.get("missing").await.is_err());
    let requests = &peer.0.lock().unwrap().0;
    assert!(requests.len() >= 11);
    for request in &requests[10..] {
        assert_eq!(
            request.1, "/shared/buzz-media/missing",
            "no bucket-root fallback"
        );
    }
    for (_, uri, _, _) in requests {
        assert!(uri.starts_with("/shared/buzz-media/"), "{uri}");
    }
    assert_eq!(requests[2].2, "bytes=1-2");
    assert!(requests[7]
        .1
        .contains("/_meta/00000000-0000-0000-0000-000000000001/hash.json"));
    assert_eq!(requests[0].3, "blob");
}

#[tokio::test]
async fn listings_and_bulk_deletes_keep_logical_keys_and_opaque_tokens() {
    let listing = "<ListBucketResult><Name>shared</Name><IsTruncated>true</IsTruncated><NextContinuationToken>opaque/+==</NextContinuationToken><Contents><Key>buzz-media/_meta/tenant/a</Key><LastModified>2026-10-06T00:00:00Z</LastModified><ETag>x</ETag><Size>3</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>";
    let versions = "<ListVersionsResult><IsTruncated>true</IsTruncated><NextKeyMarker>buzz-media/_meta/tenant/a</NextKeyMarker><NextVersionIdMarker>version/+==</NextVersionIdMarker><Version><Key>buzz-media/_meta/tenant/a</Key><VersionId>v1</VersionId><Size>3</Size></Version><DeleteMarker><Key>buzz-media/_meta/tenant/a</Key><VersionId>v0</VersionId></DeleteMarker></ListVersionsResult>";
    let deleted = "<DeleteResult><Deleted><Key>buzz-media/_meta/tenant/a</Key><DeleteMarker>true</DeleteMarker></Deleted><Error><Key>buzz-media/_meta/tenant/b</Key><Code>AccessDenied</Code><Message>denied</Message></Error></DeleteResult>";
    let (endpoint, peer, _server) = server(vec![
        ok(listing),
        ok(listing),
        ok(versions),
        ok(deleted),
        ok(deleted),
    ])
    .await;
    let storage = MediaStorage::new(&config(&endpoint, "buzz-media/")).unwrap();
    let page = storage
        .list_page(Some("opaque/+==".into()), 2)
        .await
        .unwrap();
    assert_eq!(page.objects, vec![("_meta/tenant/a".into(), 3)]);
    assert_eq!(page.next_continuation_token.as_deref(), Some("opaque/+=="));
    storage
        .list_prefix_page("_meta/tenant/", None, 2)
        .await
        .unwrap();
    let versions = storage
        .list_prefix_versions_page(
            "_meta/tenant/",
            Some("_meta/tenant/a".into()),
            Some("version/+==".into()),
            2,
        )
        .await
        .unwrap();
    assert_eq!(versions.next_key_marker.as_deref(), Some("_meta/tenant/a"));
    assert_eq!(
        versions.next_version_id_marker.as_deref(),
        Some("version/+==")
    );
    assert_eq!(versions.entries[0].key, "_meta/tenant/a");
    assert_eq!(versions.entries[1].version_id, "v0");
    let outcome = storage
        .delete_objects(&["_meta/tenant/a".into(), "_meta/tenant/b".into()])
        .await
        .unwrap();
    assert_eq!(outcome.versioned_keys, vec!["_meta/tenant/a"]);
    assert_eq!(outcome.failed[0].0, "_meta/tenant/b");
    let outcome = storage
        .delete_object_versions(&[ObjectVersionRef {
            key: "_meta/tenant/a".into(),
            version_id: "v1".into(),
        }])
        .await
        .unwrap();
    assert_eq!(outcome.deleted, 1);
    assert_eq!(outcome.failed[0].0, "_meta/tenant/b");
    let requests = &peer.0.lock().unwrap().0;
    assert!(
        requests[0].1.contains("prefix=buzz-media%2F"),
        "{}",
        requests[0].1
    );
    assert!(
        requests[0]
            .1
            .contains("continuation-token=opaque%2F%2B%3D%3D"),
        "{}",
        requests[0].1
    );
    assert!(
        requests[1]
            .1
            .contains("prefix=buzz-media%2F_meta%2Ftenant%2F"),
        "{}",
        requests[1].1
    );
    assert!(
        requests[2]
            .1
            .contains("key-marker=buzz-media%2F_meta%2Ftenant%2Fa"),
        "{}",
        requests[2].1
    );
    assert!(
        requests[2]
            .1
            .contains("version-id-marker=version%2F%2B%3D%3D"),
        "{}",
        requests[2].1
    );
    assert!(requests[3]
        .3
        .contains("<Key>buzz-media/_meta/tenant/a</Key>"));
    assert!(requests[4].3.contains("<VersionId>v1</VersionId>"));
    assert!(requests[4]
        .3
        .contains("<Key>buzz-media/_meta/tenant/a</Key>"));
}

#[tokio::test]
async fn empty_prefix_keeps_bucket_root_compatible() {
    let (endpoint, peer, _server) = server(vec![ok("blob")]).await;
    assert_eq!(
        MediaStorage::new(&config(&endpoint, ""))
            .unwrap()
            .get("hash.png")
            .await
            .unwrap(),
        b"blob"
    );
    assert_eq!(peer.0.lock().unwrap().0[0].1, "/shared/hash.png");
}

#[test]
fn unsafe_root_prefix_is_rejected_at_startup() {
    for prefix in [
        "/root",
        "../root",
        "root/../other",
        "root//other",
        "root\\other",
        "root/./other",
        " root",
        "root ",
        "root?query",
        "root#fragment",
        "root%2Fother",
        "/",
    ] {
        let config = config("http://localhost:9000", prefix);
        assert!(config.validate().is_err(), "accepted {prefix:?}");
        assert!(
            MediaStorage::new(&config).is_err(),
            "constructor accepted {prefix:?}"
        );
    }
}

#[tokio::test]
async fn foreign_s3_response_keys_fail_closed_and_empty_version_markers_stay_empty() {
    let listing = "<ListBucketResult><Name>shared</Name><Contents><Key>other/file</Key><LastModified>2026-10-06</LastModified><Size>3</Size></Contents></ListBucketResult>";
    let version = "<ListVersionsResult><Version><Key>other/file</Key><VersionId>v1</VersionId></Version></ListVersionsResult>";
    let deleted = "<DeleteResult><Error><Key>other/file</Key><Code>AccessDenied</Code><Message>denied</Message></Error></DeleteResult>";
    let empty_marker = "<ListVersionsResult><IsTruncated>false</IsTruncated><NextKeyMarker/><NextVersionIdMarker/></ListVersionsResult>";
    let (endpoint, _, _server) = server(vec![
        ok(listing),
        ok(version),
        ok(deleted),
        ok(empty_marker),
    ])
    .await;
    let storage = MediaStorage::new(&config(&endpoint, "buzz-media")).unwrap();
    assert!(storage.list_page(None, 2).await.is_err());
    assert!(storage
        .list_prefix_versions_page("", None, None, 2)
        .await
        .is_err());
    assert!(storage.delete_objects(&["file".into()]).await.is_err());
    let page = storage
        .list_prefix_versions_page("", None, None, 2)
        .await
        .unwrap();
    assert!(!page.is_truncated);
    assert_eq!(page.next_key_marker.as_deref(), Some(""));
    assert_eq!(page.next_version_id_marker.as_deref(), Some(""));
}
