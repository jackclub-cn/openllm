use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use axum::http::StatusCode;

use crate::error::AppError;
use crate::models::{RouteStrategy, RouteTarget, RouteTraceCandidate};
use crate::state::AppState;

const OFF_VALUES: &[&str] = &["0", "false", "off", "disabled", "none"];

fn route_traces_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("OPENLLM_ROUTE_TRACES")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .is_none_or(|value| !OFF_VALUES.contains(&value.as_str()))
    })
}

#[derive(Debug)]
pub(crate) struct TraceAttemptRecord {
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

#[derive(Debug)]
pub(crate) struct TraceFinal {
    pub final_provider_id: Option<i64>,
    pub final_provider_api_key_id: Option<i64>,
    pub final_upstream_model: Option<String>,
    pub final_status_code: Option<i64>,
    pub success: bool,
    pub fallback_count: i64,
}

pub(crate) struct RouteTraceRecorder {
    enabled: bool,
    request_id: String,
    route_id: Option<i64>,
    strategy: String,
    session_id: Option<String>,
    session_decision: String,
    candidates: Vec<RouteTraceCandidate>,
    attempts: Vec<TraceAttemptRecord>,
}

pub(crate) struct AttemptResume {
    pub trace: RouteTraceRecorder,
    pub attempts: usize,
}

impl RouteTraceRecorder {
    pub(crate) fn new(
        request_id: String,
        route_id: Option<i64>,
        strategy: String,
        session_id: Option<String>,
    ) -> Self {
        let session_decision = if session_id.is_some() {
            "session_key_present"
        } else {
            "no_affinity"
        }
        .to_string();
        Self {
            enabled: route_traces_enabled(),
            request_id,
            route_id,
            strategy,
            session_id,
            session_decision,
            candidates: Vec::new(),
            attempts: Vec::new(),
        }
    }

    pub(crate) async fn update_candidates(
        &mut self,
        state: &AppState,
        strategy: &str,
        targets: &[RouteTarget],
        api_key_id: Option<i64>,
    ) {
        if !self.enabled && !state.activity.is_enabled() {
            return;
        }
        let strategy = strategy.parse().unwrap_or(RouteStrategy::Priority);
        let deprioritized =
            super::routing::deprioritized_target_keys(state, strategy, targets, api_key_id).await;
        self.candidates = candidates_from_targets(targets, &deprioritized);
    }

    pub(crate) fn record_attempt(&mut self, mut attempt: TraceAttemptRecord) {
        if self.enabled {
            attempt.attempt_index = self.attempts.len() as i64 + 1;
            self.attempts.push(attempt);
        }
    }

    pub(crate) fn set_session_decision(&mut self, decision: &str) {
        if self.enabled {
            self.session_decision = decision.to_string();
        }
    }

    pub(crate) fn candidates(&self) -> &[RouteTraceCandidate] {
        &self.candidates
    }

    pub(crate) async fn finish(mut self, state: &AppState, final_outcome: TraceFinal) {
        if !self.enabled {
            return;
        }
        self.enabled = false;
        let capacity = state.acquire_background_db_slot().await;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = persist_trace(&state, &self, &final_outcome).await {
                tracing::warn!(
                    %error,
                    request_id = %self.request_id,
                    "failed to persist routing trace"
                );
            }
            drop(capacity);
        });
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TraceFailure {
    pub status_code: Option<i64>,
    pub retryable: bool,
    pub reason_code: &'static str,
    pub failure_scope: Option<&'static str>,
    pub failure_retry: Option<&'static str>,
    pub failure_confidence: Option<&'static str>,
}

pub(crate) fn classify_trace_error(error: &AppError) -> TraceFailure {
    match error {
        AppError::Upstream(_) | AppError::Http(_) => TraceFailure {
            status_code: Some(StatusCode::BAD_GATEWAY.as_u16() as i64),
            retryable: true,
            reason_code: "upstream_transport_error",
            failure_scope: Some("transport"),
            failure_retry: Some("next_target"),
            failure_confidence: Some("inferred"),
        },
        AppError::UpstreamStatus {
            status,
            retry_after,
            ..
        } => {
            let retryable = status.is_server_error()
                || *status == StatusCode::TOO_MANY_REQUESTS
                || *status == StatusCode::REQUEST_TIMEOUT;
            let (reason_code, scope) = match *status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    ("credential_rejected", "credential")
                }
                StatusCode::NOT_FOUND => ("model_not_found", "model"),
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                    ("request_shape_incompatible", "request_shape")
                }
                StatusCode::TOO_MANY_REQUESTS => ("upstream_rate_limited", "provider"),
                StatusCode::SERVICE_UNAVAILABLE => ("provider_short_circuit", "provider"),
                _ => ("upstream_status_error", "provider"),
            };
            TraceFailure {
                status_code: Some(status.as_u16() as i64),
                retryable,
                reason_code,
                failure_scope: Some(scope),
                failure_retry: Some(if retry_after.is_some() {
                    "wait_then_retry"
                } else if retryable {
                    "next_target"
                } else {
                    "do_not_retry"
                }),
                failure_confidence: Some("inferred"),
            }
        }
        AppError::TooManyRequests(_) => TraceFailure {
            status_code: Some(StatusCode::TOO_MANY_REQUESTS.as_u16() as i64),
            retryable: true,
            reason_code: "local_capacity_exhausted",
            failure_scope: Some("provider"),
            failure_retry: Some("wait_then_retry"),
            failure_confidence: Some("exact"),
        },
        AppError::BadRequest(_) | AppError::Conflict(_) => TraceFailure {
            status_code: None,
            retryable: false,
            reason_code: "local_request_rejected",
            failure_scope: Some("client"),
            failure_retry: Some("do_not_retry"),
            failure_confidence: Some("exact"),
        },
        AppError::Unauthorized(_) | AppError::Forbidden(_) | AppError::NotFound(_) => {
            TraceFailure {
                status_code: None,
                retryable: false,
                reason_code: "gateway_rejected",
                failure_scope: Some("client"),
                failure_retry: Some("do_not_retry"),
                failure_confidence: Some("exact"),
            }
        }
        AppError::Database(_) | AppError::Internal(_) => TraceFailure {
            status_code: Some(StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i64),
            retryable: false,
            reason_code: "internal_error",
            failure_scope: Some("endpoint"),
            failure_retry: Some("do_not_retry"),
            failure_confidence: Some("exact"),
        },
    }
}

