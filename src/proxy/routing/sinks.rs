use super::*;

pub(in crate::proxy) fn target_sink_key(target: &RouteTarget) -> crate::state::TargetSinkKey {
    (
        target.provider_id,
        target.provider_api_key_id,
        target.upstream_model.clone(),
    )
}

pub(super) fn target_sink_ttl(retry_after: Option<Duration>) -> Duration {
    const DEFAULT_SINK: Duration = Duration::from_secs(30);
    const MIN_SINK: Duration = Duration::from_secs(1);
    const MAX_SINK: Duration = Duration::from_secs(5 * 60);
    retry_after
        .unwrap_or(DEFAULT_SINK)
        .clamp(MIN_SINK, MAX_SINK)
}

pub(in crate::proxy) async fn mark_target_sink(
    state: &AppState,
    target: &RouteTarget,
    reason_code: &'static str,
    retry_after: Option<Duration>,
) {
    let now = Instant::now();
    let key = target_sink_key(target);
    let deadline = now + target_sink_ttl(retry_after);
    state
        .target_sink
        .lock()
        .await
        .entry(key.clone())
        .and_modify(|entry| {
            entry.until = entry.until.max(deadline);
            entry.reason_code = reason_code;
            entry.observed_at = now;
        })
        .or_insert(crate::state::TargetSinkEntry {
            until: deadline,
            reason_code,
            observed_at: now,
        });
    state.mark_target_sink_dirty(&key);
}

pub(super) async fn clear_target_sinks_for_provider_model(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
) {
    let removed = {
        let mut sinks = state.target_sink.lock().await;
        let keys = sinks
            .keys()
            .filter(|(target_provider_id, _, model)| {
                *target_provider_id == provider_id && model == upstream_model
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            sinks.remove(key);
        }
        keys
    };
    for key in &removed {
        state.mark_target_sink_dirty(key);
    }
}

pub(crate) async fn clear_target_sinks_for_route(state: &AppState, route_id: i64) -> usize {
    let pairs = sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, upstream_model FROM route_targets WHERE route_id = ?",
    )
    .bind(route_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let pairs = pairs.into_iter().collect::<HashSet<_>>();
    let removed = {
        let mut sinks = state.target_sink.lock().await;
        let keys = sinks
            .keys()
            .filter(|(provider_id, _, model)| pairs.contains(&(*provider_id, model.clone())))
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            sinks.remove(key);
        }
        keys
    };
    for key in &removed {
        state.mark_target_sink_dirty(key);
    }
    removed.len()
}

pub(in crate::proxy) async fn target_sink_active(state: &AppState, target: &RouteTarget) -> bool {
    let now = Instant::now();
    let mut sinks = state.target_sink.lock().await;
    sinks.retain(|_, entry| entry.until > now);
    sinks.contains_key(&target_sink_key(target))
}

pub(in crate::proxy) async fn busy_waitable_error(
    state: &AppState,
    target: &RouteTarget,
    error: &AppError,
) -> bool {
    match error {
        AppError::Upstream(_) => true,
        AppError::UpstreamStatus { status, .. } => match *status {
            StatusCode::TOO_MANY_REQUESTS => target_sink_active(state, target).await,
            StatusCode::REQUEST_TIMEOUT => true,
            status if status.as_u16() == 425 => true,
            status if status.is_server_error() => true,
            _ => false,
        },
        _ => false,
    }
}

pub(in crate::proxy) fn busy_wait_retry_after(error: &AppError) -> Option<Duration> {
    match error {
        AppError::UpstreamStatus { retry_after, .. } => *retry_after,
        _ => None,
    }
}

pub(in crate::proxy) fn busy_wait_delay(
    jitter_ms: i64,
    retry_after: Option<Duration>,
    round: u32,
) -> Duration {
    const MIN_DELAY: Duration = Duration::from_millis(50);
    const MAX_DELAY: Duration = Duration::from_secs(5);
    let jitter_ms = u64::try_from(jitter_ms.max(0)).unwrap_or(0);
    let jitter = if jitter_ms == 0 {
        Duration::ZERO
    } else {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos() as u64)
            .unwrap_or(0);
        Duration::from_millis(nanos % jitter_ms.saturating_add(1))
    };
    let hinted = retry_after.unwrap_or_default().min(MAX_DELAY);
    let backoff = MIN_DELAY
        .checked_mul(1u32.checked_shl(round.min(16)).unwrap_or(u32::MAX))
        .unwrap_or(MAX_DELAY)
        .min(MAX_DELAY);
    (backoff.max(hinted) + jitter).min(MAX_DELAY)
}
