use super::*;

#[tokio::test]
async fn route_diagnosis_explains_each_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'embeddings-only', 'openai', 'http://embeddings-only', 1, 1),
                (2, 'responses', 'openai', 'http://responses', 1, 1),
                (3, 'disabled', 'openai', 'http://disabled', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints
             ) VALUES
                (1, 'model', '[\"/embeddings\"]'),
                (2, 'model', '[\"/chat/completions\",\"/responses\"]'),
                (3, 'model', '[\"/responses\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 0, 1),
                (2, 1, 2, 'model', 1, 1),
                (3, 1, 3, 'model', 2, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let diagnosis = diagnose_route(&state, "shared", OPENAI_RESPONSES, None)
        .await
        .unwrap();
    assert!(diagnosis.matched);
    assert!(diagnosis.resolved);
    assert_eq!(diagnosis.match_type, "explicit_route");
    assert_eq!(diagnosis.targets.len(), 3);
    assert!(!diagnosis.targets[0].eligible);
    assert!(diagnosis.targets[0].reason.contains("does not declare"));
    assert!(diagnosis.targets[1].eligible);
    assert!(!diagnosis.targets[2].eligible);
    assert!(diagnosis.targets[2].reason.contains("provider is disabled"));
    assert!(diagnosis.runtime_targets.is_none());
}

#[tokio::test]
async fn route_diagnosis_returns_sticky_runtime_order_and_key_names() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'primary', 'openai', 'http://primary', 1, 1),
                (2, 'secondary', 'openai', 'http://secondary', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model'), (2, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'primary-a', 'sk-a', 1),
                    (12, 1, 'primary-b', 'sk-b', 1),
                    (21, 2, 'secondary-a', 'sk-c', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'sticky route', 'shared', 'weighted', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, weight, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 1, 0, 1),
                (2, 1, 2, 'model', 100, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let first = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let repeated = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();

    let first_runtime = first.runtime_targets.as_ref().unwrap();
    let repeated_runtime = repeated.runtime_targets.as_ref().unwrap();
    assert_eq!(first.session_id.as_deref(), Some("session-a"));
    assert_eq!(first_runtime.len(), 3);
    assert_eq!(
        first_runtime
            .iter()
            .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
            .collect::<Vec<_>>(),
        repeated_runtime
            .iter()
            .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
            .collect::<Vec<_>>()
    );
    assert!(
        first_runtime
            .iter()
            .any(|target| target.provider_api_key_name.as_deref() == Some("primary-a"))
    );
    assert!(
        first_runtime
            .iter()
            .all(|target| target.order > 0 && target.order <= first_runtime.len())
    );
}

#[tokio::test]
async fn route_diagnosis_explains_advanced_strategy_metrics() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'cheap', 'openai', 'http://cheap', 1, 1),
                (2, 'fast', 'openai', 'http://fast', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, cost)
             VALUES
                (1, 'model', '{\"input\":0.5,\"output\":1}'),
                (2, 'model', '{\"input\":5,\"output\":10}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (request_id, provider_id, latency) in [
        ("cheap-1", 1, 2_400),
        ("cheap-2", 1, 2_500),
        ("cheap-3", 1, 2_600),
        ("cheap-4", 1, 2_500),
        ("fast-1", 2, 500),
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
    sqlx::query(
        "INSERT INTO routes (
                id, name, model_pattern, strategy, strategy_ext, enabled
             ) VALUES (1, 'smart route', 'shared', 'priority', 'cost_optimized', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, weight, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 100, 10, 1),
                (2, 1, 2, 'model', 100, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let cost = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let cost_targets = cost.runtime_targets.as_ref().unwrap();
    assert_eq!(cost_targets[0].provider_id, 1);
    assert_eq!(cost_targets[0].decision_reason, "lowest_known_cost");
    assert_eq!(cost_targets[0].input_cost_per_million, Some(0.5));
    assert_eq!(cost_targets[0].output_cost_per_million, Some(1.0));
    assert_eq!(cost_targets[0].recent_requests, Some(4));

    sqlx::query("UPDATE routes SET strategy_ext = 'latency_optimized' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let latency = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let latency_targets = latency.runtime_targets.as_ref().unwrap();
    assert_eq!(latency_targets[0].provider_id, 2);
    assert_eq!(latency_targets[0].decision_reason, "lowest_recent_latency");
    assert_eq!(latency_targets[0].avg_latency_ms, Some(500.0));
    assert_eq!(latency_targets[0].recent_requests, Some(1));
}

#[tokio::test]
async fn disabled_route_does_not_hide_direct_prefix_diagnosis() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'prefixed', 'openai', 'http://prefixed', 'vendor/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'disabled exact', 'vendor/model', 'priority', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let diagnosis = diagnose_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    assert!(diagnosis.matched);
    assert!(diagnosis.resolved);
    assert_eq!(diagnosis.match_type, "prefix");
    assert_eq!(diagnosis.route_id, None);
}
