use super::*;

#[test]
fn completions_request_maps_prompt_to_anthropic() {
    let converted = completions_request_to_anthropic(
        &json!({
            "prompt": "write a haiku",
            "max_tokens": 64,
            "temperature": 0.5,
            "stop": ["\n\n"]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["max_tokens"], 64);
    assert_eq!(converted["temperature"], 0.5);
    assert_eq!(converted["stop_sequences"][0], "\n\n");
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "write a haiku");
}

#[test]
fn completions_request_joins_prompt_array() {
    let converted = completions_request_to_anthropic(
        &json!({ "prompt": ["hello ", "world"] }),
        "claude-x",
        false,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages[0]["content"][0]["text"], "hello world");
}

#[test]
fn completions_request_converts_to_chat() {
    let converted = completions_request_to_chat(
        &json!({
            "prompt": "summarize",
            "max_tokens": 128,
            "temperature": 0.3,
            "stop": ["\n"]
        }),
        "gpt-x",
        true,
    );
    assert_eq!(converted["model"], "gpt-x");
    assert_eq!(converted["stream"], true);
    assert_eq!(converted["max_tokens"], 128);
    assert_eq!(converted["temperature"], 0.3);
    assert_eq!(converted["stop"][0], "\n");
    assert_eq!(converted["messages"][0]["role"], "user");
    assert_eq!(converted["messages"][0]["content"], "summarize");
}

#[test]
fn chat_response_converts_to_completions() {
    let (converted, usage) = chat_response_to_completions(
        &json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "done" },
                "finish_reason": "length"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 6, "total_tokens": 11 }
        }),
        "gpt-x",
    );
    assert_eq!(converted["object"], "text_completion");
    assert_eq!(converted["id"], "cmpl-abc");
    assert_eq!(converted["choices"][0]["text"], "done");
    assert_eq!(converted["choices"][0]["finish_reason"], "length");
    assert_eq!(converted["usage"]["completion_tokens"], 6);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (5, 6));
}

#[tokio::test]
async fn chat_stream_converts_to_completions_chunks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut text = String::new();
    let mut output_chars = 0;
    let mut usage = Usage::default();
    let mut finish_reason = None;
    let mut first_token_ms = None;
    for line in [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n".as_slice(),
            b"data: [DONE]\n".as_slice(),
        ] {
            process_chat_line_for_completions(
                line,
                &mut text,
                &mut output_chars,
                &mut usage,
                &mut finish_reason,
                &mut first_token_ms,
                Instant::now(),
                "cmpl_test",
                "gpt-x",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"object\":\"text_completion\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert_eq!(text, "hi!");
    assert_eq!(output_chars, 3);
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(finish_reason.as_deref(), Some("stop"));
}

#[test]
fn anthropic_response_converts_to_text_completion() {
    let (converted, usage) = anthropic_response_to_completions(
        &json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "a haiku" }],
            "usage": { "input_tokens": 9, "output_tokens": 4 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "text_completion");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["choices"][0]["text"], "a haiku");
    assert_eq!(converted["choices"][0]["finish_reason"], "stop");
    assert_eq!(converted["usage"]["completion_tokens"], 4);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (9, 4));
}

#[tokio::test]
async fn anthropic_stream_converts_to_completions_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut text = String::new();
    let mut output_chars = 0;
    let mut stop_reason = None;
    let mut first_token_ms = None;
    for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n".as_slice(),
        ] {
            process_completions_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut text,
                &mut output_chars,
                &mut stop_reason,
                &mut first_token_ms,
                Instant::now(),
                "cmpl_test",
                "coding",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut events = Vec::new();
    while let Some(chunk) = rx.recv().await {
        events.push(String::from_utf8_lossy(&chunk.expect("chunk")).to_string());
    }
    let joined = events.join("");
    assert!(joined.contains("\"object\":\"text_completion\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert_eq!(text, "hi");
    assert_eq!(output_chars, 2);
    assert_eq!(usage.prompt_tokens, 5);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
}
