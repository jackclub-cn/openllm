use super::*;

#[tokio::test]
async fn provider_key_rpm_full_orders_a_ready_sibling_key_first() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, max_concurrency)
         VALUES (1, 'RPM', 'openai', 'https://example.com/v1', 10)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (
            id, provider_id, name, secret, enabled, requests_per_minute
         ) VALUES
            (11, 1, 'Full', 'sk-full', 1, 1),
            (12, 1, 'Ready', 'sk-ready', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    state
        .provider_key_rpm
        .lock()
        .await
        .entry(11)
        .or_default()
        .push_back(Instant::now());
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 1;
    target.upstream_model = "model".to_string();

    let ordered = order_targets(&state, 1, "priority", vec![target], None, None)
        .await
        .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.provider_api_key_id)
            .collect::<Vec<_>>(),
        vec![Some(12), Some(11)]
    );
}

#[tokio::test]
async fn provider_key_rpm_limit_rejects_with_retry_after() {
    let state = AppState::new(
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap(),
        None,
    );
    let mut target = endpoint_test_target("openai", None);
    target.provider_api_key_id = Some(11);
    target.provider_api_key_name = Some("Primary".to_string());
    target.provider_key_requests_per_minute = Some(1);

    assert!(
        acquire_upstream_slots(&state, &target)
            .await
            .unwrap()
            .is_none()
    );
    let error = acquire_upstream_slots(&state, &target)
        .await
        .unwrap_err()
        .into_error();
    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn provider_key_queue_limit_bounds_waiters_and_releases_slots() {
    let state = AppState::new(
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap(),
        None,
    );
    let mut target = endpoint_test_target("openai", None);
    target.provider_api_key_id = Some(11);
    target.provider_api_key_name = Some("Primary".to_string());
    target.provider_key_max_concurrency = Some(1);
    target.provider_key_queue_limit = Some(1);
    target.provider_key_queue_wait_ms = Some(200);

    let first = acquire_upstream_slots(&state, &target)
        .await
        .unwrap()
        .unwrap();
    let waiter_state = state.clone();
    let waiter_target = target.clone();
    let waiter =
        tokio::spawn(async move { acquire_upstream_slots(&waiter_state, &waiter_target).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let error = acquire_upstream_slots(&state, &target)
        .await
        .unwrap_err()
        .into_error();
    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(_),
            ..
        }
    ));

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .provider_key_concurrency
            .lock()
            .await
            .get(&11)
            .unwrap()
            .available_permits(),
        0
    );
    drop(second);
    assert_eq!(
        state
            .provider_key_concurrency
            .lock()
            .await
            .get(&11)
            .unwrap()
            .available_permits(),
        1
    );
}

#[tokio::test]
async fn provider_key_zero_queue_limit_rejects_without_waiting() {
    let state = AppState::new(
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap(),
        None,
    );
    let mut target = endpoint_test_target("openai", None);
    target.provider_api_key_id = Some(11);
    target.provider_api_key_name = Some("Primary".to_string());
    target.provider_key_max_concurrency = Some(1);
    target.provider_key_queue_limit = Some(0);
    target.provider_key_queue_wait_ms = Some(30_000);

    let first = acquire_upstream_slots(&state, &target)
        .await
        .unwrap()
        .unwrap();
    let error = tokio::time::timeout(
        Duration::from_millis(100),
        acquire_upstream_slots(&state, &target),
    )
    .await
    .expect("a disabled queue must reject immediately")
    .unwrap_err()
    .into_error();
    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(_),
            ..
        }
    ));
    drop(first);
}

#[tokio::test]
async fn provider_concurrency_gate_queues_then_releases_slots() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.max_concurrency = Some(1);
    target.queue_timeout_seconds = Some(1);

    let first = acquire_upstream_slots(&state, &target)
        .await
        .unwrap()
        .unwrap();
    let semaphore = state
        .provider_concurrency
        .lock()
        .await
        .get(&7)
        .unwrap()
        .clone();
    assert_eq!(semaphore.available_permits(), 0);

    let state_for_waiter = state.clone();
    let target_for_waiter = target.clone();
    let waiter =
        tokio::spawn(
            async move { acquire_upstream_slots(&state_for_waiter, &target_for_waiter).await },
        );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(semaphore.available_permits(), 0);
    drop(second);
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test]
async fn provider_concurrency_zero_queue_timeout_rejects_immediately() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.max_concurrency = Some(1);
    target.queue_timeout_seconds = Some(0);

    let first = acquire_upstream_slots(&state, &target)
        .await
        .unwrap()
        .unwrap();
    let error = acquire_upstream_slots(&state, &target)
        .await
        .unwrap_err()
        .into_error();
    assert!(matches!(
        error,
        AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            ..
        }
    ));
    drop(first);
}

