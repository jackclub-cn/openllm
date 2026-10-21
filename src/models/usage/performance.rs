use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestTimings {
    pub prepare_ms: Option<i64>,
    pub route_wait_ms: Option<i64>,
    pub upstream_headers_ms: Option<i64>,
    pub response_wait_ms: Option<i64>,
    pub response_read_ms: Option<i64>,
    pub time_to_first_token_ms: Option<i64>,
    pub generation_ms: Option<i64>,
    pub total_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestPerformance {
    pub timings: RequestTimings,
    pub attempt_count: i64,
    pub fallback_count: i64,
    /// Sum of recorded failed attempts; excludes backoff and successful work.
    pub failed_attempts_ms: i64,
    pub attempts_truncated: bool,
}
