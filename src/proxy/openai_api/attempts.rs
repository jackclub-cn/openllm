use super::*;

pub(super) async fn run_openai_attempts(
    state: &AppState,
    context: &OpenAiAttemptContext,
    emit_busy_wait_signal: bool,
    resume: Option<Box<super::route_trace::AttemptResume>>,
) -> AppResult<OpenAiAttemptOutcome> {
    let mut last_error: Option<AppError> = None;
    let mut last_rate_limit: Option<(String, Option<Duration>)> = None;
    let mut last_target: Option<(i64, Option<i64>, String)> = None;
    let mut attempts = resume.as_ref().map_or(0, |resume| resume.attempts);
    let request_deadline = context.request_deadline;
    let busy_wait_deadline = context.busy_wait_deadline;
    let mut wait_round = 0u32;
    let ordered = order_targets_with_decision(
        state,
        context.ordering_key,
        &context.resolved.strategy,
        context.retry_targets.clone(),
        context.affinity_key.as_deref(),
        context.api_key.as_ref().map(|key| key.id),
    )
    .await?;
    let mut ordered_targets = ordered.targets;
    let mut trace = resume.map(|resume| resume.trace).unwrap_or_else(|| {
        super::route_trace::RouteTraceRecorder::new(
            context.request_id.clone(),
            context.route_id,
            context.resolved.strategy.clone(),
            context.session_id.clone(),
        )
    });
    trace.set_session_decision(ordered.session_decision);
    trace
        .update_candidates(
            state,
            &context.resolved.strategy,
            &ordered_targets,
            context.api_key.as_ref().map(|key| key.id),
        )
        .await;
    state.activity_set_route(
        &context.request_id,
        context.route_id,
        context.resolved.route_name.as_deref(),
        &context.resolved.strategy,
        candidates_for_activity(&ordered_targets, trace.candidates()),
    );

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
            let opencode_session_id = opencode_session_id(
                context.session_id.as_deref(),
                context.affinity_key.as_deref(),
                &context.request_id,
            );
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
            state.activity_begin_attempt(
                &context.request_id,
                crate::state::ActivityAttemptStart {
                    attempt_index: attempts as i64,
                    round: wait_round as i64,
                    candidate_order,
                    target: crate::state::ActivityTarget {
                        provider_id: target_provider_id,
                        provider_name: target.provider_name.clone(),
                        provider_api_key_id: target_provider_key_id,
                        upstream_model: target_upstream_model.clone(),
                        context_window_tokens: target.context_limit,
                        input_limit: target.input_limit,
                    },
                },
            );
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
                    state.activity_finish_attempt(
                        &context.request_id,
                        crate::state::ActivityAttemptFinish {
                            attempt_index: attempts as i64,
                            latency_ms: attempt_started.elapsed().as_millis() as i64,
                            status_code: failure.status_code,
                            success: false,
                            reason_code: failure.reason_code.to_string(),
                            failure_scope: failure.failure_scope.map(str::to_string),
                        },
                    );
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
                Some(opencode_session_id),
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
                    state.activity_finish_attempt(
                        &context.request_id,
                        crate::state::ActivityAttemptFinish {
                            attempt_index: attempts as i64,
                            latency_ms: attempt_started.elapsed().as_millis() as i64,
                            status_code: Some(response_status.as_u16() as i64),
                            success: response_status.is_success(),
                            reason_code: if response_status.is_success() {
                                "upstream_success"
                            } else {
                                "upstream_response"
                            }
                            .to_string(),
                            failure_scope: None,
                        },
                    );
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
                    trace
                        .finish(
                            state,
                            super::route_trace::TraceFinal {
                                final_provider_id: Some(target_provider_id),
                                final_provider_api_key_id: target_provider_key_id,
                                final_upstream_model: Some(target_upstream_model),
                                final_status_code: Some(response_status.as_u16() as i64),
                                success: response_status.is_success(),
                                fallback_count: fallback_count as i64,
                            },
                        )
                        .await;
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
                    state.activity_finish_attempt(
                        &context.request_id,
                        crate::state::ActivityAttemptFinish {
                            attempt_index: attempts as i64,
                            latency_ms: attempt_started.elapsed().as_millis() as i64,
                            status_code: failure.status_code,
                            success: false,
                            reason_code: failure.reason_code.to_string(),
                            failure_scope: failure.failure_scope.map(str::to_string),
                        },
                    );
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
            return Ok(OpenAiAttemptOutcome::BusyWait(Box::new(
                super::route_trace::AttemptResume { trace, attempts },
            )));
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
        state.activity_waiting(
            &context.request_id,
            "all route targets are temporarily busy",
        );
        wait_round = wait_round.saturating_add(1);
        tracing::info!(
            request_id = %context.request_id,
            route_id = context.ordering_key,
            wait_ms = delay.as_millis() as u64,
            attempts,
            "all route targets are temporarily busy; waiting before reordering candidates"
        );
        tokio::time::sleep(delay).await;
        let ordered = order_targets_with_decision(
            state,
            context.ordering_key,
            &context.resolved.strategy,
            context.retry_targets.clone(),
            context.session_id.as_deref(),
            context.api_key.as_ref().map(|key| key.id),
        )
        .await?;
        ordered_targets = ordered.targets;
        trace.set_session_decision(ordered.session_decision);
        trace
            .update_candidates(
                state,
                &context.resolved.strategy,
                &ordered_targets,
                context.api_key.as_ref().map(|key| key.id),
            )
            .await;
        state.activity_set_route(
            &context.request_id,
            context.route_id,
            context.resolved.route_name.as_deref(),
            &context.resolved.strategy,
            candidates_for_activity(&ordered_targets, trace.candidates()),
        );
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
    trace
        .finish(
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
        )
        .await;
    Err(error)
}
