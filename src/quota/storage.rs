use super::*;

pub(crate) async fn restore(state: &AppState) -> AppResult<()> {
    let records = sqlx::query_as::<_, ProviderQuotaSnapshotRecord>(
        "SELECT provider_id, provider_key_id, model, window_key, \
                limit_value, used, remaining, unit, reset_at, source, \
                confidence, observed_at, valid_until \
         FROM provider_quota_snapshots",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut cache = state.quota_snapshots.lock().await;
    cache.clear();
    for record in records {
        cache.insert(record.key(), record);
    }
    Ok(())
}

pub(crate) async fn store_provider_quota_view(
    state: &AppState,
    provider_id: i64,
    provider_key_id: Option<i64>,
    view: &ProviderQuotaView,
) -> AppResult<Vec<ProviderQuotaSnapshotView>> {
    let records = records_from_provider_view(view, provider_id, provider_key_id);
    // Upsert instead of replacing the whole provider/key scope: a partial
    // upstream response must not erase a still-fresh reading for a window that
    // this particular fetch omitted.
    upsert_source_records(state, SOURCE_PROVIDER_API, &records).await?;
    views_for_scope(state, provider_id, provider_key_id).await
}

/// Parses only explicit rate-limit headers. Ambiguous values are ignored
/// rather than guessed, because a wrong remaining count would be worse than an
/// `unknown` snapshot.
pub(crate) async fn observe_response_headers(
    state: &AppState,
    target: &RouteTarget,
    headers: &HeaderMap,
) {
    let records = records_from_rate_limit_headers(
        target.provider_id,
        target.provider_api_key_id,
        &target.upstream_model,
        headers,
    );
    if records.is_empty() {
        return;
    }
    cache_and_persist_records(state, SOURCE_RESPONSE_HEADER, records).await;
}

/// A single explicit quota exhaustion can be short-lived. It is kept as a
/// low-confidence inference and never becomes a permanent cooldown.
pub(crate) async fn observe_runtime_quota_failure(
    state: &AppState,
    target: &RouteTarget,
    retry_after: Option<StdDuration>,
) {
    let now = Utc::now();
    let ttl = retry_after
        .map(|duration| duration.as_secs().clamp(60, DEFAULT_VALID_SECONDS as u64) as i64)
        .unwrap_or(DEFAULT_VALID_SECONDS);
    let record = ProviderQuotaSnapshotRecord {
        provider_id: target.provider_id,
        provider_key_id: target.provider_api_key_id,
        model: target.upstream_model.clone(),
        window_key: "custom".to_string(),
        used: None,
        limit: None,
        remaining: Some(0.0),
        unit: "request".to_string(),
        reset_at: retry_after.map(|duration| {
            (now + ChronoDuration::seconds(duration.as_secs() as i64)).to_rfc3339()
        }),
        source: SOURCE_RUNTIME_429.to_string(),
        confidence: "inferred".to_string(),
        observed_at: now.to_rfc3339(),
        valid_until: (now + ChronoDuration::seconds(ttl)).to_rfc3339(),
    };
    cache_and_persist_records(state, SOURCE_RUNTIME_429, vec![record]).await;
}

pub(super) async fn cache_and_persist_records(
    state: &AppState,
    source: &str,
    records: Vec<ProviderQuotaSnapshotRecord>,
) {
    {
        let mut cache = state.quota_snapshots.lock().await;
        for record in &records {
            cache.insert(record.key(), record.clone());
        }
    }

    let now = Instant::now();
    let mut persist = Vec::new();
    {
        let mut throttle = HEADER_PERSIST_THROTTLE.lock().await;
        for record in records {
            let key = record.key();
            let should_write = throttle
                .get(&key)
                .is_none_or(|last| now.saturating_duration_since(*last) >= HEADER_PERSIST_INTERVAL);
            if should_write {
                throttle.insert(key, now);
                persist.push(record);
            }
        }
    }
    if persist.is_empty() {
        return;
    }
    let state = state.clone();
    let source = source.to_string();
    tokio::spawn(async move {
        if let Err(error) = upsert_source_records(&state, &source, &persist).await {
            tracing::warn!(%error, %source, "failed to persist provider quota observation");
        }
    });
}

pub(crate) async fn upsert_manual_snapshots(
    state: &AppState,
    provider_id: i64,
    inputs: &[ProviderQuotaSnapshotInput],
) -> AppResult<Vec<ProviderQuotaSnapshotView>> {
    if inputs.is_empty() {
        return Err(AppError::BadRequest(
            "at least one quota snapshot is required".to_string(),
        ));
    }

    let observed_at = Utc::now();
    let shared_key_id = inputs.first().and_then(|input| input.key_id);
    if inputs.iter().any(|input| input.key_id != shared_key_id) {
        return Err(AppError::BadRequest(
            "one quota snapshot request must target a single provider key".to_string(),
        ));
    }
    let mut records = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let Some(key_id) = input.key_id {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM provider_api_keys WHERE id = ? AND provider_id = ?",
            )
            .bind(key_id)
            .bind(provider_id)
            .fetch_one(&state.pool)
            .await?;
            if count == 0 {
                return Err(AppError::BadRequest(
                    "quota snapshot key does not belong to this provider".to_string(),
                ));
            }
        }

        let window_key = normalize_window_key(&input.window_key);
        if !matches!(
            window_key.as_str(),
            "5h" | "daily" | "weekly" | "monthly" | "balance" | "custom"
        ) {
            return Err(AppError::BadRequest(format!(
                "unsupported quota window: {}",
                input.window_key
            )));
        }
        let unit = normalize_unit(&input.unit);
        if unit.is_empty() {
            return Err(AppError::BadRequest(
                "quota snapshot unit is required".to_string(),
            ));
        }
        if input.used.is_none() && input.limit.is_none() && input.remaining.is_none() {
            return Err(AppError::BadRequest(
                "quota snapshot needs used, limit or remaining".to_string(),
            ));
        }
        let used = finite_non_negative(input.used, "used")?;
        let limit = finite_non_negative(input.limit, "limit")?;
        let remaining = finite_non_negative(input.remaining, "remaining")?;
        let reset_at = normalize_reset_at(input.reset_at.as_deref())?;
        let valid_seconds = input
            .valid_seconds
            .unwrap_or_else(|| ttl_seconds_for(&window_key))
            .clamp(MIN_VALID_SECONDS, MAX_VALID_SECONDS);
        records.push(ProviderQuotaSnapshotRecord {
            provider_id,
            provider_key_id: input.key_id,
            model: input
                .model
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string(),
            window_key,
            used,
            limit,
            remaining,
            unit,
            reset_at,
            source: SOURCE_MANUAL.to_string(),
            confidence: "exact".to_string(),
            observed_at: observed_at.to_rfc3339(),
            valid_until: (observed_at + ChronoDuration::seconds(valid_seconds)).to_rfc3339(),
        });
    }

    upsert_source_records(state, SOURCE_MANUAL, &records).await?;
    views_for_scope(state, provider_id, shared_key_id).await
}

