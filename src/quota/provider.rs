use super::*;

pub(super) fn records_from_provider_view(
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
