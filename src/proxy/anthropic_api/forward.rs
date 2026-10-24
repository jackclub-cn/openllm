use super::*;

/// Forwards an Anthropic-shaped request to a native Anthropic provider.
///
/// The upstream already speaks the caller's protocol, so the response can be
/// streamed straight back with no translation.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_anthropic_native(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    mut body: Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
    anthropic_beta: Option<String>,
    session_id: Option<&str>,
) -> AppResult<Response> {
    apply_upstream_compat(
        state,
        request_id,
        &target,
        ANTHROPIC_MESSAGES,
        UpstreamShape::Anthropic,
        &mut body,
    )
    .await;
    let build = |body: &Value| {
        let mut request = state
            .client
            .post(join_upstream_url(&target.base_url, ANTHROPIC_MESSAGES))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            // Required by the Anthropic protocol regardless of authentication:
            // a keyless self-hosted Anthropic-compatible endpoint still rejects
            // a request that omits it.
            .header("anthropic-version", "2023-06-01")
            .json(body);
        // Feature flags such as prompt caching are opt-in per request via
        // `anthropic-beta`; dropping it silently disables them upstream.
        if let Some(beta) = &anthropic_beta {
            request = request.header("anthropic-beta", beta);
        }
        if let Some(key) = &target.api_key {
            request = request.header("x-api-key", key);
        }
        request = apply_opencode_session_header(request, &target, session_id);
        apply_custom_headers(request, &target.provider_headers)
    };

    let response = match send_provider_request_with_compat_retry(
        state,
        &target,
        api_key.map(|key| key.id),
        request_id,
        ANTHROPIC_MESSAGES,
        UpstreamShape::Anthropic,
        streamed,
        &mut body,
        build,
    )
    .await?
    {
        UpstreamAttempt::Ok(response) => response,
        UpstreamAttempt::Error {
            status,
            headers: response_headers,
            body: response_body,
        } => {
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
            let failure =
                record_upstream_failure(state, &target, status, &response_headers, &message).await;
            if failure.should_retry_next_target() {
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
                    endpoint: ANTHROPIC_MESSAGES,
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
            // Preserve the upstream status and Anthropic-shaped error body.
            return Ok(Response::builder()
                .status(status)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(Body::from(response_body))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
        }
    };
    let status = response.status();

    if streamed {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        return Ok(passthrough_stream_response(
            state.clone(),
            response.into_stream(),
            content_type,
            ANTHROPIC_MESSAGES.to_string(),
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
            receipt,
        ));
    }

    let response_bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let preview = response_preview(&response_bytes);
    let latency_ms = started.elapsed().as_millis() as i64;
    log_usage_detached(
        state.clone(),
        OwnedUsageLogEntry {
            request_id: request_id.to_string(),
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model: requested_model.to_string(),
            upstream_model: Some(target.upstream_model.clone()),
            endpoint: ANTHROPIC_MESSAGES.to_string(),
            usage,
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview,
        },
    )
    .await;
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from(response_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}

/// Forwards an OpenAI-shaped request to an OpenAI-compatible provider and
/// converts the completion back into an Anthropic message.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_openai_as_anthropic(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    request_json: &Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
    session_id: Option<&str>,
) -> AppResult<Response> {
    let upstream_endpoint =
        target_upstream_endpoint(&target, ANTHROPIC_MESSAGES).unwrap_or(OPENAI_CHAT_COMPLETIONS);
    let use_responses = upstream_endpoint == OPENAI_RESPONSES;
    let mut body = if use_responses {
        chat_request_to_responses(request_json, &target.upstream_model, streamed)
    } else {
        let mut body = request_json.clone();
        body["model"] = json!(target.upstream_model);
        body
    };
    let shape = if use_responses {
        UpstreamShape::Responses
    } else {
        UpstreamShape::Chat
    };
    apply_upstream_compat(
        state,
        request_id,
        &target,
        upstream_endpoint,
        shape,
        &mut body,
    )
    .await;
    let url = join_upstream_url(&target.base_url, upstream_endpoint);
    let build = |body: &Value| {
        let mut request = state
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        if let Some(key) = &target.api_key {
            request = request.bearer_auth(key);
        }
        request = apply_opencode_session_header(request, &target, session_id);
        apply_custom_headers(request, &target.provider_headers)
    };

    let response = match send_provider_request_with_compat_retry(
        state,
        &target,
        api_key.map(|key| key.id),
        request_id,
        upstream_endpoint,
        shape,
        streamed,
        &mut body,
        build,
    )
    .await?
    {
        UpstreamAttempt::Ok(response) => response,
        UpstreamAttempt::Error {
            status,
            headers: response_headers,
            body: response_body,
        } => {
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
            let failure =
                record_upstream_failure(state, &target, status, &response_headers, &message).await;
            if failure.should_retry_next_target() {
                return Err(upstream_failure_error(
                    status,
                    format!("{} returned {}: {}", target.provider_name, status, message),
                    retry_after_from_headers(&response_headers),
                ));
            }
            // Non-retryable: surface the upstream failure verbatim so the operator
            // sees the provider's own message rather than a generic gateway error.
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
                    endpoint: ANTHROPIC_MESSAGES,
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
            return Ok(Response::builder()
                .status(status)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&anthropic_error_body("api_error", &message))
                        .unwrap_or_default(),
                ))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
        }
    };
    let status = response.status();

    if streamed {
        if use_responses {
            return Ok(responses_stream_to_anthropic(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt.clone(),
            ));
        }
        return Ok(openai_stream_to_anthropic(
            state.clone(),
            response.into_stream(),
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
            receipt.clone(),
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let upstream: Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Upstream(format!("invalid JSON from upstream: {error}")))?;
    let (converted, usage) = if use_responses {
        responses_response_to_anthropic(&upstream, requested_model)
    } else {
        openai_response_to_anthropic(&upstream, requested_model)
    };
    let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
    let preview = response_preview(converted_bytes.as_slice());
    let latency_ms = started.elapsed().as_millis() as i64;
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model,
            upstream_model: Some(&target.upstream_model),
            endpoint: ANTHROPIC_MESSAGES,
            usage: fill_usage(usage, request_tokens, &converted),
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview.as_deref(),
        },
    )
    .await;
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from(converted_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}
