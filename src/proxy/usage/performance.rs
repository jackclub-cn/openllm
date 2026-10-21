use super::*;

pub(super) fn activity_performance_json(state: &AppState, request_id: &str) -> Option<String> {
    let snapshot = state.activity.snapshot(request_id)?;
    if snapshot.outcome == "running" {
        return None;
    }
    let summary = crate::models::RequestPerformance {
        timings: snapshot.timings,
        attempt_count: snapshot.attempt_count,
        fallback_count: snapshot.fallback_count,
        failed_attempts_ms: snapshot
            .attempts
            .iter()
            .filter(|attempt| !attempt.success && attempt.reason_code.is_some())
            .fold(0_i64, |sum, attempt| {
                sum.saturating_add(attempt.latency_ms.max(0))
            }),
        attempts_truncated: snapshot.attempts_truncated,
    };
    serde_json::to_string(&summary).ok()
}
