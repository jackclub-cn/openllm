use super::*;

pub(in crate::proxy) async fn mark_provider_api_key_used(
    state: &AppState,
    provider_api_key_id: Option<i64>,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    {
        let mut touched = state.provider_key_touched.lock().await;
        if touched
            .get(&provider_api_key_id)
            .is_some_and(|last| last.elapsed() < PROVIDER_KEY_TOUCH_INTERVAL)
        {
            return;
        }
        touched.insert(provider_api_key_id, Instant::now());
    }

    let capacity = state.acquire_background_db_slot().await;
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to update provider key usage");
        }
        drop(capacity);
    });
}

pub(in crate::proxy) async fn mark_provider_api_key_error(
    state: &AppState,
    provider_api_key_id: Option<i64>,
    status: StatusCode,
    message: &str,
    retry_after: Option<Duration>,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    let deadline = Instant::now() + provider_key_cooldown(status, retry_after);
    state
        .provider_key_cooldown
        .lock()
        .await
        .entry(provider_api_key_id)
        .and_modify(|until| *until = (*until).max(deadline))
        .or_insert(deadline);
    state.mark_provider_key_resilience_dirty(provider_api_key_id);
    state
        .provider_key_error_state
        .lock()
        .await
        .insert(provider_api_key_id);
    let message = message.chars().take(1000).collect::<String>();
    let capacity = state.acquire_background_db_slot().await;
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error = ? \
             WHERE id = ?",
        )
        .bind(message)
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to record provider key error");
        }
        drop(capacity);
    });
}

pub(in crate::proxy) async fn mark_provider_api_key_success(
    state: &AppState,
    provider_api_key_id: Option<i64>,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    state
        .provider_key_cooldown
        .lock()
        .await
        .remove(&provider_api_key_id);
    state
        .provider_key_failure_meta
        .lock()
        .await
        .remove(&provider_api_key_id);
    state.mark_provider_key_resilience_dirty(provider_api_key_id);
    let had_error = state
        .provider_key_error_state
        .lock()
        .await
        .remove(&provider_api_key_id);
    if !had_error {
        return;
    }
    if let Err(error) = sqlx::query(
        "UPDATE provider_api_keys \
         SET last_error_at = NULL, last_error = NULL \
         WHERE id = ?",
    )
    .bind(provider_api_key_id)
    .execute(&state.pool)
    .await
    {
        state
            .provider_key_error_state
            .lock()
            .await
            .insert(provider_api_key_id);
        tracing::warn!(
            %error,
            provider_api_key_id,
            "failed to clear recovered provider key error"
        );
    }
}

pub(in crate::proxy) async fn expand_target_provider_keys(
    state: &AppState,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
) -> AppResult<Vec<RouteTarget>> {
    let mut keys_by_provider = HashMap::new();
    for target in &targets {
        if keys_by_provider.contains_key(&target.provider_id) {
            continue;
        }
        let keys = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            ),
        >(
            "SELECT id, name, secret, max_concurrency, requests_per_minute, \
                    queue_limit, queue_wait_ms FROM provider_api_keys \
             WHERE provider_id = ? AND enabled = 1 \
             ORDER BY id",
        )
        .bind(target.provider_id)
        .fetch_all(&state.pool)
        .await?;
        keys_by_provider.insert(target.provider_id, keys);
    }

    let mut cooldowns = state.provider_key_cooldown.lock().await;
    let now = Instant::now();
    cooldowns.retain(|_, until| *until > now);
    let mut cursors = state.provider_key_cursor.lock().await;
    let mut expanded = Vec::new();
    for target in targets {
        let keys = keys_by_provider
            .get(&target.provider_id)
            .cloned()
            .unwrap_or_default();
        if keys.is_empty() {
            expanded.push(target);
            continue;
        }

        let available = keys
            .iter()
            .filter(|(id, _, _, _, _, _, _)| !cooldowns.contains_key(id))
            .cloned()
            .collect::<Vec<_>>();
        let mut keys = if available.is_empty() {
            keys
        } else {
            available
        };
        if let Some(session_id) = session_id {
            let mut ranked = keys
                .drain(..)
                .map(|key| {
                    (
                        session_provider_key_hash(session_id, target.provider_id, key.0),
                        key,
                    )
                })
                .collect::<Vec<_>>();
            ranked
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.0.cmp(&right.1.0)));
            keys.extend(ranked.into_iter().map(|(_, key)| key));
        } else {
            let cursor = cursors.entry(target.provider_id).or_default();
            let offset = *cursor % keys.len();
            keys.rotate_left(offset);
            *cursor = cursor.wrapping_add(1);
        }

        for (
            key_id,
            key_name,
            secret,
            max_concurrency,
            requests_per_minute,
            queue_limit,
            queue_wait_ms,
        ) in keys
        {
            let mut candidate = target.clone();
            candidate.api_key = Some(secret);
            candidate.provider_api_key_id = Some(key_id);
            candidate.provider_api_key_name = Some(key_name);
            candidate.provider_key_max_concurrency = max_concurrency;
            candidate.provider_key_requests_per_minute = requests_per_minute;
            candidate.provider_key_queue_limit = queue_limit;
            candidate.provider_key_queue_wait_ms = queue_wait_ms;
            expanded.push(candidate);
        }
    }

    let last_index = expanded.len().saturating_sub(1);
    for (index, target) in expanded.iter_mut().enumerate() {
        target.auth_retryable = index < last_index;
    }
    Ok(expanded)
}
