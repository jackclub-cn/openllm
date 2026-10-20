use super::*;

/// A 2xx whose body is not JSON is a broken upstream, not a successful turn.
///
/// Before this was checked, two things went wrong at once: the client received
/// an HTML error page under a success status, and the caller marked the
/// provider healthy on the strength of it, clearing the circuit that should
/// have routed around the provider instead.
#[tokio::test]
async fn non_json_success_body_falls_back_and_cools_the_broken_target() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let broken_attempts = Arc::new(AtomicUsize::new(0));
    let broken = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let broken_attempts = broken_attempts.clone();
            move || {
                let broken_attempts = broken_attempts.clone();
                async move {
                    broken_attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        [("content-type", "text/html")],
                        "<html><body>502 Bad Gateway</body></html>",
                    )
                        .into_response()
                }
            }
        }),
    );
    let healthy = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "id": "chatcmpl-non-json-fallback",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                })),
            )
                .into_response()
        }),
    );
    let broken_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let broken_address = broken_listener.local_addr().unwrap();
    let healthy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let broken_server = tokio::spawn(async move {
        axum::serve(broken_listener, broken).await.unwrap();
    });
    let healthy_server = tokio::spawn(async move {
        axum::serve(healthy_listener, healthy).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'broken', 'openai', ?),
            (2, 'healthy', 'openai', ?)",
    )
    .bind(format!("http://{broken_address}/v1"))
    .bind(format!("http://{healthy_address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'non-json route', 'non-json-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'non-json-model', 0),
            (1, 2, 'non-json-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('resilience', ?)")
        .bind(
            json!({
                "max_retries": 0,
                "retry_backoff_ms": 1,
                "retry_max_backoff_ms": 5
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "non-json-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "non-json-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        broken_attempts.load(Ordering::SeqCst),
        1,
        "the broken target is tried once before the fallback"
    );
    assert_eq!(response.headers()["x-openllm-routed-provider"], "healthy");
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "non-json-model".to_string())),
        "the broken model must be cooled so the next request starts elsewhere"
    );
    assert!(
        !state.provider_failure_streak.lock().await.contains_key(&1),
        "a single broken model must not open the provider-wide circuit"
    );

    broken_server.abort();
    healthy_server.abort();
}

/// With no healthy sibling, the client gets a clean gateway error instead of
/// the upstream's non-JSON body echoed back under a success status.
#[tokio::test]
async fn a_non_json_success_body_is_never_forwarded_to_the_client() {
    let broken = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            (
                StatusCode::OK,
                [("content-type", "text/html")],
                "<html><body>502 Bad Gateway</body></html>",
            )
                .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, broken).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query("INSERT INTO providers (id, name, provider_type, base_url) VALUES (1, 'broken', 'openai', ?)")
        .bind(format!("http://{address}/v1"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'only route', 'only-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'only-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "only-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    // The fallback loop reports a total failure as an error, which the HTTP
    // layer renders; it must never hand back the upstream's body as a success.
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "only-broken-request",
        None,
    )
    .await
    .expect_err("a non-JSON 200 must not be reported as success");

    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    // The upstream document is never forwarded as the response; the client gets
    // the gateway's own error envelope, with the body quoted only as a bounded
    // debugging snippet inside the message.
    let envelope: Value = serde_json::from_str(&body).expect("gateway error must be JSON");
    assert_eq!(envelope["error"]["type"], "upstream_error");
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not JSON"),
        "the error must explain what the upstream returned: {message}"
    );

    server.abort();
}
