use super::*;

#[test]
fn webhook_urls_must_be_http_or_https_with_a_host() {
    assert_eq!(
        validate_webhook_url(" https://example.com/hook ").unwrap(),
        "https://example.com/hook"
    );
    assert!(validate_webhook_url("http://127.0.0.1:9000/hook").is_ok());
    for invalid in [
        "",
        "   ",
        "ftp://example.com",
        "file:///etc/passwd",
        "example.com",
    ] {
        assert!(
            validate_webhook_url(invalid).is_err(),
            "{invalid} should be rejected"
        );
    }
}

#[test]
fn webhook_event_types_are_validated_and_stored_as_json() {
    let (events, stored) = normalize_event_types(None).unwrap();
    assert_eq!(events, vec!["request.completed", "request.failed"]);
    assert_eq!(stored, r#"["request.completed","request.failed"]"#);

    // Duplicates collapse and surrounding whitespace is trimmed.
    let (events, _) = normalize_event_types(Some(vec![
        " request.failed ".to_string(),
        "request.failed".to_string(),
    ]))
    .unwrap();
    assert_eq!(events, vec!["request.failed"]);

    // An unknown event would silently never fire, so it is rejected instead.
    assert!(normalize_event_types(Some(vec!["request.exploded".to_string()])).is_err());
    assert!(normalize_event_types(Some(Vec::new())).is_err());
}

#[test]
fn webhook_headers_are_normalized_and_gateway_headers_are_reserved() {
    let stored = normalize_webhook_headers(Some(&serde_json::json!({
        "Authorization": "Bearer receiver-token",
        "X-Tenant": "team-a",
    })))
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(parsed["authorization"], "Bearer receiver-token");
    assert_eq!(parsed["x-tenant"], "team-a");

    for headers in [
        serde_json::json!({"x-openllm-signature": "forged"}),
        serde_json::json!({"content-type": "text/plain"}),
        serde_json::json!({"X-Test": 1}),
        serde_json::json!(["Authorization", "Bearer"]),
        serde_json::json!({"bad header": "value"}),
    ] {
        assert!(
            normalize_webhook_headers(Some(&headers)).is_err(),
            "{headers} should be rejected"
        );
    }
}

#[test]
fn webhook_event_subscription_matching() {
    let webhook = Webhook {
        id: 1,
        name: "hook".to_string(),
        url: "https://example.com/hook".to_string(),
        secret: String::new(),
        headers: "{}".to_string(),
        event_types: r#"["request.failed"]"#.to_string(),
        enabled: 1,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    assert!(webhook.subscribes_to("request.failed"));
    assert!(!webhook.subscribes_to("request.completed"));

    // A corrupted row falls back to every event rather than going silent.
    let corrupted = Webhook {
        event_types: "not json".to_string(),
        ..webhook.clone()
    };
    assert!(corrupted.subscribes_to("request.completed"));
    assert!(corrupted.subscribes_to("request.failed"));
}

#[test]
fn webhook_signature_is_deterministic_and_covers_the_body() {
    let first = crate::webhooks::signature_header("secret", 1_700_000_000, r#"{"a":1}"#);
    let repeated = crate::webhooks::signature_header("secret", 1_700_000_000, r#"{"a":1}"#);
    assert_eq!(first, repeated);
    assert!(first.starts_with("t=1700000000,v1="));

    // Changing any signed input must change the digest.
    assert_ne!(
        first,
        crate::webhooks::signature_header("secret", 1_700_000_001, r#"{"a":1}"#)
    );
    assert_ne!(
        first,
        crate::webhooks::signature_header("secret", 1_700_000_000, r#"{"a":2}"#)
    );
    assert_ne!(
        first,
        crate::webhooks::signature_header("other", 1_700_000_000, r#"{"a":1}"#)
    );
}

async fn webhook_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

#[tokio::test]
async fn webhook_api_supports_crud_testing_and_delivery_history() {
    use tower::ServiceExt;

    let state = webhook_state().await;
    let (url, mut receiver, server) = spawn_receiver(200).await;
    let router = crate::build_router(state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Audit hook",
                        "url": url,
                        "secret": "topsecret",
                        "headers": {"Authorization": "Bearer receiver-token"},
                        "event_types": ["request.completed"],
                        "enabled": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["secret_set"], true);
    assert_eq!(
        created["headers"]["authorization"],
        serde_json::json!("Bearer receiver-token")
    );
    assert_eq!(
        created["event_types"],
        serde_json::json!(["request.completed"])
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/webhooks/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Failure hook",
                        "event_types": ["request.failed"],
                        "clear_secret": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let updated: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["name"], "Failure hook");
    assert_eq!(updated["secret_set"], false);
    assert_eq!(
        updated["event_types"],
        serde_json::json!(["request.failed"])
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/webhooks/{id}/test"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("webhook test delivery timed out")
        .expect("webhook receiver closed before receiving a request");
    assert_eq!(captured.event, "request.failed");
    assert!(captured.signature.is_empty());
    let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(payload["data"]["test"], true);
    assert_eq!(captured.authorization, "Bearer receiver-token");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/webhooks/{id}/deliveries"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let deliveries: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(deliveries.as_array().unwrap().len(), 1);
    assert_eq!(deliveries[0]["status_code"], 200);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/webhooks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/webhooks/{id}/deliveries"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    server.abort();
}

/// Inserts a completed request so a delivery has a real usage row to describe.
async fn seed_usage_log(state: &AppState, request_id: &str) {
    sqlx::query(
        "INSERT INTO usage_logs (
            request_id, requested_model, upstream_model, endpoint,
            prompt_tokens, completion_tokens, total_tokens, latency_ms,
            status_code, success, streamed, in_flight
         ) VALUES (?, 'model', 'model', '/v1/chat/completions', 10, 5, 15, 120, 200, 1, 0, 0)",
    )
    .bind(request_id)
    .execute(&state.pool)
    .await
    .unwrap();
}

#[derive(Debug)]
struct CapturedRequest {
    signature: String,
    event: String,
    authorization: String,
    body: String,
}

/// Spawns a receiver that reports the request it was sent through a channel.
async fn spawn_receiver(
    status: u16,
) -> (
    String,
    tokio::sync::mpsc::Receiver<CapturedRequest>,
    tokio::task::JoinHandle<()>,
) {
    use axum::Router;
    use axum::routing::post;
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: String| {
            let sender = sender.clone();
            async move {
                let _ = sender
                    .send(CapturedRequest {
                        signature: headers
                            .get("x-openllm-signature")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string(),
                        event: headers
                            .get("x-openllm-event")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string(),
                        authorization: headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string(),
                        body,
                    })
                    .await;
                StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://{address}/hook");
    (url, receiver, server)
}

#[tokio::test]
async fn usage_event_is_delivered_signed_and_recorded() {
    let state = webhook_state().await;
    seed_usage_log(&state, "req-1").await;
    let (url, mut receiver, server) = spawn_receiver(200).await;
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, headers, event_types, enabled)
         VALUES (1, 'audit', ?, 'topsecret',
                 '{\"Authorization\":\"Bearer receiver-token\"}',
                 '[\"request.completed\"]', 1)",
    )
    .bind(&url)
    .execute(&state.pool)
    .await
    .unwrap();

    crate::webhooks::deliver_usage_event(
        &state,
        crate::state::UsageEvent {
            id: 1,
            request_id: "req-1".to_string(),
            success: true,
            streamed: false,
        },
    )
    .await;

    let captured = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("webhook delivery timed out")
        .expect("webhook receiver closed before receiving a request");
    server.abort();

    assert_eq!(captured.event, "request.completed");
    assert_eq!(captured.authorization, "Bearer receiver-token");
    assert!(
        captured.signature.starts_with("t=") && captured.signature.contains(",v1="),
        "unexpected signature header: {}",
        captured.signature
    );
    let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(payload["event"], "request.completed");
    assert_eq!(payload["webhook_id"], 1);
    assert_eq!(payload["data"]["request_id"], "req-1");
    assert_eq!(payload["data"]["success"], true);

    let (status, attempts, error): (Option<i64>, i64, Option<String>) = sqlx::query_as(
        "SELECT status_code, attempts, error FROM webhook_deliveries
         WHERE webhook_id = 1 ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, Some(200), "{error:?}");
    assert_eq!(attempts, 1);
    assert_eq!(error, None);
}

#[tokio::test]
async fn failed_deliveries_are_retried_and_logged() {
    let state = webhook_state().await;
    seed_usage_log(&state, "req-2").await;
    // Nothing is listening on port 1, so every attempt fails at connect time.
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, event_types, enabled)
         VALUES (1, 'broken', 'http://127.0.0.1:1/hook', '', '[\"request.failed\"]', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    crate::webhooks::deliver_usage_event(
        &state,
        crate::state::UsageEvent {
            id: 2,
            request_id: "req-2".to_string(),
            success: false,
            streamed: false,
        },
    )
    .await;

    let (status, attempts, error): (Option<i64>, i64, Option<String>) = sqlx::query_as(
        "SELECT status_code, attempts, error FROM webhook_deliveries
         WHERE webhook_id = 1 ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, None);
    assert_eq!(attempts, 3, "transient failures should be retried");
    assert!(error.is_some());
}

#[tokio::test]
async fn unsubscribed_events_are_not_delivered() {
    let state = webhook_state().await;
    seed_usage_log(&state, "req-3").await;
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, event_types, enabled)
         VALUES (1, 'failures only', 'http://127.0.0.1:1/hook', '', '[\"request.failed\"]', 1),
                (2, 'disabled', 'http://127.0.0.1:1/hook-2', '', '[\"request.completed\"]', 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    crate::webhooks::deliver_usage_event(
        &state,
        crate::state::UsageEvent {
            id: 3,
            request_id: "req-3".to_string(),
            success: true,
            streamed: false,
        },
    )
    .await;

    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(deliveries, 0);
}
