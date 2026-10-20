use super::*;

#[test]
fn anthropic_thinking_becomes_responses_reasoning() {
    let (converted, _) = anthropic_response_to_responses(
        &json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "let me think", "signature": "sig" },
                { "type": "text", "text": "answer" }
            ],
            "usage": { "input_tokens": 3, "output_tokens": 4 }
        }),
        "coding",
    );
    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(converted["output"][0]["summary"][0]["text"], "let me think");
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][1]["content"][0]["text"], "answer");
}

#[tokio::test]
async fn anthropic_thinking_stream_becomes_responses_reasoning() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut event_name = String::new();
    for line in [
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n".as_slice(),
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":1}\n".as_slice(),
        ] {
            stream.handle_line(line, &mut event_name, &tx).await;
        }
    stream.complete(&tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("response.reasoning_summary_text.delta"));
    assert!(joined.contains("\"delta\":\"plan\""));
    assert!(joined.contains("response.output_text.delta"));
    assert_eq!(stream.output[0]["type"], "reasoning");
    assert_eq!(stream.output[0]["summary"][0]["text"], "plan");
    assert_eq!(stream.output[1]["type"], "message");
}

#[tokio::test]
async fn anthropic_max_tokens_emits_incomplete_responses_event() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    stream.stop_reason = Some("max_tokens".to_string());
    stream.complete(&tx).await;
    drop(tx);
    let mut events = String::new();
    while let Some(chunk) = rx.recv().await {
        events.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    assert!(events.contains("event: response.incomplete"));
    assert!(events.contains("\"reason\":\"max_output_tokens\""));
    assert!(!events.contains("event: response.completed"));
}

#[test]
fn responses_reasoning_does_not_forge_anthropic_signature() {
    let (converted, _) = responses_response_to_anthropic(
        &json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "thought" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "answer" }]
                }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        }),
        "coding",
    );
    assert_eq!(converted["content"].as_array().unwrap().len(), 1);
    assert_eq!(converted["content"][0]["type"], "text");
    assert_eq!(converted["content"][0]["text"], "answer");
}

#[test]
fn chat_reasoning_does_not_forge_anthropic_signature() {
    let (converted, _) = openai_response_to_anthropic(
        &json!({
            "choices": [{
                "message": {"role": "assistant", "reasoning_content": "private plan", "content": "answer"},
                "finish_reason": "stop"
            }]
        }),
        "coding",
    );
    assert_eq!(
        converted["content"],
        json!([{"type": "text", "text": "answer"}])
    );
}

#[test]
fn chat_reasoning_content_becomes_responses_reasoning() {
    let (converted, _) = chat_response_to_responses(
        &json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "thought"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        }),
        "coding",
    );
    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(converted["output"][0]["summary"][0]["text"], "thought");
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][1]["content"][0]["text"], "answer");
}

#[test]
fn chat_reasoning_text_becomes_responses_reasoning() {
    let (converted, _) = chat_response_to_responses(
        &json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_text": "thought"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        }),
        "coding",
    );
    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(converted["output"][0]["summary"][0]["text"], "thought");
    assert_eq!(
        converted["output"][0]["content"][0]["type"],
        "reasoning_text"
    );
    assert_eq!(converted["output"][0]["content"][0]["text"], "thought");
}

#[test]
fn deepseek_chat_tool_turns_get_reasoning_content() {
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "messages": [
            {"role": "user", "content": "run ls"},
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call_1", "content": "main.go"},
            {"role": "assistant", "content": "Now update the index."}
        ]
    });
    assert_eq!(normalize_chat_reasoning_history(&mut body), 2);
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages[1]["reasoning_content"], " ");
    assert_eq!(messages[3]["reasoning_content"], " ");
}

#[test]
fn deepseek_anthropic_tool_turns_get_thinking() {
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "run ls"}]},
            {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "toolu_1",
                "name": "shell",
                "input": {}
            }]},
            {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "main.go"
            }]},
            {"role": "assistant", "content": [{"type": "text", "text": "Now update the index."}]},
            {"role": "user", "content": [{"type": "text", "text": "thanks"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "Welcome."}]}
        ]
    });
    assert_eq!(normalize_anthropic_thinking_history(&mut body), 2);
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages[1]["content"][0]["type"], "thinking");
    assert_eq!(messages[1]["content"][0]["thinking"], " ");
    assert_eq!(messages[3]["content"][0]["type"], "thinking");
    assert_eq!(messages[3]["content"][0]["thinking"], " ");
    assert_eq!(messages[5]["content"].as_array().unwrap().len(), 1);
    assert_eq!(messages[5]["content"][0]["type"], "text");
}

