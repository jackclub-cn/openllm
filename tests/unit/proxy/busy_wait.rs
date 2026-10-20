use super::*;

#[tokio::test]
async fn target_sink_moves_a_rate_limited_candidate_behind_a_healthy_sibling() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES
            (1, 'Sinked', 'openai', 'https://sinked.example/v1'),
            (2, 'Healthy', 'openai', 'https://healthy.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    state.target_sink.lock().await.insert(
        (1, None, "model".to_string()),
        crate::state::TargetSinkEntry {
            until: Instant::now() + Duration::from_secs(30),
            reason_code: "rate_limited",
            observed_at: Instant::now(),
        },
    );
    let mut sinked = endpoint_test_target("openai", None);
    sinked.provider_id = 1;
    sinked.provider_name = "Sinked".to_string();
    sinked.upstream_model = "model".to_string();
    let mut healthy = endpoint_test_target("openai", None);
    healthy.provider_id = 2;
    healthy.provider_name = "Healthy".to_string();
    healthy.upstream_model = "model".to_string();

    let ordered = order_targets(&state, 1, "priority", vec![sinked, healthy], None, None)
        .await
        .unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[tokio::test]
async fn target_success_clears_its_short_lived_sink() {
    let state = AppState::new(
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap(),
        None,
    );
    let key = (1, Some(11), "model".to_string());
    state.target_sink.lock().await.insert(
        key.clone(),
        crate::state::TargetSinkEntry {
            until: Instant::now() + Duration::from_secs(30),
            reason_code: "rate_limited",
            observed_at: Instant::now(),
        },
    );
    mark_target_success(&state, 1, "model").await;
    assert!(!state.target_sink.lock().await.contains_key(&key));
}

#[tokio::test]
async fn busy_wait_lease_enforces_the_route_limit_and_releases_on_drop() {
    let state = AppState::new(
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap(),
        None,
    );
    let first = state.try_acquire_busy_wait(7, 1).unwrap();
    assert!(state.try_acquire_busy_wait(7, 1).is_none());
    drop(first);
    assert!(state.try_acquire_busy_wait(7, 1).is_some());

    let process_wide = (0..crate::state::MAX_BUSY_WAITERS)
        .map(|route_id| {
            state
                .try_acquire_busy_wait(route_id as i64, crate::state::MAX_BUSY_WAITERS)
                .expect("each process-wide slot should be available")
        })
        .collect::<Vec<_>>();
    assert!(state.try_acquire_busy_wait(999, 1).is_none());
    drop(process_wide);
    assert!(state.try_acquire_busy_wait(999, 1).is_some());
}

#[tokio::test]
async fn target_sinks_survive_the_resilience_state_restore() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Sinked', 'openai', 'https://sinked.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let deadline = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 30_000;
    sqlx::query(
        "INSERT INTO target_sink_state (
             provider_id, provider_key_id, model, sink_until_unix_ms, reason_code
         ) VALUES (1, 0, 'model', ?, 'rate_limited')",
    )
    .bind(deadline)
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    crate::resilience_state::restore(&state).await.unwrap();
    let sinks = state.target_sink.lock().await;
    let sink = sinks
        .get(&(1, None, "model".to_string()))
        .expect("the persisted sink must be restored");
    assert_eq!(
        sink.reason_code, "rate_limited",
        "the persisted reason must survive a restart"
    );
}

#[tokio::test]
async fn busy_wait_route_absorbs_a_short_rate_limit() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "0")],
                            Json(json!({
                                "error": {
                                    "message": "temporarily busy",
                                    "type": "rate_limit_error"
                                }
                            })),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-busy-wait",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
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
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'busy', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests
         ) VALUES (1, 'patient', 'patient-model', 'priority', 1, 2, 0, 2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'patient-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "patient-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "busy-wait-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("\"content\":\"ok\""));

    server.abort();
}

#[tokio::test]
async fn busy_wait_route_respects_the_waiting_request_cap() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "0")],
                        Json(json!({
                            "error": {
                                "message": "still busy",
                                "type": "rate_limit_error"
                            }
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'busy', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests
         ) VALUES (1, 'capped', 'capped-model', 'priority', 1, 5, 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'capped-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "capped-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "busy-wait-cap-request",
        None,
    )
    .await
    .expect_err("a zero waiting-request cap must fail immediately");

    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            ..
        }
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    server.abort();
}

#[tokio::test]
async fn busy_wait_route_returns_the_real_rate_limit_after_the_budget() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "1")],
                        Json(json!({
                            "error": {
                                "message": "still busy",
                                "type": "rate_limit_error"
                            }
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'busy', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests
         ) VALUES (1, 'budgeted', 'budgeted-model', 'priority', 1, 1, 0, 2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'budgeted-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "budgeted-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let started = Instant::now();
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "busy-wait-budget-request",
        None,
    )
    .await
    .expect_err("the budget must end with the real upstream error");

    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            ..
        }
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= Duration::from_millis(800));

    server.abort();
}

#[tokio::test]
async fn incompatible_openai_endpoint_does_not_enter_busy_wait() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'anthropic-only', 'anthropic', 'https://example.invalid/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests
         ) VALUES (1, 'incompatible', 'incompatible-model', 'priority', 1, 5, 0, 2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'incompatible-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "incompatible-model",
            "input": "hello"
        })
        .to_string(),
    );
    let started = Instant::now();
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/embeddings".parse().unwrap(),
        &body,
        "incompatible-request",
        None,
    )
    .await
    .expect_err("an endpoint incompatibility must fail immediately");

    assert!(
        matches!(error, AppError::Upstream(_) | AppError::BadRequest(_)),
        "unexpected error: {error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}
