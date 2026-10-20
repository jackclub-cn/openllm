use super::*;

#[test]
fn validates_overview_ranges() {
    let default_start = Utc::now() - Duration::days(13);
    let default_end = Utc::now() + Duration::days(1);
    assert_eq!(
        normalize_overview_range(None, None, default_start, default_end).unwrap(),
        (default_start, default_end)
    );

    let start = Utc::now() - Duration::days(7);
    let end = Utc::now();
    assert_eq!(
        normalize_overview_range(
            Some(&start.to_rfc3339()),
            Some(&end.to_rfc3339()),
            default_start,
            default_end,
        )
        .unwrap(),
        (start, end)
    );
    assert!(
        normalize_overview_range(Some("not-a-date"), None, default_start, default_end).is_err()
    );
    assert!(
        normalize_overview_range(
            Some(&end.to_rfc3339()),
            Some(&start.to_rfc3339()),
            default_start,
            default_end,
        )
        .is_err()
    );
    assert!(
        normalize_overview_range(
            Some(&(Utc::now() - Duration::days(367)).to_rfc3339()),
            Some(&Utc::now().to_rfc3339()),
            default_start,
            default_end,
        )
        .is_err()
    );
}

#[test]
fn overview_query_deserializes_typed_filters() {
    let uri: axum::http::Uri = "http://localhost/api/overview\
        ?tz_offset_minutes=480&provider_id=1&api_key_id=2&route_id=3\
        &provider_api_key_id=4&success=false&gateway_adjusted=true&full_breakdown=true\
        &granularity=week"
        .parse()
        .unwrap();
    let Query(query) = Query::<OverviewQuery>::try_from_uri(&uri).unwrap();
    assert_eq!(query.tz_offset_minutes, 480);
    assert_eq!(query.provider_id, Some(1));
    assert_eq!(query.api_key_id, Some(2));
    assert_eq!(query.route_id, Some(3));
    assert_eq!(query.provider_api_key_id, Some(4));
    assert_eq!(query.success, Some(false));
    assert_eq!(query.gateway_adjusted, Some(true));
    assert!(query.full_breakdown);
    assert_eq!(query.granularity, Some(AnalyticsGranularity::Week));
    assert_eq!(query.filters().provider_id, Some(1));
}

#[tokio::test]
async fn overview_full_breakdown_lifts_model_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let now = Utc::now().to_rfc3339();
    for index in 0..10 {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, latency_ms, status_code,
                    success, created_at
                 ) VALUES (?, ?, '/v1/chat/completions', 10, 5, 15, 100, 200, 1, ?)",
        )
        .bind(format!("model-{index}"))
        .bind(format!("model-{index:02}"))
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let Json(limited) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(limited.model_usage.len(), 8);

    let Json(full) = overview(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            full_breakdown: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(full.model_usage.len(), 10);
}

#[tokio::test]
async fn overview_compares_previous_equal_length_range() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let range_end = Utc::now() - Duration::hours(1);
    let range_start = range_end - Duration::hours(2);
    let previous_start = range_start - Duration::hours(2);
    for (request_id, created_at, tokens) in [
        ("previous-1", previous_start + Duration::minutes(30), 10),
        ("previous-2", previous_start + Duration::minutes(90), 20),
        ("current-1", range_start + Duration::minutes(30), 30),
        ("current-2", range_start + Duration::minutes(90), 40),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, 'test-model', '/v1/chat/completions', ?, 0, ?, 100, 200, 1, ?)",
        )
        .bind(request_id)
        .bind(tokens)
        .bind(tokens)
        .bind(created_at.to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let Json(view) = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Day),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(view.granularity, "day");
    assert_eq!(view.range_requests, 2);
    assert_eq!(view.range_tokens, 70);
    assert_eq!(view.previous_range.from, previous_start.to_rfc3339());
    assert_eq!(view.previous_range.to, range_start.to_rfc3339());
    assert_eq!(view.previous_range.requests, 2);
    assert_eq!(view.previous_range.tokens, 30);
    assert!((view.previous_range.success_rate - 100.0).abs() < 0.001);
}

#[tokio::test]
async fn overview_supports_week_and_month_buckets() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let range_start = DateTime::parse_from_rfc3339("2026-01-05T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let range_end = DateTime::parse_from_rfc3339("2026-02-02T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (request_id, created_at) in [
        ("week-1", "2026-01-06T12:00:00Z"),
        ("week-2", "2026-01-12T12:00:00Z"),
        ("week-3", "2026-02-01T12:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, 'test-model', '/v1/chat/completions', 10, 5, 15, 100, 200, 1, ?)",
        )
        .bind(request_id)
        .bind(created_at)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let Json(weekly) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Week),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(weekly.granularity, "week");
    assert_eq!(
        weekly
            .daily_usage
            .iter()
            .map(|row| row.day.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-01-05", "2026-01-12", "2026-01-19", "2026-01-26"]
    );
    assert_eq!(
        weekly
            .daily_usage
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        3
    );

    let Json(monthly) = overview(
        State(state),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Month),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(monthly.granularity, "month");
    assert_eq!(
        monthly
            .daily_usage
            .iter()
            .map(|row| row.day.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-01-01", "2026-02-01"]
    );
}

#[tokio::test]
async fn overview_rejects_oversized_hourly_range() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let start = Utc::now() - Duration::days(40);
    let result = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            from: Some(start.to_rfc3339()),
            to: Some(Utc::now().to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Hour),
            ..Default::default()
        }),
    )
    .await;
    assert!(result.is_err());
}
