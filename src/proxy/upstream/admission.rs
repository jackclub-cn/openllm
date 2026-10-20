use super::*;

/// Keeps a global admission permit alive until the outbound body completes.
///
/// Error responses finish immediately, so this is a no-op for anything that is
/// not a stream: the permit simply drops when the body is consumed.
pub(crate) fn attach_request_capacity(
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
) -> Response {
    attach_request_guards(response, permit, None)
}

/// Keeps the global admission guards alive until the outbound body completes.
///
/// Both the request-count permit and the in-flight byte guard are moved into
/// the response body, so a long stream holds its slot and its byte reservation
/// for its whole lifetime rather than just until the headers are returned.
pub(crate) fn attach_request_guards(
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
    bytes: Option<crate::state::RequestByteGuard>,
) -> Response {
    if permit.is_none() && bytes.is_none() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let mut stream = body.into_data_stream();
    let guarded = async_stream::stream! {
        let _permit = permit;
        let _bytes = bytes;
        while let Some(chunk) = stream.next().await {
            yield chunk;
        }
    };
    Response::from_parts(parts, Body::from_stream(guarded))
}

/// Builds the `429` returned when the global request cap is saturated.
///
/// `anthropic` selects the Anthropic error envelope so each client family can
/// parse the refusal with its usual error handling.
pub(crate) fn overloaded_response(limit: usize, anthropic: bool) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let message =
        format!("gateway is at its concurrency limit ({limit}); retry after {RETRY_AFTER_SECS}s");
    shed_response(
        &message,
        anthropic,
        Some(limit.to_string()),
        StatusCode::TOO_MANY_REQUESTS,
    )
}

/// Builds the `429` returned when the in-flight request-byte budget is full.
///
/// Separate from [`overloaded_response`] so the message and the advertised
/// limit (bytes, not slots) point an operator at the knob that actually shed
/// the request.
pub(crate) fn body_budget_response(limit_bytes: usize, anthropic: bool) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let message = format!(
        "gateway is at its in-flight request-body budget ({} MiB); retry after {RETRY_AFTER_SECS}s",
        limit_bytes / (1024 * 1024)
    );
    shed_response(
        &message,
        anthropic,
        Some(limit_bytes.to_string()),
        StatusCode::TOO_MANY_REQUESTS,
    )
}

/// Builds the `503` returned while the process is under memory pressure.
///
/// A `503` matches the "temporarily unavailable, retry shortly" contract
/// clients already honor for an overloaded server, and keeps it distinct from
/// the capacity `429`s.
pub(crate) fn memory_pressure_response(anthropic: bool) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let message = format!(
        "gateway is under memory pressure and is shedding new requests; retry after {RETRY_AFTER_SECS}s"
    );
    shed_response(&message, anthropic, None, StatusCode::SERVICE_UNAVAILABLE)
}

/// Renders a retryable refusal in the client's expected envelope.
pub(super) fn shed_response(
    message: &str,
    anthropic: bool,
    limit_header: Option<String>,
    status: StatusCode,
) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let body = if anthropic {
        anthropic_error_body("overloaded_error", message)
    } else {
        let error_type = if status == StatusCode::TOO_MANY_REQUESTS {
            "rate_limit_error"
        } else {
            "server_error"
        };
        json!({"error": {"message": message, "type": error_type, "code": status.as_u16()}})
    };
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&RETRY_AFTER_SECS.to_string()) {
        headers.insert(reqwest::header::RETRY_AFTER, value);
    }
    if let Some(limit_header) = limit_header
        && let Ok(value) = HeaderValue::from_str(&limit_header)
    {
        headers.insert(HeaderName::from_static("x-ratelimit-limit"), value);
    }
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from_static("0"),
    );
    response
}
