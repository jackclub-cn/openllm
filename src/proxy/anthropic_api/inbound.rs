use super::*;

#[cfg(test)]
pub(crate) async fn proxy_anthropic_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
) -> AppResult<Response> {
    proxy_anthropic_with_auth(state, headers, uri, body, request_id, None).await
}

pub(crate) async fn proxy_anthropic_authenticated_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    api_key: Option<ApiKeyRecord>,
) -> AppResult<Response> {
    proxy_anthropic_with_auth(state, headers, uri, body, request_id, Some(api_key)).await
}

async fn proxy_anthropic_with_auth(
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
        run_anthropic_request(state, headers, uri, body, request_id, authenticated_api_key),
    )
    .await
}

async fn run_anthropic_request(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    authenticated_api_key: Option<Option<ApiKeyRecord>>,
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
    let affinity_key = session_affinity_key(headers, &inbound);
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
                .then(|| request_context_breakdown(&inbound, &endpoint, request_tokens)),
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
    let context = AnthropicAttemptContext {
        request_id: request_id.to_string(),
        endpoint,
        inbound,
        request_json,
        requested_model,
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
        anthropic_beta: anthropic_beta_of(headers),
        request_deadline,
        busy_wait_deadline,
    };
    match track_usage_request(
        state,
        request_id,
        started,
        run_anthropic_attempts(state, &context, true, None),
    )
    .await?
    {
        AnthropicAttemptOutcome::Response(response) => Ok(response),
        AnthropicAttemptOutcome::BusyWait(resume) => {
            let state = state.clone();
            let context = context.clone();
            let interval = state.sse_keepalive.unwrap_or(Duration::from_secs(
                crate::state::DEFAULT_SSE_KEEPALIVE_SECS,
            ));
            Ok(stream_sse_until_response(
                interval,
                SseErrorProtocol::Anthropic,
                async move {
                    match track_usage_request(
                        &state,
                        &context.request_id,
                        context.started,
                        run_anthropic_attempts(&state, &context, false, Some(resume)),
                    )
                    .await?
                    {
                        AnthropicAttemptOutcome::Response(response) => Ok(response),
                        AnthropicAttemptOutcome::BusyWait(_) => Err(AppError::Internal(
                            anyhow::anyhow!("busy-wait stream did not resume routing"),
                        )),
                    }
                },
            ))
        }
    }
}
