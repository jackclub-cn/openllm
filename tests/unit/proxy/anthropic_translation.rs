use super::*;

#[test]
fn converts_anthropic_request_into_openai_shape() {
    let inbound = json!({
        "model": "claude-x",
        "system": "be brief",
        "max_tokens": 256,
        "temperature": 0.3,
        "stop_sequences": ["STOP"],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        ],
        "tools": [{
            "name": "get_weather",
            "description": "weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }],
        "tool_choice": {"type": "tool", "name": "get_weather"}
    });
    let openai = anthropic_request_to_openai(&inbound, "claude-x");

    assert_eq!(openai["max_tokens"], 256);
    assert_eq!(openai["temperature"], 0.3);
    assert_eq!(openai["stop"], json!(["STOP"]));
    // The system prompt becomes a leading system message.
    assert_eq!(openai["messages"][0]["role"], "system");
    assert_eq!(openai["messages"][0]["content"], "be brief");
    // A text-only user turn collapses to a plain string.
    assert_eq!(openai["messages"][1]["role"], "user");
    assert_eq!(openai["messages"][1]["content"], "hi");
    // Anthropic tool schema maps onto the OpenAI function shape.
    assert_eq!(openai["tools"][0]["type"], "function");
    assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        openai["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
    assert_eq!(openai["tool_choice"]["function"]["name"], "get_weather");
}

#[test]
fn converts_anthropic_tool_use_and_results_round_trip() {
    // Assistant asks for a tool, then the user returns its result.
    let inbound = json!({
        "model": "claude-x",
        "max_tokens": 128,
        "messages": [
            {"role": "assistant", "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C"}
            ]}
        ]
    });
    let openai = anthropic_request_to_openai(&inbound, "claude-x");
    let assistant = &openai["messages"][0];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "checking");
    assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    // The tool result becomes a dedicated role: tool message.
    let tool = &openai["messages"][1];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "toolu_1");
    assert_eq!(tool["content"], "18C");
}

#[test]
fn converts_openai_completion_into_anthropic_message() {
    let upstream = json!({
        "id": "chatcmpl-abc",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
    });
    let (message, usage) = openai_response_to_anthropic(&upstream, "claude-x");
    assert_eq!(message["type"], "message");
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["model"], "claude-x");
    assert_eq!(message["content"][0]["type"], "text");
    assert_eq!(message["content"][0]["text"], "hello");
    assert_eq!(message["stop_reason"], "end_turn");
    assert_eq!(message["usage"]["input_tokens"], 4);
    assert_eq!(message["usage"]["output_tokens"], 2);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
}

#[test]
fn converts_openai_tool_call_into_anthropic_tool_use() {
    let upstream = json!({
        "id": "chatcmpl-abc",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8}
    });
    let (message, _) = openai_response_to_anthropic(&upstream, "claude-x");
    // No text block: a tool-only answer must not emit an empty text block.
    assert_eq!(message["content"].as_array().unwrap().len(), 1);
    assert_eq!(message["content"][0]["type"], "tool_use");
    assert_eq!(message["content"][0]["id"], "call_1");
    assert_eq!(message["content"][0]["name"], "get_weather");
    assert_eq!(message["content"][0]["input"]["city"], "Paris");
    assert_eq!(message["stop_reason"], "tool_use");
}

#[test]
fn maps_openai_finish_reasons_to_anthropic_stop_reasons() {
    assert_eq!(anthropic_stop_reason(Some("length"), false), "max_tokens");
    assert_eq!(anthropic_stop_reason(Some("tool_calls"), false), "tool_use");
    assert_eq!(anthropic_stop_reason(Some("stop"), false), "end_turn");
    // A tool_use block is authoritative even without a matching reason.
    assert_eq!(anthropic_stop_reason(None, true), "tool_use");
}

