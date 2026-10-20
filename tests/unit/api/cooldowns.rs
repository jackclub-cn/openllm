use super::*;

#[tokio::test]
async fn cooldown_manager_lists_and_clears_scoped_cooldowns() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'alpha', 'openai', 'https://alpha.example/v1'),
            (2, 'beta', 'openai', 'https://beta.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) \
         VALUES (11, 1, 'main', 'sk-main')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let now = std::time::Instant::now();
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, now + std::time::Duration::from_secs(60));
    state.provider_failure_streak.lock().await.insert(1, 3);
    state.target_cooldown.lock().await.insert(
        (1, "model-a".to_string()),
        now + std::time::Duration::from_secs(30),
    );
    state.target_cooldown.lock().await.insert(
        (2, "model-b".to_string()),
        now + std::time::Duration::from_secs(30),
    );
    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, now + std::time::Duration::from_secs(45));
    state.provider_failure_meta.lock().await.insert(
        1,
        crate::state::FailureMetadata {
            scope: "provider",
            retry: "next_target",
            confidence: "inferred",
            reason_code: "upstream_unavailable",
            observed_at: now,
        },
    );
    state.target_failure_meta.lock().await.insert(
        (1, "model-a".to_string()),
        crate::state::FailureMetadata {
            scope: "model",
            retry: "next_target",
            confidence: "exact",
            reason_code: "context_limit_exceeded",
            observed_at: now,
        },
    );
    state.provider_key_failure_meta.lock().await.insert(
        11,
        crate::state::FailureMetadata {
            scope: "credential",
            retry: "next_target",
            confidence: "exact",
            reason_code: "quota_exhausted",
            observed_at: now,
        },
    );
    // An already-expired entry must not leak into the snapshot.
    state.target_cooldown.lock().await.insert(
        (2, "stale".to_string()),
        now - std::time::Duration::from_secs(5),
    );

    let Json(snapshot) = list_cooldowns(State(state.clone())).await.unwrap();
    assert_eq!(snapshot.provider_cooldowns.len(), 1);
    assert_eq!(
        snapshot.provider_cooldowns[0].provider_name.as_deref(),
        Some("alpha")
    );
    assert_eq!(snapshot.provider_cooldowns[0].failure_streak, 3);
    assert_eq!(snapshot.provider_cooldowns[0].remaining_seconds, 60);
    assert_eq!(snapshot.provider_cooldowns[0].state, "open");
    assert_eq!(snapshot.provider_cooldowns[0].scope, Some("provider"));
    assert_eq!(snapshot.provider_cooldowns[0].retry, Some("next_target"));
    assert_eq!(
        snapshot.provider_cooldowns[0].reason_code,
        Some("upstream_unavailable")
    );
    assert_eq!(snapshot.model_cooldowns.len(), 2);
    assert_eq!(snapshot.model_cooldowns[0].scope, Some("model"));
    assert_eq!(
        snapshot.model_cooldowns[0].reason_code,
        Some("context_limit_exceeded")
    );
    assert_eq!(snapshot.provider_key_cooldowns.len(), 1);
    assert_eq!(
        snapshot.provider_key_cooldowns[0].key_name.as_deref(),
        Some("main")
    );
    assert_eq!(
        snapshot.provider_key_cooldowns[0].provider_name.as_deref(),
        Some("alpha")
    );
    assert_eq!(snapshot.provider_key_cooldowns[0].scope, Some("credential"));
    assert_eq!(
        snapshot.provider_key_cooldowns[0].reason_code,
        Some("quota_exhausted")
    );

    // An empty body is rejected rather than silently clearing everything.
    let error = clear_cooldowns(State(state.clone()), Json(ClearCooldownsInput::default()))
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    // Clearing one model leaves the sibling model and the provider circuit alone.
    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_id: Some(1),
            model: Some("model-a".to_string()),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_model_cooldowns, 1);
    assert_eq!(cleared.cleared_provider_cooldowns, 0);
    assert!(
        !state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "model-a".to_string()))
    );
    assert!(
        !state
            .target_failure_meta
            .lock()
            .await
            .contains_key(&(1, "model-a".to_string()))
    );
    assert!(state.provider_cooldown.lock().await.contains_key(&1));

    // A provider-scoped clear also resets the escalation streak.
    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_id: Some(1),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_provider_cooldowns, 1);
    assert!(!state.provider_cooldown.lock().await.contains_key(&1));
    assert!(!state.provider_failure_meta.lock().await.contains_key(&1));
    assert!(!state.provider_failure_streak.lock().await.contains_key(&1));

    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_key_id: Some(11),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_provider_key_cooldowns, 1);
    assert!(!state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(
        !state
            .provider_key_failure_meta
            .lock()
            .await
            .contains_key(&11)
    );

    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            all: true,
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_model_cooldowns, 1);
    let Json(snapshot) = list_cooldowns(State(state.clone())).await.unwrap();
    assert!(snapshot.model_cooldowns.is_empty());
    assert!(snapshot.provider_cooldowns.is_empty());
    assert!(snapshot.provider_key_cooldowns.is_empty());
}

/// A provider whose cooldown elapsed but whose failure streak still trips the
/// breaker is half-open: no active cooldown, yet it must not be invisible to an
/// operator wondering why it keeps failing fast.
#[tokio::test]
async fn cooldown_snapshot_surfaces_half_open_circuits() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let now = std::time::Instant::now();
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, now - std::time::Duration::from_secs(5));

    let Json(snapshot) = list_cooldowns(State(state)).await.unwrap();
    assert_eq!(snapshot.provider_cooldowns.len(), 1);
    assert_eq!(snapshot.provider_cooldowns[0].state, "half_open");
    assert_eq!(snapshot.provider_cooldowns[0].remaining_seconds, 0);
    assert_eq!(
        snapshot.provider_open_threshold,
        crate::state::provider_open_threshold()
    );
}

/// The provider list and detail views must surface the circuit breaker state so
/// the console can show why a provider is short-circuited, not just that its
/// cooldown has some seconds left.
#[tokio::test]
async fn provider_views_expose_the_circuit_state() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    assert_eq!(providers[0].circuit_state, "closed");

    let threshold = crate::state::provider_open_threshold();
    if threshold > 0 {
        state
            .provider_failure_streak
            .lock()
            .await
            .insert(1, threshold);
        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let Json(providers) = list_providers(State(state.clone())).await.unwrap();
        assert_eq!(providers[0].circuit_state, "open");
        assert_eq!(providers[0].cooldown_seconds, Some(60));

        // Once the cooldown elapses the provider is half-open, not healthy.
        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let Json(providers) = list_providers(State(state.clone())).await.unwrap();
        assert_eq!(providers[0].circuit_state, "half_open");
        assert!(providers[0].cooldown_seconds.is_none());

        // The single-provider hydration path agrees with the batched one.
        let single = get_provider(&state, 1).await.unwrap();
        assert_eq!(single.circuit_state, "half_open");
    }
}
