use super::*;

#[test]
fn responses_request_maps_instructions_and_input_to_anthropic() {
    let converted = responses_request_to_anthropic(
        &json!({
            "instructions": "be terse",
            "max_output_tokens": 512,
            "input": [
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hello" }]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "parameters": { "type": "object", "properties": {} }
            }]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["system"], "be terse");
    assert_eq!(converted["max_tokens"], 512);
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hello");
    assert_eq!(converted["tools"][0]["name"], "get_weather");
}

#[test]
fn responses_accepts_plain_string_input() {
    let converted =
        responses_request_to_anthropic(&json!({ "input": "hello world" }), "claude-x", false);
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hello world");
}

#[test]
fn responses_function_call_items_round_trip_to_anthropic() {
    let converted = responses_request_to_anthropic(
        &json!({
            "input": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "18C"
                }
            ]
        }),
        "claude-x",
        false,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);
    let assistant = messages[1]["content"].as_array().expect("assistant blocks");
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["type"], "tool_use");
    assert_eq!(assistant[0]["id"], "call_1");
    assert_eq!(assistant[0]["name"], "get_weather");
    assert_eq!(assistant[0]["input"]["city"], "Paris");
    let tool_result = &messages[2]["content"][0];
    assert_eq!(tool_result["type"], "tool_result");
    assert_eq!(tool_result["tool_use_id"], "call_1");
    assert_eq!(tool_result["content"], "18C");
}

#[test]
fn anthropic_response_converts_to_responses_output() {
    let (converted, usage) = anthropic_response_to_responses(
        &json!({
            "id": "msg_1",
            "model": "claude-x",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "let me check" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": { "city": "Paris" }
                }
            ],
            "usage": { "input_tokens": 30, "output_tokens": 12 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "response");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["status"], "completed");
    assert_eq!(converted["output"][0]["type"], "message");
    assert_eq!(converted["output"][0]["content"][0]["text"], "let me check");
    assert_eq!(converted["output"][1]["type"], "function_call");
    assert_eq!(converted["output"][1]["call_id"], "toolu_1");
    assert_eq!(converted["output"][1]["name"], "get_weather");
    assert_eq!(converted["output"][1]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(converted["usage"]["input_tokens"], 30);
    assert_eq!(converted["usage"]["output_tokens"], 12);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
}

#[tokio::test]
async fn anthropic_stream_converts_to_responses_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut event_name = String::new();
    for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n".as_slice(),
            b"event: message_stop\n".as_slice(),
            b"data: {\"type\":\"message_stop\"}\n".as_slice(),
        ] {
            stream.handle_line(line, &mut event_name, &tx).await;
        }
    stream.complete(&tx).await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(chunk) = rx.recv().await {
        let chunk = chunk.expect("stream chunk");
        events.push(String::from_utf8_lossy(&chunk).to_string());
    }
    let joined = events.join("");
    assert!(joined.contains("event: response.created"));
    assert!(joined.contains("event: response.output_text.delta"));
    assert!(joined.contains("\"delta\":\"hello\""));
    assert!(joined.contains("event: response.output_text.done"));
    assert!(joined.contains("event: response.completed"));
    assert_eq!(stream.usage.prompt_tokens, 7);
    assert_eq!(stream.usage.completion_tokens, 3);
}

#[tokio::test]
async fn chat_stream_converts_to_responses_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut tool_index = None;
    for line in [
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hel\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n".as_slice(),
            b"data: [DONE]\n".as_slice(),
        ] {
            process_chat_chunk_line(&mut stream, line, &mut tool_index, &tx).await;
        }
    stream.ensure_created(&tx).await;
    stream.complete(&tx).await;
    drop(tx);

    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("event: response.created"));
    assert!(joined.contains("event: response.output_text.delta"));
    assert!(joined.contains("\"delta\":\"hel\""));
    assert!(joined.contains("\"delta\":\"lo\""));
    assert!(joined.contains("event: response.output_text.done"));
    assert!(joined.contains("event: response.completed"));
    assert_eq!(stream.usage.prompt_tokens, 5);
    assert_eq!(stream.usage.completion_tokens, 2);
}

#[test]
fn chat_request_converts_to_responses_shape() {
    let converted = chat_request_to_responses(
        &json!({
            "model": "coding",
            "max_tokens": 256,
            "temperature": 0.2,
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": [
                    { "type": "text", "text": "guess 2+2" }
                ] },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "calc", "arguments": "{\"expr\":\"2+2\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "4" }
            ],
            "tools": [{
                "type": "function",
                "function": { "name": "calc", "parameters": { "type": "object" } }
            }]
        }),
        "deepseek/x",
        true,
    );
    assert_eq!(converted["model"], "deepseek/x");
    assert_eq!(converted["stream"], true);
    assert_eq!(converted["instructions"], "be terse");
    assert_eq!(converted["max_output_tokens"], 256);
    let input = converted["input"].as_array().expect("input array");
    // user message, function_call, function_call_output (assistant text was empty)
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(input[0]["content"][0]["text"], "guess 2+2");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_1");
    assert_eq!(input[1]["name"], "calc");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["output"], "4");
    assert_eq!(converted["tools"][0]["name"], "calc");
}