#[tokio::test]
async fn rewrites_openai_stream_into_anthropic_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "claude-x".to_string(),
        input_tokens: 7,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();

    let lines: [&[u8]; 5] = [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n",
            b"data: [DONE]\n",
        ];
    let mut saw_done = false;
    for line in lines {
        if !process_openai_line_for_anthropic(line, &mut state, &context, &tx).await {
            saw_done = true;
        }
    }
    assert!(saw_done, "[DONE] must terminate the stream");
    // Exercise the same closing sequence the live handler uses.
    finish_anthropic_stream(&mut state, &context, &tx).await;
    drop(tx);

    let mut frames = Vec::new();
    while let Some(item) = rx.recv().await {
        frames.push(String::from_utf8(item.unwrap().to_vec()).unwrap());
    }
    let all = frames.join("");

    assert!(all.contains("event: message_start"), "{all}");
    assert!(all.contains("event: content_block_start"), "{all}");
    assert!(all.contains("\"type\":\"text_delta\""), "{all}");
    // Text deltas must not be re-joined; each chunk passes through.
    assert!(all.contains("\"text\":\"Hel\""), "{all}");
    assert!(all.contains("\"text\":\"lo\""), "{all}");
    assert!(all.contains("event: content_block_stop"), "{all}");
    assert!(all.contains("event: message_delta"), "{all}");
    assert!(all.contains("\"stop_reason\":\"end_turn\""), "{all}");
    assert!(all.contains("event: message_stop"), "{all}");
    assert_eq!(state.output_tokens, 2);
    assert_eq!(state.text, "Hello");
    assert!(state.first_token_ms.is_some());
}

#[tokio::test]
async fn anthropic_stream_maps_tool_calls_to_tool_use_blocks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "claude-x".to_string(),
        input_tokens: 5,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    // Build the frames with serde so nested JSON escaping stays correct.
    let frames = [
        json!({"choices":[{"index":0,"delta":{"content":"thinking"}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}),
    ];
    for frame in frames {
        let line = format!("data: {frame}\n");
        process_openai_line_for_anthropic(line.as_bytes(), &mut state, &context, &tx).await;
    }
    process_openai_line_for_anthropic(b"data: [DONE]\n", &mut state, &context, &tx).await;
    drop(tx);

    let mut all = String::new();
    while let Some(item) = rx.recv().await {
        all.push_str(std::str::from_utf8(&item.unwrap()).unwrap());
    }
    assert!(all.contains("\"type\":\"tool_use\""), "{all}");
    assert!(all.contains("\"name\":\"get_weather\""), "{all}");
    assert!(all.contains("\"type\":\"input_json_delta\""), "{all}");
    assert!(all.contains("partial_json"), "{all}");
    assert!(state.has_tool_use);
    assert!(state.first_token_ms.is_some());
    assert_eq!(state.finish_reason, None);
}

#[test]
fn anthropic_request_requires_a_model() {
    let missing = json!({"max_tokens": 10, "messages": []});
    assert!(requested_model_of(&missing).is_err());
    let present = json!({"model": "claude-x"});
    assert_eq!(requested_model_of(&present).unwrap(), "claude-x");
}

#[test]
fn anthropic_response_converts_to_openai_shape() {
    let (converted, usage) = convert_anthropic_response(&json!({
        "id": "msg_1",
        "model": "claude-x",
        "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": "hi there" }],
        "usage": { "input_tokens": 30, "output_tokens": 12 }
    }));
    assert_eq!(converted["object"], "chat.completion");
    assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
}

#[test]
fn converts_openai_tool_calls_and_results_to_anthropic() {
    let converted = convert_request_to_anthropic(
        &json!({
            "messages": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "18C" }
            ]
        }),
        "claude-x",
        false,
    );

    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);

    let assistant = &messages[1]["content"];
    let blocks = assistant.as_array().expect("assistant content blocks");
    assert_eq!(blocks.len(), 1, "empty text block should be dropped");
    assert_eq!(blocks[0]["type"], "tool_use");
    assert_eq!(blocks[0]["id"], "call_1");
    assert_eq!(blocks[0]["name"], "get_weather");
    assert_eq!(blocks[0]["input"]["city"], "Paris");

    let tool_result = &messages[2]["content"][0];
    assert_eq!(tool_result["type"], "tool_result");
    assert_eq!(tool_result["tool_use_id"], "call_1");
    assert_eq!(tool_result["content"], "18C");
}

#[test]
fn converts_max_output_tokens_to_anthropic_max_tokens() {
    let converted = convert_request_to_anthropic(
        &json!({
            "max_output_tokens": 1200,
            "messages": [{ "role": "user", "content": "hello" }]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["max_tokens"], 1200);
}
