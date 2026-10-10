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
