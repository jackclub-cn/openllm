use super::*;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub endpoint: Option<String>,
    pub success: Option<bool>,
    pub in_flight: Option<bool>,
    /// When true, only requests whose outbound body the gateway adjusted.
    pub gateway_adjusted: Option<bool>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Dimension filters shared by the request log and the analytics overview.
///
/// Keeping a single shape means a filter selected on the request log page can
/// be forwarded to `/api/overview` without translating names or semantics.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageFilters {
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub endpoint: Option<String>,
    pub success: Option<bool>,
    pub in_flight: Option<bool>,
    /// When true, only requests whose outbound body the gateway adjusted.
    pub gateway_adjusted: Option<bool>,
}

impl UsageQuery {
    /// Project the request-log query onto the shared filter set.
    pub fn filters(&self) -> UsageFilters {
        UsageFilters {
            provider_id: self.provider_id,
            provider_api_key_id: self.provider_api_key_id,
            api_key_id: self.api_key_id,
            route_id: self.route_id,
            model: self.model.clone(),
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
            endpoint: self.endpoint.clone(),
            success: self.success,
            in_flight: self.in_flight,
            gateway_adjusted: self.gateway_adjusted,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UsagePage {
    pub items: Vec<UsageLogView>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
pub struct UsageCleanup {
    /// Delete usage rows older than this many days. Must be at least 1 so a
    /// stray request cannot wipe the whole history.
    pub older_than_days: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageCleanupResult {
    pub deleted: u64,
    pub older_than_days: i64,
    pub cutoff: String,
}

#[derive(Debug, Deserialize)]
pub struct OverviewQuery {
    /// Minutes to add to UTC to reach the caller's local time (e.g. +480 for
    /// UTC+8). Lets "today" respect the operator's timezone instead of UTC.
    #[serde(default)]
    pub tz_offset_minutes: i64,
    /// Inclusive start of the dashboard range as an RFC3339 timestamp.
    pub from: Option<String>,
    /// Exclusive end of the dashboard range as an RFC3339 timestamp.
    pub to: Option<String>,
    /// Keep the legacy session metrics available to API callers, while the
    /// dashboard can skip their grouping scan because it no longer displays
    /// them.
    #[serde(default = "default_true")]
    pub include_session_metrics: bool,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub endpoint: Option<String>,
    pub success: Option<bool>,
    pub in_flight: Option<bool>,
    pub gateway_adjusted: Option<bool>,
    /// Return every model instead of folding the long tail behind LIMIT 8.
    #[serde(default)]
    pub full_breakdown: bool,
    /// Requested chart bucket size. `auto` keeps the existing single-day/hour
    /// and longer-range/day behavior.
    pub granularity: Option<AnalyticsGranularity>,
}

impl OverviewQuery {
    /// Project the overview query onto the shared filter set.
    pub fn filters(&self) -> UsageFilters {
        UsageFilters {
            provider_id: self.provider_id,
            provider_api_key_id: self.provider_api_key_id,
            api_key_id: self.api_key_id,
            route_id: self.route_id,
            model: self.model.clone(),
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
            endpoint: self.endpoint.clone(),
            success: self.success,
            in_flight: self.in_flight,
            gateway_adjusted: self.gateway_adjusted,
        }
    }
}

impl Default for OverviewQuery {
    fn default() -> Self {
        Self {
            tz_offset_minutes: 0,
            from: None,
            to: None,
            include_session_metrics: true,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: None,
            full_breakdown: false,
            granularity: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnalyticsGranularity {
    Auto,
    Hour,
    Day,
    Week,
    Month,
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub requests_today: i64,
    pub tokens_today: i64,
    pub prompt_tokens_today: i64,
    pub completion_tokens_today: i64,
    /// Prompt tokens served from cache today, and the resulting hit ratio.
    pub cache_read_today: i64,
    pub cache_write_today: i64,
    pub cache_hit_rate: f64,
    pub requests_total: i64,
    pub tokens_total: i64,
    pub prompt_tokens_total: i64,
    pub completion_tokens_total: i64,
    pub cache_read_total: i64,
    pub cache_write_total: i64,
    pub cost_today_micros: i64,
    pub cost_total_micros: i64,
    pub unpriced_today: i64,
    pub unpriced_total: i64,
    pub range_requests: i64,
    pub range_tokens: i64,
    pub range_prompt_tokens: i64,
    pub range_completion_tokens: i64,
    pub range_cache_read: i64,
    pub range_cache_write: i64,
    pub range_cache_hit_rate: f64,
    pub range_sessions: i64,
    pub range_session_coverage: f64,
    pub range_avg_requests_per_session: f64,
    pub range_session_cache_hit_rate: f64,
    pub range_cost_micros: i64,
    pub range_unpriced: i64,
    pub range_success_rate: f64,
    pub range_avg_latency_ms: f64,
    /// Nearest-rank percentiles for completed requests in the range.
    pub range_latency_p50_ms: Option<i64>,
    pub range_latency_p95_ms: Option<i64>,
    pub range_latency_p99_ms: Option<i64>,
    /// First-content-token latency for requests that reported it.
    pub range_first_token_p50_ms: Option<i64>,
    pub range_first_token_p95_ms: Option<i64>,
    pub range_first_token_p99_ms: Option<i64>,
    /// Aggregate throughput: output tokens divided by generation time.
    pub range_output_tps: Option<f64>,
    /// Requests in the range whose outbound body the gateway adjusted for
    /// upstream compatibility (tool-history repair or CommandCode limits).
    pub range_gateway_adjusted: i64,
    /// Same totals for the equal-length window immediately before the current
    /// range. The UI uses this for period-over-period comparisons.
    pub previous_range: OverviewComparison,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub active_providers: i64,
    pub active_routes: i64,
    pub healthy_providers: i64,
    pub failed_providers: i64,
    pub untested_providers: i64,
    pub provider_keys_total: i64,
    pub healthy_provider_keys: i64,
    pub failed_provider_keys: i64,
    pub untested_provider_keys: i64,
    pub runtime_error_provider_keys: i64,
    pub cooling_providers: i64,
    pub cooling_provider_keys: i64,
    pub in_flight_requests: i64,
    pub recent_requests: Vec<UsageLogView>,
    pub provider_usage: Vec<ProviderUsage>,
    pub model_usage: Vec<ModelUsage>,
    /// Bucket size behind `daily_usage` and the request breakdowns: `hour` for
    /// one-day ranges, `day` for anything longer.
    pub granularity: &'static str,
    pub daily_usage: Vec<DailyUsage>,
    /// Per-bucket request counts for the busiest models and providers, with
    /// the remaining traffic folded into an "其他" bucket, so the dashboard
    /// can chart traffic per model and provider over time.
    pub daily_model_requests: Vec<DailyBreakdownUsage>,
    pub daily_provider_requests: Vec<DailyBreakdownUsage>,
    /// Error and adjustment breakdowns for the selected range and filters.
    pub status_breakdown: Vec<UsageStatusCount>,
    pub error_reasons: Vec<ErrorReason>,
    pub gateway_adjustments: Vec<GatewayAdjustment>,
}

#[derive(Debug, Serialize)]
pub struct OverviewComparison {
    pub from: String,
    pub to: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
    pub cost_micros: i64,
    pub unpriced: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub output_tps: Option<f64>,
    pub gateway_adjusted: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ProviderUsage {
    pub provider_id: i64,
    pub provider_name: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: i64,
    pub success_rate: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyBreakdownUsage {
    pub day: String,
    pub label: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub cost_micros: i64,
    pub success_rate: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct UsageStatusCount {
    pub status_code: i64,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ErrorReason {
    pub label: String,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct GatewayAdjustment {
    pub category: String,
    pub count: i64,
}

/// One row of a drill-down table: an access key, upstream key, route, or
/// endpoint. `id` is absent for endpoints, which are identified by `label`.
#[derive(Debug, FromRow, Serialize)]
pub struct DimensionUsage {
    pub id: Option<i64>,
    pub label: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct SessionUsage {
    pub session_id: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_hit_rate: f64,
    pub last_seen: String,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsDimensions {
    pub api_key_usage: Vec<DimensionUsage>,
    pub provider_api_key_usage: Vec<DimensionUsage>,
    pub route_usage: Vec<DimensionUsage>,
    pub endpoint_usage: Vec<DimensionUsage>,
    pub session_usage: Vec<SessionUsage>,
}

#[derive(Debug, FromRow)]
pub struct RouteQualityTotals {
    pub traces: i64,
    pub fallback_rate: f64,
    pub avg_attempts: f64,
    pub session_requests: i64,
    pub session_hits: i64,
    pub success_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct RouteQualityStrategy {
    pub strategy: String,
    pub traces: i64,
    pub fallback_rate: f64,
    pub avg_attempts: f64,
    pub success_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct RouteQualityRoute {
    pub route_id: Option<i64>,
    pub route_name: String,
    pub traces: i64,
    pub fallback_rate: f64,
    pub avg_attempts: f64,
    pub success_rate: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct RouteQualityDecision {
    pub decision: String,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct RouteQualityFailure {
    pub reason_code: String,
    pub failure_scope: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct RouteQuality {
    pub traces: i64,
    pub fallback_rate: f64,
    pub avg_attempts: f64,
    pub session_requests: i64,
    pub session_hits: i64,
    pub session_hit_rate: f64,
    pub success_rate: f64,
    pub strategies: Vec<RouteQualityStrategy>,
    pub routes: Vec<RouteQualityRoute>,
    pub session_decisions: Vec<RouteQualityDecision>,
    pub failure_reasons: Vec<RouteQualityFailure>,
}
