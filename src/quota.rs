use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration as StdDuration, Instant};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::error::{AppError, AppResult};
use crate::models::{
    ProviderQuotaSnapshotInput, ProviderQuotaSnapshotKey, ProviderQuotaSnapshotRecord,
    ProviderQuotaSnapshotView, ProviderQuotaView, RouteTarget,
};
use crate::state::AppState;

/// Unknown readings stay neutral. A known reading at or above this pressure is
/// still usable, but eligible for a soft routing penalty in M3.
const QUOTA_LOW_WATERMARK_PCT: f64 = 90.0;
const DEFAULT_VALID_SECONDS: i64 = 15 * 60;
const MIN_VALID_SECONDS: i64 = 60;
const MAX_VALID_SECONDS: i64 = 30 * 24 * 60 * 60;

const SOURCE_PROVIDER_API: &str = "provider_api";
const SOURCE_RESPONSE_HEADER: &str = "response_header";
const SOURCE_RUNTIME_429: &str = "runtime_429";
const SOURCE_MANUAL: &str = "manual";
const HEADER_PERSIST_INTERVAL: StdDuration = StdDuration::from_secs(60);

static HEADER_PERSIST_THROTTLE: LazyLock<
    tokio::sync::Mutex<HashMap<ProviderQuotaSnapshotKey, Instant>>,
> = LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

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

