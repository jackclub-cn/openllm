use super::*;

#[tokio::test]
async fn analytics_route_quality_groups_strategies_routes_sessions_and_failures() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, strategy_ext) VALUES
            (1, 'Priority route', 'gpt-*', 'priority', ''),
            (2, 'Cost route', 'claude-*', 'priority', 'cost_optimized')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let created_at = (Utc::now() - Duration::minutes(10)).to_rfc3339();
    for (request_id, route_id, model, success, latency) in [
        ("quality-1", 1_i64, "gpt-5.1", 1_i64, 100_i64),
        ("quality-2", 2, "claude-sonnet-4", 1, 250),
        ("quality-3", 1, "gpt-5.1", 0, 800),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, route_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, ?, ?, '/v1/chat/completions', 10, 5, 15, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(route_id)
        .bind(model)
        .bind(latency)
        .bind(if success == 1 { 200 } else { 500 })
        .bind(success)
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (
        request_id,
        route_id,
        strategy,
        session_decision,
        attempt_count,
        fallback_count,
        success,
    ) in [
        ("quality-1", 1_i64, "priority", "hit", 1_i64, 0_i64, 1_i64),
        ("quality-2", 2, "cost_optimized", "miss", 2, 1, 1),
        ("quality-3", 1, "priority", "no_affinity", 1, 0, 0),
    ] {
        sqlx::query(
            "INSERT INTO routing_traces (
                 request_id, route_id, strategy, session_id, session_decision,
                 attempt_count, fallback_count, success
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(route_id)
        .bind(strategy)
        .bind((request_id != "quality-3").then_some("session-a"))
        .bind(session_decision)
        .bind(attempt_count)
        .bind(fallback_count)
        .bind(success)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO routing_attempts (
             request_id, attempt_index, round, candidate_order, started_at,
             latency_ms, status_code, success, retryable, reason_code,
             failure_scope, failure_retry, failure_confidence
         ) VALUES
            ('quality-1', 1, 0, 1, ?, 100, 200, 1, 0, 'upstream_success', NULL, NULL, 'exact'),
            ('quality-2', 1, 0, 1, ?, 120, 429, 0, 1, 'upstream_rate_limited', 'provider', 'next_target', 'inferred'),
            ('quality-2', 2, 0, 2, ?, 130, 200, 1, 0, 'upstream_success', NULL, NULL, 'exact'),
            ('quality-3', 1, 0, 1, ?, 800, 500, 0, 0, 'internal_error', 'endpoint', 'do_not_retry', 'exact')",
    )
    .bind(&created_at)
    .bind(&created_at)
    .bind(&created_at)
    .bind(&created_at)
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let to = Utc::now().to_rfc3339();
    let Json(quality) = analytics_route_quality(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(quality.traces, 3);
    assert!((quality.fallback_rate - 33.333_333).abs() < 0.001);
    assert!((quality.avg_attempts - 1.333_333).abs() < 0.001);
    assert_eq!(quality.session_requests, 2);
    assert_eq!(quality.session_hits, 1);
    assert!((quality.session_hit_rate - 50.0).abs() < 0.001);
    assert!((quality.success_rate - 66.666_666).abs() < 0.001);
    assert_eq!(quality.strategies.len(), 2);
    let priority = quality
        .strategies
        .iter()
        .find(|row| row.strategy == "priority")
        .unwrap();
    assert_eq!(priority.traces, 2);
    assert!((priority.success_rate - 50.0).abs() < 0.001);
    let cost = quality
        .strategies
        .iter()
        .find(|row| row.strategy == "cost_optimized")
        .unwrap();
    assert_eq!(cost.fallback_rate, 100.0);
    assert_eq!(quality.routes.len(), 2);
    assert_eq!(
        quality
            .routes
            .iter()
            .find(|row| row.route_id == Some(1))
            .map(|row| row.route_name.as_str()),
        Some("Priority route")
    );
    assert_eq!(
        quality
            .session_decisions
            .iter()
            .find(|row| row.decision == "hit")
            .map(|row| row.count),
        Some(1)
    );
    assert_eq!(
        quality
            .failure_reasons
            .iter()
            .find(|row| row.reason_code == "upstream_rate_limited")
            .map(|row| row.failure_scope.as_str()),
        Some("provider")
    );

    let Json(filtered) = analytics_route_quality(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            route_id: Some(1),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(filtered.traces, 2);
    assert_eq!(filtered.routes.len(), 1);
    assert_eq!(filtered.routes[0].route_id, Some(1));
}
