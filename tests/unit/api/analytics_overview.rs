use super::*;

#[tokio::test]
async fn overview_range_filters_dashboard_usage() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let old = (Utc::now() - Duration::days(20)).to_rfc3339();
    let recent = (Utc::now() - Duration::days(1)).to_rfc3339();
    let today = Utc::now().to_rfc3339();
    for (request_id, session_id, prompt, completion, cache_read, latency_ms, success, created_at) in [
        (
            "old",
            Some("old-session"),
            700_i64,
            200_i64,
            0_i64,
            500_i64,
            1_i64,
            old,
        ),
        (
            "recent",
            Some("shared-session"),
            60,
            40,
            20,
            100,
            1,
            recent.clone(),
        ),
        (
            "recent-2",
            Some("shared-session"),
            30,
            10,
            5,
            200,
            1,
            recent,
        ),
        ("today", None, 50, 0, 0, 300, 0, today),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, session_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, cache_read_tokens, latency_ms,
                    status_code, success, created_at
                 ) VALUES (?, ?, 'test-model', '/v1/chat/completions', ?, ?, ?, ?, ?, 200, ?, ?)",
        )
        .bind(request_id)
        .bind(session_id)
        .bind(prompt)
        .bind(completion)
        .bind(prompt + completion)
        .bind(cache_read)
        .bind(latency_ms)
        .bind(success)
        .bind(created_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                total_tokens, status_code, in_flight, success, created_at
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 25, 25, 0, 1, 0,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE usage_logs SET warning_message = 'repaired request'
             WHERE request_id = 'recent-2'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (name, provider_type, base_url, enabled, last_test_ok)
             VALUES
                ('healthy', 'openai', 'https://healthy.example/v1', 1, 1),
                ('failed', 'openai', 'https://failed.example/v1', 1, 0),
                ('untested', 'openai', 'https://untested.example/v1', 1, NULL),
                ('disabled', 'openai', 'https://disabled.example/v1', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (
                provider_id, name, secret, enabled, last_test_ok, last_error
             ) VALUES
                (1, 'Healthy', 'sk-healthy', 1, 1, NULL),
                (2, 'Failed', 'sk-failed', 1, 0, 'upstream 401'),
                (3, 'Untested', 'sk-untested', 1, NULL, NULL),
                (4, 'Disabled provider', 'sk-disabled-provider', 1, 1, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    state.provider_key_cooldown.lock().await.insert(
        99,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    );
    state.provider_cooldown.lock().await.insert(
        2,
        std::time::Instant::now() + std::time::Duration::from_secs(90),
    );
    let range_start = Utc::now() - Duration::days(2);
    let range_end = Utc::now() + Duration::days(1);
    let Json(view) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            include_session_metrics: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(view.requests_today, 1);
    assert_eq!(view.requests_total, 4);
    assert_eq!(view.prompt_tokens_today, 50);
    assert_eq!(view.completion_tokens_today, 0);
    assert_eq!(view.prompt_tokens_total, 840);
    assert_eq!(view.completion_tokens_total, 250);
    assert_eq!(view.range_requests, 3);
    assert_eq!(view.range_tokens, 190);
    assert_eq!(view.range_prompt_tokens, 140);
    assert_eq!(view.range_completion_tokens, 50);
    assert_eq!(view.range_cache_read, 25);
    assert_eq!(view.range_sessions, 1);
    assert!((view.range_session_coverage - 66.666_666).abs() < 0.001);
    assert_eq!(view.range_avg_requests_per_session, 2.0);
    assert!((view.range_session_cache_hit_rate - 27.777_777).abs() < 0.001);
    assert!((view.range_success_rate - 66.666_666).abs() < 0.001);
    assert_eq!(view.range_avg_latency_ms, 200.0);
    assert_eq!(view.range_gateway_adjusted, 1);
    assert_eq!(view.range_latency_p50_ms, Some(200));
    assert_eq!(view.range_latency_p95_ms, Some(300));
    assert_eq!(view.range_latency_p99_ms, Some(300));
    assert!(view.range_first_token_p50_ms.is_none());
    assert!((view.range_output_tps.unwrap() - 166.666_666).abs() < 0.001);
    assert_eq!(view.status_breakdown.len(), 1);
    assert_eq!(view.status_breakdown[0].status_code, 200);
    assert_eq!(view.status_breakdown[0].count, 3);
    assert_eq!(view.error_reasons.len(), 1);
    assert_eq!(view.error_reasons[0].label, "无错误信息");
    assert_eq!(view.error_reasons[0].count, 1);
    assert_eq!(view.gateway_adjustments.len(), 1);
    assert_eq!(view.gateway_adjustments[0].category, "other");
    assert_eq!(view.gateway_adjustments[0].count, 1);
    assert_eq!(view.active_providers, 3);
    assert_eq!(view.healthy_providers, 1);
    assert_eq!(view.failed_providers, 1);
    assert_eq!(view.untested_providers, 1);
    assert_eq!(view.provider_keys_total, 3);
    assert_eq!(view.healthy_provider_keys, 1);
    assert_eq!(view.failed_provider_keys, 1);
    assert_eq!(view.untested_provider_keys, 1);
    assert_eq!(view.runtime_error_provider_keys, 1);
    assert_eq!(view.cooling_providers, 1);
    assert_eq!(view.cooling_provider_keys, 1);
    assert_eq!(view.in_flight_requests, 1);
    assert_eq!(view.granularity, "day");
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.prompt_tokens)
            .sum::<i64>(),
        view.range_prompt_tokens
    );
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.completion_tokens)
            .sum::<i64>(),
        view.range_completion_tokens
    );
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.cost_micros)
            .sum::<i64>(),
        view.range_cost_micros
    );
    assert_eq!(view.recent_requests.len(), 4);
    assert!(view.recent_requests.iter().any(|row| row.in_flight));
    assert!(
        view.recent_requests
            .iter()
            .all(|row| row.request_id != "old")
    );
    assert_eq!(view.model_usage[0].requests, 3);
    assert_eq!(view.model_usage[0].tokens, 190);
    assert_eq!(view.model_usage[0].prompt_tokens, 140);
    assert_eq!(view.model_usage[0].completion_tokens, 50);
    assert_eq!(view.model_usage[0].cache_read, 25);
    assert!((view.model_usage[0].cache_hit_rate - 17.857_142).abs() < 0.001);
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.cache_read)
            .sum::<i64>(),
        view.range_cache_read
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        view.range_requests
    );
    assert!(
        view.daily_model_requests
            .iter()
            .all(|row| row.label == "test-model")
    );
    // Requests with no routed provider still count toward the daily totals, so
    // the share chart keeps the same denominator as the trend line.
    assert_eq!(
        view.daily_provider_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        view.range_requests
    );
    assert!(
        view.daily_provider_requests
            .iter()
            .all(|row| row.label == "其他")
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.tokens)
            .sum::<i64>(),
        view.range_tokens
    );

    let Json(view_without_sessions) = overview(
        State(state),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            include_session_metrics: false,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(view_without_sessions.range_requests, view.range_requests);
    assert_eq!(view_without_sessions.range_sessions, 0);
    assert_eq!(view_without_sessions.range_session_coverage, 0.0);
}

