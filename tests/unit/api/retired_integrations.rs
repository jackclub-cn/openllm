use super::*;

#[tokio::test]
async fn retired_webhook_endpoints_return_not_found_without_changing_legacy_data() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, enabled) \
         VALUES (1, 'legacy-hook', 'http://127.0.0.1:1/hook', 'legacy-secret', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let router = crate::build_router(state.clone());

    for (method, uri) in [
        ("GET", "/api/webhooks"),
        ("POST", "/api/webhooks"),
        ("PUT", "/api/webhooks/1"),
        ("DELETE", "/api/webhooks/1"),
        ("POST", "/api/webhooks/1/test"),
        ("GET", "/api/webhooks/1/deliveries"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"new-hook","url":"http://127.0.0.1:1/new"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }

    let retained: (String, String) =
        sqlx::query_as("SELECT name, secret FROM webhooks WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        retained,
        ("legacy-hook".to_string(), "legacy-secret".to_string())
    );
    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(deliveries, 0);
}
