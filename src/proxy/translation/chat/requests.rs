use super::*;

pub(crate) fn responses_request_to_chat(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(instructions) = input.get("instructions")
        && let Some(text) = content_text(instructions)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    append_responses_input(&mut messages, input.get("input"));

    let mut output = json!({"model": model, "messages": messages});
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("parallel_tool_calls", "parallel_tool_calls"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(effort) = input.pointer("/reasoning/effort") {
        output["reasoning_effort"] = effort.clone();
    }
    if let Some(format) = input.pointer("/text/format")
        && let Some(response_format) = responses_format_to_chat(format)
    {
        output["response_format"] = response_format;
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(responses_tool_to_chat)
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
                json!({"type": "function", "function": {"name": name}})
            }
            _ => choice.clone(),
        };
    }
    output
}

/// Maps a Responses `text.format` onto the chat-completions `response_format`.
pub(crate) fn responses_format_to_chat(format: &Value) -> Option<Value> {
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => Some(json!({"type": "json_object"})),
        Some("json_schema") => {
            let mut inner = format.clone();
            if let Some(object) = inner.as_object_mut() {
                object.remove("type");
            }
            Some(json!({"type": "json_schema", "json_schema": inner}))
        }
        _ => None,
    }
}

pub(crate) fn append_responses_input(messages: &mut Vec<Value>, input: Option<&Value>) {
    // Responses carries reasoning as its own item, while chat-completions
    // thinking mode expects it back on the assistant message of the turn that
    // produced it. Dropping it makes the upstream reject the whole request
    // ("the reasoning must be passed back to the API"), so each item is
    // buffered and re-attached to the assistant turn it belongs to.
    let mut pending_reasoning = String::new();
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        if let Some(text) = responses_reasoning_text(item) {
                            pending_reasoning.push_str(&text);
                        }
                    }
                    Some("function_call") => {
                        let call = json!({
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned()
                                .unwrap_or(Value::Null),
                            "type": "function",
                            "function": {
                                "name": item.get("name").cloned().unwrap_or(Value::Null),
                                "arguments": item.get("arguments").cloned()
                                    .unwrap_or_else(|| json!("{}"))
                            }
                        });
                        let reasoning = (!pending_reasoning.is_empty())
                            .then(|| std::mem::take(&mut pending_reasoning));
                        if let Some(last) = messages.last_mut()
                            && last.get("role").and_then(Value::as_str) == Some("assistant")
                        {
                            let calls = last
                                .as_object_mut()
                                .and_then(|object| object.get_mut("tool_calls"))
                                .and_then(Value::as_array_mut);
                            if let Some(calls) = calls {
                                calls.push(call);
                                attach_reasoning(last, reasoning);
                                continue;
                            }
                        }
                        let mut message = json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [call]
                        });
                        attach_reasoning(&mut message, reasoning);
                        messages.push(message);
                    }
                    Some("function_call_output") => {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                            "content": responses_content_to_chat(
                                item.get("output").unwrap_or(&json!(""))
                            )
                        }));
                    }
                    _ => {
                        let Some(role) = item.get("role").and_then(Value::as_str) else {
                            continue;
                        };
                        let content =
                            responses_content_to_chat(item.get("content").unwrap_or(&Value::Null));
                        let mut message = json!({"role": role, "content": content});
                        if role == "assistant" {
                            let reasoning = (!pending_reasoning.is_empty())
                                .then(|| std::mem::take(&mut pending_reasoning));
                            attach_reasoning(&mut message, reasoning);
                        }
                        messages.push(message);
                    }
                }
            }
            // Some clients emit the reasoning item after the turn it belongs
            // to; the last assistant message is still the right home for it.
            if !pending_reasoning.is_empty()
                && let Some(last) = messages.last_mut()
                && last.get("role").and_then(Value::as_str) == Some("assistant")
            {
                attach_reasoning(last, Some(std::mem::take(&mut pending_reasoning)));
            }
        }
        _ => {}
    }
}

/// Re-attaches replayed reasoning to the assistant turn it belongs to,
/// appending when the message already carries some.
fn attach_reasoning(message: &mut Value, reasoning: Option<String>) {
    let Some(reasoning) = reasoning else {
        return;
    };
    let existing = chat_reasoning_text(message).unwrap_or_default();
    let Some(object) = message.as_object_mut() else {
        return;
    };
    object.insert(
        "reasoning_content".to_string(),
        json!(format!("{existing}{reasoning}")),
    );
}

pub(crate) fn responses_content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(_) | Value::Null => content.clone(),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(responses_content_part_to_chat)
                .collect::<Vec<_>>();
            // Text-only arrays are valid Responses content, but some chat
            // upstreams only accept strings for assistant and tool messages.
            if parts.iter().all(|part| part["type"] == "text") {
                json!(
                    parts
                        .iter()
                        .filter_map(|part| part["text"].as_str())
                        .collect::<String>()
                )
            } else {
                json!(parts)
            }
        }
        _ => content.clone(),
    }
}

pub(crate) fn responses_content_part_to_chat(item: &Value) -> Option<Value> {
    match item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text" | "text") => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("refusal") => item
            .get("refusal")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("input_image") => {
            let url = item
                .get("image_url")
                .or_else(|| item.get("url"))
                .and_then(Value::as_str)?;
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        }
        Some("image_url") => Some(item.clone()),
        _ => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
    }
}

pub(crate) fn responses_tool_to_chat(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    Some(json!({
        "type": "function",
        "function": {
            "name": tool.get("name")?.clone(),
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": tool.get("parameters").cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
        }
    }))
}
