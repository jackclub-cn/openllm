use super::*;

#[tokio::test]
async fn quota_exhaustion_reported_as_403_falls_back_to_a_healthy_provider() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let exhausted_attempts = Arc::new(AtomicUsize::new(0));
    let exhausted = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let exhausted_attempts = exhausted_attempts.clone();
            move || {
                let exhausted_attempts = exhausted_attempts.clone();
                async move {
                    exhausted_attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::FORBIDDEN,
                        Json(json!({
                            "error": {
                                "message": "用户额度不足",
                                "type": "insufficient_quota"
                            }
                        })),
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
                    "id": "chatcmpl-quota-fallback",
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
    let exhausted_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let exhausted_address = exhausted_listener.local_addr().unwrap();
    let healthy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let exhausted_server = tokio::spawn(async move {
        axum::serve(exhausted_listener, exhausted).await.unwrap();
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
            (1, 'exhausted', 'openai', ?),
            (2, 'healthy', 'openai', ?)",
    )
    .bind(format!("http://{exhausted_address}/v1"))
    .bind(format!("http://{healthy_address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'quota route', 'quota-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'quota-model', 0),
            (1, 2, 'quota-model', 1)",
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
            "model": "quota-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "quota-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(exhausted_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    assert_eq!(response.headers()["x-openllm-routed-provider"], "healthy");
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "quota-model".to_string())),
        "a misreported quota refusal must cool the exhausted model"
    );
    assert!(
        state.quota_snapshots.lock().await.values().any(|record| {
            record.provider_id == 1
                && record.model == "quota-model"
                && record.source == "runtime_429"
                && record.remaining == Some(0.0)
        }),
        "an explicit quota refusal must leave a short-lived runtime snapshot"
    );

    exhausted_server.abort();
    healthy_server.abort();
}

#[tokio::test]
async fn all_rate_limited_targets_surface_429_to_the_client() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            let mut response = axum::response::Response::new(axum::body::Body::from(
                "{\"error\":{\"message\":\"rate limited\"}}",
            ));
            *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            response.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
                .headers_mut()
                .insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("30"));
            response
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
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled)
             VALUES (1, 'rate-limited', 'openai', ?, '', 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'upstream', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let uri: Uri = OPENAI_CHAT_COMPLETIONS.parse().unwrap();
    let body = Bytes::from(
        json!({
            "model": "upstream",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai(State(state), HeaderMap::new(), uri, body).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "30"
    );
    assert!(response.headers().get("x-openllm-request-id").is_some());

    server.abort();
}