#[tokio::test]
async fn overview_buckets_a_single_day_by_hour() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    for (request_id, hour) in [("morning", 9_i64), ("evening", 21)] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, status_code, success, created_at
                 ) VALUES (
                    ?, 'test-model', '/v1/chat/completions', 10, 5, 15, 200, 1, ?
                 )",
        )
        .bind(request_id)
        .bind((day_start + Duration::hours(hour)).to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let Json(view) = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(day_start.to_rfc3339()),
            to: Some((day_start + Duration::days(1)).to_rfc3339()),
            include_session_metrics: false,
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let day = day_start.format("%Y-%m-%d").to_string();
    assert_eq!(view.granularity, "hour");
    assert_eq!(view.daily_usage.len(), 24);
    assert_eq!(view.daily_usage[0].day, format!("{day} 00:00"));
    assert_eq!(view.daily_usage[23].day, format!("{day} 23:00"));

    let busy: Vec<_> = view
        .daily_usage
        .iter()
        .filter(|row| row.requests > 0)
        .collect();
    assert_eq!(busy.len(), 2);
    assert_eq!(busy[0].day, format!("{day} 09:00"));
    assert_eq!(busy[0].requests, 1);
    assert_eq!(busy[1].day, format!("{day} 21:00"));
    // The model and provider breakdowns share the same hourly buckets.
    assert!(
        view.daily_model_requests
            .iter()
            .all(|row| row.day.starts_with(&format!("{day} ")))
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        2
    );
}
