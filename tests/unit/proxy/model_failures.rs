use super::*;

#[tokio::test]
async fn model_scoped_failures_isolate_before_opening_the_provider_circuit() {
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let state = AppState::new(pool, None);
        let mut first = endpoint_test_target("openai", None);
        first.provider_id = 7;
        first.upstream_model = "model-a".to_string();
        let mut second = first.clone();
        second.upstream_model = "model-b".to_string();

        let headers = HeaderMap::new();
        record_upstream_failure(&state, &first, status, &headers, "model failed").await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                state
                    .target_sink
                    .lock()
                    .await
                    .contains_key(&(7, None, "model-a".to_string())),
                "a short rate limit must sink the model without cooling it"
            );
            assert!(
                !state
                    .target_cooldown
                    .lock()
                    .await
                    .contains_key(&(7, "model-a".to_string()))
            );
        } else {
            assert!(
                state
                    .target_cooldown
                    .lock()
                    .await
                    .contains_key(&(7, "model-a".to_string()))
            );
        }
        assert!(
            !state.provider_cooldown.lock().await.contains_key(&7),
            "one model-specific {status} must not remove healthy sibling models"
        );

        record_upstream_failure(&state, &second, status, &headers, "model failed").await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                state
                    .target_sink
                    .lock()
                    .await
                    .contains_key(&(7, None, "model-b".to_string())),
                "each short rate limit should track the affected model"
            );
            assert!(
                !state.provider_cooldown.lock().await.contains_key(&7),
                "short model rate limits must not open the provider circuit"
            );
        } else {
            assert!(
                state.provider_cooldown.lock().await.contains_key(&7),
                "multiple cooling models should open the provider circuit"
            );
        }
    }
}

#[tokio::test]
async fn exhausted_model_5xx_keeps_healthy_provider_siblings_routable() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move |Json(body): Json<Value>| {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    if body["model"] == "model-a" {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": {"message": "model overloaded"}})),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-sibling",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "sibling-ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
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
            (1, 'sibling-provider', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'sibling route', 'public-model', 'priority', 1)",
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
        "sibling-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    assert_eq!(response.headers()["x-openllm-routed-model"], "model-b");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "model-a".to_string()))
    );
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&1),
        "one failed model must leave healthy sibling models routable"
    );

    server.abort();
}

#[test]
fn request_resource_conflicts_do_not_open_the_provider_circuit() {
    assert!(!provider_circuit_should_open(StatusCode::CONFLICT, 10));
    assert!(!provider_circuit_should_open(
        StatusCode::SERVICE_UNAVAILABLE,
        1
    ));
    assert!(provider_circuit_should_open(
        StatusCode::SERVICE_UNAVAILABLE,
        2
    ));
}

#[tokio::test]
async fn model_cooldowns_rank_healthy_sibling_models_first() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let mut cooling = endpoint_test_target("openai", None);
    cooling.id = 1;
    cooling.provider_id = 7;
    cooling.upstream_model = "model-a".to_string();
    let mut healthy = cooling.clone();
    healthy.id = 2;
    healthy.upstream_model = "model-b".to_string();
    state.target_cooldown.lock().await.insert(
        (7, "model-a".to_string()),
        Instant::now() + Duration::from_secs(60),
    );

    let ordered = order_targets(&state, 1, "priority", vec![cooling, healthy], None, None)
        .await
        .unwrap();

    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].upstream_model, "model-b");
}
