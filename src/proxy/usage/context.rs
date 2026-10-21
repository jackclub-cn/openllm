use super::*;
use crate::models::{ContextBreakdown, ContextCategory};

const SYSTEM: usize = 0;
const TOOLS: usize = 1;
const TOOL_CALLS: usize = 2;
const TOOL_RESULTS: usize = 3;
const REASONING: usize = 4;
const CONVERSATION: usize = 5;
const CATEGORY_KEYS: [&str; 6] = [
    "system",
    "tools",
    "tool_calls",
    "tool_results",
    "reasoning",
    "conversation",
];

pub(crate) fn request_context_breakdown(
    body: &Value,
    endpoint: &str,
    estimated_input_tokens: i64,
) -> ContextBreakdown {
    let mut chars = [0usize; 6];
    let mut items = [0usize; 6];
    let mut opaque_reasoning_items = 0;
    if let Some(system) = body.get("system").or_else(|| body.get("instructions")) {
        chars[SYSTEM] = chars[SYSTEM].saturating_add(count_text_chars(system, None));
        items[SYSTEM] = 1;
    }
    for tools_key in ["tools", "functions"] {
        let Some(tools) = body.get(tools_key) else {
            continue;
        };
        if let Some(definitions) = tools.as_array() {
            items[TOOLS] = items[TOOLS].saturating_add(definitions.len());
            chars[TOOLS] = chars[TOOLS].saturating_add(
                definitions
                    .iter()
                    .map(|tool| count_text_chars(tool, None))
                    .sum::<usize>(),
            );
        } else {
            items[TOOLS] = items[TOOLS].saturating_add(1);
            chars[TOOLS] = chars[TOOLS].saturating_add(count_text_chars(tools, None));
        }
    }
    if endpoint == ANTHROPIC_MESSAGES {
        count_anthropic_messages(body, &mut chars, &mut items, &mut opaque_reasoning_items);
    } else if endpoint == OPENAI_RESPONSES {
        count_responses_input(body, &mut chars, &mut items, &mut opaque_reasoning_items);
    } else {
        count_chat_messages(body, &mut chars, &mut items, &mut opaque_reasoning_items);
        if let Some(prompt) = body.get("prompt") {
            chars[CONVERSATION] =
                chars[CONVERSATION].saturating_add(count_text_chars(prompt, None));
            items[CONVERSATION] = items[CONVERSATION].saturating_add(1);
        }
    }
    ContextBreakdown {
        window_tokens: None,
        input_tokens: estimated_input_tokens.max(0),
        estimated: true,
        input_estimated: true,
        categories: allocate_categories(chars, estimated_input_tokens.max(0), items),
        unmetered_items: count_request_unmetered_items(body),
        opaque_reasoning_items,
        ..Default::default()
    }
}

fn count_anthropic_messages(
    body: &Value,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let bucket = role_bucket(role);
        let Some(content) = message.get("content") else {
            continue;
        };
        count_content(content, bucket, chars, items, opaque_reasoning_items);
    }
}

fn count_responses_input(
    body: &Value,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    let Some(input) = body.get("input") else {
        return;
    };
    let Some(input_items) = input.as_array() else {
        chars[CONVERSATION] = chars[CONVERSATION].saturating_add(count_text_chars(input, None));
        items[CONVERSATION] = items[CONVERSATION].saturating_add(1);
        return;
    };
    for item in input_items {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if is_reasoning_type(item_type) {
            count_reasoning(item, chars, items, opaque_reasoning_items);
        } else if is_tool_call_type(item_type) {
            count_tool_call(item, chars, items);
        } else if is_tool_result_type(item_type) {
            count_tool_result(item, chars, items);
        } else if item_type == "message" || item_type.is_empty() {
            let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
            count_content(
                item.get("content").unwrap_or(&Value::Null),
                role_bucket(role),
                chars,
                items,
                opaque_reasoning_items,
            );
        } else if is_media_type(item_type) {
            continue;
        } else {
            chars[CONVERSATION] = chars[CONVERSATION].saturating_add(count_text_chars(item, None));
            items[CONVERSATION] = items[CONVERSATION].saturating_add(1);
        }
    }
}