async fn cache_and_persist_records(
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
    Ok(effective_records(rows)
        .into_iter()
        .map(|record| quota_snapshot_view(record, Utc::now()))
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QuotaScopeState {
    pub state: &'static str,
    pub pressure_pct: Option<f64>,
    pub reset_at: Option<String>,
}

pub(crate) type QuotaTargetKey = (i64, Option<i64>, String);

pub(crate) fn quota_target_key(target: &RouteTarget) -> QuotaTargetKey {
    (
        target.provider_id,
        target.provider_api_key_id,
        target.upstream_model.clone(),
    )
}

/// Scores a quota state for `quota_paced`.
///
/// Unknown stays neutral. Healthy targets are ranked by remaining pressure
/// divided by hours until reset, so a target approaching its reset is used
/// before its remaining allowance is wasted.
pub(crate) fn quota_pacing_score(quota: Option<&QuotaScopeState>, now: DateTime<Utc>) -> f64 {
    let Some(quota) = quota else {
        return 0.5;
    };
    match quota.state {
        "exhausted" => 0.0,
        "low" => {
            let pressure = quota.pressure_pct.unwrap_or(100.0).clamp(0.0, 100.0);
            0.25 + (1.0 - pressure / 100.0) * 0.25
        }
        "healthy" => {
            let pressure = quota.pressure_pct.unwrap_or(50.0).clamp(0.0, 100.0);
            let remaining = 1.0 - pressure / 100.0;
            let hours = quota
                .reset_at
                .as_deref()
                .and_then(parse_rfc3339)
                .map(|reset| (reset - now).num_seconds().max(900) as f64 / 3_600.0)
                .unwrap_or(1.0);
            let pace = (remaining / hours.max(0.25)).clamp(0.0, 1.0);
            0.5 + pace * 0.5
        }
        _ => 0.5,
    }
}

/// Collapses one scope's windows using the routing-facing precedence:
/// exhausted first, then low pressure, then healthy, then reset-due/unknown.
pub(crate) fn effective_scope_state(snapshots: &[ProviderQuotaSnapshotView]) -> QuotaScopeState {
    let exhausted = snapshots
        .iter()
        .filter(|snapshot| snapshot.state == "exhausted")
        .collect::<Vec<_>>();
    if !exhausted.is_empty() {
        let reset_at = exhausted
            .iter()
            .filter_map(|snapshot| snapshot.reset_at.as_deref().and_then(parse_rfc3339))
            .max()
            .map(|value| value.to_rfc3339());
        return QuotaScopeState {
            state: "exhausted",
            pressure_pct: exhausted
                .iter()
                .filter_map(|snapshot| snapshot.percent)
                .max_by(f64::total_cmp),
            reset_at,
        };
    }
    let pressure_snapshot = snapshots
        .iter()
        .filter(|snapshot| matches!(snapshot.state.as_str(), "healthy" | "low"))
        .filter(|snapshot| snapshot.percent.is_some())
        .max_by(|left, right| left.percent.unwrap().total_cmp(&right.percent.unwrap()));
    let pressure_pct = pressure_snapshot.and_then(|snapshot| snapshot.percent);
    let pressure_reset = pressure_snapshot.and_then(|snapshot| snapshot.reset_at.clone());
    if snapshots.iter().any(|snapshot| snapshot.state == "low") {
        return QuotaScopeState {
            state: "low",
            pressure_pct,
            reset_at: pressure_reset,
        };
    }
    if pressure_pct.is_some() {
        return QuotaScopeState {
            state: "healthy",
            pressure_pct,
            reset_at: pressure_reset,
        };
    }
    if snapshots
        .iter()
        .any(|snapshot| snapshot.state == "reset_due")
    {
        return QuotaScopeState {
            state: "reset_due",
            pressure_pct: None,
            reset_at: None,
        };
    }
    QuotaScopeState {
        state: "unknown",
        pressure_pct: None,
        reset_at: None,
    }
}

pub(crate) async fn target_scope_states(
    state: &AppState,
    targets: &[RouteTarget],
) -> HashMap<QuotaTargetKey, QuotaScopeState> {
    let now = Utc::now();
    let cache = state.quota_snapshots.lock().await;
    targets
        .iter()
        .map(|target| {
            let records = cache
                .values()
                .filter(|record| {
                    if record.provider_id != target.provider_id {
                        return false;
                    }
                    if !record.model.is_empty() && record.model != target.upstream_model {
                        return false;
                    }
                    match target.provider_api_key_id {
                        Some(key_id) => {
                            record.provider_key_id.is_none()
                                || record.provider_key_id == Some(key_id)
                        }
                        None => record.provider_key_id.is_none(),
                    }
                })
                .cloned()
                .collect::<Vec<_>>();
            let snapshots = effective_records(records)
                .into_iter()
                .map(|record| quota_snapshot_view(record, now))
                .collect::<Vec<_>>();
            (quota_target_key(target), effective_scope_state(&snapshots))
        })
        .collect()
}

fn records_from_rate_limit_headers(
    provider_id: i64,
    provider_key_id: Option<i64>,
    model: &str,
    headers: &HeaderMap,
) -> Vec<ProviderQuotaSnapshotRecord> {
    let now = Utc::now();
    let mut records = Vec::new();
    for (window_key, unit, limit_names, remaining_names, reset_names) in [
        (
            "requests",
            "request",
            &[
                "x-ratelimit-limit-requests",
                "anthropic-ratelimit-requests-limit",
            ][..],
            &[
                "x-ratelimit-remaining-requests",
                "anthropic-ratelimit-requests-remaining",
            ][..],
            &[
                "x-ratelimit-reset-requests",
                "anthropic-ratelimit-requests-reset",
            ][..],
        ),
        (
            "tokens",
            "token",
            &[
                "x-ratelimit-limit-tokens",
                "anthropic-ratelimit-tokens-limit",
            ][..],
            &[
                "x-ratelimit-remaining-tokens",
                "anthropic-ratelimit-tokens-remaining",
            ][..],
            &[
                "x-ratelimit-reset-tokens",
                "anthropic-ratelimit-tokens-reset",
            ][..],
        ),
        (
            "custom",
            "request",
            &["x-ratelimit-limit"][..],
            &["x-ratelimit-remaining"][..],
            &["x-ratelimit-reset"][..],
        ),
    ] {
        let limit = first_header_f64(headers, limit_names);
        let remaining = first_header_f64(headers, remaining_names);
        if limit.is_none() && remaining.is_none() {
            continue;
        }
        let reset_at = first_header_value(headers, reset_names)
            .and_then(|value| parse_header_reset(value, now));
        records.push(ProviderQuotaSnapshotRecord {
            provider_id,
            provider_key_id,
            model: model.to_string(),
            window_key: window_key.to_string(),
            used: limit
                .zip(remaining)
                .map(|(limit, remaining)| (limit - remaining).max(0.0)),
            limit,
            remaining,
            unit: unit.to_string(),
            reset_at,
            source: SOURCE_RESPONSE_HEADER.to_string(),
            confidence: if limit.is_some() && remaining.is_some() {
                "exact"
            } else {
                "inferred"
            }
            .to_string(),
            observed_at: now.to_rfc3339(),
            valid_until: (now + ChronoDuration::seconds(ttl_seconds_for(window_key))).to_rfc3339(),
        });
    }
    records
}

fn first_header_value<'a>(headers: &'a HeaderMap, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_header_f64(headers: &HeaderMap, names: &[&str]) -> Option<f64> {
    first_header_value(headers, names).and_then(|value| {
        value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
    })
}

fn parse_header_reset(value: &str, now: DateTime<Utc>) -> Option<String> {
    if let Some(timestamp) = parse_rfc3339(value) {
        return Some(timestamp.to_rfc3339());
    }
    if let Ok(number) = value.parse::<i64>() {
        if number >= 1_000_000_000_000 {
            if let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(number) {
                return Some(timestamp.to_rfc3339());
            }
        } else if number >= 1_000_000_000 {
            if let Some(timestamp) = DateTime::<Utc>::from_timestamp(number, 0) {
                return Some(timestamp.to_rfc3339());
            }
        } else {
            return Some((now + ChronoDuration::seconds(number)).to_rfc3339());
        }
    }
    parse_compact_duration(value).map(|duration| {
        (now + ChronoDuration::seconds(duration.as_secs().min(i64::MAX as u64) as i64)).to_rfc3339()
    })
}

fn parse_compact_duration(value: &str) -> Option<StdDuration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0)
            .then(|| StdDuration::from_secs_f64(seconds));
    }
    let bytes = value.as_bytes();
    let mut index = 0usize;
    let mut total = 0.0f64;
    let mut parsed = false;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let number_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if number_start == index {
            return None;
        }
        let number = value[number_start..index].parse::<f64>().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let unit_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let unit = &value[unit_start..index].to_ascii_lowercase();
        let multiplier = match unit.as_str() {
            "ms" => 0.001,
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "d" | "day" | "days" => 86_400.0,
            _ => return None,
        };
        total += number * multiplier;
        parsed = true;
    }
    parsed.then(|| StdDuration::from_secs_f64(total))
}

