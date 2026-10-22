use super::*;

/// A hard-open provider with no healthy alternative must fail fast with a 503
/// instead of spending a header timeout on an upstream call.
#[tokio::test]
async fn hard_open_provider_fails_fast_without_an_upstream_call() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let hits = hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-open",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'breaker', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'breaker route', 'breaker-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'breaker-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    // Three consecutive failures with an active cooldown: the provider is open.
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() + Duration::from_secs(60));

    let body = Bytes::from(
        json!({
            "model": "breaker-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "breaker-request",
        None,
    )
    .await
    .expect_err("an open provider must fail fast instead of calling upstream");

    match error {
        AppError::UpstreamStatus {
            status,
            retry_after,
            ..
        } => {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(
                retry_after.is_some(),
                "a short-circuit must set Retry-After"
            );
        }
        other => panic!("expected a 503 short-circuit, got {other:?}"),
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the short-circuit must not reach the upstream"
    );

    server.abort();
}

/// A non-retryable upstream response still proves the provider is reachable.
/// The half-open probe must close the circuit instead of leaving its lease
/// behind and blocking every request until the lease expires.
#[tokio::test]
async fn non_retryable_probe_response_closes_half_open_circuit() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let hits = hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": {"message": "invalid request"}})),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'recovering', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'recovering route', 'recovering-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'recovering-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));

    let body = Bytes::from(
        json!({
            "model": "recovering-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "recovering-request-1",
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state.provider_probe.lock().await.is_empty());
    assert!(!state.provider_failure_streak.lock().await.contains_key(&1));
    assert!(!state.provider_cooldown.lock().await.contains_key(&1));

    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "recovering-request-2",
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "the recovered provider must not be short-circuited by a stale probe lease"
    );

    server.abort();
}

/// An authentication failure on one key still proves the provider is
/// reachable. The half-open probe must recover before the fallback tries the
/// next key, otherwise the probe lease would short-circuit it.
#[tokio::test]
async fn half_open_probe_recovers_before_trying_the_next_key() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let hits = hits.clone();
            move |headers: HeaderMap| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let authorized = headers
                        .get(reqwest::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        == Some("Bearer sk-good");
                    if !authorized {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error": {"message": "invalid API key"}})),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-recovered",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'multi-key recovering', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) VALUES
            (11, 1, 'Broken', 'sk-bad', 1),
            (12, 1, 'Healthy', 'sk-good', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'recovering route', 'recovering-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'recovering-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));

    let body = Bytes::from(
        json!({
            "model": "recovering-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "recovering-key-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(!state.provider_key_cooldown.lock().await.contains_key(&12));
    assert!(state.provider_probe.lock().await.is_empty());
    assert!(!state.provider_failure_streak.lock().await.contains_key(&1));

    server.abort();
}
