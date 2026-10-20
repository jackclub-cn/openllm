use super::*;

#[tokio::test]
async fn overview_applies_dimension_filters() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled) VALUES
            (1, 'alpha-provider', 'openai', 'https://alpha.example/v1', 1),
            (2, 'beta-provider', 'openai', 'https://beta.example/v1', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let now = Utc::now().to_rfc3339();
    for (request_id, model, provider_id, endpoint, tokens, latency, status, success, error) in [
        (
            "alpha-ok",
            "alpha",
            1_i64,
            "/v1/chat/completions",
            10_i64,
            100_i64,
            200_i64,
            1_i64,
            None,
        ),
        (
            "alpha-failed",
            "alpha",
            1,
            "/v1/responses",
            20,
            300,
            500,
            0,
            Some("upstream failed"),
        ),
        (
            "beta-ok",
            "beta",
            2,
            "/v1/chat/completions",
            40,
            500,
            200,
            1,
            None,
        ),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, provider_id, endpoint,
                    prompt_tokens, completion_tokens, total_tokens, latency_ms,
                    status_code, success, error_message, created_at
                 ) VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(model)
        .bind(provider_id)
        .bind(endpoint)
        .bind(tokens)
        .bind(tokens)
        .bind(latency)
        .bind(status)
        .bind(success)
        .bind(error)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let call = |filters: UsageFilters| {
        let state = state.clone();
        let from = from.clone();
        let to = to.clone();
        async move {
            overview(
                State(state),
                Query(OverviewQuery {
                    tz_offset_minutes: 0,
                    from: Some(from),
                    to: Some(to),
                    include_session_metrics: false,
                    provider_id: filters.provider_id,
                    provider_api_key_id: filters.provider_api_key_id,
                    api_key_id: filters.api_key_id,
                    route_id: filters.route_id,
                    model: filters.model,
                    request_id: filters.request_id,
                    session_id: filters.session_id,
                    endpoint: filters.endpoint,
                    success: filters.success,
                    in_flight: filters.in_flight,
                    gateway_adjusted: filters.gateway_adjusted,
                    full_breakdown: false,
                    granularity: None,
                }),
            )
            .await
            .unwrap()
        }
    };

    let Json(alpha) = call(UsageFilters {
        model: Some("alpha".to_string()),
        ..Default::default()
    })
    .await;
    assert_eq!(alpha.range_requests, 2);
    assert_eq!(alpha.range_tokens, 30);
    assert!((alpha.range_success_rate - 50.0).abs() < 0.001);
    assert_eq!(alpha.model_usage.len(), 1);
    assert_eq!(alpha.model_usage[0].model, "alpha");
    assert_eq!(alpha.provider_usage.len(), 1);
    assert_eq!(alpha.provider_usage[0].provider_name, "alpha-provider");
    assert_eq!(alpha.status_breakdown.len(), 2);
    assert_eq!(
        alpha
            .status_breakdown
            .iter()
            .find(|row| row.status_code == 500)
            .map(|row| row.count),
        Some(1)
    );

    let Json(beta_provider) = call(UsageFilters {
        provider_id: Some(2),
        ..Default::default()
    })
    .await;
    assert_eq!(beta_provider.range_requests, 1);
    assert_eq!(beta_provider.range_tokens, 40);
    assert_eq!(beta_provider.model_usage[0].model, "beta");

    let Json(failed) = call(UsageFilters {
        success: Some(false),
        ..Default::default()
    })
    .await;
    assert_eq!(failed.range_requests, 1);
    assert_eq!(failed.error_reasons.len(), 1);
    assert_eq!(failed.error_reasons[0].label, "upstream failed");
    assert_eq!(failed.error_reasons[0].count, 1);
}