fn records_from_provider_view(
    view: &ProviderQuotaView,
    provider_id: i64,
    provider_key_id: Option<i64>,
) -> Vec<ProviderQuotaSnapshotRecord> {
    let observed_at = DateTime::parse_from_rfc3339(&view.fetched_at)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let mut records = Vec::new();
    for item in &view.items {
        let window_key = normalize_window_key(&item.key);
        let mut unit = normalize_unit(&item.unit);
        let mut used = item.used;
        let mut limit = item.limit;
        let mut remaining = item.remaining;
        if item.percent.is_some() && used.is_none() && limit.is_none() {
            let percent = item.percent.unwrap_or_default();
            unit = "percent".to_string();
            used = Some(percent);
            limit = Some(100.0);
            remaining = Some((100.0 - percent).max(0.0));
        } else if unit == "percent" && used.is_none() {
            used = item.percent;
            limit = some_limit_or_percent_max(limit, item.percent);
            remaining = used.zip(limit).map(|(used, limit)| (limit - used).max(0.0));
        }
        let confidence = if used.is_some() || remaining.is_some() {
            "exact"
        } else {
            "inferred"
        };
        records.push(ProviderQuotaSnapshotRecord {
            provider_id,
            provider_key_id,
            model: String::new(),
            window_key: window_key.clone(),
            used,
            limit,
            remaining,
            unit: unit.clone(),
            reset_at: item.reset_at.clone(),
            source: SOURCE_PROVIDER_API.to_string(),
            confidence: confidence.to_string(),
            observed_at: observed_at.to_rfc3339(),
            valid_until: (observed_at + ChronoDuration::seconds(ttl_seconds_for(&window_key)))
                .to_rfc3339(),
        });
    }

    // Plan pages expose model allowances, not authoritative consumption. The
    // limit is still useful as a model-scoped ceiling, while `unknown` keeps it
    // out of routing decisions until usage is actually observed.
    let monthly_reset = view
        .items
        .iter()
        .find(|item| item.key == "monthly")
        .and_then(|item| item.reset_at.clone());
    for model in &view.model_quotas {
        let Some(limit) = model.promo_credits.or(model.monthly_credits) else {
            continue;
        };
        let model = model
            .model_id
            .clone()
            .unwrap_or_else(|| model.model_name.clone());
        if model.trim().is_empty() {
            continue;
        }
        records.push(ProviderQuotaSnapshotRecord {
            provider_id,
            provider_key_id,
            model,
            window_key: "monthly".to_string(),
            used: None,
            limit: Some(limit),
            remaining: None,
            unit: "credit".to_string(),
            reset_at: monthly_reset.clone(),
            source: SOURCE_PROVIDER_API.to_string(),
            confidence: "inferred".to_string(),
            observed_at: observed_at.to_rfc3339(),
            valid_until: (observed_at + ChronoDuration::seconds(ttl_seconds_for("monthly")))
                .to_rfc3339(),
        });
    }
    records
}

