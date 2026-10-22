use super::*;

#[tokio::test]
async fn retries_openai_requests_without_tool_search_when_upstream_rejects_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (endpoint, request_json, success_body) in [
        (
            OPENAI_RESPONSES,
            json!({
                "model": "requested-model",
                "input": "hello",
                "tools": [{"type": "tool_search", "execution": "client"}]
            }),
            json!({
                "id": "ok",
                "object": "response",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "total_tokens": 12
                }
            }),
        ),
        (
            OPENAI_CHAT_COMPLETIONS,
            json!({
                "model": "requested-model",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "tool_search", "execution": "client"}]
            }),
            json!({
                "id": "ok",
                "object": "chat.completion",
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2,
                    "total_tokens": 12
                }
            }),
        ),
    ] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let success_body = success_body.clone();
        let app = axum::Router::new().route(
                endpoint,
                axum::routing::post({
                    let attempts = attempts.clone();
                    move |Json(body): Json<Value>| {
                        let attempts = attempts.clone();
                        let success_body = success_body.clone();
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
                                        "type": "BadRequest",
                                        "code": "InvalidParameter",
                                        "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
                                    })),
                                )
                            } else {
                                assert!(body.get("tools").is_none());
                                (StatusCode::OK, Json(success_body))
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
        let mut target = RouteTarget {
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

        let response = forward_to_target(
            &state,
            "tool-search-retry",
            endpoint,
            "requested-model",
            &request_json,
            &Bytes::new(),
            None,
            target.clone(),
            false,
            10,
            None,
            Instant::now(),
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

        target.tool_search_supported = 0;
        let response = forward_to_target(
            &state,
            "tool-search-cached",
            endpoint,
            "requested-model",
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
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        server.abort();
    }
}
