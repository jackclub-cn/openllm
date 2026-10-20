use super::*;

/// Minimum probe reservation margin, added on top of a configured provider
/// header timeout so the probe lease cannot expire while the upstream is still
/// allowed to answer.
pub(super) const PROVIDER_PROBE_TIMEOUT_MARGIN: Duration = Duration::from_secs(30);

pub(in crate::proxy) fn provider_probe_lease(timeout_seconds: Option<i64>) -> Duration {
    let configured = timeout_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    configured
        .map(|timeout| {
            PROVIDER_PROBE_LEASE.max(timeout.saturating_add(PROVIDER_PROBE_TIMEOUT_MARGIN))
        })
        .unwrap_or(PROVIDER_PROBE_LEASE)
}

pub(in crate::proxy) fn target_cooldown(
    status: StatusCode,
    retry_after: Option<Duration>,
    configured_seconds: Option<i64>,
) -> Duration {
    let default_base = match status {
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    let configured_base = configured_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    let base = match configured_base {
        Some(configured) => configured.max(retry_after.unwrap_or_default()),
        None => retry_after.unwrap_or(default_base),
    };
    let cap = if configured_base.is_some() {
        MAX_CONFIGURED_PROVIDER_COOLDOWN
    } else {
        MAX_TARGET_COOLDOWN
    };
    base.min(cap)
}

pub(in crate::proxy) async fn mark_provider_error(
    state: &AppState,
    provider_id: i64,
    status: Option<StatusCode>,
    retry_after: Option<Duration>,
    configured_cooldown_seconds: Option<i64>,
) {
    let now = Instant::now();
    // A probe that just failed is no longer in flight; the cooldown below
    // decides when the next recovery probe may be admitted. Whether one was in
    // flight also tells the trip counter that a HALF_OPEN probe re-opened the
    // circuit rather than a fresh streak crossing the threshold.
    let probe_was_in_flight = state
        .provider_probe
        .lock()
        .await
        .remove(&provider_id)
        .is_some();
    let threshold = provider_open_threshold();
    let (failures, opened, cooldown) = {
        // Keep the streak and deadline under one lock so a burst of requests
        // that failed just before the circuit opened cannot keep extending the
        // same outage. A half-open probe is the one exception: its failure must
        // re-open the circuit with the next escalated cooldown.
        let mut cooldowns = state.provider_cooldown.lock().await;
        let mut streaks = state.provider_failure_streak.lock().await;
        let current = streaks.get(&provider_id).copied().unwrap_or(0);
        if threshold > 0
            && !probe_was_in_flight
            && current >= threshold
            && cooldowns
                .get(&provider_id)
                .is_some_and(|until| *until > now)
        {
            tracing::debug!(
                provider_id,
                failure_streak = current,
                "ignoring a follower failure while the provider circuit is open"
            );
            return;
        }

        let failures = streaks.entry(provider_id).or_default();
        *failures = failures.saturating_add(1);
        let opened = threshold > 0
            && *failures >= threshold
            && (*failures - 1 < threshold || probe_was_in_flight);
        let cooldown =
            provider_cooldown(status, *failures, retry_after, configured_cooldown_seconds);
        let deadline = now + cooldown;
        cooldowns
            .entry(provider_id)
            .and_modify(|until| *until = (*until).max(deadline))
            .or_insert(deadline);
        (*failures, opened, cooldown)
    };
    if opened {
        *state
            .provider_circuit_trips
            .lock()
            .await
            .entry(provider_id)
            .or_default() += 1;
        tracing::warn!(
            provider_id,
            failures,
            "provider circuit opened after repeated failures; short-circuiting until it recovers"
        );
    }
    // Mirror the trip so a restart still knows this provider is unhealthy.
    state.mark_provider_resilience_dirty(provider_id);
    if opened {
        let _ = state
            .circuit_events
            .send(crate::state::ProviderCircuitEvent {
                provider_id,
                state: "open",
                reason: if probe_was_in_flight {
                    "probe_failed"
                } else {
                    "failure_threshold"
                },
                failure_streak: failures,
                cooldown_seconds: cooldown.as_secs(),
            });
    }
}

pub(in crate::proxy) async fn mark_target_error(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    retry_after: Option<Duration>,
) -> usize {
    let now = Instant::now();
    let active = {
        let mut cooldowns = state.target_cooldown.lock().await;
        cooldowns.retain(|_, until| *until > now);
        let key = (target.provider_id, target.upstream_model.clone());
        let deadline = now + target_cooldown(status, retry_after, target.cooldown_seconds);
        cooldowns
            .entry(key)
            .and_modify(|until| *until = (*until).max(deadline))
            .or_insert(deadline);
        cooldowns
            .keys()
            .filter(|(provider_id, _)| *provider_id == target.provider_id)
            .count()
    };
    state.mark_target_resilience_dirty(target.provider_id, &target.upstream_model);
    active
}

pub(in crate::proxy) async fn mark_target_success(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
) {
    let removed = state
        .target_cooldown
        .lock()
        .await
        .remove(&(provider_id, upstream_model.to_string()));
    state
        .target_failure_meta
        .lock()
        .await
        .remove(&(provider_id, upstream_model.to_string()));
    if removed.is_some() {
        state.mark_target_resilience_dirty(provider_id, upstream_model);
    }
    clear_target_sinks_for_provider_model(state, provider_id, upstream_model).await;
}

/// Records a successful request's first-token latency as a soft routing signal.
///
/// A 200 response proves the provider is reachable, so this must not open the
/// failure circuit or a hard model cooldown. The sample is only consulted by
/// `latency_optimized` routes, where it makes a target lose to healthy siblings
/// for a bounded period. That keeps a quota-drained upstream from attracting
/// every following request on a latency-first route while still eventually
/// probing it again, without touching the order configured for every other
/// strategy.
pub(in crate::proxy) async fn record_target_latency_success(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
    first_token_ms: Option<i64>,
) {
    let threshold_ms = slow_target_threshold_ms();
    if threshold_ms <= 0 {
        return;
    }
    let Some(first_token_ms) = first_token_ms.filter(|latency| *latency >= threshold_ms) else {
        return;
    };

    let now = Instant::now();
    let mut targets = state.target_slow.lock().await;
    targets.retain(|_, entry| {
        entry.until.is_some_and(|until| until > now)
            || entry.last_slow_at.is_some_and(|last| {
                now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW
            })
    });

    let entry = targets
        .entry((provider_id, upstream_model.to_string()))
        .or_default();
    let same_streak = entry
        .last_slow_at
        .is_some_and(|last| now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW);
    entry.slow_streak = if same_streak {
        entry.slow_streak.saturating_add(1)
    } else {
        1
    };
    entry.last_slow_at = Some(now);
    entry.last_first_token_ms = first_token_ms;
    let hold = slow_target_hold(first_token_ms, threshold_ms, entry.slow_streak);
    entry.until = Some(now + hold);

    tracing::warn!(
        provider_id,
        model = upstream_model,
        first_token_ms,
        threshold_ms,
        slow_streak = entry.slow_streak,
        hold_seconds = hold.as_secs(),
        "successful upstream response was slow; recorded for latency-optimized routes"
    );
}

pub(super) fn slow_target_hold(
    first_token_ms: i64,
    threshold_ms: i64,
    slow_streak: u32,
) -> Duration {
    const BASE_HOLD: Duration = Duration::from_secs(30);
    const MAX_OBSERVED_ADDITION: Duration = Duration::from_secs(120);

    let observed_addition = Duration::from_millis(
        first_token_ms
            .saturating_sub(threshold_ms)
            .max(0)
            .unsigned_abs()
            .min(MAX_OBSERVED_ADDITION.as_millis() as u64)
            / 2,
    );
    let multiplier = 1u32 << slow_streak.saturating_sub(1).min(3);
    (BASE_HOLD + observed_addition)
        .saturating_mul(multiplier)
        .min(MAX_SLOW_TARGET_HOLD)
}

/// Cools a target after its response stream failed mid-flight.
///
/// A stream that dies after the headers were sent (idle timeout, reset) is the
/// same class of transport fault as a pre-header failure, so it takes the
/// provider-wide path `send_provider_request` uses. The client already has a
/// truncated answer, but without this the next request would pick the same
/// unhealthy provider and stall again.
pub(in crate::proxy) async fn mark_stream_failure(
    state: &AppState,
    target: &RouteTarget,
    message: &str,
) {
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        %message,
        "upstream stream failed before it finished; cooling the provider"
    );
    record_transport_failure(state, target, "upstream_stream_failed", message).await;
}

