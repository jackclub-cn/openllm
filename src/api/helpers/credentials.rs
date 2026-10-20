use super::*;

impl From<ProviderApiKeyRecord> for ProviderApiKeyView {
    fn from(value: ProviderApiKeyRecord) -> Self {
        let api_key_suffix = api_key_suffix(&value.secret);
        Self {
            id: value.id,
            name: value.name,
            api_key_set: !value.secret.is_empty(),
            api_key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            last_error_at: value.last_error_at,
            last_error: value.last_error,
            last_test_at: value.last_test_at,
            last_test_ok: value.last_test_ok.map(|value| value != 0),
            last_test_latency_ms: value.last_test_latency_ms,
            last_test_checked: value.last_test_checked,
            last_test_message: value.last_test_message,
            requests: value.lifetime_requests,
            success_rate: if value.lifetime_requests > 0 {
                value.lifetime_successes as f64 / value.lifetime_requests as f64 * 100.0
            } else {
                0.0
            },
            avg_latency_ms: if value.lifetime_requests > 0 {
                value.lifetime_latency_ms as f64 / value.lifetime_requests as f64
            } else {
                0.0
            },
            prompt_tokens: value.lifetime_prompt_tokens,
            completion_tokens: value.lifetime_completion_tokens,
            max_concurrency: value.max_concurrency,
            requests_per_minute: value.requests_per_minute,
            queue_limit: value.queue_limit,
            queue_wait_ms: value.queue_wait_ms,
            cooldown_seconds: None,
            created_at: value.created_at,
        }
    }
}

pub(in crate::api) fn api_key_suffix(secret: &str) -> String {
    let suffix = secret.chars().rev().take(4).collect::<String>();
    suffix.chars().rev().collect()
}

