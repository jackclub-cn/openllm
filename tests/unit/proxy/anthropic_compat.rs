use super::*;

#[tokio::test]
async fn anthropic_forward_repairs_orphan_tool_calls_for_openai_upstream() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured = Arc::new(Mutex::new(None));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let captured = captured.clone();
            move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-1",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "done"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 3,
                                "completion_tokens": 1,
                                "total_tokens": 4
                            }
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
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "mock".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
        model_prefix: String::new(),
        api_key: None,
        provider_headers: "{}".to_string(),
        supported_endpoints: None,
        cost: None,
        cost_input_override: None,
        cost_output_override: None,
        cost_cache_read_override: None,
        cost_cache_write_override: None,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        reasoning_efforts: None,
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        timeout_seconds: None,
        cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
        model_max_concurrency: None,
        model_queue_timeout_seconds: None,
        provider_health: None,
        upstream_model: "upstream".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        provider_key_max_concurrency: None,
        provider_key_requests_per_minute: None,
        provider_key_queue_limit: None,
        provider_key_queue_wait_ms: None,
        auth_retryable: false,
    };
    // OpenAI chat shape, as produced from an Anthropic caller, carrying an
    // assistant tool call whose result never came back.
    let request_json = json!({
        "model": "requested-model",
        "messages": [
            {"role": "user", "content": "hi"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_missing",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]
            },
            {"role": "user", "content": "continue"}
        ]
    });

    let response = forward_openai_as_anthropic(
        &state,
        "anthropic-tool-repair",
        "requested-model",
        &request_json,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await.clone().unwrap();
    let messages = captured["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|message| message.get("tool_calls").is_none()),
        "orphan tool_calls must be removed before the upstream call: {captured}"
    );
    assert_eq!(messages.len(), 2);

    server.abort();
}

#[tokio::test]
async fn anthropic_forward_retries_openai_upstream_without_tool_search() {
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
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        assert!(
                            body["tools"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|tool| tool["type"] == "tool_search")
                        );
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "unknown tool type: tool_search"
                                }
                            })),
                        )
                    } else {
                        assert!(body.get("tools").is_none());
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "chatcmpl-1",
                                "object": "chat.completion",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "done"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 1,
                                    "total_tokens": 4
                                }
                            })),
                        )
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
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "mock".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
        model_prefix: String::new(),
        api_key: None,
        provider_headers: "{}".to_string(),
        supported_endpoints: None,
        cost: None,
        cost_input_override: None,
        cost_output_override: None,
        cost_cache_read_override: None,
        cost_cache_write_override: None,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        reasoning_efforts: None,
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        timeout_seconds: None,
        cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
        model_max_concurrency: None,
        model_queue_timeout_seconds: None,
        provider_health: None,
        upstream_model: "upstream".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        provider_key_max_concurrency: None,
        provider_key_requests_per_minute: None,
        provider_key_queue_limit: None,
        provider_key_queue_wait_ms: None,
        auth_retryable: false,
    };
    let request_json = json!({
        "model": "requested-model",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{"type": "tool_search", "execution": "client"}]
    });

    let response = forward_openai_as_anthropic(
        &state,
        "anthropic-tool-search-retry",
        "requested-model",
        &request_json,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let tool_search_supported: i64 =
        sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tool_search_supported, 0);
    let tool_search_checked_at: Option<String> =
        sqlx::query_scalar("SELECT tool_search_checked_at FROM providers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(tool_search_checked_at.is_some());

    server.abort();
}
