use super::*;

#[test]
fn strips_tool_search_for_compat_retry() {
    let body = json!({
        "model": "m",
        "tools": [
            {"type": "function", "name": "lookup"},
            {"type": "tool_search", "execution": "client"}
        ]
    });
    let stripped = strip_tool_search_tools(&body).unwrap();
    assert_eq!(stripped["tools"].as_array().unwrap().len(), 1);
    assert_eq!(stripped["tools"][0]["name"], "lookup");

    let only_tool_search = json!({
        "tools": [{"type": "tool_search", "execution": "client"}]
    });
    let stripped = strip_tool_search_tools(&only_tool_search).unwrap();
    assert!(stripped.get("tools").is_none());

    let standard_tools = json!({
        "tools": [{"type": "function", "name": "lookup"}]
    });
    assert!(strip_tool_search_tools(&standard_tools).is_none());
}

#[test]
fn strips_strict_tool_fields_for_compat_retry() {
    let body = json!({
        "tools": [
            {"type": "function", "name": "lookup", "strict": true},
            {
                "type": "function",
                "function": {"name": "search", "strict": true, "parameters": {}}
            }
        ]
    });
    let stripped = strip_strict_tools(&body).unwrap();
    assert!(stripped["tools"][0].get("strict").is_none());
    assert!(stripped["tools"][1]["function"].get("strict").is_none());
    assert!(stripped["tools"][0].get("name").is_some());
    assert!(strip_strict_tools(&json!({"tools": []})).is_none());
}

#[test]
fn recognizes_unsupported_strict_tool_error() {
    for error in [
        r#"{"error":{"message":"Invalid parameter: unknown strict field on tool schema"}}"#,
        r#"{"error":{"message":"tool function strict is not supported by this model"}}"#,
        r#"{"error":{"message":"additional property strict is not allowed for tools"}}"#,
    ] {
        assert!(upstream_rejects_strict_tools(error.as_bytes()));
    }
    for error in [
        r#"{"error":{"message":"strict mode is enabled for the request"}}"#,
        r#"{"error":{"message":"invalid output in strict mode for a tool call"}}"#,
        r#"{"error":{"message":"rate limit exceeded for tool calls"}}"#,
    ] {
        assert!(!upstream_rejects_strict_tools(error.as_bytes()));
    }
}

#[tokio::test]
async fn strict_tool_rejection_is_retried_and_learned_on_the_same_target() {
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
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        assert_eq!(body["tools"][0]["strict"], true);
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "Invalid parameter: unknown strict field on tool schema"
                                }
                            })),
                        )
                            .into_response();
                    }
                    assert!(body["tools"][0].get("strict").is_none());
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-strict",
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
         VALUES (1, 'strict-learning', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    let request_json = json!({
        "model": "strict-model",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{
            "type": "function",
            "name": "lookup",
            "strict": true,
            "parameters": {"type": "object", "properties": {}}
        }]
    });
    let mut target = endpoint_test_target("openai", None);
    target.base_url = format!("http://{address}");
    target.upstream_model = "strict-model".to_string();

    let response = forward_to_target(
        &state,
        "strict-learning",
        OPENAI_CHAT_COMPLETIONS,
        "strict-model",
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
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let learned: (i64, String) = sqlx::query_as(
        "SELECT supported, source FROM provider_capability_profiles
         WHERE provider_id = 1 AND capability = 'strict_tools'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(learned, (0, "runtime".to_string()));

    server.abort();
}

#[test]
fn repairs_orphaned_responses_tool_items() {
    let mut body = json!({
        "input": [
            {"type": "function_call", "call_id": "call_paired", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_paired", "output": "ok"},
            {"type": "function_call", "call_id": "call_missing", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_orphan", "output": "stale"},
            {"role": "user", "content": "continue"}
        ]
    });

    assert_eq!(sanitize_responses_tool_history(&mut body), 2);
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["call_id"], "call_paired");
    assert_eq!(input[1]["call_id"], "call_paired");
    assert_eq!(input[2]["role"], "user");
}

#[test]
fn repairs_orphaned_chat_tool_messages() {
    let mut body = json!({
        "messages": [
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_paired",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"}
                    },
                    {
                        "id": "call_missing",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"}
                    }
                ]
            },
            {"role": "tool", "tool_call_id": "call_paired", "content": "ok"},
            {"role": "tool", "tool_call_id": "call_orphan", "content": "stale"},
            {"role": "user", "content": "continue"}
        ]
    });

    assert_eq!(sanitize_chat_tool_history(&mut body), 2);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["tool_calls"][0]["id"], "call_paired");
    assert_eq!(messages[1]["tool_call_id"], "call_paired");
    assert_eq!(messages[2]["role"], "user");
}

#[test]
fn repairs_chat_tool_messages_that_do_not_follow_their_calls() {
    let mut body = json!({
        "messages": [
            {"role": "tool", "tool_call_id": "call_early", "content": "too early"},
            {"role": "user", "content": "continue"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_cross_turn",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]
            },
            {"role": "user", "content": "skip the result"},
            {"role": "tool", "tool_call_id": "call_cross_turn", "content": "late"},
            {"role": "assistant", "content": "done"}
        ]
    });

    assert_eq!(sanitize_chat_tool_history(&mut body), 4);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["content"], "skip the result");
    assert_eq!(messages[2]["role"], "assistant");
    assert!(
        messages
            .iter()
            .all(|message| message.get("tool_calls").is_none()
                && message.get("role").and_then(Value::as_str) != Some("tool")),
        "unordered tool history must be removed: {body}"
    );
}

