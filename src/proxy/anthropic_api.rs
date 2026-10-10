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
    let model_patterns = api_key_model_patterns(api_key.as_ref())?;
    resolve_route_with_patterns(
        state,
        &requested_model,
        ANTHROPIC_MESSAGES,
        model_patterns.as_deref(),
    )
    .await?;

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
    let response = match proxy_anthropic_inner(&state, &headers, &uri, &body, &request_id).await {
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

pub(crate) async fn proxy_anthropic_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let inbound: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&inbound)?;
    // Anthropic requires `max_tokens`; the real API answers 400 with an
    // `invalid_request_error` when it is missing (verified against the live
    // provider). Defaulting silently would hide a client bug and let the
    // request fail later with a less obvious error.
    validate_anthropic_max_tokens(&inbound)?;
    let streamed = inbound
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Convert once up front: routing, barrel clamping and token estimation all
    // operate on the OpenAI shape.
    let mut request_json = anthropic_request_to_openai(&inbound, &requested_model);
    let session_id = upstream_session_id(headers, &inbound);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = authenticate_gateway(state, headers).await?;
    enforce_policy_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    enforce_request_guardrails_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
        &request_json,
        request_tokens,
    )
    .await?;
    let budget_usd = budget_usd_from_headers(headers)?;
    let mut resolved = resolve_route_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    if let Err(error) = enforce_context_capacity(request_tokens, resolved.barrel.as_ref()) {
        let message = error.to_string();
        log_request_rejection(
            state,
            api_key.as_ref(),
            request_id,
            session_id.as_deref(),
            &requested_model,
            &endpoint,
            streamed,
            started,
            400,
            &message,
        )
        .await;
        return Err(error);
    }
    let route_id = resolved.route_id;
    let requested_output_tokens = requested_output_tokens_of(&request_json);
    let clamped_output_tokens = clamp_output_request(&mut request_json, resolved.barrel.as_ref());
    let budget_output_tokens = clamped_output_tokens
        .or(requested_output_tokens)
        .or_else(|| {
            resolved
                .barrel
                .as_ref()
                .and_then(|barrel| barrel.capabilities.as_ref())
                .and_then(|capabilities| capabilities.output_limit)
        })
        .unwrap_or(0);
    let budget_excluded_targets = match budget_usd {
        Some(budget) => match apply_budget_filter(
            &mut resolved.targets,
            budget,
            request_tokens,
            budget_output_tokens,
        ) {
            Ok(removed) => removed,
            Err(error) => {
                let message = error.to_string();
                log_request_rejection(
                    state,
                    api_key.as_ref(),
                    request_id,
                    session_id.as_deref(),
                    &requested_model,
                    &endpoint,
                    streamed,
                    started,
                    400,
                    &message,
                )
                .await;
                return Err(error);
            }
        },
        None => 0,
    };
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    // The API key's stored policy is the default; per-request headers override
    // individual fields. Only the request's own headers are echoed back.
    let routing_overrides = match request_routing_overrides(headers).and_then(|request| {
        let overrides =
            merge_routing_overrides(routing_overrides_from_key(api_key.as_ref()), request.clone());
        apply_routing_overrides(&mut resolved, &requested_model, &overrides)?;
        Ok(request)
    }) {
        Ok(overrides) => overrides,
        Err(error) => {
            let message = error.to_string();
            log_request_rejection(
                state,
                api_key.as_ref(),
                request_id,
                session_id.as_deref(),
                &requested_model,
                &endpoint,
                streamed,
                started,
                400,
                &message,
            )
            .await;
            return Err(error);
        }
    };
    let ordering_key = route_id.unwrap_or(-1);
    let ordered_targets = order_targets(
        state,
        ordering_key,
        &resolved.strategy,
        resolved.targets,
        session_id.as_deref(),
    )
    .await?;
    enforce_api_key_rate_limit_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
        started,
    )
    .await?;
    let inspector = state.inspector_settings().await?;
    let captured_request_preview = inspector
        .capture_request_previews
        .then(|| {
            request_preview(
                &inbound,
                inspector.request_preview_max_chars.max(0) as usize,
            )
        })
        .flatten();
    log_usage_started(
        state,
        request_id,
        session_id.as_deref(),
        api_key.as_ref().map(|key| key.id),
        route_id,
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
        captured_request_preview.as_deref(),
    )
    .await;
    if let (Some(budget), true) = (budget_usd, budget_excluded_targets > 0) {
        log_usage_warning(
            state,
            request_id,
            &format!(
                "budget ${budget:.6} excluded {budget_excluded_targets} target(s) whose estimated cost exceeded the ceiling"
            ),
        )
        .await;
    }

    let mut last_error: Option<AppError> = None;
    // Mirrors the OpenAI path: a provider 429 is surfaced to the client when no
    // target could serve, unless a later non-429 failure supersedes it.
    let mut last_rate_limit: Option<(String, Option<Duration>)> = None;
    let mut last_target = None;
    let mut attempts = 0usize;
    let mut capacity_exhausted_providers = HashSet::new();
    for target in ordered_targets {
        let target_provider_id = target.provider_id;
        if capacity_exhausted_providers.contains(&target_provider_id) {
            continue;
        }
        let target_provider_name = target.provider_name.clone();
        let target_provider_key_id = target.provider_api_key_id;
        let target_upstream_model = target.upstream_model.clone();
        attempts = attempts.saturating_add(1);
        let fallback_count = attempts.saturating_sub(1);
        log_usage_target(
            state,
            request_id,
            target_provider_id,
            &target_upstream_model,
            target_provider_key_id,
        )
        .await;
        mark_provider_api_key_used(state, target_provider_key_id).await;
        last_target = Some((target_provider_id, target_upstream_model.clone()));
        let provider_slot = match acquire_provider_slot(state, &target).await {
            Ok(slot) => slot,
            Err(error) => {
                capacity_exhausted_providers.insert(target_provider_id);
                last_rate_limit = match &error {
                    AppError::UpstreamStatus {
                        message,
                        retry_after,
                        ..
                    } => Some((message.clone(), *retry_after)),
                    _ => None,
                };
                last_error = Some(error);
                continue;
            }
        };
        let result = if target.provider_type == "anthropic" {
            // Native target: forward the caller's Anthropic payload unchanged,
            // only swapping in the resolved upstream model.
            let mut native = inbound.clone();
            native["model"] = json!(target.upstream_model);
            forward_anthropic_native(
                state,
                request_id,
                &requested_model,
                native,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
                anthropic_beta_of(headers),
                session_id.as_deref(),
            )
            .await
        } else {
            forward_openai_as_anthropic(
                state,
                request_id,
                &requested_model,
                &request_json,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
                session_id.as_deref(),
            )
            .await
        };
        match result {
            Ok(mut response) => {
                if response.status().is_success() {
                    mark_provider_success(state, target_provider_id).await;
                    mark_target_success(state, target_provider_id, &target_upstream_model).await;
                    mark_provider_api_key_success(state, target_provider_key_id).await;
                }
                apply_routing_headers(
                    &mut response,
                    attempts,
                    fallback_count,
                    &target_provider_name,
                    &target_upstream_model,
                );
                apply_budget_headers(&mut response, budget_usd, budget_excluded_targets);
                apply_override_headers(&mut response, &routing_overrides);
                return Ok(attach_provider_slot(response, provider_slot));
            }
            Err(error) => {
                last_rate_limit = match &error {
                    AppError::UpstreamStatus {
                        message,
                        retry_after,
                        ..
                    } => Some((message.clone(), *retry_after)),
                    _ => None,
                };
                last_error = Some(error);
            }
        }
    }

    let error = last_error
        .unwrap_or_else(|| AppError::Upstream("all configured route targets failed".to_string()));
    let (status_code, error) = match last_rate_limit {
        Some((message, retry_after)) => (
            StatusCode::TOO_MANY_REQUESTS.as_u16() as i64,
            AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message,
                retry_after,
            },
        ),
        None => (502, error),
    };
    let message = error.to_string();
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.as_ref().map(|key| key.id),
            route_id,
            provider_id: last_target.as_ref().map(|(provider_id, _)| *provider_id),
            requested_model: &requested_model,
            upstream_model: last_target
                .as_ref()
                .map(|(_, upstream_model)| upstream_model.as_str()),
            endpoint: &endpoint,
            usage: Usage::new(request_tokens, 0),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code,
            success: false,
            streamed,
            error_message: Some(&message),
            response_preview: None,
        },
    )
    .await;
    Err(error)
}

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
        request_id,
        ANTHROPIC_MESSAGES,
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
            record_upstream_failure(state, &target, status, &response_headers, &message).await;
            if should_try_next_target(&target, status) {
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
            response,
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
    );
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
    apply_upstream_compat(state, request_id, &target, shape, &mut body).await;
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
        request_id,
        ANTHROPIC_MESSAGES,
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
            record_upstream_failure(state, &target, status, &response_headers, &message).await;
            if should_try_next_target(&target, status) {
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
                response,
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
            response,
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
