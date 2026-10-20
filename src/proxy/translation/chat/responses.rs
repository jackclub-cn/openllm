use super::*;

/// Converts a Responses object into a chat-completions response.
pub(crate) fn responses_response_to_chat(value: &Value, requested_model: &str) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = String::new();
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    if let Some(text) = responses_reasoning_text(item) {
                        reasoning.push_str(&text);
                    }
                }
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(part) = part.get("text").and_then(Value::as_str) {
                                text.push_str(part);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(json!({
                        "id": item.get("call_id").or_else(|| item.get("id"))
                            .cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": item.get("arguments").cloned()
                                .unwrap_or_else(|| json!("{}"))
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let incomplete = value.get("status").and_then(Value::as_str) == Some("incomplete");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if incomplete {
        "length"
    } else {
        "stop"
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

    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("chatcmpl-{}", id.trim_start_matches("resp_")))
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let created = value
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": created,
            "model": requested_model,
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

/// Converts a legacy `/v1/completions` request into the chat-completions shape
/// so OpenAI-family upstreams can serve it through their chat endpoint.
pub(crate) fn completions_request_to_chat(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(prompt) = completions_prompt_text(input.get("prompt")) {
        messages.push(json!({"role": "user", "content": prompt}));
    }

    let mut chat = json!({"model": model, "messages": messages});
    if streamed {
        chat["stream"] = json!(true);
    }
    for key in [
        "max_tokens",
        "temperature",
        "top_p",
        "stop",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "response_format",
    ] {
        if let Some(value) = input.get(key) {
            chat[key] = value.clone();
        }
    }
    chat
}

/// Flattens the legacy `prompt` field into a single string. OpenAI allows a
/// plain string, an array of strings, or token arrays; token arrays cannot be
/// decoded without the provider's tokenizer, so they are skipped.
pub(crate) fn completions_prompt_text(prompt: Option<&Value>) -> Option<String> {
    match prompt {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let mut text = String::new();
            for item in items {
                match item {
                    Value::String(part) => text.push_str(part),
                    Value::Array(parts) => {
                        for part in parts {
                            if let Some(part) = part.as_str() {
                                text.push_str(part);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(text)
        }
        _ => None,
    }
}

/// Converts a chat completion into the legacy `/v1/completions` shape.
pub(crate) fn chat_response_to_completions(value: &Value, requested_model: &str) -> (Value, Usage) {
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| {
            format!(
                "cmpl-{}",
                id.trim_start_matches("msg_")
                    .trim_start_matches("chatcmpl-")
            )
        })
        .unwrap_or_else(|| format!("cmpl-{}", uuid::Uuid::new_v4().simple()));
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    (
        json!({
            "id": id,
            "object": "text_completion",
            "created": chrono::Utc::now().timestamp(),
            "model": requested_model,
            "choices": [{
                "text": text,
                "index": 0,
                "logprobs": Value::Null,
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
