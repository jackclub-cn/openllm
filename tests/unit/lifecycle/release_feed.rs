use super::*;
use axum::{Router, routing::get};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn release_server(
    body: Value,
    status: axum::http::StatusCode,
) -> (
    String,
    std::sync::Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/release",
        get(move |headers: axum::http::HeaderMap| {
            let count = count.clone();
            let body = body.clone();
            async move {
                assert!(
                    headers
                        .get("user-agent")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("OpenLLM/")
                );
                count.fetch_add(1, Ordering::SeqCst);
                (status, Json(body))
            }
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/release"), requests, task)
}

#[tokio::test]
async fn release_checks_coalesce_and_cache_stable_versions() {
    let (url, count, server) = release_server(
        json!({"tag_name":"v99.0.0","prerelease":false,"draft":false}),
        axum::http::StatusCode::OK,
    )
    .await;
    let cache = crate::release_feed::ReleaseFeed::default();
    let client = reqwest::Client::new();
    let (first, second) = tokio::join!(cache.latest(&client, &url), cache.latest(&client, &url));
    assert_eq!(first.unwrap(), "v99.0.0");
    assert_eq!(second.unwrap(), "v99.0.0");
    assert_eq!(cache.latest(&client, &url).await.unwrap(), "v99.0.0");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn failed_or_prerelease_feeds_are_not_reported_as_latest_stable() {
    for (body, status) in [
        (
            json!({"message":"rate limit"}),
            axum::http::StatusCode::FORBIDDEN,
        ),
        (
            json!({"tag_name":"v99.0.0","prerelease":true}),
            axum::http::StatusCode::OK,
        ),
        (
            json!({"tag_name":"v99.0.0","draft":true}),
            axum::http::StatusCode::OK,
        ),
        (json!({}), axum::http::StatusCode::OK),
    ] {
        let (url, count, server) = release_server(body, status).await;
        let cache = crate::release_feed::ReleaseFeed::default();
        let client = reqwest::Client::new();
        assert!(cache.latest(&client, &url).await.is_err());
        assert!(cache.latest(&client, &url).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }
}

#[tokio::test]
async fn a_different_upgrade_source_does_not_reuse_the_previous_feed_result() {
    let (first_url, _, first_server) =
        release_server(json!({"version":"1.0.0"}), axum::http::StatusCode::OK).await;
    let (second_url, _, second_server) =
        release_server(json!({"version":"2.0.0"}), axum::http::StatusCode::OK).await;
    let cache = crate::release_feed::ReleaseFeed::default();
    let client = reqwest::Client::new();
    assert_eq!(cache.latest(&client, &first_url).await.unwrap(), "1.0.0");
    assert_eq!(cache.latest(&client, &second_url).await.unwrap(), "2.0.0");
    first_server.abort();
    second_server.abort();
}
