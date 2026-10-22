use super::*;
use tower::ServiceExt;

async fn event_test_state(admin_token: Option<&str>) -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, admin_token.map(ToOwned::to_owned))
}

#[tokio::test]
async fn event_stream_uses_a_one_use_ticket_instead_of_the_admin_token() {
    use axum::body::Body;
    use axum::http::Request;

    let state = event_test_state(Some("admin-secret")).await;
    let router = crate::build_router(state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/events/ticket")
                .header("x-admin-token", "admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let ticket: Value = serde_json::from_slice(&body).unwrap();
    let ticket = ticket["ticket"].as_str().unwrap();

    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["content-type"], "text/event-stream");
    drop(first);

    let replay = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        replay.status(),
        StatusCode::UNAUTHORIZED,
        "a consumed event ticket must not open another stream"
    );

    let leaked_query = router
        .oneshot(
            Request::builder()
                .uri("/api/events?admin_token=admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        leaked_query.status(),
        StatusCode::UNAUTHORIZED,
        "the persistent admin token must never authenticate from the URL"
    );
}

#[tokio::test]
async fn management_routes_reject_unlisted_browser_origins() {
    use axum::body::Body;
    use axum::http::Request;

    let state = event_test_state(None).await;
    let router = crate::build_router(state.clone());
    let request = |origin: &str| {
        Request::builder()
            .uri("/api/status")
            .header(header::HOST, "127.0.0.1:8080")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .unwrap()
    };

    let rejected = router
        .clone()
        .oneshot(request("https://evil.example"))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let same_origin = router
        .clone()
        .oneshot(request("http://127.0.0.1:8080"))
        .await
        .unwrap();
    assert_eq!(same_origin.status(), StatusCode::OK);

    let mut configured = state;
    configured.cors_origins = std::sync::Arc::from(vec!["https://console.example".to_string()]);
    let configured_router = crate::build_router(configured);
    let allowed = configured_router
        .oneshot(request("https://console.example"))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://console.example"
    );
}
