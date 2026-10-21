use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct ActivityTarget {
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: String,
    pub context_window_tokens: Option<i64>,
    pub input_limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityCandidate {
    pub order: i64,
    pub provider_id: i64,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: String,
    pub eligible: bool,
    pub deprioritized: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityAttempt {
    pub attempt_index: i64,
    pub round: i64,
    pub candidate_order: i64,
    pub provider_id: Option<i64>,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub upstream_model: Option<String>,
    pub started_at: String,
    pub latency_ms: i64,
    pub upstream_headers_ms: Option<i64>,
    pub status_code: Option<i64>,
    pub success: bool,
    pub reason_code: Option<String>,
    pub failure_scope: Option<String>,
}

pub type ActivityTimings = crate::models::RequestTimings;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ActivityCache {
    pub state: String,
    pub read_tokens: i64,
    pub write_tokens: i64,
    pub hit_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestActivitySnapshot {
    pub request_id: String,
    pub revision: u64,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub phase: String,
    pub outcome: String,
    pub endpoint: String,
    pub streamed: bool,
    pub session_id: Option<String>,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub strategy: Option<String>,
    pub requested_model: Option<String>,
    pub target: Option<ActivityTarget>,
    pub request_tokens_estimated: i64,
    pub context: Option<ContextBreakdown>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub status_code: Option<i64>,
    pub error_code: Option<String>,
    pub waiting_reason: Option<String>,
    pub fallback_count: i64,
    pub attempt_count: i64,
    pub candidate_count: usize,
    pub candidates_truncated: bool,
    pub attempts_truncated: bool,
    pub timings: ActivityTimings,
    pub cache: ActivityCache,
    pub candidates: Vec<ActivityCandidate>,
    pub attempts: Vec<ActivityAttempt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEvent {
    pub kind: &'static str,
    pub sequence: u64,
    pub instance_id: String,
    pub active_count: usize,
    pub snapshot: RequestActivitySnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityList {
    pub enabled: bool,
    pub generated_at: String,
    pub active_count: usize,
    pub sequence: u64,
    pub instance_id: String,
    pub items: Vec<RequestActivitySnapshot>,
}

#[derive(Debug, Clone)]
pub struct ActivityDescription {
    pub requested_model: String,
    pub streamed: bool,
    pub session_id: Option<String>,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub request_tokens_estimated: i64,
    pub context: Option<ContextBreakdown>,
}

#[derive(Debug, Clone)]
pub struct ActivityAttemptStart {
    pub attempt_index: i64,
    pub round: i64,
    pub candidate_order: i64,
    pub target: ActivityTarget,
}

#[derive(Debug, Clone)]
pub struct ActivityAttemptFinish {
    pub attempt_index: i64,
    pub latency_ms: i64,
    pub status_code: Option<i64>,
    pub success: bool,
    pub reason_code: String,
    pub failure_scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActivityFinish {
    pub status_code: i64,
    pub success: bool,
    pub error_code: Option<String>,
    pub latency_ms: i64,
    pub first_token_ms: Option<i64>,
    pub usage: Usage,
}
