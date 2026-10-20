use super::*;

#[tokio::test]
async fn quota_paced_orders_known_states_and_keeps_a_sole_exhausted_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let target = |id| {
        let mut target = endpoint_test_target("openai", None);
        target.id = id;
        target.provider_id = id;
        target
    };
    let exhausted = target(1);
    let healthy = target(2);
    let low = target(3);
    let unknown = target(4);
    insert_test_quota_snapshot(
        &state,
        1,
        None,
        "model",
        Some(100.0),
        Some(100.0),
        Some(0.0),
        Some(chrono::Duration::hours(1)),
    )
    .await;
    insert_test_quota_snapshot(
        &state,
        2,
        None,
        "model",
        Some(10.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(4)),
    )
    .await;
    insert_test_quota_snapshot(
        &state,
        3,
        None,
        "model",
        Some(95.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(4)),
    )
    .await;

    let ordered = order_targets(
        &state,
        1,
        "quota_paced",
        vec![exhausted.clone(), healthy, low, unknown],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![2, 4, 3],
        "healthy, unknown and low pressure should remain while exhaustion is skipped"
    );

    let sole = order_targets(&state, 1, "quota_paced", vec![exhausted], None, None)
        .await
        .unwrap();
    assert_eq!(sole.len(), 1);
    assert_eq!(sole[0].provider_id, 1);
}

#[tokio::test]
async fn quota_paced_prefers_the_earlier_reset_when_remaining_is_equal() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let target = |id| {
        let mut target = endpoint_test_target("openai", None);
        target.id = id;
        target.provider_id = id;
        target
    };
    insert_test_quota_snapshot(
        &state,
        1,
        None,
        "model",
        Some(50.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(1)),
    )
    .await;
    insert_test_quota_snapshot(
        &state,
        2,
        None,
        "model",
        Some(50.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(4)),
    )
    .await;

    let ordered = order_targets(
        &state,
        1,
        "quota_paced",
        vec![target(2), target(1)],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn quota_paced_keeps_model_level_exhaustion_scoped() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let mut exhausted = endpoint_test_target("openai", None);
    exhausted.id = 1;
    exhausted.provider_id = 1;
    exhausted.upstream_model = "model-a".to_string();
    let mut healthy = endpoint_test_target("openai", None);
    healthy.id = 2;
    healthy.provider_id = 1;
    healthy.upstream_model = "model-b".to_string();
    insert_test_quota_snapshot(
        &state,
        1,
        None,
        "model-a",
        Some(100.0),
        Some(100.0),
        Some(0.0),
        Some(chrono::Duration::hours(1)),
    )
    .await;
    insert_test_quota_snapshot(
        &state,
        1,
        None,
        "model-b",
        Some(5.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(1)),
    )
    .await;

    let ordered = order_targets(
        &state,
        1,
        "quota_paced",
        vec![exhausted, healthy],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.upstream_model)
            .collect::<Vec<_>>(),
        vec!["model-b"]
    );
}

#[tokio::test]
async fn quota_paced_applies_key_snapshots_after_key_expansion() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Quota', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
         VALUES (11, 1, 'Exhausted', 'sk-exhausted', 1),
                (12, 1, 'Healthy', 'sk-healthy', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    insert_test_quota_snapshot(
        &state,
        1,
        Some(11),
        "model",
        Some(100.0),
        Some(100.0),
        Some(0.0),
        Some(chrono::Duration::hours(1)),
    )
    .await;
    insert_test_quota_snapshot(
        &state,
        1,
        Some(12),
        "model",
        Some(10.0),
        Some(100.0),
        None,
        Some(chrono::Duration::hours(1)),
    )
    .await;

    let ordered = order_targets(
        &state,
        1,
        "quota_paced",
        vec![endpoint_test_target("openai", None)],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].provider_api_key_id, Some(12));
}