#[test]
fn responses_result_converts_to_chat_completion() {
    let (converted, usage) = responses_response_to_chat(
        &json!({
            "id": "resp_abc",
            "status": "completed",
            "output": [
                { "type": "reasoning", "summary": [] },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi there" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_9",
                    "name": "calc",
                    "arguments": "{\"expr\":\"2+2\"}"
                }
            ],
            "usage": { "input_tokens": 11, "output_tokens": 5, "total_tokens": 16 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "chat.completion");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
    assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_9"
    );
    assert_eq!(converted["usage"]["prompt_tokens"], 11);
    assert_eq!(converted["usage"]["completion_tokens"], 5);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (11, 5));
}

#[tokio::test]
async fn responses_stream_converts_to_chat_chunks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut state = ChatStreamState::default();
    for line in [
            b"event: response.reasoning_summary_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"plan\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" there\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3},\"status\":\"completed\"}}\n".as_slice(),
        ] {
            process_responses_line_for_chat(
                line,
                &mut event_name,
                &mut state,
                "chatcmpl_test",
                "coding",
                Instant::now(),
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"content\":\"hi\""));
    assert!(joined.contains("\"content\":\" there\""));
    assert!(joined.contains("\"reasoning_content\":\"plan\""));
    assert_eq!(state.text, "hi there");
    assert_eq!(state.prompt_tokens, 7);
    assert_eq!(state.completion_tokens, 3);
    assert!(state.response_terminated);
    assert!(state.response_error.is_none());
}

#[tokio::test]
async fn responses_reasoning_text_stream_converts_to_chat_reasoning_content() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut state = ChatStreamState::default();
    for line in [
        b"event: response.reasoning_text.delta\n".as_slice(),
        b"data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"plan\"}\n".as_slice(),
    ] {
        process_responses_line_for_chat(
            line,
            &mut event_name,
            &mut state,
            "chatcmpl_test",
            "coding",
            Instant::now(),
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
async fn responses_stream_failure_is_not_a_successful_chat_or_anthropic_turn() {
    let (tx, _rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let failed = b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"quota exhausted\"},\"usage\":{\"input_tokens\":6,\"output_tokens\":0}}}\n";
    let mut chat = ChatStreamState::default();
    process_responses_line_for_chat(
        failed,
        &mut String::new(),
        &mut chat,
        "chatcmpl_test",
        "coding",
        Instant::now(),
        &tx,
    )
    .await;
    assert!(chat.response_terminated);
    assert_eq!(chat.response_error.as_deref(), Some("quota exhausted"));
    assert_eq!(chat.prompt_tokens, 6);

    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 6,
        started: Instant::now(),
    };
    let mut anthropic = AnthropicStreamState::default();
    process_responses_line_for_anthropic(
        failed,
        &mut String::new(),
        &mut anthropic,
        &mut std::collections::HashMap::new(),
        &context,
        &tx,
    )
    .await;
    assert!(anthropic.response_terminated);
    assert_eq!(anthropic.response_error.as_deref(), Some("quota exhausted"));
    assert_eq!(anthropic.input_tokens, 6);
}

#[tokio::test]
async fn incomplete_responses_stream_has_length_finish_reason() {
    let (tx, _rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let incomplete = b"data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n";
    let mut chat = ChatStreamState::default();
    process_responses_line_for_chat(
        incomplete,
        &mut String::new(),
        &mut chat,
        "chatcmpl_test",
        "coding",
        Instant::now(),
        &tx,
    )
    .await;
    assert_eq!(chat.finish_reason(), "length");
    assert!(chat.response_error.is_none());
}

#[test]
fn responses_result_converts_to_anthropic_message() {
    let (converted, usage) = responses_response_to_anthropic(
        &json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "calc",
                    "arguments": "{\"x\":1}"
                }
            ],
            "usage": { "input_tokens": 4, "output_tokens": 2 }
        }),
        "coding",
    );
    assert_eq!(converted["type"], "message");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["content"][0]["type"], "text");
    assert_eq!(converted["content"][0]["text"], "hi");
    assert_eq!(converted["content"][1]["type"], "tool_use");
    assert_eq!(converted["content"][1]["name"], "calc");
    assert_eq!(converted["content"][1]["input"]["x"], 1);
    assert_eq!(converted["stop_reason"], "tool_use");
    assert_eq!(converted["usage"]["output_tokens"], 2);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
}

#[tokio::test]
async fn responses_stream_converts_to_anthropic_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 5,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    let mut tool_indices = std::collections::HashMap::new();
    let mut event_name = String::new();
    for line in [
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":6,\"output_tokens\":2}}}\n".as_slice(),
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
    assert!(joined.contains("event: message_start"));
    assert!(joined.contains("event: content_block_start"));
    assert!(joined.contains("\"text_delta\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert!(joined.contains("event: message_stop"));
    assert_eq!(state.text, "hi!");
    assert_eq!(state.output_tokens, 2);
}
