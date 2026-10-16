use super::*;

#[tokio::test]
async fn routing_trace_persists_candidates_attempts_and_cascades_with_usage() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, requested_model, endpoint, status_code, success, in_flight
         ) VALUES ('trace-request', 'model-a', '/v1/chat/completions', 200, 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let mut recorder = RouteTraceRecorder::new(
        "trace-request".to_string(),
        Some(7),
        "cost_optimized".to_string(),
        Some("session-a".to_string()),
    );
    recorder.candidates = vec![
        RouteTraceCandidate {
            order: 1,
            provider_id: 1,
            provider_api_key_id: None,
            upstream_model: "model-a".to_string(),
            eligible: true,
            deprioritized: false,
            reason_code: "ordered".to_string(),
        },
        RouteTraceCandidate {
            order: 2,
            provider_id: 2,
            provider_api_key_id: Some(8),
            upstream_model: "model-b".to_string(),
            eligible: true,
            deprioritized: true,
            reason_code: "target_sink".to_string(),
        },
    ];
    recorder.record_attempt(TraceAttemptRecord {
        attempt_index: 1,
        round: 0,
        candidate_order: 1,
        provider_id: Some(1),
        provider_api_key_id: None,
        upstream_model: Some("model-a".to_string()),
        started_at: "2026-10-16T00:00:00Z".to_string(),
        latency_ms: 42,
        status_code: Some(503),
        success: false,
        retryable: true,
        reason_code: "provider_short_circuit".to_string(),
        failure_scope: Some("provider".to_string()),
        failure_retry: Some("next_target".to_string()),
        failure_confidence: Some("inferred".to_string()),
    });
    persist_trace(
        &state,
        &recorder,
        &TraceFinal {
            final_provider_id: Some(2),
            final_provider_api_key_id: Some(8),
            final_upstream_model: Some("model-b".to_string()),
            final_status_code: Some(200),
            success: true,
            fallback_count: 1,
        },
    )
    .await
    .unwrap();

    let (candidates_json, attempt_count, fallback_count, success): (String, i64, i64, i64) =
        sqlx::query_as(
            "SELECT candidates_json, attempt_count, fallback_count, success \
             FROM routing_traces WHERE request_id = 'trace-request'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let candidates = serde_json::from_str::<Vec<RouteTraceCandidate>>(&candidates_json).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[1].reason_code, "target_sink");
    assert_eq!(attempt_count, 1);
    assert_eq!(fallback_count, 1);
    assert_eq!(success, 1);

    let attempt: (i64, Option<i64>, i64, String) = sqlx::query_as(
        "SELECT attempt_index, status_code, latency_ms, reason_code \
         FROM routing_attempts WHERE request_id = 'trace-request'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        attempt,
        (1, Some(503), 42, "provider_short_circuit".to_string())
    );

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'trace-request'")
        .execute(&state.pool)
        .await
        .unwrap();
    let traces: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM routing_traces WHERE request_id = 'trace-request'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM routing_attempts WHERE request_id = 'trace-request'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(traces, 0);
    assert_eq!(attempts, 0);
}