pub(crate) async fn views_for_scope(
    state: &AppState,
    provider_id: i64,
    provider_key_id: Option<i64>,
) -> AppResult<Vec<ProviderQuotaSnapshotView>> {
    let rows = if let Some(provider_key_id) = provider_key_id {
        sqlx::query_as::<_, ProviderQuotaSnapshotRecord>(
            "SELECT provider_id, provider_key_id, model, window_key, \
                    limit_value, used, remaining, unit, reset_at, source, \
                    confidence, observed_at, valid_until \
             FROM provider_quota_snapshots \
             WHERE provider_id = ? AND provider_key_id IS ?",
        )
        .bind(provider_id)
        .bind(provider_key_id)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, ProviderQuotaSnapshotRecord>(
            "SELECT provider_id, provider_key_id, model, window_key, \
                    limit_value, used, remaining, unit, reset_at, source, \
                    confidence, observed_at, valid_until \
             FROM provider_quota_snapshots \
             WHERE provider_id = ?",
        )
        .bind(provider_id)
        .fetch_all(&state.pool)
        .await?
    };
    Ok(effective_snapshot_views(rows))
}

pub(super) async fn upsert_source_records(
    state: &AppState,
    source: &str,
    records: &[ProviderQuotaSnapshotRecord],
) -> AppResult<()> {
    let mut transaction = state.pool.begin().await?;
    for record in records {
        sqlx::query(
            "DELETE FROM provider_quota_snapshots \
             WHERE provider_id = ? AND provider_key_id IS ? AND model = ? \
               AND window_key = ? AND unit = ? AND source = ?",
        )
        .bind(record.provider_id)
        .bind(record.provider_key_id)
        .bind(&record.model)
        .bind(&record.window_key)
        .bind(&record.unit)
        .bind(source)
        .execute(&mut *transaction)
        .await?;
        insert_record(&mut transaction, record).await?;
    }
    transaction.commit().await?;
    for record in records {
        update_cache(
            state,
            record.provider_id,
            record.provider_key_id,
            source,
            std::slice::from_ref(record),
        )
        .await;
    }
    Ok(())
}

pub(super) async fn insert_record(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &ProviderQuotaSnapshotRecord,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO provider_quota_snapshots \
         (provider_id, provider_key_id, model, window_key, used, limit_value, \
          remaining, unit, reset_at, source, confidence, observed_at, valid_until) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.provider_id)
    .bind(record.provider_key_id)
    .bind(&record.model)
    .bind(&record.window_key)
    .bind(record.used)
    .bind(record.limit)
    .bind(record.remaining)
    .bind(&record.unit)
    .bind(&record.reset_at)
    .bind(&record.source)
    .bind(&record.confidence)
    .bind(&record.observed_at)
    .bind(&record.valid_until)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(super) async fn update_cache(
    state: &AppState,
    provider_id: i64,
    provider_key_id: Option<i64>,
    source: &str,
    records: &[ProviderQuotaSnapshotRecord],
) {
    let mut cache = state.quota_snapshots.lock().await;
    cache.retain(|key, _| {
        !(key.provider_id == provider_id
            && key.provider_key_id == provider_key_id
            && key.source == source)
    });
    for record in records {
        cache.insert(record.key(), record.clone());
    }
}