async fn upsert_source_records(
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

async fn insert_record(
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

async fn update_cache(
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

fn effective_records(
    records: Vec<ProviderQuotaSnapshotRecord>,
) -> Vec<ProviderQuotaSnapshotRecord> {
    let mut chosen = HashMap::<ProviderQuotaSnapshotKey, ProviderQuotaSnapshotRecord>::new();
    for record in records {
        let mut key = record.key();
        key.source.clear();
        let replace = chosen
            .get(&key)
            .is_none_or(|current| source_rank(&record.source) > source_rank(&current.source));
        if replace {
            chosen.insert(key, record);
        }
    }
    let mut records = chosen.into_values().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.model
            .cmp(&right.model)
            .then_with(|| left.window_key.cmp(&right.window_key))
            .then_with(|| left.unit.cmp(&right.unit))
            .then_with(|| left.source.cmp(&right.source))
    });
    records
}

fn source_rank(source: &str) -> u8 {
    match source {
        "manual" => 4,
        "response_header" => 3,
        "runtime_429" => 2,
        "provider_api" => 1,
        _ => 0,
    }
}

fn quota_snapshot_view(
    record: ProviderQuotaSnapshotRecord,
    now: DateTime<Utc>,
) -> ProviderQuotaSnapshotView {
    let reset_due = record
        .reset_at
        .as_deref()
        .and_then(parse_rfc3339)
        .is_some_and(|reset_at| reset_at <= now);
    let stale = parse_rfc3339(&record.valid_until).is_none_or(|valid_until| valid_until < now);
    let percent = quota_percent(&record);
    let depleted = quota_is_depleted(&record, percent);
    let state = if reset_due {
        "reset_due"
    } else if stale {
        "unknown"
    } else if depleted {
        if record.reset_at.is_some() {
            "exhausted"
        } else {
            "low"
        }
    } else if percent.is_some_and(|value| value >= QUOTA_LOW_WATERMARK_PCT) {
        "low"
    } else if quota_is_known(&record, percent) {
        "healthy"
    } else {
        "unknown"
    };
    ProviderQuotaSnapshotView {
        provider_id: record.provider_id,
        provider_key_id: record.provider_key_id,
        model: record.model,
        window_key: record.window_key,
        used: record.used,
        limit: record.limit,
        remaining: record.remaining,
        unit: record.unit,
        percent,
        reset_at: record.reset_at,
        source: record.source,
        confidence: record.confidence,
        observed_at: record.observed_at,
        valid_until: record.valid_until,
        state: state.to_string(),
    }
}

fn quota_percent(record: &ProviderQuotaSnapshotRecord) -> Option<f64> {
    if record.unit == "percent" {
        return record.used.map(|value| value.clamp(0.0, 100.0));
    }
    if let (Some(used), Some(limit)) = (record.used, record.limit)
        && limit > 0.0
    {
        return Some((used / limit * 100.0).clamp(0.0, 100.0));
    }
    if let (Some(remaining), Some(limit)) = (record.remaining, record.limit)
        && limit > 0.0
    {
        return Some(((limit - remaining) / limit * 100.0).clamp(0.0, 100.0));
    }
    None
}

fn quota_is_depleted(record: &ProviderQuotaSnapshotRecord, percent: Option<f64>) -> bool {
    record.remaining.is_some_and(|value| value <= 0.0)
        || matches!((record.used, record.limit), (Some(used), Some(limit)) if limit > 0.0 && used >= limit)
        || percent.is_some_and(|value| value >= 100.0)
}

fn quota_is_known(record: &ProviderQuotaSnapshotRecord, percent: Option<f64>) -> bool {
    percent.is_some()
        || record.remaining.is_some()
        || (record.used.is_some() && record.limit.is_some())
}

fn normalize_window_key(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "5h" | "five_hour" | "five-hour" | "rolling" => "5h".to_string(),
        "daily" | "day" => "daily".to_string(),
        "weekly" | "week" => "weekly".to_string(),
        "monthly" | "month" => "monthly".to_string(),
        "balance" => "balance".to_string(),
        "custom" => "custom".to_string(),
        other if other.starts_with("balance_") => "balance".to_string(),
        other => other.to_string(),
    }
}

