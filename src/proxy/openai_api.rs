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

#[derive(Clone)]
struct OpenAiAttemptContext {
    request_id: String,
    endpoint: String,
    request_json: Value,
    requested_model: String,
    body: Bytes,
    session_id: Option<String>,
    affinity_key: Option<String>,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    route_id: Option<i64>,
    ordering_key: i64,
    retry_targets: Vec<RouteTarget>,
    resolved: ResolvedRoute,
    budget_usd: Option<f64>,
    budget_excluded_targets: usize,
    receipt: Option<Value>,
    routing_overrides: RequestRoutingOverrides,
    started: Instant,
    request_deadline: Option<Instant>,
    busy_wait_deadline: Option<Instant>,
}

enum OpenAiAttemptOutcome {
    Response(Response),
    BusyWait,
}

pub(crate) async fn proxy_openai(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let result = proxy_openai_inner(&state, &headers, &uri, &body, &request_id, None).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    with_gateway_request_id(response, &request_id)
}

pub(crate) async fn proxy_openai_console(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let capacity = acquire_request_capacity(&state, state.admission_wait).await;
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
    let affinity_key = session_affinity_key(headers, &request_json);
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
    // A candidate over its provider-pool allocation is dropped before ordering,
    // so the request falls back to a provider with headroom instead of failing.
    // This is a local routing decision: it never marks the provider as failed.
    let allocation = match crate::allocation::apply_allocation_filter(
        state,
        api_key.as_ref(),
        &mut resolved.targets,
    )
    .await
    {
        Ok(outcome) => outcome,
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
                429,
                &message,
            )
            .await;
            return Err(error);
        }
    };
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    // The API key's stored policy is the default; per-request headers override
    // individual fields. Only the request's own headers are echoed back.
    let routing_overrides = match request_routing_overrides(headers).and_then(|request| {
        let overrides = merge_routing_overrides(
            routing_overrides_from_key(api_key.as_ref()),
            request.clone(),
        );
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
    let retry_targets = resolved.targets.clone();
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
    if !allocation.notes.is_empty() {
        log_usage_warning(
            state,
            request_id,
            &format!("provider pool allocation: {}", allocation.notes.join("; ")),
        )
        .await;
    }

    let request_deadline = state.request_deadline();
    let busy_wait_deadline = (resolved.all_targets_busy_wait_seconds > 0).then(|| {
        Instant::now()
            + Duration::from_secs(
                u64::try_from(resolved.all_targets_busy_wait_seconds).unwrap_or(0),
            )
    });
    let context = OpenAiAttemptContext {
        request_id: request_id.to_string(),
        endpoint,
        request_json,
        requested_model,
        body: body.clone(),
        session_id,
        affinity_key,
        streamed,
        request_tokens,
        api_key,
        route_id,
        ordering_key,
        retry_targets,
        resolved,
        budget_usd,
        budget_excluded_targets,
        receipt,
        routing_overrides,
        started,
        request_deadline,
        busy_wait_deadline,
    };
    match run_openai_attempts(state, &context, true).await? {
        OpenAiAttemptOutcome::Response(response) => Ok(response),
        OpenAiAttemptOutcome::BusyWait => {
            let state = state.clone();
            let context = context.clone();
            let interval = state.sse_keepalive.unwrap_or(Duration::from_secs(
                crate::state::DEFAULT_SSE_KEEPALIVE_SECS,
            ));
            Ok(stream_sse_until_response(
                interval,
                SseErrorProtocol::OpenAi,
                async move {
                    match run_openai_attempts(&state, &context, false).await? {
                        OpenAiAttemptOutcome::Response(response) => Ok(response),
                        OpenAiAttemptOutcome::BusyWait => Err(AppError::Internal(anyhow::anyhow!(
                            "busy-wait stream did not resume routing"
                        ))),
                    }
                },
            ))
        }
    }
}

async fn run_openai_attempts(
    state: &AppState,
    context: &OpenAiAttemptContext,
    emit_busy_wait_signal: bool,
) -> AppResult<OpenAiAttemptOutcome> {
    let mut last_error: Option<AppError> = None;
    let mut last_rate_limit: Option<(String, Option<Duration>)> = None;
    let mut last_target: Option<(i64, Option<i64>, String)> = None;
    let mut attempts = 0usize;
    let request_deadline = context.request_deadline;
    let busy_wait_deadline = context.busy_wait_deadline;
    let mut wait_round = 0u32;
    let mut ordered_targets = order_targets(
        state,
        context.ordering_key,
        &context.resolved.strategy,
        context.retry_targets.clone(),
        context.affinity_key.as_deref(),
        context.api_key.as_ref().map(|key| key.id),
    )
    .await?;
    let mut trace = super::route_trace::RouteTraceRecorder::new(
        context.request_id.clone(),
        context.route_id,
        context.resolved.strategy.clone(),
        context.session_id.clone(),
    );
    trace
        .update_candidates(
            state,
            &context.resolved.strategy,
            &ordered_targets,
            context.api_key.as_ref().map(|key| key.id),
        )
        .await;

    loop {
        let mut all_targets_busy = true;
        let mut attempted_any = false;
        let mut next_retry_after: Option<Duration> = None;
        let mut capacity_exhausted_providers = HashSet::new();
        for (candidate_index, target) in ordered_targets.iter().cloned().enumerate() {
            let candidate_order = candidate_index as i64 + 1;
            // Stop *starting* new targets once the budget is spent; an attempt
            // that is already running is never cut short.
            if request_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let elapsed = context.started.elapsed().as_secs();
                last_error = Some(AppError::UpstreamStatus {
                    status: StatusCode::GATEWAY_TIMEOUT,
                    message: format!(
                        "request budget exhausted after {elapsed}s and {attempts} attempt(s); giving up"
                    ),
                    retry_after: None,
                });
                break;
            }
            let target_provider_id = target.provider_id;
            if capacity_exhausted_providers.contains(&target_provider_id) {
                continue;
            }
            let target_provider_name = target.provider_name.clone();
            let target_provider_key_id = target.provider_api_key_id;
            let target_upstream_model = target.upstream_model.clone();
            last_target = Some((
                target_provider_id,
                target_provider_key_id,
                target_upstream_model.clone(),
            ));
            if target.provider_type == "anthropic"
                && context.endpoint != OPENAI_CHAT_COMPLETIONS
                && context.endpoint != OPENAI_COMPLETIONS
                && context.endpoint != OPENAI_RESPONSES
            {
                trace.record_attempt(super::route_trace::TraceAttemptRecord {
                    attempt_index: attempts as i64 + 1,
                    round: wait_round as i64,
                    candidate_order,
                    provider_id: Some(target_provider_id),
                    provider_api_key_id: target_provider_key_id,
                    upstream_model: Some(target_upstream_model.clone()),
                    started_at: Utc::now().to_rfc3339(),
                    latency_ms: 0,
                    status_code: None,
                    success: false,
                    retryable: true,
                    reason_code: "endpoint_unsupported".to_string(),
                    failure_scope: Some("endpoint".to_string()),
                    failure_retry: Some("next_target".to_string()),
                    failure_confidence: Some("exact".to_string()),
                });
                all_targets_busy = false;
                last_rate_limit = None;
                last_error = Some(AppError::Upstream(format!(
                    "{} does not support the {} endpoint",
                    target.provider_name, context.endpoint
                )));
                continue;
            }
            attempts = attempts.saturating_add(1);
            let fallback_count = attempts.saturating_sub(1);
            let attempt_started = Instant::now();

            log_usage_target(
                state,
                &context.request_id,
                target_provider_id,
                &target_upstream_model,
                target_provider_key_id,
            )
            .await;
            mark_provider_api_key_used(state, target_provider_key_id).await;
            attempted_any = true;
            let upstream_slots = match acquire_upstream_slots(state, &target).await {
                Ok(slots) => slots,
                Err(error) => {
                    let provider_exhausted = error.provider_exhausted();
                    let error = error.into_error();
                    let failure = super::route_trace::classify_trace_error(&error);
                    trace.record_attempt(super::route_trace::TraceAttemptRecord {
                        attempt_index: attempts as i64,
                        round: wait_round as i64,
                        candidate_order,
                        provider_id: Some(target_provider_id),
                        provider_api_key_id: target_provider_key_id,
                        upstream_model: Some(target_upstream_model.clone()),
                        started_at: Utc::now().to_rfc3339(),
                        latency_ms: super::route_trace::attempt_elapsed(attempt_started),
                        status_code: failure.status_code,
                        success: false,
                        retryable: failure.retryable,
                        reason_code: failure.reason_code.to_string(),
                        failure_scope: failure.failure_scope.map(str::to_string),
                        failure_retry: failure.failure_retry.map(str::to_string),
                        failure_confidence: failure.failure_confidence.map(str::to_string),
                    });
                    if provider_exhausted {
                        capacity_exhausted_providers.insert(target_provider_id);
                    }
                    if let AppError::UpstreamStatus {
                        status: StatusCode::TOO_MANY_REQUESTS,
                        retry_after,
                        ..
                    } = &error
                    {
                        mark_target_sink(state, &target, "local_capacity", *retry_after).await;
                    }
                    if !busy_waitable_error(state, &target, &error).await {
                        all_targets_busy = false;
                    }
                    if let Some(retry_after) = busy_wait_retry_after(&error) {
                        next_retry_after = Some(
                            next_retry_after
                                .map_or(retry_after, |current| current.min(retry_after)),
                        );
                    }
                    last_rate_limit = match &error {
                        AppError::UpstreamStatus {
                            status,
                            message,
                            retry_after,
                        } if *status == StatusCode::TOO_MANY_REQUESTS => {
                            Some((message.clone(), *retry_after))
                        }
                        _ => None,
                    };
                    last_error = Some(error);
                    continue;
                }
            };
            match forward_to_target(
                state,
                &context.request_id,
                &context.endpoint,
                &context.requested_model,
                &context.request_json,
                &context.body,
                context.session_id.as_deref(),
                target.clone(),
                context.streamed,
                context.request_tokens,
                context.api_key.as_ref(),
                context.started,
                context.receipt.clone(),
            )
            .await
            {
                Ok(mut response) => {
                    let response_status = response.status();
                    trace.record_attempt(super::route_trace::TraceAttemptRecord {
                        attempt_index: attempts as i64,
                        round: wait_round as i64,
                        candidate_order,
                        provider_id: Some(target_provider_id),
                        provider_api_key_id: target_provider_key_id,
                        upstream_model: Some(target_upstream_model.clone()),
                        started_at: Utc::now().to_rfc3339(),
                        latency_ms: super::route_trace::attempt_elapsed(attempt_started),
                        status_code: Some(response_status.as_u16() as i64),
                        success: response_status.is_success(),
                        retryable: false,
                        reason_code: if response_status.is_success() {
                            "upstream_success"
                        } else {
                            "upstream_response"
                        }
                        .to_string(),
                        failure_scope: None,
                        failure_retry: None,
                        failure_confidence: None,
                    });
                    if response_status.is_success() {
                        mark_provider_success(state, target_provider_id).await;
                        mark_target_success(state, target_provider_id, &target_upstream_model)
                            .await;
                        mark_provider_api_key_success(state, target_provider_key_id).await;
                    } else {
                        mark_provider_reachable(state, target_provider_id).await;
                    }
                    apply_routing_headers(
                        &mut response,
                        attempts,
                        fallback_count,
                        &target_provider_name,
                        &target_upstream_model,
                    );
                    apply_budget_headers(
                        &mut response,
                        context.budget_usd,
                        context.budget_excluded_targets,
                    );
                    apply_override_headers(&mut response, &context.routing_overrides);
                    trace.finish(
                        state,
                        super::route_trace::TraceFinal {
                            final_provider_id: Some(target_provider_id),
                            final_provider_api_key_id: target_provider_key_id,
                            final_upstream_model: Some(target_upstream_model),
                            final_status_code: Some(response_status.as_u16() as i64),
                            success: response_status.is_success(),
                            fallback_count: fallback_count as i64,
                        },
                    );
                    return Ok(OpenAiAttemptOutcome::Response(attach_provider_slot(
                        response,
                        upstream_slots,
                        state.sse_keepalive,
                    )));
                }
                Err(error) => {
                    let failure =
                        super::route_trace::classify_trace_error_for_target(state, &target, &error)
                            .await;
                    trace.record_attempt(super::route_trace::TraceAttemptRecord {
                        attempt_index: attempts as i64,
                        round: wait_round as i64,
                        candidate_order,
                        provider_id: Some(target_provider_id),
                        provider_api_key_id: target_provider_key_id,
                        upstream_model: Some(target_upstream_model),
                        started_at: Utc::now().to_rfc3339(),
                        latency_ms: super::route_trace::attempt_elapsed(attempt_started),
                        status_code: failure.status_code,
                        success: false,
                        retryable: failure.retryable,
                        reason_code: failure.reason_code.to_string(),
                        failure_scope: failure.failure_scope.map(str::to_string),
                        failure_retry: failure.failure_retry.map(str::to_string),
                        failure_confidence: failure.failure_confidence.map(str::to_string),
                    });
                    if !busy_waitable_error(state, &target, &error).await {
                        all_targets_busy = false;
                    }
                    if let Some(retry_after) = busy_wait_retry_after(&error) {
                        next_retry_after = Some(
                            next_retry_after
                                .map_or(retry_after, |current| current.min(retry_after)),
                        );
                    }
                    last_rate_limit = match &error {
                        AppError::UpstreamStatus {
                            status,
                            message,
                            retry_after,
                        } if *status == StatusCode::TOO_MANY_REQUESTS => {
                            Some((message.clone(), *retry_after))
                        }
                        _ => None,
                    };
                    last_error = Some(error);
                }
            }
        }

        let Some(busy_wait_deadline) = busy_wait_deadline else {
            break;
        };
        if !attempted_any || !all_targets_busy {
            break;
        }
        if context.streamed && !context.resolved.keepalive_during_wait {
            break;
        }
        if context.streamed && context.resolved.keepalive_during_wait && emit_busy_wait_signal {
            return Ok(OpenAiAttemptOutcome::BusyWait);
        }
        let remaining = busy_wait_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Some(_wait_lease) = state.try_acquire_busy_wait(
            context.ordering_key,
            usize::try_from(context.resolved.max_waiting_requests.max(0)).unwrap_or(0),
        ) else {
            break;
        };
        let delay = busy_wait_delay(
            context.resolved.retry_jitter_ms,
            next_retry_after,
            wait_round,
        )
        .min(remaining);
        wait_round = wait_round.saturating_add(1);
        tracing::info!(
            request_id = %context.request_id,
            route_id = context.ordering_key,
            wait_ms = delay.as_millis() as u64,
            attempts,
            "all route targets are temporarily busy; waiting before reordering candidates"
        );
        tokio::time::sleep(delay).await;
        ordered_targets = order_targets(
            state,
            context.ordering_key,
            &context.resolved.strategy,
            context.retry_targets.clone(),
            context.session_id.as_deref(),
            context.api_key.as_ref().map(|key| key.id),
        )
        .await?;
        trace
            .update_candidates(
                state,
                &context.resolved.strategy,
                &ordered_targets,
                context.api_key.as_ref().map(|key| key.id),
            )
            .await;
    }

    let error = match last_rate_limit {
        Some((message, retry_after)) => AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message,
            retry_after,
        },
        None => last_error.unwrap_or_else(|| {
            AppError::Upstream("all configured route targets failed".to_string())
        }),
    };
    let status_code = finalized_fallback_status(&error);
    let message = error.to_string();
    log_usage(
        state,
        UsageLogEntry {
            request_id: &context.request_id,
            api_key_id: context.api_key.as_ref().map(|key| key.id),
            route_id: context.route_id,
            provider_id: last_target.as_ref().map(|(provider_id, _, _)| *provider_id),
            requested_model: &context.requested_model,
            upstream_model: last_target
                .as_ref()
                .map(|(_, _, upstream_model)| upstream_model.as_str()),
            endpoint: &context.endpoint,
            usage: Usage::new(context.request_tokens, 0),
            latency_ms: context.started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code,
            success: false,
            streamed: context.streamed,
            error_message: Some(&message),
            response_preview: None,
        },
    )
    .await;
    trace.finish(
        state,
        super::route_trace::TraceFinal {
            final_provider_id: last_target.as_ref().map(|(provider_id, _, _)| *provider_id),
            final_provider_api_key_id: last_target.as_ref().and_then(|(_, key_id, _)| *key_id),
            final_upstream_model: last_target
                .as_ref()
                .map(|(_, _, upstream_model)| upstream_model.clone()),
            final_status_code: Some(status_code),
            success: false,
            fallback_count: attempts.saturating_sub(1) as i64,
        },
    );
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
