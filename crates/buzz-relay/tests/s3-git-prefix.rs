use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    response::Response,
    Router,
};
use buzz_relay::api::git::store::{CasOutcome, GitStore, Precond};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(String, String, String, Vec<u8>)>>>;
async fn s3_peer(State(requests): State<Requests>, req: Request) -> Response {
    let uri = req.uri().to_string();
    let condition = req
        .headers()
        .get("if-none-match")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let method = req.method().to_string();
    let body = to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    requests
        .lock()
        .unwrap()
        .push((method, uri, condition, body));
    Response::builder()
        .header("etag", "\"v1\"")
        .header("content-length", 4)
        .body(Body::from("blob"))
        .unwrap()
}

#[tokio::test]
async fn git_store_uses_media_root_without_changing_cas_keys_or_headers() {
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(s3_peer).with_state(requests.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config: buzz_media::MediaConfig = serde_json::from_value(serde_json::json!({
        "s3_endpoint": endpoint, "s3_access_key":"test", "s3_secret_key":"test", "s3_bucket":"shared",
        "s3_prefix":"buzz-media", "max_image_bytes":10, "max_gif_bytes":10, "public_base_url":"http://localhost/media"
    })).unwrap();
    let store = GitStore::from_config(&config).unwrap();
    let pack = store.put_pack(b"blob").await.unwrap();
    assert_eq!(pack, GitStore::content_key("packs", b"blob"));
    assert_eq!(store.get(&pack).await.unwrap().as_ref(), b"blob");
    assert_eq!(
        store.get_limited(&pack, 10).await.unwrap().as_ref(),
        b"blob"
    );
    let digest = pack.strip_prefix("packs/").unwrap();
    assert_eq!(
        store.put_idx(digest, b"blob").await.unwrap(),
        format!("idx/{digest}")
    );
    assert_eq!(
        store.get_idx(digest, 10).await.unwrap().unwrap().as_ref(),
        b"blob"
    );
    let pointer = "refs/tenant/repo/pointer";
    assert!(matches!(
        store
            .put_pointer(pointer, b"blob", Precond::IfNoneMatchStar)
            .await
            .unwrap(),
        CasOutcome::Won(_)
    ));
    let (etag, body) = store.get_pointer(pointer).await.unwrap().unwrap();
    assert_eq!(etag.0, "\"v1\"");
    assert_eq!(body.as_ref(), b"blob");
    server.abort();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    for (_, uri, _, _) in requests.iter() {
        assert!(uri.starts_with("/shared/buzz-media/"), "{uri}");
    }
    assert_eq!(requests[0].2, "*");
    assert_eq!(requests[4].2, "*");
    assert_eq!(requests[7].2, "*");
}