/// Records a transport fault that happened before an upstream response was
/// received. Transport failures are provider-wide by nature, but the target
/// still gets a model cooldown so the request path does not immediately choose
/// the same broken connection again.
pub(in crate::proxy) async fn record_transport_failure(
    state: &AppState,
    target: &RouteTarget,
    reason_code: &'static str,
    message: &str,
) -> UpstreamFailure {
    let failure = UpstreamFailure::transport(reason_code);
    count_failure_classification(state, failure).await;
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        provider_key_id = target.provider_api_key_id,
        scope = failure.scope.as_str(),
        retry = failure.retry.as_str(),
        confidence = failure.confidence.as_str(),
        reason = failure.reason_code,
        %message,
        "classified upstream transport failure"
    );
    mark_provider_error(
        state,
        target.provider_id,
        None,
        None,
        target.cooldown_seconds,
    )
    .await;
    mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
    let metadata = failure.metadata();
    state.target_failure_meta.lock().await.insert(
        (target.provider_id, target.upstream_model.clone()),
        metadata.clone(),
    );
    state
        .provider_failure_meta
        .lock()
        .await
        .insert(target.provider_id, metadata);
    failure
}

pub(super) async fn count_failure_classification(state: &AppState, failure: UpstreamFailure) {
    *state
        .failure_classifications
        .lock()
        .await
        .entry((
            failure.scope.as_str().to_string(),
            failure.retry.as_str().to_string(),
            failure.reason_code.to_string(),
        ))
        .or_default() += 1;
}