fn normalize_unit(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "$" | "usd" => "usd".to_string(),
        "%" | "percent" | "percentage" => "percent".to_string(),
        "token" | "tokens" => "token".to_string(),
        "request" | "requests" => "request".to_string(),
        "credit" | "credits" => "credit".to_string(),
        "cny" | "rmb" | "¥" => "cny".to_string(),
        other => other.to_string(),
    }
}

fn ttl_seconds_for(window_key: &str) -> i64 {
    match window_key {
        "balance" => 5 * 60,
        "5h" => 10 * 60,
        "daily" => DEFAULT_VALID_SECONDS,
        "weekly" => 30 * 60,
        "monthly" => 6 * 60 * 60,
        _ => DEFAULT_VALID_SECONDS,
    }
}

fn finite_non_negative(value: Option<f64>, field: &str) -> AppResult<Option<f64>> {
    match value {
        Some(value) if !value.is_finite() || value < 0.0 => Err(AppError::BadRequest(format!(
            "quota snapshot {field} must be a finite non-negative number"
        ))),
        value => Ok(value),
    }
}

fn normalize_reset_at(value: Option<&str>) -> AppResult<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = parse_rfc3339(value).ok_or_else(|| {
        AppError::BadRequest("quota snapshot reset_at must be an RFC3339 timestamp".to_string())
    })?;
    Ok(Some(parsed.to_rfc3339()))
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn some_limit_or_percent_max(limit: Option<f64>, percent: Option<f64>) -> Option<f64> {
    limit.or_else(|| percent.map(|_| 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ProviderPriceView, ProviderQuotaDetail, ProviderQuotaItem, ProviderQuotaModel,
    };

    fn record(
        source: &str,
        used: Option<f64>,
        limit: Option<f64>,
        remaining: Option<f64>,
        unit: &str,
        reset_at: Option<String>,
        valid_until: String,
    ) -> ProviderQuotaSnapshotRecord {
        ProviderQuotaSnapshotRecord {
            provider_id: 1,
            provider_key_id: Some(7),
            model: "model-a".to_string(),
            window_key: "5h".to_string(),
            used,
            limit,
            remaining,
            unit: unit.to_string(),
            reset_at,
            source: source.to_string(),
            confidence: "exact".to_string(),
            observed_at: Utc::now().to_rfc3339(),
            valid_until,
        }
    }

    #[test]
    fn quota_state_never_confuses_unknown_with_exhausted() {
        let now = Utc::now();
        let future = (now + ChronoDuration::hours(1)).to_rfc3339();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();

        let exhausted = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                None,
                None,
                Some(0.0),
                "request",
                Some(future.clone()),
                future.clone(),
            ),
            now,
        );
        assert_eq!(exhausted.state, "exhausted");

        let healthy = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                Some(10.0),
                Some(100.0),
                None,
                "usd",
                None,
                future.clone(),
            ),
            now,
        );
        assert_eq!(healthy.state, "healthy");
        assert_eq!(healthy.percent, Some(10.0));

        let low = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                Some(95.0),
                Some(100.0),
                None,
                "usd",
                None,
                future.clone(),
            ),
            now,
        );
        assert_eq!(low.state, "low");

        let unknown = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                None,
                None,
                None,
                "credit",
                None,
                future.clone(),
            ),
            now,
        );
        assert_eq!(unknown.state, "unknown");

        let stale = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                None,
                None,
                Some(0.0),
                "request",
                Some(future),
                past,
            ),
            now,
        );
        assert_eq!(stale.state, "unknown");

        let reset_due = quota_snapshot_view(
            record(
                SOURCE_PROVIDER_API,
                None,
                None,
                Some(0.0),
                "request",
                Some((now - ChronoDuration::minutes(1)).to_rfc3339()),
                (now + ChronoDuration::hours(1)).to_rfc3339(),
            ),
            now,
        );
        assert_eq!(reset_due.state, "reset_due");
    }

    #[test]
    fn manual_snapshot_wins_over_provider_api_for_the_same_scope() {
        let future = (Utc::now() + ChronoDuration::hours(1)).to_rfc3339();
        let provider = record(
            SOURCE_PROVIDER_API,
            Some(95.0),
            Some(100.0),
            None,
            "usd",
            None,
            future.clone(),
        );
        let manual = record(
            SOURCE_MANUAL,
            Some(20.0),
            Some(100.0),
            None,
            "usd",
            None,
            future,
        );
        let effective = effective_records(vec![provider, manual]);
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].source, SOURCE_MANUAL);
        assert_eq!(effective[0].used, Some(20.0));
    }

    #[test]
    fn provider_quota_view_normalizes_percent_and_model_snapshots() {
        let fetched_at = Utc::now().to_rfc3339();
        let view = ProviderQuotaView {
            kind: "opencode_go".to_string(),
            title: "OpenCode Go".to_string(),
            plan_name: Some("Go".to_string()),
            key_id: Some(7),
            key_name: Some("Primary".to_string()),
            key_suffix: Some("1234".to_string()),
            source_url: None,
            items: vec![ProviderQuotaItem {
                key: "rolling".to_string(),
                label: "5 小时".to_string(),
                used: None,
                limit: None,
                remaining: None,
                unit: "%".to_string(),
                percent: Some(92.0),
                reset_at: None,
            }],
            model_quotas: vec![ProviderQuotaModel {
                model_name: "Model A".to_string(),
                model_id: Some("model-a".to_string()),
                monthly_credits: Some(80.0),
                promo_credits: Some(100.0),
                note: None,
            }],
            details: Vec::<ProviderQuotaDetail>::new(),
            prices: Vec::<ProviderPriceView>::new(),
            snapshots: Vec::new(),
            fetched_at,
        };
        let records = records_from_provider_view(&view, 1, Some(7));
        assert_eq!(records.len(), 2);
        let window = records
            .iter()
            .find(|record| record.model.is_empty())
            .unwrap();
        assert_eq!(window.window_key, "5h");
        assert_eq!(window.unit, "percent");
        assert_eq!(window.used, Some(92.0));
        assert_eq!(window.limit, Some(100.0));
        assert_eq!(window.remaining, Some(8.0));
        let model = records
            .iter()
            .find(|record| record.model == "model-a")
            .unwrap();
        assert_eq!(model.window_key, "monthly");
        assert_eq!(model.unit, "credit");
        assert_eq!(model.limit, Some(100.0));
        assert_eq!(model.confidence, "inferred");
    }

    #[test]
    fn standard_rate_limit_headers_are_parsed_without_guessing_unknown_values() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "120".parse().unwrap());
        headers.insert("x-ratelimit-remaining-requests", "15".parse().unwrap());
        headers.insert("x-ratelimit-reset-requests", "1m30s".parse().unwrap());
        headers.insert("x-ratelimit-limit-tokens", "not-a-number".parse().unwrap());
        let records = records_from_rate_limit_headers(1, Some(7), "model-a", &headers);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].window_key, "requests");
        assert_eq!(records[0].unit, "request");
        assert_eq!(records[0].used, Some(105.0));
        assert_eq!(records[0].limit, Some(120.0));
        assert_eq!(records[0].remaining, Some(15.0));
        assert_eq!(records[0].source, SOURCE_RESPONSE_HEADER);
        assert!(records[0].reset_at.is_some());
        assert_eq!(
            parse_compact_duration("1m30s"),
            Some(StdDuration::from_secs(90))
        );
        assert_eq!(
            parse_compact_duration("500ms"),
            Some(StdDuration::from_millis(500))
        );
    }

    #[test]
    fn effective_scope_prefers_the_latest_exhausted_reset() {
        let now = Utc::now();
        let snapshot = |window_key: &str, state: &str, percent: Option<f64>, reset_at| {
            ProviderQuotaSnapshotView {
                provider_id: 1,
                provider_key_id: Some(7),
                model: "model-a".to_string(),
                window_key: window_key.to_string(),
                used: None,
                limit: None,
                remaining: None,
                unit: "request".to_string(),
                percent,
                reset_at,
                source: SOURCE_PROVIDER_API.to_string(),
                confidence: "exact".to_string(),
                observed_at: now.to_rfc3339(),
                valid_until: (now + ChronoDuration::minutes(15)).to_rfc3339(),
                state: state.to_string(),
            }
        };
        let state = effective_scope_state(&[
            snapshot(
                "5h",
                "exhausted",
                Some(100.0),
                Some((now + ChronoDuration::hours(1)).to_rfc3339()),
            ),
            snapshot(
                "weekly",
                "exhausted",
                Some(100.0),
                Some((now + ChronoDuration::days(2)).to_rfc3339()),
            ),
            snapshot("monthly", "healthy", Some(20.0), None),
        ]);
        assert_eq!(state.state, "exhausted");
        assert_eq!(
            state.reset_at,
            Some((now + ChronoDuration::days(2)).to_rfc3339())
        );

        let low_reset = (now + ChronoDuration::hours(2)).to_rfc3339();
        let state = effective_scope_state(&[
            snapshot("5h", "low", Some(94.0), Some(low_reset.clone())),
            snapshot("weekly", "healthy", Some(10.0), None),
        ]);
        assert_eq!(state.state, "low");
        assert_eq!(state.pressure_pct, Some(94.0));
        assert_eq!(state.reset_at, Some(low_reset));
    }
}
