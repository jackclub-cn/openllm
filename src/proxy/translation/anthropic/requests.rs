use super::*;

/// Collects Anthropic text from either a bare string or an array of blocks.
pub(crate) fn anthropic_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// Converts one Anthropic image block into an OpenAI `image_url` part.
pub(crate) fn anthropic_image_to_openai(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media_type};base64,{data}")
        }
        Some("url") => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

/// Converts one non-`tool_result` Anthropic content block into an OpenAI part.
pub(crate) fn anthropic_block_to_openai_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("image") => anthropic_image_to_openai(block),
        _ => None,
    }
}

pub(crate) fn anthropic_tool_choice_to_openai(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("tool") => Some(json!({
            "type": "function",
            "function": {"name": choice.get("name").cloned().unwrap_or(Value::Null)}
        })),
        _ => None,
    }
}

/// Converts an inbound Anthropic Messages body into the OpenAI chat shape that
/// the rest of the gateway (routing, barrel clamping, forwarding) understands.
pub(crate) fn anthropic_request_to_openai(input: &Value, model: &str) -> Value {
    let mut messages = Vec::new();
    // Anthropic carries the system prompt outside `messages`.
    if let Some(system) = input.get("system")
        && let Some(text) = anthropic_text(system)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            anthropic_message_to_openai(role, &content, &mut messages);
        }
    }

    let mut output = json!({"model": model, "messages": messages});
    for (source, target) in [
        ("max_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(stop) = input.get("stop_sequences") {
        output["stop"] = stop.clone();
    }
    if input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        output["stream"] = json!(true);
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                    }
                }))
            })
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            output["tools"] = json!(converted);
        }
    }
    if let Some(choice) = input.get("tool_choice")
        && let Some(mapped) = anthropic_tool_choice_to_openai(choice)
    {
        output["tool_choice"] = mapped;
    }
    output
}

/// Appends the OpenAI messages equivalent to one Anthropic message.
///
/// A single Anthropic user turn may mix text and `tool_result` blocks. Only the
/// latter become `role: tool` messages, and they are emitted in place so they
/// still follow the assistant turn whose `tool_calls` they answer.
pub(crate) fn anthropic_message_to_openai(role: &str, content: &Value, messages: &mut Vec<Value>) {
    if let Some(text) = content.as_str() {
        messages.push(json!({"role": role, "content": text}));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };

    if role == "assistant" {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(
                            block.get("input").unwrap_or(&json!({}))
                        ).unwrap_or_else(|_| "{}".to_string())
                    }
                })),
                _ => {}
            }
        }
        let mut message = json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { json!(text) }
        });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        messages.push(message);
        return;
    }

    // User (and any other) turn: split tool results out into their own messages.
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                "content": anthropic_text(block.get("content").unwrap_or(&Value::Null))
                    .unwrap_or_default()
            }));
        } else if let Some(part) = anthropic_block_to_openai_part(block) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return;
    }
    // Collapse text-only content to a plain string: most providers (and some
    // strict validators) handle that more reliably than a one-element array.
    if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        messages.push(json!({"role": role, "content": text}));
    } else {
        messages.push(json!({"role": role, "content": parts}));
    }
}

pub(crate) fn convert_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let mut output = json!({
        "model": model,
        "max_tokens": requested_output_tokens_of(input).unwrap_or(4096),
        "stream": streamed
    });

    let mut system = Vec::new();
    let mut messages = Vec::new();
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content) {
                    system.push(text);
                }
                continue;
            }

            // A tool result carries its association through `tool_call_id`.
            // Anthropic expects a user message holding a `tool_result` block.
            if role == "tool" {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_text(&content).unwrap_or_default()
                    }]
                }));
                continue;
            }

            let anthropic_role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            let mut blocks = convert_anthropic_content(&content);
            // An assistant turn may request tools; Anthropic represents those
            // as `tool_use` blocks alongside any text.
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
                && let Some(array) = blocks.as_array_mut()
            {
                // An assistant turn that only calls tools has no text content;
                // drop the placeholder empty block so Anthropic does not reject it.
                array.retain(|block| {
                    block.get("type").and_then(Value::as_str) != Some("text")
                        || block
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty())
                });
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    let Ok(arguments) = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .parse::<Value>()
                    else {
                        continue;
                    };
                    array.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "input": arguments
                    }));
                }
            }
            messages.push(json!({
                "role": anthropic_role,
                "content": blocks
            }));
        }
    }

    if !system.is_empty() {
        output["system"] = json!(system.join("\n\n"));
    }
    output["messages"] = json!(messages);

    if let Some(temperature) = input.get("temperature") {
        output["temperature"] = temperature.clone();
    }
    if let Some(top_p) = input.get("top_p") {
        output["top_p"] = top_p.clone();
    }
    if let Some(stop) = input.get("stop") {
        output["stop_sequences"] = match stop {
            Value::String(value) => json!([value]),
            Value::Array(_) => stop.clone(),
            _ => Value::Null,
        };
    }

    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let anthropic_tools = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function").unwrap_or(tool);
                let name = function.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": function.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": function.get("parameters").cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                }))
            })
            .collect::<Vec<_>>();
        if !anthropic_tools.is_empty() {
            output["tools"] = json!(anthropic_tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::String(value) if value == "required" => json!({"type": "any"}),
            Value::String(value) if value == "auto" || value == "none" => {
                json!({"type": value})
            }
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "tool", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

/// Converts an OpenAI Responses request into the Anthropic Messages shape.
///
/// Responses is item-oriented while Messages is message-oriented, so the
/// conversion first normalizes the input into the chat shape that
/// `convert_request_to_anthropic` already knows how to handle.
pub(crate) fn responses_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let chat = responses_request_to_chat(input, model, streamed);
    convert_request_to_anthropic(&chat, model, streamed)
}

pub(crate) fn completions_request_to_anthropic(
    input: &Value,
    model: &str,
    streamed: bool,
) -> Value {
    let chat = completions_request_to_chat(input, model, streamed);
    convert_request_to_anthropic(&chat, model, streamed)
}

pub(crate) fn convert_anthropic_content(content: &Value) -> Value {
    match content {
        Value::String(text) => json!([{"type": "text", "text": text}]),
        Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = item.pointer("/image_url/url").and_then(Value::as_str) {
                            if let Some((meta, data)) = url.split_once(',') {
                                let media_type = meta
                                    .strip_prefix("data:")
                                    .and_then(|value| value.split(';').next())
                                    .unwrap_or("image/png");
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data
                                    }
                                }));
                            } else {
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {"type": "url", "url": url}
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if blocks.is_empty() {
                json!([{"type": "text", "text": ""}])
            } else {
                json!(blocks)
            }
        }
        _ => json!([{"type": "text", "text": ""}]),
    }
}
