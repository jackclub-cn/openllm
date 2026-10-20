use super::*;

/// Converts a legacy OpenAI `/v1/completions` request into the Anthropic
/// Messages shape by way of the chat shape that `convert_request_to_anthropic`
/// already understands. The legacy `prompt` field replaces the message list.
/// Converts an OpenAI chat-completions request into the Responses shape.
///
/// This is the inverse of [`responses_request_to_chat`] and lets a
/// Responses-only upstream serve callers that speak `/v1/chat/completions`.
pub(crate) fn chat_request_to_responses(input: &Value, model: &str, streamed: bool) -> Value {
    let mut instructions = Vec::new();
    let mut items = Vec::new();
    if let Some(messages) = input.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content)
                    && !text.is_empty()
                {
                    instructions.push(text);
                }
                continue;
            }
            if role == "tool" {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "output": content_text(&content).unwrap_or_default()
                }));
                continue;
            }
            let text_type = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            // Chat completions carries replayed reasoning on the assistant
            // message; Responses carries it as its own item, and thinking-mode
            // upstreams reject the history when it is missing.
            if role == "assistant"
                && let Some(reasoning) = chat_reasoning_text(message)
            {
                items.push(json!({
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning}],
                    "content": [{"type": "reasoning_text", "text": reasoning}]
                }));
            }
            let parts = chat_content_to_responses_parts(&content, text_type);
            if !parts.is_empty() {
                items.push(json!({"type": "message", "role": role, "content": parts}));
            }
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
            {
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call
                            .get("id")
                            .or_else(|| call.get("call_id"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": match function.get("arguments") {
                            Some(Value::String(text)) => json!(text),
                            Some(other) => json!(other.to_string()),
                            None => json!("{}"),
                        }
                    }));
                }
            }
        }
    }

    let mut output = json!({"model": model, "input": items});
    if !instructions.is_empty() {
        output["instructions"] = json!(instructions.join("\n\n"));
    }
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_tokens", "max_output_tokens"),
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("parallel_tool_calls", "parallel_tool_calls"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(effort) = input.get("reasoning_effort") {
        output["reasoning"] = json!({"effort": effort.clone()});
    }
    if let Some(format) = input
        .get("response_format")
        .and_then(chat_format_to_responses)
    {
        output["text"] = json!({"format": format});
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(chat_tool_to_responses)
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            output["tools"] = json!(tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "function", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

pub(crate) fn chat_content_to_responses_parts(content: &Value, text_type: &str) -> Vec<Value> {
    match content {
        Value::String(text) => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": text_type, "text": text})]
            }
        }
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                Some("image_url") => item
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .map(|url| json!({"type": "input_image", "image_url": url})),
                _ => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!({"type": text_type, "text": text})),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Maps a chat-completions `response_format` onto the Responses `text.format`.
pub(crate) fn chat_format_to_responses(format: &Value) -> Option<Value> {
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => Some(json!({"type": "json_object"})),
        Some("json_schema") => {
            let inner = format.get("json_schema").cloned().unwrap_or(Value::Null);
            let mut mapped = inner;
            if let Some(object) = mapped.as_object_mut() {
                object.insert("type".to_string(), json!("json_schema"));
            }
            Some(mapped)
        }
        _ => None,
    }
}

pub(crate) fn chat_tool_to_responses(tool: &Value) -> Option<Value> {
    let function = tool.get("function").unwrap_or(tool);
    function.get("name")?;
    Some(json!({
        "type": "function",
        "name": function.get("name")?.clone(),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    }))
}

pub(crate) fn chat_response_to_responses(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut output = Vec::new();
    if let Some(reasoning) = message.and_then(chat_reasoning_text) {
        output.push(json!({
            "id": format!("rs_{}", uuid::Uuid::new_v4().simple()),
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": reasoning}],
            "content": [{"type": "reasoning_text", "text": reasoning}]
        }));
    }
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
                "logprobs": []
            }]
        }));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let call_id = call
                .get("id")
                .cloned()
                .unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple())));
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!("{}"));
            output.push(json!({
                "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "arguments": match arguments {
                    Value::String(_) => arguments,
                    other => json!(serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()))
                }
            }));
        }
    }

    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let incomplete = finish_reason == Some("length");
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("resp_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let created_at = value
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "response",
            "created_at": created_at,
            "status": if incomplete { "incomplete" } else { "completed" },
            "error": Value::Null,
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
            "instructions": Value::Null,
            "max_output_tokens": Value::Null,
            "model": requested_model,
            "output": output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": Value::Null,
            "store": false,
            "temperature": Value::Null,
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "truncation": "disabled",
            "usage": responses_usage_json(&usage)
        }),
        usage,
    )
}
