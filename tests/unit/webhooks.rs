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

    // Circuit events are opt-in and do not silently expand the default set.
    let (events, _) =
        normalize_event_types(Some(vec!["provider.circuit_open".to_string()])).unwrap();
    assert_eq!(events, vec!["provider.circuit_open"]);
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
    let webhook = test_webhook("generic", "{}", r#"["request.failed"]"#);
    assert!(webhook.subscribes_to("request.failed"));
    assert!(!webhook.subscribes_to("request.completed"));

    // A corrupted row falls back to every event rather than going silent.
    let corrupted = Webhook {
        event_types: "not json".to_string(),
        ..webhook.clone()
    };
    assert!(corrupted.subscribes_to("request.completed"));
    assert!(corrupted.subscribes_to("request.failed"));
    assert!(corrupted.subscribes_to("provider.circuit_open"));
    assert!(corrupted.subscribes_to("provider.circuit_closed"));
}

/// A webhook row as the delivery path sees it, for tests that only care about
/// how the body is rendered.
fn test_webhook(kind: &str, body_fields: &str, event_types: &str) -> Webhook {
    Webhook {
        id: 7,
        name: "alerts".to_string(),
        url: "https://example.com/hook".to_string(),
        secret: String::new(),
        headers: "{}".to_string(),
        kind: kind.to_string(),
        body_fields: body_fields.to_string(),
        event_types: event_types.to_string(),
        enabled: 1,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

#[test]
fn webhook_channel_kinds_and_body_fields_are_validated() {
    assert_eq!(normalize_channel_kind(None).unwrap(), "generic");
    assert_eq!(normalize_channel_kind(Some(" slack ")).unwrap(), "slack");
    assert_eq!(normalize_channel_kind(Some("")).unwrap(), "generic");
    for invalid in ["teams", "slack2", "webhook"] {
        assert!(
            normalize_channel_kind(Some(invalid)).is_err(),
            "{invalid} should be rejected"
        );
    }

    assert_eq!(normalize_body_fields("generic", None).unwrap(), "{}");
    assert_eq!(
        normalize_body_fields("generic", Some(&serde_json::json!(null))).unwrap(),
        "{}"
    );
    for invalid in [
        serde_json::json!(["a"]),
        serde_json::json!("text"),
        serde_json::json!({"": "blank key"}),
    ] {
        assert!(
            normalize_body_fields("slack", Some(&invalid)).is_err(),
            "{invalid} should be rejected"
        );
    }

    // Telegram addresses the chat in the body, so a missing chat id is caught
    // while saving rather than on every alert that fails to deliver.
    assert!(normalize_body_fields("telegram", Some(&serde_json::json!({}))).is_err());
    assert!(
        normalize_body_fields("telegram", Some(&serde_json::json!({"chat_id": "  "}))).is_err(),
        "a blank chat id is as unusable as a missing one"
    );
    let stored = normalize_body_fields(
        "telegram",
        Some(&serde_json::json!({"chat_id": 42})),
    )
    .unwrap();
    assert_eq!(stored, r#"{"chat_id":42}"#);
}

/// A chat channel must receive its own body shape, while the generic kind keeps
/// the envelope every existing integration is written against.
#[test]
fn webhook_channel_kinds_render_the_body_each_platform_expects() {
    let base = test_webhook("generic", "{}", r#"["provider.circuit_open"]"#);
    let data = serde_json::json!({
        "provider_id": 2,
        "provider_name": "DeepSeek",
        "state": "open",
        "reason": "failure_threshold",
        "failure_streak": 3,
        "cooldown_seconds": 80
    });

    let generic: serde_json::Value =
        serde_json::from_str(&crate::webhooks::delivery_body(
            &base,
            "provider.circuit_open",
            &data,
        ))
        .unwrap();
    assert_eq!(generic["event"], "provider.circuit_open");
    assert_eq!(generic["webhook_id"], 7);
    assert_eq!(generic["data"], data);

    let rendered = |kind: &str, body_fields: &str| -> serde_json::Value {
        let webhook = Webhook {
            kind: kind.to_string(),
            body_fields: body_fields.to_string(),
            ..base.clone()
        };
        serde_json::from_str(&crate::webhooks::delivery_body(
            &webhook,
            "provider.circuit_open",
            &data,
        ))
        .unwrap()
    };

    let slack = rendered("slack", r##"{"channel":"#ops"}"##);
    assert!(slack.get("event").is_none(), "chat channels get no envelope");
    assert_eq!(slack["channel"], "#ops");
    let text = slack["text"].as_str().unwrap();
    assert!(text.contains("provider circuit OPEN: DeepSeek"), "{text}");
    assert!(text.contains("reason: failure_threshold"), "{text}");
    assert!(text.contains("consecutive failures: 3"), "{text}");
    assert!(text.contains("cooldown (s): 80"), "{text}");

    assert_eq!(
        rendered("discord", "{}")["content"],
        serde_json::json!(text)
    );
    assert_eq!(
        rendered("telegram", r#"{"chat_id":"-100200"}"#)["chat_id"],
        "-100200"
    );
    assert_eq!(
        rendered("feishu", "{}")["content"]["text"],
        serde_json::json!(text)
    );
    assert_eq!(
        rendered("dingtalk", "{}")["text"]["content"],
        serde_json::json!(text)
    );

    // An explicit body field never replaces a key the renderer owns.
    assert_eq!(
        rendered("slack", r#"{"text":"hijacked"}"#)["text"],
        serde_json::json!(text)
    );
}

#[test]
fn webhook_alert_messages_stay_short_and_readable() {
    // A missing field must not leave a dangling "label: " line behind.
    let sparse = serde_json::json!({"provider_name": "alpha"});
    let text = crate::webhooks::delivery_body(
        &test_webhook("slack", "{}", r#"["provider.circuit_closed"]"#),
        "provider.circuit_closed",
        &sparse,
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    let message = parsed["text"].as_str().unwrap();
    assert_eq!(message, "[openllm] provider circuit CLOSED: alpha");

    // An unknown field falls back to a labelled headline instead of an empty
    // message a channel would reject.
    let other = serde_json::json!({"provider_name": "alpha"});
    let fallback = crate::webhooks::delivery_body(
        &test_webhook("slack", "{}", "[]"),
        "provider.unknown",
        &other,
    );
    let parsed: serde_json::Value = serde_json::from_str(&fallback).unwrap();
    assert_eq!(parsed["text"], "[openllm] provider.unknown");
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

/// The kind chosen through the API is what the delivery path renders, so a
/// channel configured from the console receives a message it can actually read.
#[tokio::test]
async fn a_channel_created_through_the_api_delivers_a_chat_message() {
    use tower::ServiceExt;

    let state = webhook_state().await;
    seed_usage_log(&state, "req-channel").await;
    let (url, mut receiver, server) = spawn_receiver(200).await;
    let router = crate::build_router(state.clone());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Ops channel",
                        "url": url,
                        "kind": "slack",
                        "body_fields": {"channel": "#ops"},
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
    assert_eq!(created["kind"], "slack");
    assert_eq!(
        created["body_fields"],
        serde_json::json!({"channel": "#ops"})
    );

    // A kind that cannot deliver without an extra field is refused while
    // saving instead of failing every alert afterwards.
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Telegram",
                        "url": "https://api.telegram.org/botTOKEN/sendMessage",
                        "kind": "telegram",
                        "body_fields": {},
                        "event_types": ["provider.circuit_open"],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    crate::webhooks::deliver_usage_event(
        &state,
        crate::state::UsageEvent {
            id: 1,
            request_id: "req-channel".to_string(),
            success: true,
            streamed: false,
        },
    )
    .await;

    let captured = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("channel delivery timed out")
        .expect("webhook receiver closed before receiving a request");
    server.abort();

    let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert!(
        payload.get("event").is_none(),
        "a chat channel must not receive the gateway envelope: {payload}"
    );
    assert_eq!(payload["channel"], "#ops");
    let text = payload["text"].as_str().unwrap();
    assert!(text.contains("[openllm] request completed: HTTP 200"), "{text}");
    assert!(text.contains("request_id: req-channel"), "{text}");
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
async fn provider_circuit_event_is_delivered_and_recorded() {
    let state = webhook_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let (url, mut receiver, server) = spawn_receiver(200).await;
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, event_types, enabled)
         VALUES (1, 'circuit alerts', ?, '', '[\"provider.circuit_open\"]', 1)",
    )
    .bind(&url)
    .execute(&state.pool)
    .await
    .unwrap();

    crate::webhooks::deliver_circuit_event(
        &state,
        crate::state::ProviderCircuitEvent {
            provider_id: 1,
            state: "open",
            reason: "failure_threshold",
            failure_streak: 3,
            cooldown_seconds: 60,
        },
    )
    .await;

    let captured = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("circuit webhook delivery timed out")
        .expect("webhook receiver closed before receiving a request");
    server.abort();

    assert_eq!(captured.event, "provider.circuit_open");
    let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(payload["event"], "provider.circuit_open");
    assert_eq!(payload["webhook_id"], 1);
    assert_eq!(payload["data"]["provider_id"], 1);
    assert_eq!(payload["data"]["provider_name"], "alpha");
    assert_eq!(payload["data"]["state"], "open");
    assert_eq!(payload["data"]["reason"], "failure_threshold");
    assert_eq!(payload["data"]["failure_streak"], 3);
    assert_eq!(payload["data"]["cooldown_seconds"], 60);

    let (request_id, status): (Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT request_id, status_code FROM webhook_deliveries
         WHERE webhook_id = 1 ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(request_id, None);
    assert_eq!(status, Some(200));
}

#[tokio::test]
async fn dispatcher_delivers_provider_circuit_events() {
    let state = webhook_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let (url, mut receiver, server) = spawn_receiver(200).await;
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, secret, event_types, enabled)
         VALUES (1, 'circuit alerts', ?, '', '[\"provider.circuit_closed\"]', 1)",
    )
    .bind(&url)
    .execute(&state.pool)
    .await
    .unwrap();

    let dispatcher = tokio::spawn(crate::webhooks::run_dispatcher(state.clone()));
    for _ in 0..50 {
        if state.circuit_events.receiver_count() > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        state.circuit_events.receiver_count() > 0,
        "webhook dispatcher did not subscribe to circuit events"
    );
    state
        .circuit_events
        .send(crate::state::ProviderCircuitEvent {
            provider_id: 1,
            state: "closed",
            reason: "successful_request",
            failure_streak: 0,
            cooldown_seconds: 0,
        })
        .unwrap();

    let captured = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("dispatcher circuit webhook timed out")
        .expect("webhook receiver closed before receiving a request");
    server.abort();
    dispatcher.abort();

    assert_eq!(captured.event, "provider.circuit_closed");
    let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(payload["data"]["provider_name"], "alpha");
    assert_eq!(payload["data"]["state"], "closed");
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
