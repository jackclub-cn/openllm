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
    pub context_breakdown_json: Option<String>,
    #[sqlx(default)]
    pub cache_usage_reported: Option<i64>,
    #[sqlx(default)]
    pub performance_json: Option<String>,
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
    pub context: Option<ContextBreakdown>,
    pub cache_usage_reported: Option<bool>,
    pub performance: Option<RequestPerformance>,
    pub warning_message: Option<String>,
    pub created_at: String,
}

impl From<UsageLog> for UsageLogView {
    fn from(value: UsageLog) -> Self {
        let streamed = value.streamed != 0;
        let context = value
            .context_breakdown_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
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
            context,
            cache_usage_reported: value.cache_usage_reported.map(|value| value != 0),
            performance: value
                .performance_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
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
    #[sqlx(default)]
    pub context_breakdown_json: Option<String>,
    #[sqlx(default)]
    pub cache_usage_reported: Option<i64>,
    #[sqlx(default)]
    pub performance_json: Option<String>,
    pub warning_message: Option<String>,
    pub created_at: String,
}

impl From<UsageLogDetailRow> for UsageLogView {
    fn from(value: UsageLogDetailRow) -> Self {
        let streamed = value.streamed != 0;
        let context = value
            .context_breakdown_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
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
            context,
            cache_usage_reported: value.cache_usage_reported.map(|value| value != 0),
            performance: value
                .performance_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            warning_message: value.warning_message,
            created_at: value.created_at,
        }
    }
}
