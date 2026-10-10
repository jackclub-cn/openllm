use super::*;

/// Exposes the gateway request ID so clients can correlate a response with the
/// request-log row even when the upstream response is streamed or failed.
pub(crate) fn with_gateway_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response
            .headers_mut()
            .insert("x-openllm-request-id", value.clone());
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

pub(crate) fn apply_routing_headers(
    response: &mut Response,
    attempts: usize,
    fallback_count: usize,
    provider_name: &str,
    upstream_model: &str,
) {
    let mut insert = |name: &'static str, value: String| {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    };
    insert("x-openllm-routing-attempts", attempts.to_string());
    insert("x-openllm-fallback-count", fallback_count.to_string());
    insert("x-openllm-routed-provider", provider_name.to_string());
    insert("x-openllm-routed-model", upstream_model.to_string());
}

/// Reports the applied cost ceiling so a caller can prove the budget took
/// effect instead of inferring it from the chosen provider.
pub(crate) fn apply_budget_headers(
    response: &mut Response,
    budget_usd: Option<f64>,
    removed_targets: usize,
) {
    let Some(budget_usd) = budget_usd else {
        return;
    };
    let mut insert = |name: &'static str, value: String| {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    };
    insert(BUDGET_USD_HEADER, format!("{budget_usd:.6}"));
    insert(
        "x-openllm-budget-excluded-targets",
        removed_targets.to_string(),
    );
}

pub(crate) async fn proxy_openai(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let capacity = acquire_request_capacity(&state);
    if let RequestCapacity::Overloaded = &capacity {
        let response = overloaded_response(state.request_capacity_limit, false);
        return with_gateway_request_id(response, &request_id);
    }
    let result = proxy_openai_inner(&state, &headers, &uri, &body, &request_id, None).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let permit = match capacity {
        RequestCapacity::Acquired(permit) => Some(permit),
        _ => None,
    };
    with_gateway_request_id(attach_request_capacity(response, permit), &request_id)
}

pub(crate) async fn proxy_openai_console(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let capacity = acquire_request_capacity(&state);
    if let RequestCapacity::Overloaded = &capacity {
        let response = overloaded_response(state.request_capacity_limit, false);
        return with_gateway_request_id(response, &request_id);
    }
    let result = proxy_openai_console_inner(&state, &headers, &body, &request_id).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let permit = match capacity {
        RequestCapacity::Acquired(permit) => Some(permit),
        _ => None,
    };
    with_gateway_request_id(attach_request_capacity(response, permit), &request_id)
}

pub(crate) async fn proxy_openai_console_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: &str,
) -> AppResult<Response> {
    let api_key = selected_console_api_key(state, headers).await?;
    let uri = Uri::from_static(OPENAI_CHAT_COMPLETIONS);
    proxy_openai_inner(state, headers, &uri, body, request_id, api_key).await
}

pub(crate) async fn proxy_openai_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    api_key_override: Option<ApiKeyRecord>,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let mut request_json: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&request_json)?;
    let session_id = upstream_session_id(headers, &request_json);
    let streamed = request_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = match api_key_override {
        Some(api_key) => Some(api_key),
        None => authenticate_gateway(state, headers).await?,
    };
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
    // Barrel mode: clamp the requested output length to the strictest common
    // ceiling across every target, so no target is picked that would reject the
    // request as too large.
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
                &request_json,
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
    // The most recent rate-limit failure, kept separate so a later non-429
    // failure supersedes it instead of the client seeing a stale 429.
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
        last_target = Some((target_provider_id, target_upstream_model.clone()));
        if target.provider_type == "anthropic"
            && endpoint != OPENAI_CHAT_COMPLETIONS
            && endpoint != OPENAI_COMPLETIONS
            && endpoint != OPENAI_RESPONSES
        {
            last_rate_limit = None;
            last_error = Some(AppError::Upstream(format!(
                "{} does not support the {} endpoint",
                target.provider_name, endpoint
            )));
            continue;
        }
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
        let upstream_slots = match acquire_upstream_slots(state, &target).await {
            Ok(slots) => slots,
            Err(error) => {
                let provider_exhausted = error.provider_exhausted();
                let error = error.into_error();
                if provider_exhausted {
                    capacity_exhausted_providers.insert(target_provider_id);
                }
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
        match forward_to_target(
            state,
            request_id,
            &endpoint,
            &requested_model,
            &request_json,
            body,
            session_id.as_deref(),
            target,
            streamed,
            request_tokens,
            api_key.as_ref(),
            started,
            receipt.clone(),
        )
        .await
        {
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
                return Ok(attach_provider_slot(response, upstream_slots, state.sse_keepalive));
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

pub(crate) fn build_upstream_request(
    state: &AppState,
    url: &str,
    provider_type: ProviderType,
    target: &RouteTarget,
    request_body: &Value,
    session_id: Option<&str>,
) -> AppResult<RequestBuilder> {
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(request_body);

    request = match provider_type {
        ProviderType::Anthropic => {
            if let Some(key) = &target.api_key {
                request = request
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
            request
        }
        _ => {
            if let Some(key) = &target.api_key {
                request = request.bearer_auth(key);
            }
            request
        }
    };
    request = apply_opencode_session_header(request, target, session_id);
    apply_custom_headers(request, &target.provider_headers)
}