#[test]
fn deepseek_anthropic_replaces_redacted_thinking() {
    let mut body = json!({
        "model": "deepseek-v4.1-flash",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "run ls"}]},
            {"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "opaque"},
                {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {}}
            ]}
        ]
    });
    assert!(normalize_anthropic_thinking_history(&mut body) > 0);
    let content = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], " ");
    assert_eq!(content[1]["type"], "tool_use");
    assert!(
        !content
            .iter()
            .any(|block| block["type"] == "redacted_thinking")
    );
}

/// The mirror case: a chat client talking to a Responses-wire thinking
/// upstream must have its `reasoning_content` promoted to a reasoning item.
#[test]
fn chat_reasoning_content_replays_as_responses_reasoning_item() {
    let converted = chat_request_to_responses(
        &json!({
            "model": "coding",
            "messages": [
                {"role": "user", "content": "hi"},
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "plan",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "calc", "arguments": "{}" }
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "4"}
            ]
        }),
        "gpt/x",
        true,
    );
    let input = converted["input"].as_array().expect("input array");
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["summary"][0]["type"], "summary_text");
    assert_eq!(input[1]["summary"][0]["text"], "plan");
    assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
    assert_eq!(input[1]["content"][0]["text"], "plan");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[3]["type"], "function_call_output");
}

#[test]
fn chat_reasoning_text_replays_as_responses_reasoning_item() {
    let converted = chat_request_to_responses(
        &json!({
            "model": "coding",
            "messages": [
                {"role": "user", "content": "hi"},
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_text": "plan",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "calc", "arguments": "{}" }
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "4"}
            ]
        }),
        "gpt/x",
        true,
    );
    let input = converted["input"].as_array().expect("input array");
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
    assert_eq!(input[1]["content"][0]["text"], "plan");
}

#[test]
fn anthropic_thinking_sets_chat_reasoning_content() {
    let (converted, _) = convert_anthropic_response(&json!({
        "id": "msg_1",
        "stop_reason": "end_turn",
        "content": [
            { "type": "thinking", "thinking": "plan", "signature": "sig" },
            { "type": "text", "text": "ok" }
        ],
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    }));
    assert_eq!(
        converted["choices"][0]["message"]["reasoning_content"],
        "plan"
    );
    assert_eq!(converted["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn anthropic_thinking_stream_emits_chat_reasoning_content() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut output_chars = 0usize;
    let mut text = String::new();
    let mut sent_role = false;
    let mut saw_tool_use = false;
    let mut next_tool_index = 0usize;
    let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
    let mut first_token_ms: Option<i64> = None;
    for line in [
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"reasoning_content\":\"plan\""));
}

#[tokio::test]
async fn chat_reasoning_text_stream_becomes_responses_reasoning_item() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut tool_index = None;
    process_chat_chunk_line(
        &mut stream,
        b"data: {\"choices\":[{\"delta\":{\"reasoning_text\":\"plan\"}}]}\n",
        &mut tool_index,
        &tx,
    )
    .await;
    stream.complete(&tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("response.reasoning_summary_text.delta"));
    assert_eq!(stream.output[0]["type"], "reasoning");
    assert_eq!(stream.output[0]["summary"][0]["text"], "plan");
    assert_eq!(stream.output[0]["content"][0]["type"], "reasoning_text");
    assert_eq!(stream.output[0]["content"][0]["text"], "plan");
}

#[tokio::test]
async fn responses_reasoning_stream_does_not_emit_unsigned_anthropic_thinking() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 1,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    let mut tool_indices = std::collections::HashMap::new();
    let mut event_name = String::new();
    for line in [
            b"event: response.output_item.added\n".as_slice(),
            b"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n".as_slice(),
            b"event: response.reasoning_summary_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"hmm\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n".as_slice(),
        ] {
            process_responses_line_for_anthropic(
                line,
                &mut event_name,
                &mut state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
    finish_anthropic_stream(&mut state, &context, &tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(!joined.contains("\"thinking_delta\""));
    assert!(!joined.contains("\"signature\":\"\""));
    assert!(joined.contains("\"text\":\"done\""));
    assert!(joined.contains("event: message_stop"));
}