pub(in crate::proxy) async fn record_upstream_failure(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    response_headers: &HeaderMap,
    message: &str,
) -> UpstreamFailure {
    let retry_after = retry_after_from_headers(response_headers);
    let failure = classify_upstream_failure(target, status, message.as_bytes());
    crate::quota::observe_response_headers(state, target, response_headers).await;
    if status == StatusCode::TOO_MANY_REQUESTS && failure.reason_code == "quota_exhausted" {
        crate::quota::observe_runtime_quota_failure(state, target, retry_after).await;
    }
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        provider_key_id = target.provider_api_key_id,
        status = status.as_u16(),
        scope = failure.scope.as_str(),
        retry = failure.retry.as_str(),
        confidence = failure.confidence.as_str(),
        reason = failure.reason_code,
        "classified upstream failure"
    );
    count_failure_classification(state, failure).await;
    if failure.should_sink() {
        mark_target_sink(state, target, failure.reason_code, retry_after).await;
    }
    let metadata = failure.metadata();
    let mut provider_rested = false;
    if failure.should_rest_target() {
        let active_model_cooldowns = mark_target_error(state, target, status, retry_after).await;
        state.target_failure_meta.lock().await.insert(
            (target.provider_id, target.upstream_model.clone()),
            metadata.clone(),
        );
        // A model-level error must not remove every healthy sibling model on
        // the provider. Only escalate to provider-wide isolation after enough
        // distinct models have failed; transport errors take the direct path
        // in `send_provider_request` because they are provider-wide by nature.
        if provider_circuit_should_open(status, active_model_cooldowns) {
            mark_provider_error(
                state,
                target.provider_id,
                Some(status),
                retry_after,
                target.cooldown_seconds,
            )
            .await;
            provider_rested = true;
        }
    }
    if provider_reachable_status(status) {
        mark_provider_reachable(state, target.provider_id).await;
    }
    if failure.should_rest_credential() {
        if let Some(provider_api_key_id) = target.provider_api_key_id {
            state
                .provider_key_failure_meta
                .lock()
                .await
                .insert(provider_api_key_id, metadata.clone());
        }
        mark_provider_api_key_error(
            state,
            target.provider_api_key_id,
            status,
            &format!("{} returned {}: {}", target.provider_name, status, message),
            retry_after,
        )
        .await;
    }
    if provider_rested {
        state
            .provider_failure_meta
            .lock()
            .await
            .insert(target.provider_id, metadata);
    }
    failure
}

pub(in crate::proxy) fn provider_circuit_should_open(
    status: StatusCode,
    active_model_cooldowns: usize,
) -> bool {
    // A 409 usually describes this request's resource state, not the
    // provider's health, so it should fall through without opening a circuit.
    status != StatusCode::CONFLICT && active_model_cooldowns >= PROVIDER_MODEL_COOLDOWN_THRESHOLD
}

