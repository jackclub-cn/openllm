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

mod storage;
pub(crate) use storage::*;
mod pacing;
pub(crate) use pacing::*;
mod headers;
use headers::*;
mod provider;
use provider::records_from_provider_view;
mod normalization;
use normalization::*;