pub(crate) async fn classify_trace_error_for_target(
    state: &AppState,
    target: &RouteTarget,
    error: &AppError,
) -> TraceFailure {
    let target_metadata = state
        .target_failure_meta
        .lock()
        .await
        .get(&(target.provider_id, target.upstream_model.clone()))
        .cloned();
    let key_metadata = if let Some(key_id) = target.provider_api_key_id {
        state
            .provider_key_failure_meta
            .lock()
            .await
            .get(&key_id)
            .cloned()
    } else {
        None
    };
    let metadata = target_metadata.or(key_metadata);
    if let Some(metadata) = metadata
        && metadata.observed_at.elapsed() <= std::time::Duration::from_secs(5)
    {
        let fallback = classify_trace_error(error);
        return TraceFailure {
            status_code: fallback.status_code,
            retryable: matches!(metadata.retry, "next_target" | "wait_then_retry"),
            reason_code: metadata.reason_code,
            failure_scope: Some(metadata.scope),
            failure_retry: Some(metadata.retry),
            failure_confidence: Some(metadata.confidence),
        };
    }
    classify_trace_error(error)
}

fn candidates_from_targets(
    targets: &[RouteTarget],
    deprioritized: &HashMap<(i64, Option<i64>, String), &'static str>,
) -> Vec<RouteTraceCandidate> {
    targets
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let key = (
                target.provider_id,
                target.provider_api_key_id,
                target.upstream_model.clone(),
            );
            let reason_code = deprioritized
                .get(&key)
                .copied()
                .unwrap_or("ordered")
                .to_string();
            RouteTraceCandidate {
                order: index as i64 + 1,
                provider_id: target.provider_id,
                provider_api_key_id: target.provider_api_key_id,
                upstream_model: target.upstream_model.clone(),
                eligible: true,
                deprioritized: reason_code != "ordered",
                reason_code,
            }
        })
        .collect()
}

async fn persist_trace(
    state: &AppState,
    trace: &RouteTraceRecorder,
    final_outcome: &TraceFinal,
) -> Result<(), sqlx::Error> {
    let candidates_json = serde_json::to_string(&trace.candidates).unwrap_or_else(|_| "[]".into());
    let mut transaction = state.pool.begin().await?;
    sqlx::query("DELETE FROM routing_attempts WHERE request_id = ?")
        .bind(&trace.request_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO routing_traces (
             request_id, route_id, strategy, session_id, session_decision,
             candidates_json, attempt_count, fallback_count,
             final_provider_id, final_provider_api_key_id,
             final_upstream_model, final_status_code, success, finished_at
         ) VALUES (
             ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )
         ON CONFLICT(request_id) DO UPDATE SET
             route_id = excluded.route_id,
             strategy = excluded.strategy,
             session_id = excluded.session_id,
             session_decision = excluded.session_decision,
             candidates_json = excluded.candidates_json,
             attempt_count = excluded.attempt_count,
             fallback_count = excluded.fallback_count,
             final_provider_id = excluded.final_provider_id,
             final_provider_api_key_id = excluded.final_provider_api_key_id,
             final_upstream_model = excluded.final_upstream_model,
             final_status_code = excluded.final_status_code,
             success = excluded.success,
             finished_at = excluded.finished_at",
    )
    .bind(&trace.request_id)
    .bind(trace.route_id)
    .bind(&trace.strategy)
    .bind(&trace.session_id)
    .bind(&trace.session_decision)
    .bind(candidates_json)
    .bind(trace.attempts.len() as i64)
    .bind(final_outcome.fallback_count)
    .bind(final_outcome.final_provider_id)
    .bind(final_outcome.final_provider_api_key_id)
    .bind(&final_outcome.final_upstream_model)
    .bind(final_outcome.final_status_code)
    .bind(final_outcome.success as i64)
    .execute(&mut *transaction)
    .await?;

    for attempt in &trace.attempts {
        sqlx::query(
            "INSERT INTO routing_attempts (
                 request_id, attempt_index, round, candidate_order,
                 provider_id, provider_api_key_id, upstream_model,
                 started_at, latency_ms, status_code, success, retryable,
                 reason_code, failure_scope, failure_retry, failure_confidence
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&trace.request_id)
        .bind(attempt.attempt_index)
        .bind(attempt.round)
        .bind(attempt.candidate_order)
        .bind(attempt.provider_id)
        .bind(attempt.provider_api_key_id)
        .bind(&attempt.upstream_model)
        .bind(&attempt.started_at)
        .bind(attempt.latency_ms)
        .bind(attempt.status_code)
        .bind(attempt.success as i64)
        .bind(attempt.retryable as i64)
        .bind(&attempt.reason_code)
        .bind(&attempt.failure_scope)
        .bind(&attempt.failure_retry)
        .bind(&attempt.failure_confidence)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await
}

pub(crate) fn attempt_elapsed(started: Instant) -> i64 {
    started.elapsed().as_millis().min(i64::MAX as u128) as i64
}

#[cfg(test)]
#[path = "../../tests/unit/route_trace.rs"]
mod tests;