#[tokio::test]
async fn saturated_model_falls_back_to_healthy_provider_sibling() {
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
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-model-sibling",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "sibling-ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                        })),
                    )
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
        "INSERT INTO providers (
            id, name, provider_type, base_url, enabled, max_concurrency, queue_timeout_seconds
         ) VALUES (1, 'model-concurrency', 'openai', ?, 1, 2, 0)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
            provider_id, model_name, enabled, max_concurrency, queue_timeout_seconds
         ) VALUES
            (1, 'model-a', 1, 1, 0),
            (1, 'model-b', 1, 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'model concurrency route', 'public-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'model-a', 0),
            (1, 1, 'model-b', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let mut hot_target = endpoint_test_target("openai", None);
    hot_target.provider_id = 1;
    hot_target.provider_name = "model-concurrency".to_string();
    hot_target.upstream_model = "model-a".to_string();
    hot_target.max_concurrency = Some(2);
    hot_target.queue_timeout_seconds = Some(0);
    hot_target.model_max_concurrency = Some(1);
    hot_target.model_queue_timeout_seconds = Some(0);
    let held = acquire_upstream_slots(&state, &hot_target)
        .await
        .unwrap()
        .unwrap();

    let body = Bytes::from(
        json!({
            "model": "public-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "model-concurrency-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-openllm-fallback-count"],
        "0",
        "a model at its concurrency cap is skipped in favour of a sibling with \
         headroom, instead of waiting out its queue"
    );
    assert_eq!(response.headers()["x-openllm-routed-model"], "model-b");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&1),
        "local model backpressure must not open the provider circuit"
    );

    drop(held);
    server.abort();
}

#[tokio::test]
async fn provider_slot_is_held_until_streamed_body_completes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.max_concurrency = Some(1);
    target.queue_timeout_seconds = Some(0);

    let slots = acquire_upstream_slots(&state, &target)
        .await
        .unwrap()
        .unwrap();
    let semaphore = state
        .provider_concurrency
        .lock()
        .await
        .get(&7)
        .unwrap()
        .clone();
    let stream = futures_util::stream::iter([
        Ok::<_, std::convert::Infallible>(Bytes::from_static(b"first")),
        Ok::<_, std::convert::Infallible>(Bytes::from_static(b"second")),
    ]);
    let response = Response::new(Body::from_stream(stream));
    let response = attach_provider_slot(response, Some(slots), None);

    assert_eq!(semaphore.available_permits(), 0);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"firstsecond");
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test]
async fn saturated_provider_falls_back_without_opening_its_circuit() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "id": "chatcmpl-ha",
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
        "INSERT INTO providers (
            id, name, provider_type, base_url, enabled, max_concurrency, queue_timeout_seconds
         ) VALUES
            (1, 'busy', 'openai', 'http://127.0.0.1:9/v1', 1, 1, 0),
            (2, 'healthy', 'openai', ?, 1, NULL, NULL)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'ha route', 'ha-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority, enabled)
         VALUES (1, 1, 'ha-model', 0, 1), (1, 2, 'ha-model', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let held = semaphore.clone().acquire_owned().await.unwrap();
    state.provider_concurrency.lock().await.insert(1, semaphore);
    let body = Bytes::from(
        json!({
            "model": "ha-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );

    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "ha-request",
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-openllm-routed-provider"], "healthy");
    assert_eq!(
        response.headers()["x-openllm-fallback-count"],
        "0",
        "live capacity is part of the candidate score, so a provider at its \
         concurrency cap ranks behind one with headroom and no queue wait is spent"
    );
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&1),
        "local concurrency backpressure must not open the provider circuit"
    );

    drop(held);
    server.abort();
}
