use super::*;

#[tokio::test]
async fn forward_repairs_tool_history_and_normalizes_command_code_request() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured = Arc::new(Mutex::new(None));
    let app = axum::Router::new().route(
        OPENAI_RESPONSES,
        axum::routing::post({
            let captured = captured.clone();
            move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "resp_ok",
                            "object": "response",
                            "output": [],
                            "usage": {
                                "input_tokens": 1,
                                "output_tokens": 1,
                                "total_tokens": 2
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
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'command-code', 'openai', ?, 'commandcode/')",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let request_json = json!({
        "model": "commandcode/deepseek/deepseek-v4.1-flash",
        "input": [
            {"type": "function_call", "call_id": "call_pair", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_pair", "output": "ok"},
            {"type": "function_call", "call_id": "call_missing", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_orphan", "output": "stale"},
            {"role": "user", "content": "continue"}
        ],
        "max_output_tokens": 500_000,
        "reasoning": {"effort": "none"}
    });
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "command-code".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
        model_prefix: "commandcode/".to_string(),
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
        upstream_model: "deepseek/deepseek-v4.1-flash".to_string(),
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

    let response = forward_to_target(
        &state,
        "tool-history-repair",
        OPENAI_RESPONSES,
        "commandcode/deepseek/deepseek-v4.1-flash",
        &request_json,
        &Bytes::new(),
        None,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await.clone().unwrap();
    assert_eq!(captured["model"], "deepseek/deepseek-v4.1-flash");
    assert_eq!(captured["max_output_tokens"], 200_000);
    assert_eq!(captured["reasoning"]["effort"], "none");
    let input = captured["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["call_id"], "call_pair");
    assert_eq!(input[1]["call_id"], "call_pair");
    assert_eq!(input[2]["role"], "user");

    server.abort();
}

#[test]
fn classifies_upstream_shape_from_body_and_provider() {
    assert_eq!(
        classify_upstream_shape(ProviderType::Openai, &json!({"input": "hi"})),
        UpstreamShape::Responses
    );
    assert_eq!(
        classify_upstream_shape(
            ProviderType::Openai,
            &json!({"messages": [{"role": "user", "content": "hi"}]})
        ),
        UpstreamShape::Chat
    );
    // An Anthropic provider keeps the Anthropic repair rules even though
    // the body also carries a `messages` array.
    assert_eq!(
        classify_upstream_shape(
            ProviderType::Anthropic,
            &json!({"messages": [{"role": "user", "content": []}]})
        ),
        UpstreamShape::Anthropic
    );
}