pub(in crate::api) async fn provider_api_key_records(
    pool: &sqlx::SqlitePool,
    provider_id: i64,
) -> AppResult<Vec<ProviderApiKeyRecord>> {
    Ok(sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

pub(in crate::api) async fn clear_provider_key_cooldowns(
    state: &AppState,
    provider_api_key_ids: impl IntoIterator<Item = i64>,
) {
    let provider_api_key_ids = provider_api_key_ids.into_iter().collect::<Vec<_>>();
    let mut removed = Vec::new();
    {
        let mut cooldowns = state.provider_key_cooldown.lock().await;
        for provider_api_key_id in &provider_api_key_ids {
            if cooldowns.remove(provider_api_key_id).is_some() {
                removed.push(*provider_api_key_id);
            }
        }
    }
    {
        let mut metadata = state.provider_key_failure_meta.lock().await;
        for provider_api_key_id in &provider_api_key_ids {
            metadata.remove(provider_api_key_id);
        }
    }
    {
        let mut rpm = state.provider_key_rpm.lock().await;
        for provider_api_key_id in &provider_api_key_ids {
            rpm.remove(provider_api_key_id);
        }
    }
    {
        let mut concurrency = state.provider_key_concurrency.lock().await;
        for provider_api_key_id in &provider_api_key_ids {
            concurrency.remove(provider_api_key_id);
        }
    }
    {
        let mut queues = state.provider_key_queue.lock().await;
        for provider_api_key_id in &provider_api_key_ids {
            queues.remove(provider_api_key_id);
        }
    }
    let removed_sinks = {
        let mut sinks = state.target_sink.lock().await;
        let keys = sinks
            .keys()
            .filter(|(_, provider_key_id, _)| {
                provider_key_id.is_some_and(|key_id| provider_api_key_ids.contains(&key_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            sinks.remove(key);
        }
        keys
    };
    // Cleared cooldowns must be removed from the persisted mirror as well, or a
    // restart would restore a cooldown an operator explicitly lifted.
    for provider_api_key_id in removed {
        state.mark_provider_key_resilience_dirty(provider_api_key_id);
    }
    for key in &removed_sinks {
        state.mark_target_sink_dirty(key);
    }
}

pub(in crate::api) async fn provider_key_candidates(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<(Option<i64>, Option<String>)>> {
    let mut candidates = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| (Some(record.id), Some(record.secret)))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates.push((None, provider.api_key.clone()));
    }
    Ok(candidates)
}

pub(in crate::api) async fn provider_probe_keys(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<ProviderProbeKey>> {
    let mut keys = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| ProviderProbeKey {
            id: Some(record.id),
            name: record.name,
            api_key_suffix: api_key_suffix(&record.secret),
            secret: Some(record.secret),
        })
        .collect::<Vec<_>>();
    if keys.is_empty()
        && let Some(secret) = normalize_optional(provider.api_key.clone())
    {
        keys.push(ProviderProbeKey {
            id: None,
            name: "Default".to_string(),
            api_key_suffix: api_key_suffix(&secret),
            secret: Some(secret),
        });
    }
    Ok(keys)
}

pub(in crate::api) async fn hydrate_provider_view(
    state: &AppState,
    view: &mut ProviderView,
) -> AppResult<()> {
    view.api_keys = provider_api_key_records(&state.pool, view.id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    let now = std::time::Instant::now();
    let open_until = state.provider_cooldown.lock().await.get(&view.id).copied();
    let cooling = open_until.is_some_and(|until| until > now);
    view.cooldown_seconds = open_until.and_then(|until| {
        let remaining = until.saturating_duration_since(now);
        (!remaining.is_zero()).then(|| (remaining.as_secs_f64().ceil() as i64).max(1))
    });
    let streak = state
        .provider_failure_streak
        .lock()
        .await
        .get(&view.id)
        .copied()
        .unwrap_or(0);
    view.circuit_state = crate::state::provider_circuit_state(
        streak,
        cooling,
        crate::state::provider_open_threshold(),
    );
    let cooldowns = state.provider_key_cooldown.lock().await;
    for key in &mut view.api_keys {
        if let Some(until) = cooldowns.get(&key.id) {
            let remaining = until.saturating_duration_since(now);
            if !remaining.is_zero() {
                key.cooldown_seconds = Some((remaining.as_secs_f64().ceil() as i64).max(1));
            }
        }
    }
    view.api_key_set = !view.api_keys.is_empty();
    Ok(())
}

pub(in crate::api) async fn hydrate_provider_views(
    state: &AppState,
    views: &mut [ProviderView],
) -> AppResult<()> {
    if views.is_empty() {
        return Ok(());
    }

    let mut models_by_provider = HashMap::<i64, Vec<String>>::new();
    for (provider_id, model_name) in sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, model_name \
         FROM provider_models \
         WHERE enabled = 1 \
         ORDER BY provider_id, model_name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    {
        models_by_provider
            .entry(provider_id)
            .or_default()
            .push(model_name);
    }

    let mut keys_by_provider = HashMap::<i64, Vec<ProviderApiKeyView>>::new();
    for record in sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys ORDER BY provider_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        if let Some(provider_id) = record.provider_id {
            keys_by_provider
                .entry(provider_id)
                .or_default()
                .push(record.into());
        }
    }

    let now = std::time::Instant::now();
    let threshold = crate::state::provider_open_threshold();
    let failure_streaks = state.provider_failure_streak.lock().await.clone();
    let provider_cooldowns = state.provider_cooldown.lock().await;
    let cooldowns = state.provider_key_cooldown.lock().await;
    for view in views {
        view.models = models_by_provider.remove(&view.id).unwrap_or_default();
        view.api_keys = keys_by_provider.remove(&view.id).unwrap_or_default();
        let until = provider_cooldowns.get(&view.id).copied();
        let cooling = until.is_some_and(|until| until > now);
        view.cooldown_seconds = until.and_then(|until| {
            let remaining = until.saturating_duration_since(now);
            (!remaining.is_zero()).then(|| (remaining.as_secs_f64().ceil() as i64).max(1))
        });
        view.circuit_state = crate::state::provider_circuit_state(
            failure_streaks.get(&view.id).copied().unwrap_or(0),
            cooling,
            threshold,
        );
        for key in &mut view.api_keys {
            if let Some(until) = cooldowns.get(&key.id) {
                let remaining = until.saturating_duration_since(now);
                if !remaining.is_zero() {
                    key.cooldown_seconds = Some((remaining.as_secs_f64().ceil() as i64).max(1));
                }
            }
        }
        view.api_key_set = !view.api_keys.is_empty();
    }
    Ok(())
}

pub(in crate::api) fn provider_api_key_input(
    id: Option<i64>,
    name: impl Into<String>,
    api_key: Option<String>,
    enabled: bool,
) -> ProviderApiKeyInput {
    ProviderApiKeyInput {
        id,
        name: name.into(),
        api_key,
        enabled,
        max_concurrency: None,
        requests_per_minute: None,
        queue_limit: None,
        queue_wait_ms: None,
    }
}

#[derive(Default)]
pub(in crate::api) struct ProviderApiKeyMutation {
    pub(in crate::api) changed_secret_ids: Vec<i64>,
    pub(in crate::api) removed_ids: Vec<i64>,
}

pub(in crate::api) async fn replace_provider_api_keys(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    inputs: &[ProviderApiKeyInput],
) -> AppResult<ProviderApiKeyMutation> {
    let existing = sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|record| (record.id, record))
    .collect::<HashMap<_, _>>();
    let mut retained = HashSet::new();
    let mut secrets = HashSet::new();
    let mut mutation = ProviderApiKeyMutation::default();

    for (index, input) in inputs.iter().enumerate() {
        let current = match input.id {
            Some(id) => Some(
                existing
                    .get(&id)
                    .ok_or_else(|| {
                        AppError::BadRequest("API key does not belong to this provider".to_string())
                    })?
                    .clone(),
            ),
            None => None,
        };
        let requested_secret = normalize_optional(input.api_key.clone());
        let secret = match (requested_secret, current.as_ref()) {
            (Some(secret), _) => secret,
            (None, Some(current)) => current.secret.clone(),
            (None, None) => {
                return Err(AppError::BadRequest(
                    "new provider API keys must include a secret".to_string(),
                ));
            }
        };
        if !secrets.insert(secret.clone()) {
            return Err(AppError::BadRequest(
                "provider API keys must be unique".to_string(),
            ));
        }
        let name = match input.name.trim() {
            "" => current
                .as_ref()
                .map(|current| current.name.clone())
                .unwrap_or_else(|| format!("Key {}", index + 1)),
            name => name.to_string(),
        };
        if name.chars().count() > 80 {
            return Err(AppError::BadRequest(
                "provider API key name must be at most 80 characters".to_string(),
            ));
        }
        let max_concurrency = normalize_provider_concurrency(
            input
                .max_concurrency
                .or_else(|| current.as_ref().and_then(|key| key.max_concurrency)),
        )?;
        let requests_per_minute = normalize_provider_key_rpm(
            input
                .requests_per_minute
                .or_else(|| current.as_ref().and_then(|key| key.requests_per_minute)),
        )?;
        let queue_limit = normalize_provider_key_queue_limit(
            input
                .queue_limit
                .or_else(|| current.as_ref().and_then(|key| key.queue_limit)),
        )?;
        let queue_wait_ms = normalize_provider_key_queue_wait(
            input
                .queue_wait_ms
                .or_else(|| current.as_ref().and_then(|key| key.queue_wait_ms)),
        )?;

        if let Some(current) = current {
            retained.insert(current.id);
            if current.secret != secret {
                mutation.changed_secret_ids.push(current.id);
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         last_used_at = NULL, last_error_at = NULL, last_error = NULL, \
                         last_test_at = NULL, last_test_ok = NULL, \
                         last_test_latency_ms = NULL, last_test_checked = NULL, \
                         last_test_message = NULL, \
                         max_concurrency = ?, requests_per_minute = ?, \
                         queue_limit = ?, queue_wait_ms = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(max_concurrency)
                .bind(requests_per_minute)
                .bind(queue_limit)
                .bind(queue_wait_ms)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            } else {
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         max_concurrency = ?, requests_per_minute = ?, \
                         queue_limit = ?, queue_wait_ms = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(max_concurrency)
                .bind(requests_per_minute)
                .bind(queue_limit)
                .bind(queue_wait_ms)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            }
        } else {
            let result = sqlx::query(
                "INSERT INTO provider_api_keys ( \
                    provider_id, name, secret, enabled, max_concurrency, \
                    requests_per_minute, queue_limit, queue_wait_ms \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(provider_id)
            .bind(name)
            .bind(secret)
            .bind(input.enabled as i64)
            .bind(max_concurrency)
            .bind(requests_per_minute)
            .bind(queue_limit)
            .bind(queue_wait_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlite_conflict)?;
            retained.insert(result.last_insert_rowid());
        }
    }

    for id in existing.keys().filter(|id| !retained.contains(id)) {
        sqlx::query("DELETE FROM provider_api_keys WHERE id = ? AND provider_id = ?")
            .bind(id)
            .bind(provider_id)
            .execute(&mut **tx)
            .await?;
        mutation.removed_ids.push(*id);
    }

    let first_secret = sqlx::query_scalar::<_, String>(
        "SELECT secret FROM provider_api_keys \
         WHERE provider_id = ? AND enabled = 1 ORDER BY id LIMIT 1",
    )
    .bind(provider_id)
    .fetch_optional(&mut **tx)
    .await?;
    sqlx::query("UPDATE providers SET api_key = ? WHERE id = ?")
        .bind(first_secret)
        .bind(provider_id)
        .execute(&mut **tx)
        .await?;
    Ok(mutation)
}
