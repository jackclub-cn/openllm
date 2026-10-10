use super::*;

#[derive(Debug, FromRow)]
pub struct UsageLog {
    pub id: i64,
    pub request_id: String,
    pub session_id: Option<String>,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    /// Time to first content token, in milliseconds. Non-streamed responses
    /// use the total response time because no incremental token timestamp is
    /// available, while failed requests leave this empty.
    pub first_token_ms: Option<i64>,
    pub status_code: i64,
    pub in_flight: i64,
    pub success: i64,
    pub streamed: i64,
    pub error_message: Option<String>,
    pub request_preview: Option<String>,
    pub response_preview: Option<String>,
    #[sqlx(default)]
    pub warning_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct UsageLogView {
    pub id: i64,
    pub request_id: String,
    /// Stable client conversation identifier when the caller supplied one.
    pub session_id: Option<String>,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub provider_id: Option<i64>,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// Estimated request cost in micro-US dollars; `None` means unpriced.
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    pub first_token_ms: Option<i64>,
    /// Generation throughput in tokens per second.
    ///
    /// For streamed requests this measures the generation phase only, i.e. it
    /// excludes the wait for the first token, which is what "tokens per second"
    /// means in practice. Non-streamed requests use the whole request time.
    /// `None` when it cannot be derived (no output, or no elapsed time).
    pub output_tps: Option<f64>,
    pub status_code: i64,
    pub in_flight: bool,
    pub success: bool,
    pub streamed: bool,
    pub error_message: Option<String>,
    pub request_preview: Option<String>,
    pub response_preview: Option<String>,
    pub warning_message: Option<String>,
    pub created_at: String,
}

impl From<UsageLog> for UsageLogView {
    fn from(value: UsageLog) -> Self {
        let streamed = value.streamed != 0;
        let output_tps = output_tps_of(
            value.completion_tokens,
            value.latency_ms,
            value.first_token_ms,
            streamed,
        );
        Self {
            id: value.id,
            request_id: value.request_id,
            session_id: value.session_id,
            api_key_id: value.api_key_id,
            api_key_name: None,
            route_id: value.route_id,
            route_name: None,
            provider_id: value.provider_id,
            provider_name: None,
            provider_api_key_id: value.provider_api_key_id,
            provider_api_key_name: value.provider_api_key_name,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            estimated_cost_micros: value.estimated_cost_micros,
            latency_ms: value.latency_ms,
            first_token_ms: value.first_token_ms,
            output_tps,
            status_code: value.status_code,
            in_flight: value.in_flight != 0,
            success: value.success != 0,
            streamed,
            error_message: value.error_message,
            request_preview: value.request_preview,
            response_preview: value.response_preview,
            warning_message: value.warning_message,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, FromRow)]
pub struct UsageLogDetailRow {
    pub id: i64,
    pub request_id: String,
    pub session_id: Option<String>,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub provider_id: Option<i64>,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    pub first_token_ms: Option<i64>,
    pub status_code: i64,
    pub in_flight: i64,
    pub success: i64,
    pub streamed: i64,
    pub error_message: Option<String>,
    pub request_preview: Option<String>,
    pub response_preview: Option<String>,
    pub warning_message: Option<String>,
    pub created_at: String,
}

impl From<UsageLogDetailRow> for UsageLogView {
    fn from(value: UsageLogDetailRow) -> Self {
        let streamed = value.streamed != 0;
        let output_tps = output_tps_of(
            value.completion_tokens,
            value.latency_ms,
            value.first_token_ms,
            streamed,
        );
        Self {
            id: value.id,
            request_id: value.request_id,
            session_id: value.session_id,
            api_key_id: value.api_key_id,
            api_key_name: value.api_key_name,
            route_id: value.route_id,
            route_name: value.route_name,
            provider_id: value.provider_id,
            provider_name: value.provider_name,
            provider_api_key_id: value.provider_api_key_id,
            provider_api_key_name: value.provider_api_key_name,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            estimated_cost_micros: value.estimated_cost_micros,
            latency_ms: value.latency_ms,
            first_token_ms: value.first_token_ms,
            output_tps,
            status_code: value.status_code,
            in_flight: value.in_flight != 0,
            success: value.success != 0,
            streamed,
            error_message: value.error_message,
            request_preview: value.request_preview,
            response_preview: value.response_preview,
            warning_message: value.warning_message,
            created_at: value.created_at,
        }
    }
}

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
    /// Requests in the range whose outbound body the gateway adjusted for
    /// upstream compatibility (tool-history repair or CommandCode limits).
    pub range_gateway_adjusted: i64,
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
    pub daily_usage: Vec<DailyUsage>,
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
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Prompt tokens served from the provider's cache, billed at a discount.
    pub cache_read_tokens: i64,
    /// Prompt tokens written into the cache for later reuse.
    pub cache_write_tokens: i64,
}

impl Usage {
    /// Convenience constructor for the common case with no cache traffic.
    pub fn new(prompt_tokens: i64, completion_tokens: i64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    pub fn normalized(mut self) -> Self {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
        // Defensive: a provider reporting a negative or absurd cache count must
        // not corrupt aggregates.
        self.cache_read_tokens = self.cache_read_tokens.max(0);
        self.cache_write_tokens = self.cache_write_tokens.max(0);
        self
    }

    pub fn has_tokens(&self) -> bool {
        self.prompt_tokens > 0 || self.completion_tokens > 0
    }
}

/// Merges manual price overrides into a models.dev cost object.
///
/// Overrides are applied to both the base object and every tier, so a manual
/// correction wins regardless of which context tier the request selects.
pub fn effective_cost_value(
    cost: Option<&serde_json::Value>,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Option<serde_json::Value> {
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return cost.cloned().filter(|value| value.is_object());
    }
    let mut object = cost
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in [
        ("input", input),
        ("output", output),
        ("cache_read", cache_read),
        ("cache_write", cache_write),
    ] {
        let Some(value) = value else {
            continue;
        };
        let value = serde_json::json!(value);
        object.insert(key.to_string(), value.clone());
        if let Some(tiers) = object
            .get_mut("tiers")
            .and_then(serde_json::Value::as_array_mut)
        {
            for tier in tiers {
                if let Some(tier) = tier.as_object_mut() {
                    tier.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    Some(serde_json::Value::Object(object))
}

/// Reads a base, non-tiered price for display in the management console.
pub fn cost_base_price(cost: &serde_json::Value, key: &str) -> Option<f64> {
    cost_price(cost, cost, key)
}

/// Estimates a request's cost in micro-US dollars from models.dev pricing.
///
/// Prices are expressed per million tokens, so multiplying a token count by
/// the price directly yields micro-dollars. Cache traffic is separated from
/// fresh input because providers bill it at different rates. Tiered context
/// pricing uses the largest published tier that the prompt has crossed.
pub fn estimate_cost_micros(cost: Option<&serde_json::Value>, usage: Usage) -> Option<i64> {
    let cost = cost?;
    let usage = usage.normalized();
    let selected = select_cost_tier(cost, usage.prompt_tokens);
    let input = cost_price(selected, cost, "input");
    let output = cost_price(selected, cost, "output");
    let cache_read = cost_price(selected, cost, "cache_read").or(input);
    let cache_write = cost_price(selected, cost, "cache_write").or(input);
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return None;
    }

    let prompt = usage.prompt_tokens.max(0);
    let read = usage.cache_read_tokens.max(0).min(prompt);
    let write = usage.cache_write_tokens.max(0).min(prompt - read);
    let fresh = prompt - read - write;
    let completion = usage.completion_tokens.max(0);

    let amount = (fresh as f64) * input.unwrap_or(0.0)
        + (read as f64) * cache_read.unwrap_or(0.0)
        + (write as f64) * cache_write.unwrap_or(0.0)
        + (completion as f64) * output.unwrap_or(0.0);
    Some(amount.round().max(0.0).min(i64::MAX as f64) as i64)
}

pub(crate) fn select_cost_tier(cost: &serde_json::Value, prompt_tokens: i64) -> &serde_json::Value {
    let Some(tiers) = cost.get("tiers").and_then(serde_json::Value::as_array) else {
        return cost;
    };
    let mut selected = cost;
    let mut selected_size = -1;
    for tier in tiers {
        let Some(size) = tier
            .pointer("/tier/size")
            .and_then(serde_json::Value::as_i64)
        else {
            continue;
        };
        if tier
            .pointer("/tier/type")
            .and_then(serde_json::Value::as_str)
            != Some("context")
            || size > prompt_tokens
            || size < selected_size
        {
            continue;
        }
        selected = tier;
        selected_size = size;
    }
    selected
}

pub(crate) fn cost_price(
    selected: &serde_json::Value,
    base: &serde_json::Value,
    key: &str,
) -> Option<f64> {
    selected
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .or_else(|| base.get(key).and_then(serde_json::Value::as_f64))
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Derives output throughput from a completed request.
///
/// Streamed requests measure the generation phase alone (total latency minus
/// the first-token wait), since including queue time would understate a fast
/// model behind a slow first token. Requests without a first-token timestamp
/// fall back to the full latency. Returns `None` rather than infinity or zero
/// when there is nothing meaningful to report, so the UI can show a dash.
pub(crate) fn output_tps_of(
    completion_tokens: i64,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    streamed: bool,
) -> Option<f64> {
    if completion_tokens <= 0 {
        return None;
    }
    let elapsed_ms = if streamed {
        match first_token_ms {
            // Guard against a first-token stamp that exceeds the total
            // (possible with clock granularity): fall back to full latency.
            Some(first) if latency_ms > first => latency_ms - first,
            _ => latency_ms,
        }
    } else {
        latency_ms
    };
    if elapsed_ms <= 0 {
        return None;
    }
    let tps = completion_tokens as f64 / (elapsed_ms as f64 / 1000.0);
    tps.is_finite().then_some(tps)
}