pub(in crate::proxy) async fn mark_provider_success(state: &AppState, provider_id: i64) {
    close_provider_circuit(state, provider_id, "successful_request").await;
}

/// A reachable upstream response proves a half-open probe succeeded even when
/// the status itself is a client error or a rate limit. Close the provider
/// circuit, but leave model and key cooldowns in place.
pub(in crate::proxy) async fn mark_provider_reachable(state: &AppState, provider_id: i64) {
    let probe_was_active = state
        .provider_probe
        .lock()
        .await
        .remove(&provider_id)
        .is_some();
    if !probe_was_active {
        return;
    }
    close_provider_circuit(state, provider_id, "reachable_response").await;
}

pub(super) async fn close_provider_circuit(
    state: &AppState,
    provider_id: i64,
    reason: &'static str,
) {
    state.provider_cooldown.lock().await.remove(&provider_id);
    state
        .provider_failure_meta
        .lock()
        .await
        .remove(&provider_id);
    let streak = state
        .provider_failure_streak
        .lock()
        .await
        .remove(&provider_id)
        .unwrap_or(0);
    state.provider_probe.lock().await.remove(&provider_id);
    // A recovery must be persisted too, or a restart would restore the
    // cooldown this success just cleared.
    state.mark_provider_resilience_dirty(provider_id);
    let threshold = provider_open_threshold();
    if threshold > 0 && streak >= threshold {
        tracing::info!(provider_id, reason, "provider circuit closed");
        let _ = state
            .circuit_events
            .send(crate::state::ProviderCircuitEvent {
                provider_id,
                state: "closed",
                reason,
                failure_streak: 0,
                cooldown_seconds: 0,
            });
    }
}

/// Applies the per-provider circuit breaker to one upstream attempt.
///
/// Returns `Ok(())` when the request may try the provider, or `Err(retry_after)`
/// when a hard-open circuit short-circuits it.
///
/// A provider below the configured failure threshold is always allowed: its
/// runtime cooldown only deprioritises it (see `order_targets`), so one
/// transient blip on an otherwise healthy provider cannot turn into an outage.
/// At or above the threshold the provider stays blocked while its cooldown is
/// active, and once the cooldown elapses exactly one request is admitted to
/// probe recovery. That probe is what lets the provider leave the open state:
/// a success clears the streak, a failure re-opens with the escalated cooldown.
pub(in crate::proxy) async fn admit_provider_attempt_with_lease(
    state: &AppState,
    provider_id: i64,
    probe_lease: Duration,
) -> Result<(), Duration> {
    let threshold = provider_open_threshold();
    if threshold == 0 {
        return Ok(());
    }
    let failures = state
        .provider_failure_streak
        .lock()
        .await
        .get(&provider_id)
        .copied()
        .unwrap_or(0);
    if failures < threshold {
        return Ok(());
    }

    let now = Instant::now();
    let open_until = state
        .provider_cooldown
        .lock()
        .await
        .get(&provider_id)
        .copied();
    if let Some(until) = open_until
        && until > now
    {
        return Err(until.saturating_duration_since(now));
    }

    // Cooldown elapsed: half-open. Admit exactly one probe; a probe that is
    // still within its lease means another request is already testing recovery.
    let mut probes = state.provider_probe.lock().await;
    probes.retain(|_, started| now.saturating_duration_since(*started) < probe_lease);
    if probes.contains_key(&provider_id) {
        return Err(probe_lease);
    }
    probes.insert(provider_id, now);
    Ok(())
}

/// Releases a recovery-probe lease without touching the failure streak.
///
/// A half-open provider admits exactly one probe (see [`admit_provider_attempt`])
/// so a burst cannot stampede a provider that is only just recovering. If that
/// admitted attempt is then stopped before it reaches the upstream (today a
/// provider-pool denial), the probe no longer represents anything in flight, so
/// it has to be released here. Leaving it would keep the provider short-circuited
/// for the whole probe lease even though nothing ever tested it.
pub(in crate::proxy) async fn release_provider_probe(state: &AppState, provider_id: i64) {
    state.provider_probe.lock().await.remove(&provider_id);
}
