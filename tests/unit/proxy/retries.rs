use super::*;

#[tokio::test]
async fn transient_5xx_is_retried_on_the_same_target_before_falling_back() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": {"message": "upstream overloaded"}})),
                        )
                            .into_response()
                    } else {
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "chatcmpl-retry",
                                "object": "chat.completion",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "recovered"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 1,
                                    "total_tokens": 4
                                }
                            })),
                        )
                            .into_response()
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'flaky', 'openai', ?),
            (2, 'never-used', 'openai', 'http://127.0.0.1:9/v1')",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'retry route', 'retry-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'retry-model', 0),
            (1, 2, 'retry-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Keep the retry fast so the test stays quick.
    sqlx::query("INSERT INTO settings (key, value) VALUES ('resilience', ?)")
        .bind(
            json!({
                "max_retries": 1,
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
            "model": "retry-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "retry-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // The retry stayed on the first target, so no fallback was recorded.
    assert_eq!(response.headers()["x-openllm-fallback-count"], "0");
    assert_eq!(response.headers()["x-openllm-routed-provider"], "flaky");
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&1),
        "a recovered retry must not leave the provider cooling down"
    );

    server.abort();
}

#[tokio::test]
async fn retries_can_be_disabled_and_then_fall_back() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let flaky_attempts = Arc::new(AtomicUsize::new(0));
    let flaky = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let flaky_attempts = flaky_attempts.clone();
            move || {
                let flaky_attempts = flaky_attempts.clone();
                async move {
                    flaky_attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": {"message": "down"}})),
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
                    "id": "chatcmpl-fallback",
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
    let flaky_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let flaky_address = flaky_listener.local_addr().unwrap();
    let healthy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let flaky_server = tokio::spawn(async move {
        axum::serve(flaky_listener, flaky).await.unwrap();
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
            (1, 'flaky', 'openai', ?),
            (2, 'healthy', 'openai', ?)",
    )
    .bind(format!("http://{flaky_address}/v1"))
    .bind(format!("http://{healthy_address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'no-retry route', 'no-retry-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'no-retry-model', 0),
            (1, 2, 'no-retry-model', 1)",
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
            "model": "no-retry-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "no-retry-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        flaky_attempts.load(Ordering::SeqCst),
        1,
        "disabling retries must send exactly one request to the failing target"
    );
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    assert_eq!(response.headers()["x-openllm-routed-provider"], "healthy");

    let mut trace = None;
    for _ in 0..50 {
        trace = sqlx::query_as::<_, (String, i64, i64, i64)>(
            "SELECT candidates_json, attempt_count, fallback_count, success
             FROM routing_traces WHERE request_id = 'no-retry-request'",
        )
        .fetch_optional(&state.pool)
        .await
        .unwrap();
        if trace.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (candidates_json, attempt_count, fallback_count, success) =
        trace.expect("the completed request should persist a routing trace");
    assert_eq!(attempt_count, 2);
    assert_eq!(fallback_count, 1);
    assert_eq!(success, 1);
    assert!(candidates_json.contains("\"deprioritized\":false"));
    assert!(
        !candidates_json.contains("hello"),
        "routing traces must not contain request bodies"
    );
    let attempts = sqlx::query_as::<_, (i64, Option<i64>, i64, String)>(
        "SELECT attempt_index, status_code, success, reason_code
         FROM routing_attempts WHERE request_id = 'no-retry-request'
         ORDER BY attempt_index",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        attempts,
        vec![
            (
                1,
                Some(StatusCode::BAD_GATEWAY.as_u16() as i64),
                0,
                "upstream_unavailable".to_string()
            ),
            (
                2,
                Some(StatusCode::OK.as_u16() as i64),
                1,
                "upstream_success".to_string()
            )
        ]
    );

    flaky_server.abort();
    healthy_server.abort();
}

#[tokio::test]
async fn unreadable_resilience_settings_do_not_break_the_data_path() {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async { StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // No migrations: the `settings` table does not exist, which stands in for a
    // corrupt or unavailable policy store.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.base_url = format!("http://{address}");

    let url = format!("http://{address}/v1/chat/completions");
    let response = send_provider_request(&state, &target, false, || Ok(state.client.post(&url)))
        .await
        .expect("a settings read failure must not fail the request");
    assert_eq!(response.status(), StatusCode::OK);

    server.abort();
}
