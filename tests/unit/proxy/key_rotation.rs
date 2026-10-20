use super::*;

#[tokio::test]
async fn provider_keys_are_sticky_per_session_without_advancing_rotation() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Sticky', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1),
                    (13, 1, 'Third', 'sk-third', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;
    let keys = |targets: Vec<RouteTarget>| {
        targets
            .into_iter()
            .map(|target| target.api_key.unwrap())
            .collect::<Vec<_>>()
    };

    let first = keys(
        order_targets(
            &state,
            1,
            "priority",
            vec![target.clone()],
            Some("session-a"),
            None,
        )
        .await
        .unwrap(),
    );
    let repeated = keys(
        order_targets(
            &state,
            1,
            "priority",
            vec![target.clone()],
            Some("session-a"),
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(first, repeated);

    let no_session_first = keys(
        order_targets(&state, 1, "priority", vec![target.clone()], None, None)
            .await
            .unwrap(),
    );
    let no_session_second = keys(
        order_targets(&state, 1, "priority", vec![target], None, None)
            .await
            .unwrap(),
    );
    assert_eq!(no_session_first[0], "sk-first");
    assert_eq!(no_session_second[0], "sk-second");
}

#[tokio::test]
async fn provider_keys_rotate_and_expand_a_route_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Rotating', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
             VALUES (1, 'First', 'sk-first', 1), (1, 'Second', 'sk-second', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;

    let first = order_targets(&state, 1, "priority", vec![target.clone()], None, None)
        .await
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-first"), Some("sk-second")]
    );
    assert!(first[0].auth_retryable);
    assert!(!first[1].auth_retryable);
    assert!(
        first
            .iter()
            .all(|target| target.provider_api_key_id.is_some())
    );

    let second = order_targets(&state, 1, "priority", vec![target], None, None)
        .await
        .unwrap();
    assert_eq!(
        second
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-second"), Some("sk-first")]
    );
}

#[tokio::test]
async fn provider_keys_skip_cooling_candidates_when_alternatives_exist() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Cooling', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;

    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, Instant::now() + Duration::from_secs(60));
    let available = order_targets(&state, 1, "priority", vec![target.clone()], None, None)
        .await
        .unwrap();
    assert_eq!(
        available
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-second")]
    );

    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(12, Instant::now() + Duration::from_secs(60));
    let fallback = order_targets(&state, 1, "priority", vec![target], None, None)
        .await
        .unwrap();
    assert_eq!(fallback.len(), 2);
    assert!(
        fallback
            .iter()
            .all(|target| target.provider_api_key_id.is_some())
    );
}
