use super::*;

#[tokio::test]
async fn provider_api_key_pool_supports_create_update_and_clear() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "Primary", Some("sk-one"), true),
                provider_key_input(None, "Backup", Some("sk-two"), true),
            ],
        )),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys.len(), 2);
    assert_eq!(created.api_keys[0].name, "Primary");
    assert_eq!(created.api_keys[0].api_key_suffix, "-one");
    assert_eq!(created.api_keys[1].api_key_suffix, "-two");

    let first_id = created.api_keys[0].id;
    state.provider_key_cooldown.lock().await.insert(
        first_id,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![
            provider_key_input(Some(first_id), "Primary", None, false),
            provider_key_input(None, "Replacement", Some("sk-three"), true),
        ]))),
    )
    .await
    .unwrap();
    assert_eq!(updated.api_keys.len(), 2);
    assert!(!updated.api_keys[0].enabled);
    assert_eq!(updated.api_keys[1].name, "Replacement");
    assert!(
        state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&first_id),
        "editing metadata without replacing the secret must preserve the key cooldown"
    );

    let retained_secret: String =
        sqlx::query_scalar("SELECT secret FROM provider_api_keys WHERE id = ?")
            .bind(first_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(retained_secret, "sk-one");
    let mirrored: Option<String> = sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(mirrored.as_deref(), Some("sk-three"));

    let Json(cleared) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(Vec::new()))),
    )
    .await
    .unwrap();
    assert!(cleared.api_keys.is_empty());
    assert!(!cleared.api_key_set);
    assert!(
        !state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&first_id),
        "removing a key must clear its runtime cooldown"
    );
    let mirrored: Option<String> = sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(mirrored, None);
}

#[tokio::test]
async fn provider_api_key_channel_limits_round_trip_through_the_api() {
    let state = provider_key_test_state().await;
    let mut primary = provider_key_input(None, "Primary", Some("sk-limited"), true);
    primary.max_concurrency = Some(3);
    primary.requests_per_minute = Some(120);
    primary.queue_limit = Some(10);
    primary.queue_wait_ms = Some(1_500);
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(None, vec![primary])),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys[0].max_concurrency, Some(3));
    assert_eq!(created.api_keys[0].requests_per_minute, Some(120));
    assert_eq!(created.api_keys[0].queue_limit, Some(10));
    assert_eq!(created.api_keys[0].queue_wait_ms, Some(1_500));

    let mut updated_key = provider_key_input(Some(created.api_keys[0].id), "Primary", None, true);
    updated_key.max_concurrency = Some(5);
    updated_key.requests_per_minute = Some(240);
    updated_key.queue_limit = Some(0);
    updated_key.queue_wait_ms = Some(0);
    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![updated_key]))),
    )
    .await
    .unwrap();
    assert_eq!(updated.api_keys[0].max_concurrency, Some(5));
    assert_eq!(updated.api_keys[0].requests_per_minute, Some(240));
    assert_eq!(updated.api_keys[0].queue_limit, Some(0));
    assert_eq!(updated.api_keys[0].queue_wait_ms, Some(0));
}

#[tokio::test]
async fn provider_api_key_rotation_resets_stale_health_state() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![provider_key_input(None, "Primary", Some("sk-old"), true)],
        )),
    )
    .await
    .unwrap();
    let key_id = created.api_keys[0].id;
    sqlx::query(
        "UPDATE provider_api_keys \
             SET last_used_at = '2026-01-01T00:00:00Z', \
                 last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale 401', \
                 last_test_at = '2026-01-01T00:00:00Z', last_test_ok = 0, \
                 last_test_latency_ms = 42, last_test_checked = 'inference', \
                 last_test_message = 'old failure' \
             WHERE id = ?",
    )
    .bind(key_id)
    .execute(&state.pool)
    .await
    .unwrap();
    state.provider_key_cooldown.lock().await.insert(
        key_id,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );

    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![provider_key_input(
            Some(key_id),
            "Primary",
            Some("sk-rotated"),
            true,
        )]))),
    )
    .await
    .unwrap();

    assert_eq!(updated.api_keys[0].api_key_suffix, "ated");
    assert_eq!(updated.api_keys[0].last_test_ok, None);
    assert_eq!(updated.api_keys[0].last_error, None);
    assert!(
        !state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&key_id)
    );
    let stale_fields: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_api_keys \
             WHERE id = ? AND ( \
                 last_used_at IS NOT NULL OR last_error_at IS NOT NULL OR last_error IS NOT NULL \
                 OR last_test_at IS NOT NULL OR last_test_ok IS NOT NULL \
                 OR last_test_latency_ms IS NOT NULL OR last_test_checked IS NOT NULL \
                 OR last_test_message IS NOT NULL \
             )",
    )
    .bind(key_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(stale_fields, 0);
}

#[tokio::test]
async fn provider_api_key_pool_rejects_duplicate_secrets() {
    let state = provider_key_test_state().await;
    let error = create_provider(
        State(state),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "One", Some("sk-same"), true),
                provider_key_input(None, "Two", Some("sk-same"), true),
            ],
        )),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("must be unique"));
}

#[tokio::test]
async fn provider_api_key_pool_accepts_legacy_single_key_input() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state),
        Json(provider_input(Some("sk-legacy"), Vec::new())),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys.len(), 1);
    assert_eq!(created.api_keys[0].name, "Default");
    assert_eq!(created.api_keys[0].api_key_suffix, "gacy");
}
