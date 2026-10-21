use super::*;

/// Anthropic token-counting endpoint (`POST /v1/messages/count_tokens`).
///
/// Claude Code calls this before sending a request to decide how much context
/// remains, so its absence breaks the client outright. Most OpenAI-compatible
/// upstreams have no equivalent (verified: CommandCode returns 404), so this
/// answers locally with an estimate rather than proxying.
///
/// The estimate is deliberately deterministic: the same body always yields the
/// same count, which clients rely on to detect real context growth.
pub(crate) async fn count_tokens_anthropic(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match count_tokens_inner(&state, &headers, &body).await {
        Ok(response) => response,
        Err(error) => anthropic_error_response(error),
    }
}

pub(crate) async fn count_tokens_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
) -> AppResult<Response> {
    let inbound: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    // `model` is required by the real endpoint; reject early rather than
    // silently counting a body the client meant for a different model.
    let requested_model = requested_model_of(&inbound)?;
    let api_key = authenticate_gateway(state, headers).await?;
    enforce_api_key_model_access(api_key.as_ref(), &requested_model)?;
    let input_tokens = estimate_request_tokens(&inbound);
    let guardrails = state.guardrail_settings().await?;
    enforce_request_guardrails(&guardrails, &inbound, input_tokens)?;
    resolve_route(state, &requested_model, ANTHROPIC_MESSAGES).await?;

    Ok(Json(json!({"input_tokens": input_tokens})).into_response())
}

/// Inbound entry point for the Anthropic Messages API (`POST /v1/messages`).
///
/// Anthropic-protocol clients (Claude Code, the Anthropic SDKs) can point their
/// base URL at this gateway. The request is converted into the gateway's
/// internal OpenAI shape so it reuses routing, barrel clamping and capability
/// headers, then the answer is converted back into Anthropic's message and
/// event protocol. Native Anthropic targets skip the conversion entirely.
pub(crate) async fn proxy_anthropic(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let result = proxy_anthropic_inner(&state, &headers, &uri, &body, &request_id).await;
    finish_activity_for_pre_response(&state, &request_id, &result);
    let response = match result {
        Ok(response) => response,
        Err(error) => anthropic_error_response(error),
    };
    with_gateway_request_id(response, &request_id)
}

/// Renders a gateway error in Anthropic's envelope and status vocabulary so an
/// Anthropic client can parse it with its usual error handling.
pub(crate) fn anthropic_error_response(error: AppError) -> Response {
    let retry_after = match &error {
        AppError::UpstreamStatus { retry_after, .. } => *retry_after,
        _ => None,
    };
    let (status, error_type) = match &error {
        AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
        AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
        AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "permission_error"),
        AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
        AppError::Conflict(_) => (StatusCode::CONFLICT, "invalid_request_error"),
        AppError::TooManyRequests(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "api_error"),
        AppError::UpstreamStatus { status, .. } => (
            *status,
            if *status == StatusCode::TOO_MANY_REQUESTS {
                "rate_limit_error"
            } else {
                "api_error"
            },
        ),
        AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "api_error")
        }
    };
    if status.is_server_error() {
        tracing::error!(error = %error, "anthropic request failed");
    }
    let mut response = (
        status,
        Json(anthropic_error_body(error_type, &error.to_string())),
    )
        .into_response();
    if let Some(retry_after) = retry_after {
        let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
        if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
            response
                .headers_mut()
                .insert(reqwest::header::RETRY_AFTER, value);
        }
    }
    response
}

#[derive(Clone)]
pub(super) struct AnthropicAttemptContext {
    pub(super) request_id: String,
    pub(super) endpoint: String,
    pub(super) inbound: Value,
    pub(super) request_json: Value,
    pub(super) requested_model: String,
    pub(super) session_id: Option<String>,
    pub(super) affinity_key: Option<String>,
    pub(super) streamed: bool,
    pub(super) request_tokens: i64,
    pub(super) api_key: Option<ApiKeyRecord>,
    pub(super) route_id: Option<i64>,
    pub(super) ordering_key: i64,
    pub(super) retry_targets: Vec<RouteTarget>,
    pub(super) resolved: ResolvedRoute,
    pub(super) budget_usd: Option<f64>,
    pub(super) budget_excluded_targets: usize,
    pub(super) receipt: Option<Value>,
    pub(super) routing_overrides: RequestRoutingOverrides,
    pub(super) started: Instant,
    pub(super) anthropic_beta: Option<String>,
    pub(super) request_deadline: Option<Instant>,
    pub(super) busy_wait_deadline: Option<Instant>,
}

pub(super) enum AnthropicAttemptOutcome {
    Response(Response),
    BusyWait(Box<super::route_trace::AttemptResume>),
}
