use super::*;

/// Builds the error used when a retryable upstream failure should move on to
/// the next route target.
///
/// A provider rate limit keeps its status and `Retry-After` all the way through
/// the fallback loop, so that when *every* target is rate limited the client
/// still receives a `429` it can back off from instead of an opaque `502`.
pub(crate) fn upstream_failure_error(
    status: StatusCode,
    message: String,
    retry_after: Option<Duration>,
) -> AppError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        AppError::UpstreamStatus {
            status,
            message,
            retry_after,
        }
    } else {
        AppError::Upstream(message)
    }
}

/// Logged HTTP status for the final failure of a fallback loop.
///
/// Mirrors how `AppError` renders so the usage row matches the response: a
/// carried `UpstreamStatus` keeps its own code (a `429` rate limit or a `504`
/// budget timeout), and everything else logs `502` like the client sees.
pub(crate) fn finalized_fallback_status(error: &AppError) -> i64 {
    match error {
        AppError::UpstreamStatus { status, .. } => status.as_u16() as i64,
        _ => 502,
    }
}

pub(crate) fn upstream_rejects_tool_search(body: &[u8]) -> bool {
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    if !message.contains("tool_search") {
        return false;
    }
    if [
        "unknown tool type",
        "unsupported tool type",
        "invalid tool type",
        "tool_search is not supported",
        "tool_search not supported",
        "tool_search is unsupported",
        "does not support tool_search",
        "doesn't support tool_search",
        "unsupported tool_search",
        "unknown tool: tool_search",
        "unsupported tool: tool_search",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return true;
    }
    message.contains("tool.type")
        && [
            "unknown",
            "unsupported",
            "invalid",
            "not valid",
            "not supported",
        ]
        .iter()
        .any(|marker| message.contains(marker))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upstream_error_response(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    target: &RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    status: StatusCode,
    response_headers: HeaderMap,
    response_body: Bytes,
) -> AppResult<Response> {
    let message = String::from_utf8_lossy(&response_body)
        .chars()
        .take(600)
        .collect::<String>();
    let reported_status = status;
    let status = effective_upstream_status(status, &response_body);
    if status != reported_status {
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            %reported_status,
            %status,
            "upstream reported quota exhaustion with a non-retryable status; treating it as a rate limit"
        );
    }

    let failure = record_upstream_failure(state, target, status, &response_headers, &message).await;

    if failure.should_retry_next_target() {
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            %status,
            reason = failure.reason_code,
            "upstream failed, trying next route target"
        );
        return Err(upstream_failure_error(
            status,
            format!("{} returned {}: {}", target.provider_name, status, message),
            retry_after_from_headers(&response_headers),
        ));
    }

    let preview = response_preview(&response_body);
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model,
            upstream_model: Some(&target.upstream_model),
            endpoint,
            usage: Usage::new(request_tokens, 0),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: status.as_u16() as i64,
            success: false,
            streamed,
            error_message: Some(&message),
            response_preview: preview.as_deref(),
        },
    )
    .await;

    let mut builder = Response::builder().status(status);
    if let Some(value) = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        builder = builder.header(reqwest::header::CONTENT_TYPE, value);
    }
    Ok(builder
        .body(Body::from(response_body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()))
}

#[cfg(test)]
pub(crate) fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// Whether a response proves the provider itself is reachable.
///
/// Client errors, authentication failures and rate limits all show that the
/// upstream answered, so they can close a half-open circuit even though they
/// still fail the current request or cool a specific key/model. Timeouts and
/// 5xx responses remain provider-level failures.
pub(crate) fn provider_reachable_status(status: StatusCode) -> bool {
    !matches!(status.as_u16(), 408 | 500..=599)
}

/// Statuses worth one more attempt against the *same* target before the request
/// falls back to another route target.
///
/// Rate limits (`429`) are deliberately excluded: the provider just refused the
/// call, and an immediate retry would spend quota instead of letting the cooldown
/// and fallback machinery work. `409` is excluded too because it is usually a
/// client-side state conflict rather than a blip.
pub(crate) fn retryable_same_target_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 500..=599)
}

/// A transport fault that another attempt could plausibly survive.
///
/// Builder errors (bad URL, unsupported scheme) are permanent, so they are not
/// retried; connection, timeout, and body faults are.
pub(crate) fn retryable_transport_error(error: &reqwest::Error) -> bool {
    !error.is_builder()
        && (error.is_connect() || error.is_timeout() || error.is_request() || error.is_body())
}

/// Detects the opaque 4xx that aggregator upstreams return when one of their
/// internal channels fails: an "invalid request error" that carries only a
/// trace id and no actionable `param`. These are transient routing failures, so
/// retrying is worthwhile instead of surfacing them as client errors.
pub(crate) fn transient_upstream_4xx(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("trace_id") && text.contains("invalid request") && !text.contains("\"param\"")
}

/// Detects a thinking-history compatibility error that should move to another
/// route target instead of being surfaced as a client error.
///
/// DeepSeek can reject a request even after the gateway has repaired the
/// known Responses, Chat, and Anthropic shapes, for example when a client
/// sends a new variation of a tool-call step. The provider error itself is a
/// 400, but the request is not malformed for the client; another target may
/// still serve it.
pub(crate) fn compat_thinking_history_error(status: StatusCode, body: &[u8]) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("thinking mode")
        && text.contains("passed back")
        && (text.contains("reasoning_text") || text.contains("content[].thinking"))
}

/// Corrects an upstream that reports quota exhaustion with a non-retryable
/// status.
///
/// Several OpenAI-compatible gateways return `403` or `400` (and occasionally
/// `402`) when the account behind the key is out of credits. Treating that as a
/// permanent client error stops the fallback loop and surfaces an auth-shaped
/// failure to the caller, even though another route target may still serve the
/// request. The corrected status makes the existing cooldown and fallback
/// machinery handle it exactly like an upstream `429`.
pub(crate) fn effective_upstream_status(status: StatusCode, body: &[u8]) -> StatusCode {
    if status == StatusCode::PAYMENT_REQUIRED {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    if !matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN) {
        return status;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if quota_exhaustion_marker(&text) {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        status
    }
}

pub(crate) fn upstream_trace_id(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let message = value.pointer("/error/message")?.as_str()?;
    let start = message.to_ascii_lowercase().find("trace_id:")? + "trace_id:".len();
    let trace = message[start..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect::<String>();
    (!trace.is_empty()).then_some(trace)
}

#[cfg(test)]
pub(crate) fn should_try_next_target(target: &RouteTarget, status: StatusCode) -> bool {
    classify_upstream_failure(target, status, &[]).should_retry_next_target()
}
