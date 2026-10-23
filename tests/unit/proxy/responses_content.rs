use super::*;

#[test]
fn responses_text_content_uses_chat_strings_and_preserves_images() {
    let converted = responses_request_to_chat(
        &json!({
            "input": [
                {"role": "user", "content": [
                    {"type": "input_text", "text": "hello"},
                    {"type": "input_text", "text": " world"}
                ]},
                {"role": "assistant", "content": [
                    {"type": "output_text", "text": "answer"},
                    {"type": "refusal", "refusal": "cannot continue"}
                ]},
                {"role": "user", "content": [
                    {"type": "input_text", "text": "inspect"},
                    {
                        "type": "input_image",
                        "image_url": "data:image/png;base64,AAAA",
                        "detail": "original"
                    }
                ]}
            ]
        }),
        "upstream",
        false,
    );
    assert_eq!(converted["messages"][0]["content"], "hello world");
    assert_eq!(converted["messages"][1]["content"], "answercannot continue");
    assert_eq!(
        converted["messages"][2]["content"],
        json!([
            {"type": "text", "text": "inspect"},
            {
                "type": "image_url",
                "image_url": {
                    "url": "data:image/png;base64,AAAA",
                    "detail": "original"
                }
            }
        ])
    );
}

#[test]
fn chat_image_parts_keep_detail_when_converted_to_responses() {
    let converted = chat_request_to_responses(
        &json!({
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "data:image/png;base64,AAAA",
                            "detail": "high"
                        }
                    },
                    {
                        "type": "image_url",
                        "image_url": "data:image/jpeg;base64,BBBB"
                    }
                ]
            }]
        }),
        "upstream",
        false,
    );
    assert_eq!(
        converted["input"][0]["content"],
        json!([
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA",
                "detail": "high"
            },
            {
                "type": "input_image",
                "image_url": "data:image/jpeg;base64,BBBB"
            }
        ])
    );
}

#[tokio::test]
async fn responses_tool_outputs_are_valid_for_strict_chat_upstreams() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|Json(body): Json<Value>| async move {
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 20);
            assert_eq!(messages[19]["role"], "tool");
            if messages.iter().any(|message| {
                message
                    .get("content")
                    .is_some_and(|content| !content.is_string() && !content.is_null())
            }) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": {
                        "message": "Invalid input",
                        "type": "invalid_request_error",
                        "param": "messages.19.content"
                    }})),
                );
            }
            assert_eq!(messages[19]["tool_call_id"], "call_shell");
            assert_eq!(messages[19]["content"], "exit=0\nfile.rs");
            (
                StatusCode::OK,
                Json(json!({
                    "id": "chat_ok",
                    "choices": [{"message": {"role": "assistant", "content": "done"},
                                 "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = circuit_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'strict-chat', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    let mut target = endpoint_test_target("openai", Some(r#"["/chat/completions"]"#));
    target.base_url = format!("http://{address}");
    let mut input = (0..18)
        .map(|index| {
            json!({
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": [{"type": if index % 2 == 0 { "input_text" } else { "output_text" },
                             "text": format!("turn {index}")}]
            })
        })
        .collect::<Vec<_>>();
    input.push(json!({
        "type": "function_call", "call_id": "call_shell", "name": "shell", "arguments": "{}"
    }));
    input.push(json!({
        "type": "function_call_output", "call_id": "call_shell",
        "output": [
            {"type": "input_text", "text": "exit=0\n"},
            {"type": "input_text", "text": "file.rs"}
        ]
    }));
    let request = json!({"model": "coding", "input": input});
    let response = forward_to_target(
        &state,
        "strict-chat-tool-output",
        OPENAI_RESPONSES,
        "coding",
        &request,
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
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    server.abort();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["output"][0]["content"][0]["text"], "done");
}
