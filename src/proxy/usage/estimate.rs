use super::*;

pub(crate) fn estimated_completion_usage(request_tokens: i64, bytes: &[u8]) -> Usage {
    let completion_tokens = (bytes.len() / 4).max(1) as i64;
    Usage::new(request_tokens, completion_tokens)
}

pub(crate) fn estimate_request_tokens(value: &Value) -> i64 {
    // Walk the structured content and sum only text-bearing fields. The
    // previous implementation stringified the whole payload, so JSON keys,
    // braces and quotes inflated the estimate on every request.
    let chars = count_text_chars(value, None);
    // Rough per-item framing overhead: providers wrap each message and tool in
    // a handful of structural tokens beyond its visible text.
    let items = value
        .get("messages")
        .or_else(|| value.get("input"))
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.len())
        .unwrap_or(0);
    let framing = (items as i64) * 4 + (tools as i64) * 8;
    (chars as i64 / 4 + framing).max(1)
}

/// Field names whose string values represent prompt content worth counting.
/// Anything else (ids, roles, media URLs, base64 blobs) is ignored so the
/// estimate tracks text, not wire format.
pub(crate) const TEXT_BEARING_KEYS: [&str; 9] = [
    "text",
    "content",
    "input",
    "prompt",
    "system",
    "description",
    "arguments",
    "partial_json",
    "name",
];

/// Sums the characters of text-bearing strings nested anywhere in `value`.
///
/// A string is counted only when its key is text-bearing, or when it is the
/// root value (as in a bare `input: "..."` payload). This deliberately skips
/// `image_url` / base64 data, which providers meter separately from text.
pub(crate) fn count_text_chars(value: &Value, key: Option<&str>) -> usize {
    match value {
        Value::String(text) => {
            if key.is_none_or(|key| TEXT_BEARING_KEYS.contains(&key)) {
                text.chars().count()
            } else {
                0
            }
        }
        Value::Array(items) => items.iter().map(|item| count_text_chars(item, key)).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| count_text_chars(value, Some(key)))
            .sum(),
        _ => 0,
    }
}

pub(crate) fn fill_usage(usage: Usage, request_tokens: i64, response: &Value) -> Usage {
    let mut usage = usage;
    if usage.prompt_tokens == 0 {
        usage.prompt_tokens = request_tokens;
    }
    if usage.completion_tokens == 0 {
        usage.completion_tokens = (response.to_string().chars().count() / 4).max(1) as i64;
    }
    usage.normalized()
}

pub(crate) fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}
