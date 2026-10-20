use super::*;

#[tokio::test]
async fn slow_success_deprioritizes_only_latency_optimized_routes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix, enabled
             ) VALUES
                (1, 'Slow', 'openai', 'https://slow.example/v1', '', 1),
                (2, 'Healthy', 'openai', 'https://healthy.example/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut slow = endpoint_test_target("openai", None);
    slow.id = 1;
    slow.provider_id = 1;
    slow.priority = 0;
    slow.provider_health = Some(1);
    let mut healthy = endpoint_test_target("openai", None);
    healthy.id = 2;
    healthy.provider_id = 2;
    healthy.priority = 10;
    healthy.provider_health = None;

    assert_eq!(
        order_targets(
            &state,
            1,
            "priority",
            vec![slow.clone(), healthy.clone()],
            None,
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|target| target.provider_id)
        .collect::<Vec<_>>(),
        vec![1, 2]
    );

    record_target_latency_success(&state, 1, "model", Some(60_000)).await;
    // A fixed strategy keeps the order the operator configured: the recent
    // first-token signal belongs to the latency-first strategy alone.
    assert_eq!(
        order_targets(
            &state,
            1,
            "priority",
            vec![slow.clone(), healthy.clone()],
            None,
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|target| target.provider_id)
        .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        order_targets(
            &state,
            1,
            "latency_optimized",
            vec![slow, healthy],
            None,
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|target| target.provider_id)
        .collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[tokio::test]
async fn slow_success_keeps_the_only_target_routable() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix, enabled
             ) VALUES
                (1, 'Slow', 'openai', 'https://slow.example/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let target = endpoint_test_target("openai", None);

    record_target_latency_success(&state, 1, "model", Some(60_000)).await;
    let ordered = order_targets(&state, 1, "latency_optimized", vec![target], None, None)
        .await
        .unwrap();
    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].provider_id, 1);
}

#[tokio::test]
async fn non_streaming_latency_is_not_treated_as_a_slow_first_token() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix, enabled
             ) VALUES
                (1, 'Slow', 'openai', 'https://slow.example/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);

    log_usage(
        &state,
        UsageLogEntry {
            request_id: "non-streaming-slow-latency",
            api_key_id: None,
            route_id: None,
            provider_id: Some(1),
            requested_model: "model",
            upstream_model: Some("model"),
            endpoint: OPENAI_CHAT_COMPLETIONS,
            usage: Usage::new(10, 5),
            latency_ms: 60_000,
            first_token_ms: Some(60_000),
            status_code: 200,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: None,
        },
    )
    .await;

    assert!(
        state.target_slow.lock().await.is_empty(),
        "a buffered response has no real first-token measurement"
    );
    let status: i64 = sqlx::query_scalar("SELECT status_code FROM usage_logs WHERE request_id = ?")
        .bind("non-streaming-slow-latency")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, 200);
}