fn count_chat_messages(
    body: &Value,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "tool" {
            count_tool_result(message, chars, items);
            continue;
        }
        let bucket = role_bucket(role);
        if let Some(reasoning) = message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning_text"))
        {
            count_reasoning(reasoning, chars, items, opaque_reasoning_items);
        }
        if let Some(content) = message.get("content") {
            count_content(content, bucket, chars, items, opaque_reasoning_items);
        }
        if let Some(tool_calls) = message.get("tool_calls") {
            if let Some(calls) = tool_calls.as_array() {
                for call in calls {
                    count_tool_call(call, chars, items);
                }
            } else if !tool_calls.is_null() {
                count_tool_call(tool_calls, chars, items);
            }
        }
    }
}

fn count_content(
    content: &Value,
    default_bucket: usize,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    match content {
        Value::Array(parts) => {
            for part in parts {
                count_content_part(part, default_bucket, chars, items, opaque_reasoning_items);
            }
        }
        Value::Null => {}
        _ => count_content_part(
            content,
            default_bucket,
            chars,
            items,
            opaque_reasoning_items,
        ),
    }
}

fn count_content_part(
    part: &Value,
    default_bucket: usize,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    let block_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
    if is_tool_result_type(block_type) {
        count_tool_result(part, chars, items);
    } else if is_tool_call_type(block_type) {
        count_tool_call(part, chars, items);
    } else if is_reasoning_type(block_type) {
        count_reasoning(part, chars, items, opaque_reasoning_items);
    } else if !is_media_type(block_type) {
        chars[default_bucket] = chars[default_bucket].saturating_add(count_text_chars(part, None));
        items[default_bucket] = items[default_bucket].saturating_add(1);
    }
}

fn count_reasoning(
    value: &Value,
    chars: &mut [usize; 6],
    items: &mut [usize; 6],
    opaque_reasoning_items: &mut usize,
) {
    let reasoning_chars = count_reasoning_chars(value);
    chars[REASONING] = chars[REASONING].saturating_add(reasoning_chars);
    items[REASONING] = items[REASONING].saturating_add(1);
    if reasoning_chars == 0 && has_opaque_reasoning(value) {
        *opaque_reasoning_items = opaque_reasoning_items.saturating_add(1);
    }
}

fn count_tool_call(value: &Value, chars: &mut [usize; 6], items: &mut [usize; 6]) {
    chars[TOOL_CALLS] = chars[TOOL_CALLS].saturating_add(count_tool_call_chars(value));
    items[TOOL_CALLS] = items[TOOL_CALLS].saturating_add(1);
}

fn count_tool_result(value: &Value, chars: &mut [usize; 6], items: &mut [usize; 6]) {
    chars[TOOL_RESULTS] = chars[TOOL_RESULTS].saturating_add(count_tool_result_chars(value));
    items[TOOL_RESULTS] = items[TOOL_RESULTS].saturating_add(1);
}

fn role_bucket(role: &str) -> usize {
    if matches!(role, "system" | "developer") {
        SYSTEM
    } else {
        CONVERSATION
    }
}

fn is_tool_call_type(value: &str) -> bool {
    matches!(
        value,
        "tool_use" | "function_call" | "custom_tool_call" | "local_shell_call"
    ) || value.ends_with("_call")
}

fn is_tool_result_type(value: &str) -> bool {
    matches!(
        value,
        "tool_result" | "function_call_output" | "custom_tool_call_output"
    ) || value.ends_with("_output")
        || value.ends_with("_result")
}

fn is_reasoning_type(value: &str) -> bool {
    matches!(
        value,
        "reasoning" | "thinking" | "redacted_thinking" | "reasoning_text" | "summary_text"
    )
}

fn is_media_type(value: &str) -> bool {
    matches!(
        value,
        "image"
            | "input_image"
            | "image_url"
            | "file"
            | "input_file"
            | "document"
            | "audio"
            | "input_audio"
            | "video"
    )
}

