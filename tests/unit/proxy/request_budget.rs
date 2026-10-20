use super::*;

#[test]
fn request_budget_parsing_treats_zero_as_disabled() {
    use crate::state::{DEFAULT_REQUEST_TIMEOUT_SECS, parse_optional_secs};

    // A missing, blank, or unparsable value keeps the default so a typo cannot
    // silently remove the budget.
    assert_eq!(
        parse_optional_secs(None, DEFAULT_REQUEST_TIMEOUT_SECS),
        Some(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
    );
    assert_eq!(
        parse_optional_secs(Some("  "), DEFAULT_REQUEST_TIMEOUT_SECS),
        Some(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
    );
    assert_eq!(
        parse_optional_secs(Some("nope"), DEFAULT_REQUEST_TIMEOUT_SECS),
        Some(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
    );
    // An explicit `0` turns the budget off on purpose.
    assert_eq!(
        parse_optional_secs(Some("0"), DEFAULT_REQUEST_TIMEOUT_SECS),
        None
    );
    assert_eq!(
        parse_optional_secs(Some(" 90 "), DEFAULT_REQUEST_TIMEOUT_SECS),
        Some(Duration::from_secs(90))
    );
}

/// Once the request-budget deadline elapses the fallback loop must stop
/// *starting* targets, so a route full of slow-failing targets cannot pin an
/// admission slot for the sum of every per-attempt timeout.
#[tokio::test]
async fn request_budget_stops_the_fallback_loop() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let slow_attempts = Arc::new(AtomicUsize::new(0));
    let slow = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let slow_attempts = slow_attempts.clone();
            move || {
                let slow_attempts = slow_attempts.clone();
                async move {
                    slow_attempts.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": {"message": "down"}})),
                    )
                        .into_response()
                }
            }
        }),
    );
    let healthy_attempts = Arc::new(AtomicUsize::new(0));
    let healthy = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let healthy_attempts = healthy_attempts.clone();
            move || {
                let healthy_attempts = healthy_attempts.clone();
                async move {
                    healthy_attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-budget",
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
    let slow_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slow_address = slow_listener.local_addr().unwrap();
    let healthy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let slow_server = tokio::spawn(async move { axum::serve(slow_listener, slow).await.unwrap() });
    let healthy_server =
        tokio::spawn(async move { axum::serve(healthy_listener, healthy).await.unwrap() });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'slow', 'openai', ?),
            (2, 'healthy', 'openai', ?)",
    )
    .bind(format!("http://{slow_address}/v1"))
    .bind(format!("http://{healthy_address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'budget route', 'budget-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'budget-model', 0),
            (1, 2, 'budget-model', 1)",
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

    let mut state = AppState::new(pool, None);
    // The first (slow) target alone blows the budget, so the healthy target
    // must never be started.
    state.request_timeout = Some(Duration::from_millis(30));

    let body = Bytes::from(
        json!({
            "model": "budget-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let error = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "budget-request",
        None,
    )
    .await
    .expect_err("a spent budget must give up with a timeout, not fall through");

    match error {
        AppError::UpstreamStatus { status, .. } => {
            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        }
        other => panic!("expected a 504 budget error, got {other:?}"),
    }
    assert_eq!(
        slow_attempts.load(Ordering::SeqCst),
        1,
        "the failing target is attempted exactly once"
    );
    assert_eq!(
        healthy_attempts.load(Ordering::SeqCst),
        0,
        "the budget must stop the loop before the next target starts"
    );

    slow_server.abort();
    healthy_server.abort();
}
