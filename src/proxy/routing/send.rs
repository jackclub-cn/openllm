use super::*;

/// The outcome of an upstream attempt that may have been resent once.
pub(in crate::proxy) enum UpstreamAttempt {
    /// The upstream answered successfully; the body has not been consumed.
    Ok(UpstreamResponse),
    /// The upstream failed; the status, headers and body are already read.
    Error {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
}

/// Reads the retry policy for the data path.
///
/// A broken or missing settings row must never take inference down, so a read
/// failure degrades to the built-in defaults with a warning instead of
/// propagating.
pub(super) async fn resilience_policy(state: &AppState) -> crate::models::ResilienceSettings {
    match state.resilience_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "failed to load resilience settings; using defaults");
            crate::models::ResilienceSettings::default()
        }
    }
}

/// The result of one upstream send attempt, before retry decisions.
pub(super) enum SendError {
    /// The transport itself failed (connect, reset, timeout, body).
    Http(reqwest::Error),
    /// A streaming call did not receive response headers within its budget.
    HeaderTimeout(Duration),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "{error}"),
            Self::HeaderTimeout(budget) => {
                write!(
                    formatter,
                    "no response headers within {}s",
                    budget.as_secs()
                )
            }
        }
    }
}

pub(in crate::proxy) async fn send_provider_request(
    state: &AppState,
    target: &RouteTarget,
    streamed: bool,
    build: impl Fn() -> AppResult<RequestBuilder>,
) -> AppResult<reqwest::Response> {
    let resilience = resilience_policy(state).await;
    let mut retries_left = resilience.max_retries.clamp(0, MAX_SAME_TARGET_RETRIES) as u32;
    let mut retries_used = 0u32;

    loop {
        let request = match build() {
            Ok(request) => request,
            Err(error) => {
                // The request could not even be built, so nothing reached the
                // upstream. Hand back a half-open probe the caller admitted
                // instead of holding it for the whole lease; the transport path
                // below already releases it through `mark_provider_error`.
                release_provider_probe(state, target.provider_id).await;
                return Err(error);
            }
        };
        let budget = target
            .timeout_seconds
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
        // A total-request timeout would abort a long streaming generation, so a
        // streamed call only gets its provider budget on the wait for response
        // headers; the shared client's idle read timeout governs the body. A
        // buffered call keeps the total timeout because it has no stream to
        // protect and an unbounded body read would be a memory risk.
        let outcome = match budget {
            Some(budget) if streamed => match tokio::time::timeout(budget, request.send()).await {
                Ok(result) => result.map_err(SendError::Http),
                Err(_) => Err(SendError::HeaderTimeout(budget)),
            },
            Some(budget) => request
                .timeout(budget)
                .send()
                .await
                .map_err(SendError::Http),
            None => request.send().await.map_err(SendError::Http),
        };

        match outcome {
            Ok(response) => {
                crate::quota::observe_response_headers(state, target, response.headers()).await;
                return Ok(response);
            }
            Err(error) => {
                let retryable = match &error {
                    SendError::Http(error) => retryable_transport_error(error),
                    SendError::HeaderTimeout(_) => true,
                };
                // Transport faults are worth one more attempt: a reset socket or
                // a slow upstream often recovers without the caller noticing.
                if retries_left > 0 && retryable {
                    retries_left -= 1;
                    let delay = retry_backoff(&resilience, retries_used);
                    retries_used += 1;
                    tracing::warn!(
                        provider = %target.provider_name,
                        model = %target.upstream_model,
                        retry = retries_used,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "upstream transport error; retrying the same target"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                record_transport_failure(
                    state,
                    target,
                    "upstream_transport_error",
                    &error.to_string(),
                )
                .await;
                return Err(AppError::Upstream(format!(
                    "{} request failed: {error}",
                    target.provider_name
                )));
            }
        }
    }
}

/// Sends a request with the compatibility retries shared by every forwarding
/// path:
///
/// - resend once when the upstream answers with the opaque aggregator 4xx (an
///   `invalid request error` carrying only a trace id) that usually means an
///   internal channel failed rather than a bad request;
/// - if an upstream still rejects `tool_search`, strip it, remember that the
///   provider does not support it, and retry the same target once without it.
/// - if an upstream rejects replayed reasoning history or strict tool fields,
///   repair the request once, retry the same target, and persist the learned
///   capability for the provider/model/endpoint scope.
/// - if an upstream reports the accepted reasoning effort enum, map the
///   requested value to the nearest supported level, remember the enum on the
///   synced model, and retry once.
/// - if an upstream rejects the `original` image detail level, downlevel it
///   and remember the capability for the provider/model/endpoint scope.
///
/// `body` is the exact body sent upstream, so compatibility retries update the
/// request rather than replaying the rejected payload.
///
/// The argument list is flat on purpose: the callers forward values they already
/// hold, and a parameter struct would only add a construction step at each of
/// the three dispatch sites.
#[allow(clippy::too_many_arguments)]
pub(in crate::proxy) async fn send_provider_request_with_compat_retry<F>(
    state: &AppState,
    target: &RouteTarget,
    api_key_id: Option<i64>,
    request_id: &str,
    endpoint: &str,
    shape: UpstreamShape,
    streamed: bool,
    body: &mut Value,
    build: F,
) -> AppResult<UpstreamAttempt>
where
    F: Fn(&Value) -> AppResult<RequestBuilder>,
{
    // Circuit breaker: short-circuit a hard-open provider here, before any
    // upstream call, so a known-bad provider cannot pin an admission slot for a
    // full header timeout. The fallback loop moves to the next target, and if
    // none can serve, the client gets a fast 503 with a Retry-After.
    let probe_lease = provider_probe_lease(target.timeout_seconds);
    if let Err(retry_after) =
        admit_provider_attempt_with_lease(state, target.provider_id, probe_lease).await
    {
        return Err(AppError::UpstreamStatus {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "{} is short-circuited after repeated failures; retry shortly or use another target",
                target.provider_name
            ),
            retry_after: Some(retry_after.max(Duration::from_secs(1))),
        });
    }

    // Provider quota pool, charged once per attempt and only after every local
    // gate above has passed: charging earlier (in the caller, before the
    // concurrency slots and the circuit breaker) would let traffic the gateway
    // rejects locally burn quota that no upstream call ever consumed, and
    // charging later would race another attempt for the last free slot of the
    // window. A denial here is a routing decision, so the fallback loop simply
    // moves to the next target.
    if let Some((reason, retry_after)) =
        crate::allocation::charge_target_attempt(state, target.provider_id, api_key_id).await
    {
        // The attempt was admitted as a half-open recovery probe above but is
        // denied before any upstream call, so hand the probe back. Otherwise the
        // provider stays "probe in flight" and short-circuits every request for
        // `PROVIDER_PROBE_LEASE` even though nothing was ever tested.
        release_provider_probe(state, target.provider_id).await;
        return Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("excluded provider '{}': {reason}", target.provider_name),
            retry_after: Some(retry_after),
        });
    }
    let resilience = resilience_policy(state).await;
    let mut retries_left = resilience.max_retries.clamp(0, MAX_SAME_TARGET_RETRIES) as u32;
    let mut retries_used = 0u32;
    let mut stream_retries_left = if streamed && resilience.stream_recovery_enabled {
        resilience
            .stream_recovery_max_retries
            .clamp(0, crate::models::STREAM_RECOVERY_MAX_RETRIES) as u32
    } else {
        0
    };
    let mut stream_retries_used = 0u32;

    let mut response = UpstreamResponse::new(
        send_activity_request(state, target, request_id, streamed, || build(body)).await?,
    );
    let mut transient_retry_used = false;
    let mut image_detail_retry_used = false;
    let mut reasoning_effort_retry_used = false;
    let mut reasoning_retry_used = false;
    let mut strict_tools_retry_used = false;

    loop {
        let status = response.status();
        if status.is_success() {
            if streamed && resilience.stream_recovery_enabled {
                match response.recover_prefix().await {
                    Ok(recovered) => {
                        return Ok(UpstreamAttempt::Ok(
                            recovered.with_activity(state, request_id),
                        ));
                    }
                    Err(error) if stream_retries_left > 0 => {
                        stream_retries_left -= 1;
                        let delay = retry_backoff(&resilience, stream_retries_used);
                        stream_retries_used += 1;
                        tracing::warn!(
                            provider = %target.provider_name,
                            model = %target.upstream_model,
                            request_id,
                            endpoint,
                            retry = stream_retries_used,
                            delay_ms = delay.as_millis() as u64,
                            %error,
                            "upstream stream ended before commit; retrying the same target"
                        );
                        tokio::time::sleep(delay).await;
                        response = UpstreamResponse::new(
                            send_activity_request(state, target, request_id, streamed, || {
                                build(body)
                            })
                            .await?,
                        );
                        continue;
                    }
                    Err(error) => {
                        record_transport_failure(
                            state,
                            target,
                            "upstream_stream_failed",
                            &error.to_string(),
                        )
                        .await;
                        return Err(AppError::Upstream(format!(
                            "{} stream ended before commit: {error}",
                            target.provider_name
                        )));
                    }
                }
            }
            return Ok(UpstreamAttempt::Ok(
                response.with_activity(state, request_id),
            ));
        }
        let headers = response.headers().clone();
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;

        // One bounded retry against the same target turns a momentary 5xx into
        // a served request instead of a fallback or a client-visible failure.
        if retries_left > 0 && retryable_same_target_status(status) {
            retries_left -= 1;
            let delay = retry_backoff(&resilience, retries_used);
            retries_used += 1;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                retry = retries_used,
                delay_ms = delay.as_millis() as u64,
                "upstream returned a transient status; retrying the same target"
            );
            tokio::time::sleep(delay).await;
            response = UpstreamResponse::new(
                send_activity_request(state, target, request_id, streamed, || build(body)).await?,
            );
            continue;
        }

        if !transient_retry_used && transient_upstream_4xx(status, &response_body) {
            transient_retry_used = true;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                trace_id = tracing::field::display(
                    upstream_trace_id(&response_body).as_deref().unwrap_or("unknown")
                ),
                has_tool_search = strip_tool_search_tools(body).is_some(),
                "upstream returned an opaque 4xx; retrying once"
            );
            response = UpstreamResponse::new(
                send_activity_request(state, target, request_id, streamed, || build(body)).await?,
            );
            continue;
        }

        if !image_detail_retry_used
            && let Some(supported) =
                super::compat::rejected_image_detail_options(status, &response_body)
        {
            let repaired = super::compat::normalize_image_detail_original(body, &supported);
            if repaired > 0 {
                image_detail_retry_used = true;
                tracing::warn!(
                    provider = %target.provider_name,
                    model = %target.upstream_model,
                    request_id,
                    endpoint,
                    supported = ?supported,
                    image_parts = repaired,
                    "upstream rejected original image detail; retrying with a supported level"
                );
                crate::capabilities::record_capability(
                    state,
                    crate::capabilities::CapabilityKey {
                        provider_id: target.provider_id,
                        model: target.upstream_model.clone(),
                        endpoint: endpoint.to_string(),
                        capability: crate::capabilities::CAPABILITY_IMAGE_DETAIL_ORIGINAL
                            .to_string(),
                    },
                    false,
                    crate::capabilities::SOURCE_RUNTIME,
                    "exact",
                )
                .await;
                response = UpstreamResponse::new(
                    send_activity_request(state, target, request_id, streamed, || build(body))
                        .await?,
                );
                continue;
            }
        }

        if !reasoning_effort_retry_used
            && let Some(supported) =
                super::compat::rejected_reasoning_effort_options(status, &response_body, body)
        {
            let repaired = super::compat::normalize_reasoning_effort(body, &supported);
            if repaired > 0 {
                reasoning_effort_retry_used = true;
                tracing::warn!(
                    provider = %target.provider_name,
                    model = %target.upstream_model,
                    request_id,
                    endpoint,
                    supported = ?supported,
                    "upstream rejected reasoning effort; retrying with the nearest supported value"
                );
                crate::capabilities::record_reasoning_efforts(
                    state,
                    target.provider_id,
                    &target.upstream_model,
                    &supported,
                )
                .await;
                response = UpstreamResponse::new(
                    send_activity_request(state, target, request_id, streamed, || build(body))
                        .await?,
                );
                continue;
            }
        }

        if !reasoning_retry_used
            && (status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY)
            && compat_thinking_history_error(status, &response_body)
        {
            let repaired = match shape {
                UpstreamShape::Responses => {
                    super::compat::normalize_responses_reasoning_history(body)
                }
                UpstreamShape::Chat => super::compat::normalize_chat_reasoning_history(body),
                UpstreamShape::Anthropic => {
                    super::compat::normalize_anthropic_thinking_history(body)
                }
            };
            if repaired > 0 {
                reasoning_retry_used = true;
                tracing::warn!(
                    provider = %target.provider_name,
                    model = %target.upstream_model,
                    request_id,
                    endpoint,
                    repaired,
                    "upstream rejected reasoning history; retrying with repaired history"
                );
                crate::capabilities::record_capability(
                    state,
                    crate::capabilities::CapabilityKey {
                        provider_id: target.provider_id,
                        model: target.upstream_model.clone(),
                        endpoint: endpoint.to_string(),
                        capability: crate::capabilities::CAPABILITY_REASONING_HISTORY.to_string(),
                    },
                    true,
                    crate::capabilities::SOURCE_RUNTIME,
                    "exact",
                )
                .await;
                response = UpstreamResponse::new(
                    send_activity_request(state, target, request_id, streamed, || build(body))
                        .await?,
                );
                continue;
            }
        }

        if !strict_tools_retry_used
            && matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            )
            && super::compat::upstream_rejects_strict_tools(&response_body)
            && let Some(compat_body) = super::compat::strip_strict_tools(body)
        {
            strict_tools_retry_used = true;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                "upstream rejected strict tool fields; retrying without them"
            );
            crate::capabilities::record_capability(
                state,
                crate::capabilities::CapabilityKey {
                    provider_id: target.provider_id,
                    model: target.upstream_model.clone(),
                    endpoint: endpoint.to_string(),
                    capability: crate::capabilities::CAPABILITY_STRICT_TOOLS.to_string(),
                },
                false,
                crate::capabilities::SOURCE_RUNTIME,
                "exact",
            )
            .await;
            *body = compat_body;
            response = UpstreamResponse::new(
                send_activity_request(state, target, request_id, streamed, || build(body)).await?,
            );
            continue;
        }

        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) && upstream_rejects_tool_search(&response_body)
            && let Some(compat_body) = strip_tool_search_tools(body)
        {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                "upstream rejected tool_search; retrying without it"
            );
            mark_provider_tool_search_unsupported(state, target.provider_id).await;
            *body = compat_body;
            response = UpstreamResponse::new(
                send_activity_request(state, target, request_id, streamed, || build(body)).await?,
            );
            continue;
        }

        if transient_retry_used && transient_upstream_4xx(status, &response_body) {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                trace_id = tracing::field::display(
                    upstream_trace_id(&response_body).as_deref().unwrap_or("unknown")
                ),
                "upstream still returned an opaque 4xx after retry"
            );
        }

        return Ok(UpstreamAttempt::Error {
            status,
            headers,
            body: response_body,
        });
    }
}
