use super::*;
use crate::models::{ContextBreakdown, ContextCategory};

pub(crate) fn request_context_breakdown(
    body: &Value,
    endpoint: &str,
    estimated_input_tokens: i64,
) -> ContextBreakdown {
    let mut chars = [0usize; 4];
    if let Some(system) = body.get("system").or_else(|| body.get("instructions")) {
        chars[0] = chars[0].saturating_add(count_text_chars(system, None));
    }
    for tools_key in ["tools", "functions"] {
        if let Some(tools) = body.get(tools_key) {
            chars[1] = chars[1].saturating_add(count_text_chars(tools, None));
        }
    }
    if endpoint == ANTHROPIC_MESSAGES {
        count_anthropic_messages(body, &mut chars);
    } else if endpoint == OPENAI_RESPONSES {
        count_responses_input(body, &mut chars);
    } else {
        count_chat_messages(body, &mut chars);
        if let Some(prompt) = body.get("prompt") {
            chars[3] = chars[3].saturating_add(count_text_chars(prompt, None));
        }
    }
    ContextBreakdown {
        window_tokens: None,
        input_tokens: estimated_input_tokens.max(0),
        estimated: true,
        input_estimated: true,
        categories: allocate_categories(
            chars,
            estimated_input_tokens.max(0),
            context_item_counts(body),
        ),
        unmetered_items: body
            .get("messages")
            .or_else(|| body.get("input"))
            .map(count_unmetered_items)
            .unwrap_or_default(),
        ..Default::default()
    }
}

fn count_anthropic_messages(body: &Value, chars: &mut [usize; 4]) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(content) = message.get("content") else {
            continue;
        };
        if let Some(blocks) = content.as_array() {
            for block in blocks {
                let block_type = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if block_type == "tool_result" {
                    chars[2] = chars[2].saturating_add(count_text_chars(block, None));
                } else if matches!(role, "system" | "developer") {
                    chars[0] = chars[0].saturating_add(count_text_chars(block, None));
                } else {
                    chars[3] = chars[3].saturating_add(count_text_chars(block, None));
                }
            }
        } else if matches!(role, "system" | "developer") {
            chars[0] = chars[0].saturating_add(count_text_chars(content, None));
        } else {
            chars[3] = chars[3].saturating_add(count_text_chars(content, None));
        }
    }
}

fn count_responses_input(body: &Value, chars: &mut [usize; 4]) {
    let Some(input) = body.get("input") else {
        return;
    };
    let Some(items) = input.as_array() else {
        chars[3] = chars[3].saturating_add(count_text_chars(input, None));
        return;
    };
    for item in items {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
        let bucket = if item_type == "function_call_output" || item_type == "tool_result" {
            2
        } else if matches!(role, "system" | "developer") {
            0
        } else {
            3
        };
        let text = if item_type == "function_call_output" {
            item.get("output")
                .map(|output| count_text_chars(output, None))
                .unwrap_or(0)
        } else {
            count_text_chars(item, None)
        };
        chars[bucket] = chars[bucket].saturating_add(text);
    }
}

fn count_chat_messages(body: &Value, chars: &mut [usize; 4]) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "tool" {
            chars[2] = chars[2].saturating_add(count_text_chars(message, None));
            continue;
        }
        if matches!(role, "system" | "developer") {
            chars[0] = chars[0].saturating_add(count_text_chars(message, None));
            continue;
        }
        if let Some(content) = message.get("content") {
            if let Some(blocks) = content.as_array() {
                for block in blocks {
                    let block_type = block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let bucket = if block_type == "tool_result" { 2 } else { 3 };
                    chars[bucket] = chars[bucket].saturating_add(count_text_chars(block, None));
                }
            } else {
                chars[3] = chars[3].saturating_add(count_text_chars(content, None));
            }
        }
        if let Some(tool_calls) = message.get("tool_calls") {
            chars[3] = chars[3].saturating_add(count_text_chars(tool_calls, None));
        }
    }
}

fn allocate_categories(
    chars: [usize; 4],
    total_tokens: i64,
    counts: [usize; 4],
) -> Vec<ContextCategory> {
    const KEYS: [&str; 4] = ["system", "tools", "tool_results", "conversation"];
    let character_total = chars.iter().sum::<usize>();
    if character_total == 0 {
        return KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| ContextCategory {
                key: (*key).to_string(),
                tokens: if *key == "conversation" {
                    total_tokens
                } else {
                    0
                },
                items: counts[index],
            })
            .collect();
    }
    let mut allocated = 0i64;
    KEYS.iter()
        .enumerate()
        .map(|(index, key)| {
            let tokens = if index + 1 == KEYS.len() {
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

fn context_item_counts(body: &Value) -> [usize; 4] {
    let mut counts = [0; 4];
    counts[0] = usize::from(
        body.get("system")
            .or_else(|| body.get("instructions"))
            .is_some(),
    );
    counts[1] = ["tools", "functions"]
        .iter()
        .filter_map(|key| body.get(key)?.as_array())
        .map(Vec::len)
        .sum();
    if let Some(items) = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
    {
        for item in items {
            let bucket = if matches!(
                item.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            ) {
                0
            } else if item.get("role").and_then(Value::as_str) == Some("tool")
                || item.get("type").and_then(Value::as_str) == Some("function_call_output")
            {
                2
            } else {
                3
            };
            if let Some(blocks) = item.get("content").and_then(Value::as_array) {
                for block in blocks {
                    let index = if block.get("type").and_then(Value::as_str) == Some("tool_result")
                    {
                        2
                    } else {
                        bucket
                    };
                    counts[index] += 1;
                }
            } else {
                counts[bucket] += 1;
            }
        }
    } else if body.get("input").or_else(|| body.get("prompt")).is_some() {
        counts[3] += 1;
    }
    counts
}

fn count_unmetered_items(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_unmetered_items).sum(),
        Value::Object(map) => {
            if !map.contains_key("type")
                && !map.contains_key("role")
                && !map.contains_key("content")
                && !map.contains_key("output")
            {
                return 1;
            }
            if let Some(kind) = map.get("type").and_then(Value::as_str)
                && !matches!(
                    kind,
                    "text"
                        | "input_text"
                        | "output_text"
                        | "message"
                        | "tool_result"
                        | "tool_use"
                        | "function_call"
                        | "function_call_output"
                        | "thinking"
                        | "redacted_thinking"
                )
            {
                return 1;
            }
            map.iter()
                .filter(|(key, _)| matches!(key.as_str(), "content" | "output"))
                .map(|(_, value)| count_unmetered_items(value))
                .sum()
        }
        _ => 0,
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/proxy/usage_context.rs"]
mod tests;
