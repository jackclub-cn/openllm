use super::*;

#[tokio::test]
async fn cost_optimized_orders_known_prices_before_unknown_targets() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let mut expensive = endpoint_test_target("openai", None);
    expensive.id = 1;
    expensive.provider_id = 1;
    expensive.cost = Some(json!({"input": 10.0, "output": 30.0}).to_string());
    let mut cheap = endpoint_test_target("openai", None);
    cheap.id = 2;
    cheap.provider_id = 2;
    cheap.cost = Some(json!({"input": 1.0, "output": 2.0}).to_string());
    let mut unknown = endpoint_test_target("openai", None);
    unknown.id = 3;
    unknown.provider_id = 3;

    let ordered = order_targets(
        &state,
        1,
        "cost_optimized",
        vec![expensive, unknown, cheap],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        vec![2, 1, 3]
    );
}

#[tokio::test]
async fn latency_optimized_prefers_recent_successful_samples() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    for (id, name) in [(1, "Fast"), (2, "Slow"), (3, "New")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (request_id, provider_id, latency) in [
        ("fast-1", 1, 400),
        ("fast-2", 1, 600),
        ("slow-1", 2, 1_200),
        ("slow-2", 2, 1_400),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', ?, 200, 1, 0)",
        )
        .bind(request_id)
        .bind(provider_id)
        .bind(latency)
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = AppState::new(pool, None);
    let targets = (1..=3)
        .map(|id| {
            let mut target = endpoint_test_target("openai", None);
            target.id = id;
            target.provider_id = id;
            target.upstream_model = "model".to_string();
            target
        })
        .collect::<Vec<_>>();

    let ordered = order_targets(&state, 1, "latency_optimized", targets, None, None)
        .await
        .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn p95_latency_optimized_prefers_a_better_tail_over_a_lower_average() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    for (id, name) in [(1, "Spiky"), (2, "Steady")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for index in 0..20 {
        let (provider_id, latency) = if index < 18 {
            (1, 100)
        } else if index < 20 {
            (1, 5_000)
        } else {
            (2, 700)
        };
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', ?, 200, 1, 0)",
        )
        .bind(format!("spiky-{index}"))
        .bind(provider_id)
        .bind(latency)
        .execute(&pool)
        .await
        .unwrap();
    }
    for index in 0..20 {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, 2, 'model', 'model', '/v1/chat/completions', 700, 200, 1, 0)",
        )
        .bind(format!("steady-{index}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = AppState::new(pool, None);
    refresh_runtime_target_p95(&state).await.unwrap();
    let targets = (1..=2)
        .map(|id| {
            let mut target = endpoint_test_target("openai", None);
            target.id = id;
            target.provider_id = id;
            target.upstream_model = "model".to_string();
            target
        })
        .collect::<Vec<_>>();

    let average = order_targets(&state, 1, "latency_optimized", targets.clone(), None, None)
        .await
        .unwrap();
    assert_eq!(
        average[0].provider_id, 1,
        "the spiky provider still has the better average"
    );

    let p95 = order_targets(&state, 1, "p95_latency_optimized", targets, None, None)
        .await
        .unwrap();
    assert_eq!(
        p95[0].provider_id, 2,
        "P95 routing must avoid the provider with 5s tail samples"
    );
}

#[tokio::test]
async fn least_used_prefers_the_target_with_less_recent_traffic() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    for (id, name) in [(1, "Busy"), (2, "Quiet")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (request_id, provider_id) in [("busy-1", 1), ("busy-2", 1), ("busy-3", 1), ("quiet-1", 2)] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', 500, 200, 1, 0)",
        )
        .bind(request_id)
        .bind(provider_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = AppState::new(pool, None);
    let mut busy = endpoint_test_target("openai", None);
    busy.id = 1;
    busy.provider_id = 1;
    let mut quiet = endpoint_test_target("openai", None);
    quiet.id = 2;
    quiet.provider_id = 2;

    let ordered = order_targets(&state, 1, "least_used", vec![busy, quiet], None, None)
        .await
        .unwrap();
    assert_eq!(ordered[0].provider_id, 2);
}
