use super::*;

/// Keep a short, human-readable slice of the response for later debugging
/// without storing unbounded payloads.
pub(crate) fn response_preview(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = trimmed.chars().take(2000).collect::<String>();
    Some(preview)
}

/// Builds a bounded request preview with common credential fields masked.
///
/// Stored previews are opt-in because prompts may contain sensitive data. The
/// masking pass covers the usual secret-bearing keys without touching normal
/// budget fields such as `max_tokens`.
pub(crate) fn request_preview(body: &Value, max_chars: usize) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let masked = mask_sensitive_values(body);
    let text = serde_json::to_string_pretty(&masked).ok()?;
    let preview = text.trim().chars().take(max_chars).collect::<String>();
    (!preview.is_empty()).then_some(preview)
}

pub(super) fn mask_sensitive_values(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = if sensitive_request_key(key) {
                        Value::String("<redacted>".to_string())
                    } else {
                        mask_sensitive_values(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask_sensitive_values).collect()),
        _ => value.clone(),
    }
}

pub(super) fn sensitive_request_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key"
            | "apikey"
            | "authorization"
            | "password"
            | "secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "cookie"
            | "set-cookie"
    ) || key.ends_with("_token")
        || key.ends_with("-token")
}

/// Upper bound on how much streamed text is retained for the preview. Anything
/// beyond this is discarded as it arrives, so a very long generation does not
/// buffer its entire output in memory just to store 2000 characters.
pub(crate) const PREVIEW_CHAR_LIMIT: usize = 2000;

/// Appends `chunk` to `target` only while the preview budget allows it.
pub(crate) fn push_preview_text(target: &mut String, chunk: &str) {
    let remaining = PREVIEW_CHAR_LIMIT.saturating_sub(target.chars().count());
    if remaining == 0 {
        return;
    }
    target.extend(chunk.chars().take(remaining));
}
