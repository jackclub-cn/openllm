use super::*;

#[tokio::test]
async fn reasoning_text_rejection_falls_back_to_a_healthy_provider() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let rejected_attempts = Arc::new(AtomicUsize::new(0));
    let rejected = axum::Router::new().route(
        OPENAI_RESPONSES,
        axum::routing::post({
            let rejected_attempts = rejected_attempts.clone();
            move || {
                let rejected_attempts = rejected_attempts.clone();
                async move {
                    rejected_attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": {
                                "message": "The `reasoning_text` in the thinking mode must be passed back to the API.",
                                "type": "invalid_request_error"
                            }
                        })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let healthy = axum::Router::new().route(
        OPENAI_RESPONSES,
        axum::routing::post(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "id": "resp_healthy",
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok"}]
                    }],
                    "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4}
                })),
            )
                .into_response()
        }),
    );
    let rejected_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rejected_address = rejected_listener.local_addr().unwrap();
    let healthy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let rejected_server = tokio::spawn(async move {
        axum::serve(rejected_listener, rejected).await.unwrap();
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
            (1, 'rejected', 'openai', ?),
            (2, 'healthy', 'openai', ?)",
    )
    .bind(format!("http://{rejected_address}/v1"))
    .bind(format!("http://{healthy_address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'reasoning route', 'reasoning-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'reasoning-model', 0),
            (1, 2, 'reasoning-model', 1)",
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
            "model": "reasoning-model",
            "input": [{"type": "message", "role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/responses".parse().unwrap(),
        &body,
        "reasoning-fallback",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(rejected_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(response.headers()["x-openllm-fallback-count"], "1");
    assert_eq!(response.headers()["x-openllm-routed-provider"], "healthy");
    assert!(
        !state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "reasoning-model".to_string())),
        "a request-shape compatibility error must not cool the provider"
    );

    rejected_server.abort();
    healthy_server.abort();
}

#[tokio::test]
async fn reasoning_history_rejection_is_repaired_and_learned_on_the_same_target() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_RESPONSES,
        axum::routing::post({
            let attempts = attempts.clone();
            move |Json(body): Json<Value>| {
                let attempts = attempts.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "The `reasoning_text` in the thinking mode must be passed back to the API.",
                                    "type": "invalid_request_error"
                                }
                            })),
                        )
                            .into_response();
                    }
                    let has_reasoning = body["input"].as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("type").and_then(Value::as_str) == Some("reasoning")
                                && item
                                    .get("content")
                                    .and_then(Value::as_array)
                                    .is_some_and(|parts| {
                                        parts.iter().any(|part| {
                                            part.get("type").and_then(Value::as_str)
                                                == Some("reasoning_text")
                                                && part
                                                    .get("text")
                                                    .and_then(Value::as_str)
                                                    .is_some_and(|text| !text.is_empty())
                                        })
                                    })
                        })
                    });
                    assert!(has_reasoning, "the retry must include repaired reasoning");
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "resp_repaired",
                            "status": "completed",
                            "output": [{
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": "ok"}]
                            }],
                            "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4}
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
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'shape-learning', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'shape route', 'reasoning-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'reasoning-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    let body = Bytes::from(
        json!({
            "model": "reasoning-model",
            "input": [
                {"type": "message", "role": "user", "content": "hello"},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "result"
                }
            ]
        })
        .to_string(),
    );

    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/responses".parse().unwrap(),
        &body,
        "shape-learning",
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let learned: (i64, String, String, String) = sqlx::query_as(
        "SELECT supported, source, model, endpoint
         FROM provider_capability_profiles
         WHERE provider_id = 1 AND capability = 'reasoning_history'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        learned,
        (
            1,
            "runtime".to_string(),
            "reasoning-model".to_string(),
            "/v1/responses".to_string()
        )
    );

    server.abort();
}

