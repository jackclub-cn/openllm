use super::*;

#[tokio::test]
async fn usage_page_nests_failed_attempts_before_final_outcome() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
         VALUES (11, 1, 'Primary', 'sk-primary', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, provider_id, provider_api_key_id, requested_model,
             upstream_model, endpoint, status_code, success, in_flight
         ) VALUES ('tree-api', 1, 11, 'model-a', 'model-a',
                   '/v1/chat/completions', 200, 1, 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_traces (
             request_id, strategy, candidates_json, attempt_count, fallback_count,
             final_provider_id, final_provider_api_key_id, final_upstream_model,
             final_status_code, success
         ) VALUES ('tree-api', 'priority', '[]', 2, 1, 1, 11, 'model-a', 200, 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_attempts (
             request_id, attempt_index, round, candidate_order,
             provider_id, provider_api_key_id, upstream_model,
             started_at, latency_ms, status_code, success, retryable, reason_code
         ) VALUES
             ('tree-api', 1, 0, 1, 1, 11, 'model-a',
              '2026-10-16T00:00:00Z', 42, 502, 0, 1, 'upstream_unavailable'),
             ('tree-api', 2, 0, 2, 1, 11, 'model-a',
              '2026-10-16T00:00:01Z', 10, 200, 1, 0, 'upstream_success')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
    let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].children.len(), 1);
    let attempt = &page.items[0].children[0];
    assert_eq!(attempt.attempt_index, 1);
    assert!(!attempt.success);
    assert_eq!(attempt.status_code, Some(502));
    assert_eq!(attempt.provider_name.as_deref(), Some("Provider"));
    assert_eq!(attempt.provider_api_key_name.as_deref(), Some("Primary"));
}

#[tokio::test]
async fn route_trace_api_exposes_candidates_and_attempts() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, requested_model, endpoint, status_code, success, in_flight
         ) VALUES ('trace-api', 'model-a', '/v1/chat/completions', 200, 1, 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let candidates = serde_json::to_string(&vec![RouteTraceCandidate {
        order: 1,
        provider_id: 1,
        provider_api_key_id: Some(11),
        upstream_model: "model-a".to_string(),
        eligible: true,
        deprioritized: false,
        reason_code: "ordered".to_string(),
    }])
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_traces (
             request_id, route_id, strategy, session_id, session_decision,
             candidates_json, attempt_count, fallback_count,
             final_provider_id, final_provider_api_key_id,
             final_upstream_model, final_status_code, success
         ) VALUES ('trace-api', 7, 'priority', 'session-a', 'session_key_present',
                   ?, 1, 0, 1, 11, 'model-a', 200, 1)",
    )
    .bind(candidates)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_attempts (
             request_id, attempt_index, round, candidate_order,
             provider_id, provider_api_key_id, upstream_model,
             started_at, latency_ms, status_code, success, retryable,
             reason_code, failure_scope, failure_retry, failure_confidence
         ) VALUES ('trace-api', 1, 0, 1, 1, 11, 'model-a',
                   '2026-10-16T00:00:00Z', 42, 200, 1, 0,
                   'upstream_success', NULL, NULL, NULL)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(trace) = get_route_trace(State(state.clone()), Path("trace-api".to_string()))
        .await
        .unwrap();
    assert_eq!(trace.route_id, Some(7));
    assert_eq!(trace.session_decision, "session_key_present");
    assert_eq!(trace.candidates.len(), 1);
    assert_eq!(trace.candidates[0].provider_id, 1);
    assert!(trace.candidates[0].eligible);
    assert_eq!(trace.attempts.len(), 1);
    assert!(trace.attempts[0].success);
    assert_eq!(trace.attempts[0].reason_code, "upstream_success");

    let error = get_route_trace(State(state), Path("missing-trace".to_string()))
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)));
}

#[tokio::test]
async fn usage_views_expose_provider_api_key_name() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model,
                endpoint, prompt_tokens, completion_tokens, total_tokens,
                latency_ms, status_code, success, warning_message
             ) VALUES (
                'request-with-provider-key', 1, 11, 'Primary', 'gpt-test',
                '/v1/chat/completions', 10, 4, 14, 123, 200, 1, 'repaired request'
             )",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
    let Json(page) = list_usage(State(state.clone()), Query(query))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].provider_api_key_id, Some(11));
    assert_eq!(
        page.items[0].provider_api_key_name.as_deref(),
        Some("Primary")
    );

    let Json(detail) = get_usage_detail(
        State(state.clone()),
        Path("request-with-provider-key".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(detail.provider_api_key_id, Some(11));
    assert_eq!(detail.provider_api_key_name.as_deref(), Some("Primary"));
    assert_eq!(detail.warning_message.as_deref(), Some("repaired request"));

    let provider = get_provider(&state, 1).await.unwrap();
    assert_eq!(provider.api_keys[0].requests, 1);
    assert_eq!(provider.api_keys[0].success_rate, 100.0);
    assert_eq!(provider.api_keys[0].avg_latency_ms, 123.0);
    assert_eq!(provider.api_keys[0].prompt_tokens, 10);
    assert_eq!(provider.api_keys[0].completion_tokens, 4);
    state.provider_key_cooldown.lock().await.insert(
        11,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    );
    let provider = get_provider(&state, 1).await.unwrap();
    assert!(provider.api_keys[0].cooldown_seconds.is_some());
    assert!(provider.api_keys[0].cooldown_seconds.unwrap() > 0);

    state.provider_cooldown.lock().await.insert(
        1,
        std::time::Instant::now() + std::time::Duration::from_secs(90),
    );
    let provider = get_provider(&state, 1).await.unwrap();
    assert!(provider.cooldown_seconds.is_some());
    assert!(provider.cooldown_seconds.unwrap() > 0);
}

#[tokio::test]
async fn unfiltered_usage_total_combines_lifetime_and_in_flight_counts() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success
             ) VALUES
                ('pending', 'm', '/v1/chat/completions', 0, 1, 0),
                ('finished', 'm', '/v1/chat/completions', 200, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();

    let Json(page) = list_usage(State(state.clone()), Query(query))
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'finished'")
        .execute(&state.pool)
        .await
        .unwrap();
    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
    let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "pending");
}

#[tokio::test]
async fn usage_filter_can_select_provider_api_key() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1),
                    (12, 1, 'Backup', 'sk-backup', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model, endpoint, status_code, success
             ) VALUES
                ('primary', 1, 11, 'Primary', 'gpt-test', '/v1/chat/completions', 200, 1),
                ('backup', 1, 12, 'Backup', 'gpt-test', '/v1/chat/completions', 200, 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let query: UsageQuery = serde_json::from_value(json!({
        "provider_api_key_id": 12
    }))
    .unwrap();
    let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "backup");
    assert_eq!(
        page.items[0].provider_api_key_name.as_deref(),
        Some("Backup")
    );
}

#[test]
fn escapes_csv_fields() {
    assert_eq!(csv_field("plain"), "plain");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
}
