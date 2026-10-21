use super::*;
use crate::models::RequestPerformance;
use crate::state::{
    ActivityAttemptFinish, ActivityAttemptStart, ActivityDescription, ActivityTarget,
};

async fn performance_state() -> (tempfile::TempDir, AppState) {
    let directory = tempfile::tempdir().unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("performance.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    (directory, AppState::new(pool, None))
}

async fn begin(state: &AppState, id: &str, streamed: bool) {
    state.activity_start(id, OPENAI_CHAT_COMPLETIONS);
    state.activity_describe(
        id,
        ActivityDescription {
            requested_model: "model".into(),
            streamed,
            session_id: None,
            api_key_id: None,
            api_key_name: None,
            request_tokens_estimated: 10,
            context: None,
        },
    );
    log_usage_started(
        state,
        id,
        None,
        None,
        None,
        "model",
        OPENAI_CHAT_COMPLETIONS,
        10,
        streamed,
        None,
    )
    .await;
}

fn attempt(state: &AppState, id: &str, index: i64, success: bool, latency_ms: i64) {
    state.activity_begin_attempt(
        id,
        ActivityAttemptStart {
            attempt_index: index,
            round: 1,
            candidate_order: index,
            target: ActivityTarget {
                provider_id: 1,
                provider_name: "provider".into(),
                provider_api_key_id: None,
                upstream_model: "model".into(),
                context_window_tokens: None,
                input_limit: None,
            },
        },
    );
    state.activity.upstream_started(id);
    state.activity.headers_received(id);
    state.activity_finish_attempt(
        id,
        ActivityAttemptFinish {
            attempt_index: index,
            latency_ms,
            status_code: Some(if success { 200 } else { 503 }),
            success,
            reason_code: if success { "success" } else { "upstream_5xx" }.into(),
            failure_scope: (!success).then(|| "provider".into()),
        },
    );
}

async fn finish(state: &AppState, id: &str, streamed: bool, success: bool) {
    log_usage(
        state,
        UsageLogEntry {
            request_id: id,
            api_key_id: None,
            route_id: None,
            provider_id: None,
            requested_model: "model",
            upstream_model: None,
            endpoint: OPENAI_CHAT_COMPLETIONS,
            usage: Usage::new(10, 5),
            latency_ms: 1_000,
            first_token_ms: Some(if streamed { 500 } else { 1_000 }),
            status_code: if success { 200 } else { 502 },
            success,
            streamed,
            error_message: (!success).then_some("upstream failed"),
            response_preview: None,
        },
    )
    .await;
}

async fn detail(state: AppState, id: &str) -> crate::models::UsageLogView {
    crate::api::get_usage_detail(State(state), axum::extract::Path(id.into()))
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn final_timings_and_failed_attempt_cost_survive_a_new_process_state() {
    let (_directory, state) = performance_state().await;
    begin(&state, "performance", true).await;
    attempt(&state, "performance", 1, false, 300);
    attempt(&state, "performance", 2, true, 50);
    state.activity_first_token("performance", 500);
    finish(&state, "performance", true, true).await;
    let expected = state.activity.snapshot("performance").unwrap().timings;

    let restarted = AppState::new(state.pool.clone(), None);
    assert!(restarted.activity.snapshot("performance").is_none());
    let summary = detail(restarted.clone(), "performance")
        .await
        .performance
        .unwrap();
    assert_eq!(summary.timings, expected);
    assert_eq!(summary.timings.total_ms, 1_000);
    assert_eq!(summary.timings.time_to_first_token_ms, Some(500));
    assert_eq!(summary.attempt_count, 2);
    assert_eq!(summary.fallback_count, 1);
    assert_eq!(summary.failed_attempts_ms, 300);
    assert!(!summary.attempts_truncated);

    let page = crate::api::list_usage(
        State(restarted),
        axum::extract::Query(serde_json::from_value(json!({"request_id":"performance"})).unwrap()),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(page.items[0].performance.as_ref(), Some(&summary));
}

#[tokio::test]
async fn nonstreamed_failure_persists_without_inventing_first_token_or_generation() {
    let (_directory, state) = performance_state().await;
    begin(&state, "failed", false).await;
    attempt(&state, "failed", 1, false, 300);
    finish(&state, "failed", false, false).await;
    let summary = detail(state, "failed").await.performance.unwrap();
    assert_eq!(summary.timings.time_to_first_token_ms, None);
    assert_eq!(summary.timings.generation_ms, None);
    assert_eq!(summary.failed_attempts_ms, 300);
}

#[tokio::test]
async fn cancellation_while_waiting_persists_final_timings() {
    let (_directory, state) = performance_state().await;
    begin(&state, "cancelled", true).await;
    state.activity_waiting("cancelled", "capacity");
    let worker = state.clone();
    let (ready, started) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        track_usage_request(&worker, "cancelled", Instant::now(), async move {
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        })
        .await;
    });
    started.await.unwrap();
    task.abort();
    let _ = task.await;
    let raw: String = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row: Option<String> = sqlx::query_scalar(
                "SELECT performance_json FROM usage_logs WHERE request_id = 'cancelled' AND in_flight = 0",
            ).fetch_optional(&state.pool).await.unwrap().flatten();
            if let Some(row) = row { break row; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let performance: RequestPerformance = serde_json::from_str(&raw).unwrap();
    assert!(performance.timings.route_wait_ms.is_some());
    assert_eq!(performance.timings.generation_ms, None);
    assert_eq!(detail(state, "cancelled").await.status_code, 499);
}

#[tokio::test]
async fn absent_or_invalid_performance_remains_unknown_on_legacy_logs() {
    let (_directory, state) = performance_state().await;
    finish(&state, "legacy", false, true).await;
    assert!(detail(state.clone(), "legacy").await.performance.is_none());
    sqlx::query("UPDATE usage_logs SET performance_json = 'invalid' WHERE request_id = 'legacy'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(detail(state, "legacy").await.performance.is_none());
}

#[tokio::test]
async fn truncated_attempts_report_only_the_retained_failed_cost() {
    let (_directory, state) = performance_state().await;
    begin(&state, "truncated", false).await;
    for index in 1..=40 {
        attempt(&state, "truncated", index, index == 40, 10);
    }
    finish(&state, "truncated", false, true).await;
    let summary = detail(state, "truncated").await.performance.unwrap();
    assert_eq!(summary.attempt_count, 40);
    assert_eq!(summary.fallback_count, 39);
    assert!(summary.attempts_truncated);
    assert_eq!(summary.failed_attempts_ms, 31 * 10);
}