/// Thinking-mode chat upstreams reject a replayed assistant turn whose
/// reasoning was dropped, so a Responses `reasoning` item has to survive the
/// conversion back into `reasoning_content`.
#[test]
fn responses_reasoning_is_replayed_on_tool_turns() {
    let converted = responses_request_to_chat(
        &json!({
            "model": "coding",
            "input": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "go"}]},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "think "}]},
                {"type": "reasoning", "content": [{"type": "reasoning_text", "text": "hard"}]},
                {"type": "function_call", "call_id": "call_1", "name": "calc", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "4"},
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "continue"}]}
            ]
        }),
        "deepseek/x",
        true,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["reasoning_content"], "think hard");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[3]["role"], "user");
    assert!(
        messages[3].get("reasoning_content").is_none(),
        "reasoning must not leak onto the following user turn"
    );
}

#[test]
fn responses_reasoning_is_replayed_on_text_turns() {
    let converted = responses_request_to_chat(
        &json!({
            "model": "coding",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}]},
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "answer"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "next"}]}
            ]
        }),
        "deepseek/x",
        false,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["reasoning_content"], "plan");
    assert_eq!(messages[2]["role"], "user");
    assert!(messages[2].get("reasoning_content").is_none());
}

#[test]
fn responses_reasoning_text_is_preserved_when_converted_to_chat() {
    let (converted, _) = responses_response_to_chat(
        &json!({
            "id": "resp_1",
            "status": "completed",
            "output": [{
                "type": "reasoning",
                "content": [{"type": "reasoning_text", "text": "think hard"}]
            }],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }),
        "coding",
    );
    assert_eq!(
        converted["choices"][0]["message"]["reasoning_content"],
        "think hard"
    );
    assert_eq!(converted["choices"][0]["message"]["content"], Value::Null);
}

#[test]
fn responses_top_level_reasoning_text_is_read() {
    assert_eq!(
        responses_reasoning_text(&json!({
            "type": "reasoning",
            "reasoning_text": "think hard"
        })),
        Some("think hard".to_string())
    );
}

#[test]
fn deepseek_responses_step_leads_with_reasoning_text() {
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "input": [
            {"type": "message", "role": "user",
             "content": [{"type": "input_text", "text": "run ls"}]},
            {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "main.go"},
            {"type": "message", "role": "assistant",
             "content": [{"type": "output_text", "text": "Now update the index."}]},
            {"type": "function_call", "call_id": "call_2", "name": "shell", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_2", "output": "main.go"}
        ]
    });
    assert_eq!(normalize_responses_reasoning_history(&mut body), 2);

    let input = body["input"].as_array().expect("input array");
    let kinds = input
        .iter()
        .map(|item| item["type"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "message",
            "reasoning",
            "function_call",
            "function_call_output",
            "reasoning",
            "message",
            "function_call",
            "function_call_output"
        ]
    );
    assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
    assert_eq!(input[1]["content"][0]["text"], " ");
    assert_eq!(input[4]["content"][0]["type"], "reasoning_text");
    assert_eq!(input[4]["content"][0]["text"], " ");
}

#[test]
fn deepseek_responses_moves_reasoning_before_a_note() {
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "input": [
            {"type": "message", "role": "user",
             "content": [{"type": "input_text", "text": "run ls"}]},
            {"type": "message", "role": "assistant",
             "content": [{"type": "output_text", "text": "Looking at the tree."}]},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}]},
            {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{}"}
        ]
    });
    assert_eq!(normalize_responses_reasoning_history(&mut body), 2);

    let input = body["input"].as_array().expect("input array");
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["content"][0]["text"], "plan");
    assert_eq!(input[2]["type"], "message");
    assert_eq!(input[3]["type"], "function_call");
}

#[tokio::test]
async fn deepseek_responses_compat_inserts_reasoning_before_forwarding() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_name = "CommandCode".to_string();
    target.base_url = "https://api.commandcode.ai/provider/v1".to_string();
    target.upstream_model = "deepseek-v4.1-flash".to_string();
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "input": [
            {"type": "message", "role": "user", "content": "run ls"},
            {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "main.go"}
        ]
    });

    apply_upstream_compat(
        &state,
        "req_test",
        &target,
        OPENAI_RESPONSES,
        UpstreamShape::Responses,
        &mut body,
    )
    .await;

    assert_eq!(body["input"][1]["type"], "reasoning");
    assert_eq!(body["input"][1]["content"][0]["type"], "reasoning_text");
    assert_eq!(body["input"][2]["type"], "function_call");
    assert_eq!(body["input"][3]["type"], "function_call_output");
}