#[test]
fn normalizes_command_code_output_caps_and_reasoning_effort() {
    let mut responses = json!({
        "input": "hi",
        "max_output_tokens": 500_000,
        "reasoning": {"effort": "none"}
    });
    assert_eq!(normalize_command_code_request(&mut responses), 1);
    assert_eq!(responses["max_output_tokens"], 200_000);
    assert_eq!(responses["reasoning"]["effort"], "none");

    let mut chat = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": -1,
        "reasoning_effort": "minimal"
    });
    assert_eq!(normalize_command_code_request(&mut chat), 2);
    assert!(chat.get("max_tokens").is_none());
    assert_eq!(chat["reasoning_effort"], "low");
}

#[test]
fn maps_reasoning_effort_to_the_nearest_model_value() {
    let supported = ["low", "high", "max"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut responses = json!({
        "input": "hi",
        "reasoning": {"effort": "xhigh"}
    });
    assert_eq!(normalize_reasoning_effort(&mut responses, &supported), 1);
    assert_eq!(responses["reasoning"]["effort"], "max");

    let mut chat = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "medium"
    });
    assert_eq!(normalize_reasoning_effort(&mut chat, &supported), 1);
    assert_eq!(chat["reasoning_effort"], "high");
}

#[test]
fn maps_original_image_detail_to_a_supported_level() {
    let supported = ["auto", "low", "high"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut responses = json!({
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA",
                "detail": "original"
            }]
        }]
    });
    assert_eq!(
        normalize_image_detail_original(&mut responses, &supported),
        1
    );
    assert_eq!(
        responses["input"][0]["content"][0]["detail"],
        Value::String("high".to_string())
    );

    let mut chat = json!({
        "messages": [{
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": {
                    "url": "data:image/png;base64,AAAA",
                    "detail": "original"
                }
            }]
        }]
    });
    assert_eq!(normalize_image_detail_original(&mut chat, &supported), 1);
    assert_eq!(
        chat["messages"][0]["content"][0]["image_url"]["detail"],
        Value::String("high".to_string())
    );
}

#[test]
fn distinguishes_image_detail_from_reasoning_effort_rejections() {
    let body = br#"{"error":{"code":"server_error","message":"Upstream request failed: [invalid_request_error] content part: unknown variant `original`, expected one of `auto`, `low`, `high` (request_id: gw-test)"}}"#;
    assert!(
        rejected_reasoning_effort_options(
            StatusCode::BAD_REQUEST,
            body,
            &json!({"reasoning": {"effort": "xhigh"}})
        )
        .is_none(),
        "image detail options are not reasoning efforts"
    );
    assert_eq!(
        rejected_image_detail_options(StatusCode::BAD_REQUEST, body),
        Some(vec![
            "auto".to_string(),
            "low".to_string(),
            "high".to_string()
        ])
    );
    assert!(
        rejected_image_detail_options(StatusCode::OK, body).is_none(),
        "successful responses must not be treated as compatibility errors"
    );

    let reasoning = br#"{"error":{"message":"unknown variant `auto` for reasoning effort, expected one of `low`, `high`"}}"#;
    assert_eq!(
        rejected_reasoning_effort_options(
            StatusCode::BAD_REQUEST,
            reasoning,
            &json!({"reasoning": {"effort": "auto"}})
        ),
        Some(vec!["low".to_string(), "high".to_string()])
    );
    assert!(
        rejected_reasoning_effort_options(
            StatusCode::BAD_REQUEST,
            reasoning,
            &json!({"reasoning": {"effort": "xhigh"}})
        )
        .is_none(),
        "an enum rejection for another field must not change reasoning effort"
    );
    assert!(rejected_image_detail_options(StatusCode::BAD_REQUEST, reasoning).is_none());
}

#[test]
fn identifies_command_code_targets_by_base_url_or_prefix() {
    let mut target = endpoint_test_target("openai", None);
    target.base_url = "https://api.commandcode.ai/provider/v1".to_string();
    assert!(is_command_code_target(&target));

    target.base_url = "https://example.com/v1".to_string();
    target.model_prefix = "commandcode/".to_string();
    assert!(is_command_code_target(&target));

    target.model_prefix = "gateway/".to_string();
    assert!(!is_command_code_target(&target));
}

#[test]
fn recognizes_unsupported_tool_search_error() {
    for error in [
        r#"{
                "type": "BadRequest",
                "code": "InvalidParameter",
                "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
            }"#,
        r#"{"error":{"message":"unsupported tool type: tool_search"}}"#,
        r#"{"error":{"message":"tool_search is not supported by this model"}}"#,
        r#"{"error":{"message":"unknown tool: tool_search"}}"#,
    ] {
        assert!(upstream_rejects_tool_search(error.as_bytes()));
    }
    for error in [
        r#"{"error":{"message":"rate limit exceeded"}}"#,
        r#"{"error":{"message":"tool_search returned an invalid result"}}"#,
        r#"{"error":{"message":"tool.type is required when tool_search is present"}}"#,
    ] {
        assert!(!upstream_rejects_tool_search(error.as_bytes()));
    }
}
