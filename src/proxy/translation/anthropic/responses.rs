use super::*;

/// Converts a non-streamed OpenAI completion into an Anthropic message.
pub(crate) fn openai_response_to_anthropic(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut content = Vec::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").cloned().unwrap_or_else(
                    || json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))
                ),
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "input": input
            }));
        }
    }
    let has_tool_use = content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    let finish = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("msg_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple()));
    (
        json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": requested_model,
            "content": content,
            "stop_reason": anthropic_stop_reason(finish, has_tool_use),
            "stop_sequence": Value::Null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        }),
        usage,
    )
}

/// Converts a Responses object into an Anthropic message.
///
/// Responses-first OpenAI upstreams serve Anthropic clients by normalising the
/// payload through the chat shape the Anthropic converter already understands.
pub(crate) fn responses_response_to_anthropic(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = responses_response_to_chat(value, requested_model);
    let (anthropic, _) = openai_response_to_anthropic(&chat, requested_model);
    (anthropic, usage)
}

pub(crate) fn convert_anthropic_response(value: &Value) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = String::new();
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    if let Some(part) = block.get("thinking").and_then(Value::as_str) {
                        reasoning.push_str(part);
                    }
                }
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "id": block.get("id").cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": serde_json::to_string(
                                block.get("input").unwrap_or(&Value::Null)
                            ).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let finish_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    };
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { json!(text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }

    // Reuse the shared reader so cache counters (`cache_read_input_tokens`,
    // `cache_creation_input_tokens`) are captured for native Anthropic too.
    let usage = value
        .get("usage")
        .map(|usage| {
            Usage {
                prompt_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                completion_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                total_tokens: 0,
                cache_read_tokens: cache_read_of(usage),
                cache_write_tokens: cache_write_of(usage),
            }
            .normalized()
        })
        .unwrap_or_default();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let model = value.get("model").cloned().unwrap_or(Value::Null);

    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}

pub(crate) fn anthropic_response_to_responses(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let (response, _) = chat_response_to_responses(&chat, requested_model);
    (response, usage)
}

pub(crate) fn anthropic_response_to_completions(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let (completions, _) = chat_response_to_completions(&chat, requested_model);
    (completions, usage)
}
