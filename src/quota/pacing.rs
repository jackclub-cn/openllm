use super::*;

pub(crate) fn effective_snapshot_views(
    records: Vec<ProviderQuotaSnapshotRecord>,
) -> Vec<ProviderQuotaSnapshotView> {
    let now = Utc::now();
    effective_records(records)
        .into_iter()
        .map(|record| quota_snapshot_view(record, now))
        .collect()
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
