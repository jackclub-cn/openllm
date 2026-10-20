use super::*;

pub(super) fn effective_records(
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

pub(super) fn source_rank(source: &str) -> u8 {
    match source {
        "manual" => 4,
        "response_header" => 3,
        "runtime_429" => 2,
        "provider_api" => 1,
        _ => 0,
    }
}

pub(super) fn quota_snapshot_view(
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

pub(super) fn quota_percent(record: &ProviderQuotaSnapshotRecord) -> Option<f64> {
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

pub(super) fn quota_is_depleted(
    record: &ProviderQuotaSnapshotRecord,
    percent: Option<f64>,
) -> bool {
    record.remaining.is_some_and(|value| value <= 0.0)
        || matches!((record.used, record.limit), (Some(used), Some(limit)) if limit > 0.0 && used >= limit)
        || percent.is_some_and(|value| value >= 100.0)
}

pub(super) fn quota_is_known(record: &ProviderQuotaSnapshotRecord, percent: Option<f64>) -> bool {
    percent.is_some()
        || record.remaining.is_some()
        || (record.used.is_some() && record.limit.is_some())
}

pub(super) fn normalize_window_key(value: &str) -> String {
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

pub(super) fn normalize_unit(value: &str) -> String {
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

pub(super) fn ttl_seconds_for(window_key: &str) -> i64 {
    match window_key {
        "balance" => 5 * 60,
        "5h" => 10 * 60,
        "daily" => DEFAULT_VALID_SECONDS,
        "weekly" => 30 * 60,
        "monthly" => 6 * 60 * 60,
        _ => DEFAULT_VALID_SECONDS,
    }
}

pub(super) fn finite_non_negative(value: Option<f64>, field: &str) -> AppResult<Option<f64>> {
    match value {
        Some(value) if !value.is_finite() || value < 0.0 => Err(AppError::BadRequest(format!(
            "quota snapshot {field} must be a finite non-negative number"
        ))),
        value => Ok(value),
    }
}

pub(super) fn normalize_reset_at(value: Option<&str>) -> AppResult<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = parse_rfc3339(value).ok_or_else(|| {
        AppError::BadRequest("quota snapshot reset_at must be an RFC3339 timestamp".to_string())
    })?;
    Ok(Some(parsed.to_rfc3339()))
}

pub(super) fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

pub(super) fn some_limit_or_percent_max(limit: Option<f64>, percent: Option<f64>) -> Option<f64> {
    limit.or_else(|| percent.map(|_| 100.0))
}
