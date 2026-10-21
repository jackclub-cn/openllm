use super::*;
use crate::state::{ActivityCandidate, ActivityFinish};

mod observation;
pub(crate) use observation::{observe_activity_stream, observe_activity_value};
mod lifecycle;
pub(crate) use lifecycle::track_activity_request;

pub(crate) async fn send_activity_request(
    state: &AppState,
    target: &RouteTarget,
    request_id: &str,
    streamed: bool,
    build: impl Fn() -> AppResult<RequestBuilder>,
) -> AppResult<reqwest::Response> {
    let _ = state.activity.upstream_started(request_id);
    let result = send_provider_request(state, target, streamed, build).await;
    if result.is_ok() {
        let _ = state.activity.headers_received(request_id);
    }
    result
}

pub(crate) fn finish_activity_for_pre_response(
    state: &AppState,
    request_id: &str,
    result: &AppResult<Response>,
) {
    match result {
        Err(error) => fail_activity_for_error(state, request_id, error),
        Ok(response) if !response.status().is_success() => state.activity_fail(
            request_id,
            Some(response.status().as_u16() as i64),
            "gateway_response_error".to_string(),
        ),
        Ok(_) => {}
    }
}

pub(crate) fn candidates_for_activity(
    targets: &[RouteTarget],
    candidates: &[crate::models::RouteTraceCandidate],
) -> Vec<ActivityCandidate> {
    candidates
        .iter()
        .zip(targets.iter())
        .map(|(candidate, target)| ActivityCandidate {
            order: candidate.order,
            provider_id: candidate.provider_id,
            provider_name: Some(target.provider_name.clone()),
            provider_api_key_id: candidate.provider_api_key_id,
            upstream_model: candidate.upstream_model.clone(),
            eligible: candidate.eligible,
            deprioritized: candidate.deprioritized,
            reason_code: candidate.reason_code.clone(),
        })
        .collect()
}

pub(crate) fn fail_activity_for_error(state: &AppState, request_id: &str, error: &AppError) {
    let failure = super::route_trace::classify_trace_error(error);
    let status = match error {
        AppError::BadRequest(_) => 400,
        AppError::Unauthorized(_) => 401,
        AppError::Forbidden(_) => 403,
        AppError::NotFound(_) => 404,
        AppError::Conflict(_) => 409,
        AppError::Http(_) | AppError::Database(_) | AppError::Internal(_) => 500,
        _ => failure.status_code.unwrap_or(500),
    };
    state.activity_fail(request_id, Some(status), failure.reason_code.to_string());
}

pub(crate) fn finish_activity_from_usage(state: &AppState, entry: &UsageLogEntry<'_>) {
    state.activity_finish(
        entry.request_id,
        ActivityFinish {
            status_code: entry.status_code,
            success: entry.success,
            error_code: (!entry.success).then(|| "request_failed".to_string()),
            latency_ms: entry.latency_ms,
            first_token_ms: entry.streamed.then_some(entry.first_token_ms).flatten(),
            usage: entry.usage.normalized(),
        },
    );
}
