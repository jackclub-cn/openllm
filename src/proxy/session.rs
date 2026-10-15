use super::*;

/// Reads the caller's `anthropic-beta` feature flags, if any.
///
/// Anthropic gates capabilities such as prompt caching behind this header, so
/// it must reach a native upstream verbatim; the gateway never invents its own
/// value.
pub(crate) fn anthropic_beta_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Resolves a stable conversation identifier for OpenCode Go.
///
/// OpenCode clients send `x-opencode-session` directly. Codex sends its native
/// `session-id` and `thread-id` headers, while some proxies and Responses
/// clients only preserve the stable prompt cache key in the JSON body.
pub(crate) fn upstream_session_id(headers: &HeaderMap, body: &Value) -> Option<String> {
    for name in [
        OPENCODE_SESSION_HEADER,
        "session-id",
        "thread-id",
        "x-session-id",
        "x-claude-code-session-id",
    ] {
        if let Some(value) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.chars().take(SESSION_ID_MAX_CHARS).collect());
        }
    }

    for pointer in [
        "/prompt_cache_key",
        "/client_metadata/session_id",
        "/metadata/session_id",
    ] {
        if let Some(value) = body
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.chars().take(SESSION_ID_MAX_CHARS).collect());
        }
    }

    None
}

/// Stable routing affinity key for a request.
///
/// Explicit client session identifiers always win. When a client does not
/// send one, a sufficiently long first prompt prefix is hashed as a fallback.
/// The hash is process-local routing input and is never written as raw prompt
/// text; the database mirror stores a second scope hash as well.
pub(crate) fn session_affinity_key(headers: &HeaderMap, body: &Value) -> Option<String> {
    if let Some(session_id) = upstream_session_id(headers, body) {
        return Some(session_id);
    }
    let text = prompt_affinity_text(body)?;
    if text.chars().count() < PROMPT_AFFINITY_MIN_CHARS {
        return None;
    }
    let bounded = text.chars().take(PROMPT_AFFINITY_MAX_CHARS).collect::<String>();
    let mut hasher = Sha256::new();
    hasher.update(b"openllm-prompt-affinity-v1");
    hasher.update(bounded.as_bytes());
    Some(format!("prompt:sha256:{:x}", hasher.finalize()))
}

const PROMPT_AFFINITY_MIN_CHARS: usize = 64;
const PROMPT_AFFINITY_MAX_CHARS: usize = 4096;

fn prompt_affinity_text(body: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(system) = body.get("system").and_then(affinity_text) {
        parts.push(system);
    }
    if let Some(instructions) = body.get("instructions").and_then(affinity_text) {
        parts.push(instructions);
    }
    if let Some(system) = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find(|message| {
                matches!(
                    message.get("role").and_then(Value::as_str),
                    Some("system" | "developer")
                )
            })
        })
        .and_then(|message| message.get("content").or(Some(message)))
        .and_then(affinity_text)
    {
        parts.push(system);
    }

    let conversation = if let Some(input) = body.get("input") {
        first_affinity_input_text(input)
    } else if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        messages
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .or_else(|| messages.first())
            .and_then(|message| message.get("content").or(Some(message)))
            .and_then(affinity_text)
    } else {
        body.get("prompt").and_then(affinity_text)
    };
    if let Some(conversation) = conversation {
        parts.push(conversation);
    }

    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn first_affinity_input_text(input: &Value) -> Option<String> {
    match input {
        Value::Array(items) => items
            .iter()
            .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
            .or_else(|| items.first())
            .and_then(|item| item.get("content").or(Some(item)))
            .and_then(affinity_text),
        value => affinity_text(value),
    }
}

fn affinity_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    if let Some(content) = value.get("content") {
        let text = content_text(content)?;
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    let text = content_text(value)?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub(crate) fn is_opencode_go_target(target: &RouteTarget) -> bool {
    let provider_name = target.provider_name.to_ascii_lowercase();
    let model_prefix = target.model_prefix.trim().trim_end_matches('/');
    let base_url = target.base_url.to_ascii_lowercase();
    provider_name.contains("opencode go")
        || model_prefix.eq_ignore_ascii_case("opencode-go")
        || base_url.contains("opencode.ai/zen/go")
}

pub(crate) fn custom_headers_contain(name: &str, headers: &str) -> bool {
    serde_json::from_str::<Value>(headers)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|headers| headers.keys().any(|key| key.eq_ignore_ascii_case(name)))
}

pub(crate) fn apply_opencode_session_header(
    request: RequestBuilder,
    target: &RouteTarget,
    session_id: Option<&str>,
) -> RequestBuilder {
    let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) else {
        return request;
    };
    if !is_opencode_go_target(target)
        || custom_headers_contain(OPENCODE_SESSION_HEADER, &target.provider_headers)
    {
        return request;
    }
    request.header(OPENCODE_SESSION_HEADER, session_id)
}
