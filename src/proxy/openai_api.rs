use super::*;

mod attempts;
use attempts::run_openai_attempts;

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
    BusyWait(Box<super::route_trace::AttemptResume>),
}

pub(crate) async fn proxy_openai(
    State(state): State<AppState>,
    Extension(auth): Extension<GatewayAuth>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let result =
        proxy_openai_with_auth(&state, &headers, &uri, &body, &request_id, Some(auth.0)).await;
    finish_activity_for_pre_response(&state, &request_id, &result);
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
    finish_activity_for_pre_response(&state, &request_id, &result);
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
    proxy_openai_with_auth(
        state,
        headers,
        uri,
        body,
        request_id,
        api_key_override.map(Some),
    )
    .await
}

async fn proxy_openai_with_auth(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    authenticated_api_key: Option<Option<ApiKeyRecord>>,
) -> AppResult<Response> {
    track_activity_request(
        state,
        request_id,
        uri.path(),
        run_openai_request(state, headers, uri, body, request_id, authenticated_api_key),
    )
    .await
}

async fn run_openai_request(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    authenticated_api_key: Option<Option<ApiKeyRecord>>,
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

    let api_key = match authenticated_api_key {
        Some(api_key) => api_key,
        None => authenticate_gateway(state, headers).await?,
    };
    state.activity_describe(
        request_id,
        crate::state::ActivityDescription {
            requested_model: requested_model.clone(),
            streamed,
            session_id: session_id.clone(),
            api_key_id: api_key.as_ref().map(|key| key.id),
            api_key_name: api_key.as_ref().map(|key| key.name.clone()),
            request_tokens_estimated: request_tokens,
            context: state
                .activity
                .is_enabled()
                .then(|| request_context_breakdown(&request_json, &endpoint, request_tokens)),
        },
    );
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
    match track_usage_request(
        state,
        request_id,
        started,
        run_openai_attempts(state, &context, true, None),
    )
    .await?
    {
        OpenAiAttemptOutcome::Response(response) => Ok(response),
        OpenAiAttemptOutcome::BusyWait(resume) => {
            let state = state.clone();
            let context = context.clone();
            let interval = state.sse_keepalive.unwrap_or(Duration::from_secs(
                crate::state::DEFAULT_SSE_KEEPALIVE_SECS,
            ));
            Ok(stream_sse_until_response(
                interval,
                SseErrorProtocol::OpenAi,
                async move {
                    match track_usage_request(
                        &state,
                        &context.request_id,
                        context.started,
                        run_openai_attempts(&state, &context, false, Some(resume)),
                    )
                    .await?
                    {
                        OpenAiAttemptOutcome::Response(response) => Ok(response),
                        OpenAiAttemptOutcome::BusyWait(_) => Err(AppError::Internal(
                            anyhow::anyhow!("busy-wait stream did not resume routing"),
                        )),
                    }
                },
            ))
        }
    }
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
