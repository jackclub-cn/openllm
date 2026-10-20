use super::*;

#[tokio::test]
async fn analytics_dimensions_groups_keys_routes_endpoints_and_sessions() {
    #[expect(
        clippy::too_many_arguments,
        reason = "fixture mirrors usage log columns"
    )]
    async fn insert_usage(
        pool: &sqlx::SqlitePool,
        request_id: &str,
        api_key_id: Option<i64>,
        route_id: Option<i64>,
        provider_id: Option<i64>,
        provider_api_key_id: Option<i64>,
        model: &str,
        endpoint: &str,
        session_id: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read: i64,
        cache_write: i64,
        latency_ms: i64,
        status_code: i64,
        success: i64,
        cost_micros: Option<i64>,
        created_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, api_key_id, route_id, provider_id, provider_api_key_id,
                    requested_model, provider_api_key_name, endpoint, session_id,
                    prompt_tokens, completion_tokens, total_tokens,
                    cache_read_tokens, cache_write_tokens, latency_ms,
                    status_code, success, estimated_cost_micros, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(api_key_id)
        .bind(route_id)
        .bind(provider_id)
        .bind(provider_api_key_id)
        .bind(model)
        .bind(provider_api_key_id.map(|_| "Primary"))
        .bind(endpoint)
        .bind(session_id)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(prompt_tokens + completion_tokens)
        .bind(cache_read)
        .bind(cache_write)
        .bind(latency_ms)
        .bind(status_code)
        .bind(success)
        .bind(cost_micros)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'Alpha', 'openai', 'https://alpha.example/v1'),
            (2, 'Beta', 'openai', 'https://beta.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, key_suffix) VALUES
            (1, 'Console', 'dimension-hash-1', 'sk-console', 'a1b2'),
            (2, 'CI', 'dimension-hash-2', 'sk-ci', 'c3d4')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy) VALUES
            (1, 'Chat', 'gpt-*', 'priority'),
            (2, 'Responses', 'claude-*', 'priority')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) VALUES
            (3, 1, 'Primary', 'dimension-alpha-key'),
            (4, 2, 'Primary', 'dimension-beta-key')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let base = Utc::now() - Duration::hours(1);
    let t1 = (base - Duration::minutes(30)).to_rfc3339();
    let t2 = base.to_rfc3339();
    let t3 = (base + Duration::minutes(10)).to_rfc3339();
    let t4 = (base + Duration::minutes(20)).to_rfc3339();
    insert_usage(
        &pool,
        "r1",
        Some(1),
        Some(1),
        Some(1),
        Some(3),
        "gpt-5.1",
        "/v1/chat/completions",
        Some("s1"),
        100,
        50,
        40,
        10,
        1000,
        200,
        1,
        Some(1000),
        &t1,
    )
    .await;
    insert_usage(
        &pool,
        "r2",
        Some(1),
        Some(1),
        Some(1),
        Some(3),
        "gpt-5.1",
        "/v1/chat/completions",
        Some("s1"),
        200,
        0,
        0,
        0,
        2000,
        500,
        0,
        None,
        &t2,
    )
    .await;
    insert_usage(
        &pool,
        "r3",
        Some(2),
        Some(2),
        Some(2),
        Some(4),
        "claude-sonnet-4",
        "/v1/responses",
        Some("s2"),
        100,
        100,
        50,
        0,
        3000,
        200,
        1,
        Some(2000),
        &t3,
    )
    .await;
    insert_usage(
        &pool,
        "r4",
        None,
        None,
        None,
        None,
        "local-model",
        "/v1/responses",
        None,
        50,
        0,
        0,
        0,
        500,
        200,
        1,
        None,
        &t4,
    )
    .await;

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let Json(dimensions) = analytics_dimensions(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(dimensions.api_key_usage.len(), 3);
    let console = dimensions
        .api_key_usage
        .iter()
        .find(|row| row.id == Some(1))
        .unwrap();
    assert_eq!(console.label, "Console");
    assert_eq!(console.requests, 2);
    assert_eq!(console.tokens, 350);
    assert_eq!(console.cache_read, 40);
    assert!((console.cache_hit_rate - 13.333_333).abs() < 0.001);
    assert!((console.success_rate - 50.0).abs() < 0.001);
    assert_eq!(console.cost_micros, Some(1000));

    assert_eq!(dimensions.provider_api_key_usage.len(), 3);
    let alpha_key = dimensions
        .provider_api_key_usage
        .iter()
        .find(|row| row.id == Some(3))
        .unwrap();
    assert_eq!(alpha_key.label, "Alpha · Primary");
    assert_eq!(alpha_key.requests, 2);
    assert!((alpha_key.cache_hit_rate - 13.333_333).abs() < 0.001);

    assert_eq!(dimensions.route_usage.len(), 3);
    let chat = dimensions
        .route_usage
        .iter()
        .find(|row| row.id == Some(1))
        .unwrap();
    assert_eq!(chat.label, "Chat");
    assert_eq!(chat.requests, 2);

    assert_eq!(dimensions.endpoint_usage.len(), 2);
    let responses = dimensions
        .endpoint_usage
        .iter()
        .find(|row| row.label == "/v1/responses")
        .unwrap();
    assert_eq!(responses.id, None);
    assert_eq!(responses.requests, 2);

    assert_eq!(dimensions.session_usage.len(), 2);
    let session_one = dimensions
        .session_usage
        .iter()
        .find(|row| row.session_id == "s1")
        .unwrap();
    assert_eq!(session_one.requests, 2);
    assert_eq!(session_one.last_seen, t2);
    assert!((session_one.cache_hit_rate - 13.333_333).abs() < 0.001);
    let session_two = dimensions
        .session_usage
        .iter()
        .find(|row| row.session_id == "s2")
        .unwrap();
    assert_eq!(session_two.requests, 1);
    assert_eq!(session_two.last_seen, t3);

    let Json(filtered) = analytics_dimensions(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            provider_id: Some(2),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(filtered.api_key_usage.len(), 1);
    assert_eq!(filtered.api_key_usage[0].id, Some(2));
    assert_eq!(filtered.route_usage.len(), 1);
    assert_eq!(filtered.route_usage[0].label, "Responses");
    assert_eq!(filtered.session_usage.len(), 1);
    assert_eq!(filtered.session_usage[0].session_id, "s2");
}