fn count_tool_call_chars(value: &Value) -> usize {
    let function = value.get("function").unwrap_or(value);
    let mut chars = function
        .get("name")
        .or_else(|| value.get("name"))
        .map(|name| count_text_chars(name, None))
        .unwrap_or_default();
    for key in ["arguments", "input", "action", "code", "query"] {
        if let Some(argument) = function.get(key).or_else(|| value.get(key)) {
            chars = chars.saturating_add(count_all_string_chars(argument));
        }
    }
    chars
}

fn count_tool_result_chars(value: &Value) -> usize {
    ["content", "output"]
        .iter()
        .filter_map(|key| value.get(key))
        .map(|content| count_text_chars(content, None))
        .sum()
}

fn count_reasoning_chars(value: &Value) -> usize {
    if let Some(text) = value.as_str() {
        return text.chars().count();
    }
    ["content", "summary", "reasoning_text", "thinking", "text"]
        .iter()
        .filter_map(|key| value.get(key))
        .map(|content| count_text_chars(content, None))
        .sum()
}

fn has_opaque_reasoning(value: &Value) -> bool {
    value
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty())
        || value
            .get("data")
            .and_then(Value::as_str)
            .is_some_and(|data| !data.is_empty())
}

fn count_all_string_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items.iter().map(count_all_string_chars).sum(),
        Value::Object(map) => map.values().map(count_all_string_chars).sum(),
        _ => 0,
    }
}

fn allocate_categories(
    chars: [usize; 6],
    total_tokens: i64,
    counts: [usize; 6],
) -> Vec<ContextCategory> {
    let character_total = chars.iter().sum::<usize>();
    if character_total == 0 {
        let fallback = [REASONING, TOOL_CALLS, CONVERSATION]
            .into_iter()
            .find(|index| counts[*index] > 0)
            .unwrap_or(CONVERSATION);
        return CATEGORY_KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| ContextCategory {
                key: (*key).to_string(),
                tokens: if index == fallback { total_tokens } else { 0 },
                items: counts[index],
            })
            .collect();
    }
    let mut allocated = 0i64;
    CATEGORY_KEYS
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let tokens = if index + 1 == CATEGORY_KEYS.len() {
                total_tokens.saturating_sub(allocated)
            } else {
                ((chars[index] as u128 * total_tokens as u128) / character_total as u128) as i64
            };
            allocated = allocated.saturating_add(tokens);
            ContextCategory {
                key: (*key).to_string(),
                tokens,
                items: counts[index],
            }
        })
        .collect()
}

fn count_unmetered_items(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_unmetered_items).sum(),
        Value::Object(map) => {
            let kind = map.get("type").and_then(Value::as_str).unwrap_or_default();
            if is_media_type(kind) {
                return 1;
            }
            if is_tool_call_type(kind)
                || is_tool_result_type(kind)
                || is_reasoning_type(kind)
                || matches!(
                    kind,
                    "text"
                        | "input_text"
                        | "output_text"
                        | "message"
                        | "refusal"
                        | "function_call"
                        | "function_call_output"
                )
            {
                return ["content", "output", "summary"]
                    .iter()
                    .filter_map(|key| map.get(*key))
                    .map(count_unmetered_items)
                    .sum();
            }
            if !map.contains_key("type")
                && !map.contains_key("role")
                && !map.contains_key("content")
                && !map.contains_key("output")
            {
                return 1;
            }
            if kind.is_empty() {
                return ["content", "output", "summary"]
                    .iter()
                    .filter_map(|key| map.get(*key))
                    .map(count_unmetered_items)
                    .sum();
            }
            1
        }
        _ => 0,
    }
}

fn count_request_unmetered_items(body: &Value) -> usize {
    ["instructions", "system", "messages", "input"]
        .iter()
        .filter_map(|key| body.get(*key))
        .map(count_unmetered_items)
        .sum()
}

#[cfg(test)]
#[path = "../../../tests/unit/proxy/usage_context.rs"]
mod tests;
