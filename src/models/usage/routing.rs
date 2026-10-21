use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTraceCandidate {
    pub order: i64,
    pub provider_id: i64,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: String,
    pub eligible: bool,
    pub deprioritized: bool,
    pub reason_code: String,
}

#[derive(Debug, FromRow)]
pub struct RouteTraceRecord {
    pub request_id: String,
    pub created_at: String,
    pub finished_at: String,
    pub route_id: Option<i64>,
    pub strategy: String,
    pub session_id: Option<String>,
    pub session_decision: String,
    pub candidates_json: String,
    pub attempt_count: i64,
    pub fallback_count: i64,
    pub final_provider_id: Option<i64>,
    pub final_provider_api_key_id: Option<i64>,
    pub final_upstream_model: Option<String>,
    pub final_status_code: Option<i64>,
    pub success: i64,
}

#[derive(Debug, FromRow)]
pub struct RouteTraceAttemptRow {
    pub attempt_index: i64,
    pub round: i64,
    pub candidate_order: i64,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: Option<String>,
    pub started_at: String,
    pub latency_ms: i64,
    pub status_code: Option<i64>,
    pub success: i64,
    pub retryable: i64,
    pub reason_code: String,
    pub failure_scope: Option<String>,
    pub failure_retry: Option<String>,
    pub failure_confidence: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteTraceAttemptView {
    pub attempt_index: i64,
    pub round: i64,
    pub candidate_order: i64,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: Option<String>,
    pub started_at: String,
    pub latency_ms: i64,
    pub status_code: Option<i64>,
    pub success: bool,
    pub retryable: bool,
    pub reason_code: String,
    pub failure_scope: Option<String>,
    pub failure_retry: Option<String>,
    pub failure_confidence: Option<String>,
}

impl From<RouteTraceAttemptRow> for RouteTraceAttemptView {
    fn from(value: RouteTraceAttemptRow) -> Self {
        Self {
            attempt_index: value.attempt_index,
            round: value.round,
            candidate_order: value.candidate_order,
            provider_id: value.provider_id,
            provider_api_key_id: value.provider_api_key_id,
            upstream_model: value.upstream_model,
            started_at: value.started_at,
            latency_ms: value.latency_ms,
            status_code: value.status_code,
            success: value.success != 0,
            retryable: value.retryable != 0,
            reason_code: value.reason_code,
            failure_scope: value.failure_scope,
            failure_retry: value.failure_retry,
            failure_confidence: value.failure_confidence,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RouteTraceView {
    pub request_id: String,
    pub created_at: String,
    pub finished_at: String,
    pub route_id: Option<i64>,
    pub strategy: String,
    pub session_id: Option<String>,
    pub session_decision: String,
    pub candidates: Vec<RouteTraceCandidate>,
    pub attempt_count: i64,
    pub fallback_count: i64,
    pub final_provider_id: Option<i64>,
    pub final_provider_api_key_id: Option<i64>,
    pub final_upstream_model: Option<String>,
    pub final_status_code: Option<i64>,
    pub success: bool,
    pub attempts: Vec<RouteTraceAttemptView>,
}
