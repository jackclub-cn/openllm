use super::*;

/// A restart must not send traffic straight back to a provider that was still
/// cooling: the circuit and its cooldown deadline are restored from SQLite.
#[tokio::test]
async fn persisted_provider_circuit_survives_a_restart() {
    let (_directory, state) = restartable_circuit_state().await;
    let failure = StatusCode::SERVICE_UNAVAILABLE;
    for _ in 0..crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD {
        mark_provider_error(&state, 1, Some(failure), None, None).await;
    }
    crate::resilience_state::flush(&state).await.unwrap();
    assert_eq!(count_rows(&state, "provider_circuit_state").await, 1);

    let restarted = AppState::new(state.pool.clone(), None);
    let mut events = restarted.circuit_events.subscribe();
    assert_eq!(
        crate::resilience_state::restore(&restarted).await.unwrap(),
        1,
        "the provider circuit must be restored"
    );
    assert!(
        events.try_recv().is_err(),
        "restoring state is not a circuit transition and must not alert"
    );
    assert_eq!(
        restarted
            .provider_failure_streak
            .lock()
            .await
            .get(&1)
            .copied(),
        Some(crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD)
    );
    let retry_after = admit_provider_attempt_with_lease(&restarted, 1, PROVIDER_PROBE_LEASE)
        .await
        .expect_err("a restored open circuit must still short-circuit the attempt");
    assert!(
        retry_after >= Duration::from_secs(1),
        "the restored block must carry a Retry-After hint, got {retry_after:?}"
    );

    // The cooldown manager must show the restored remainder, or an operator
    // cannot tell an outage from a cooldown that survived the restart.
    let axum::Json(snapshot) = crate::api::list_cooldowns(axum::extract::State(restarted.clone()))
        .await
        .unwrap();
    let restored = snapshot
        .provider_cooldowns
        .iter()
        .find(|entry| entry.provider_id == 1)
        .expect("the restored cooldown must be listed");
    assert_eq!(restored.state, "open");
    assert!(restored.remaining_seconds > 0);
    assert_eq!(
        restored.failure_streak,
        crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD
    );
}

/// A cooldown that elapsed while the process was down must come back as
/// half-open, so a restart admits exactly one recovery probe instead of either
/// blocking the provider forever or stampeding it.
#[tokio::test]
async fn restored_expired_cooldown_stays_half_open() {
    let (_directory, state) = restartable_circuit_state().await;
    for _ in 0..crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD {
        mark_provider_error(&state, 1, Some(StatusCode::BAD_GATEWAY), None, None).await;
    }
    crate::resilience_state::flush(&state).await.unwrap();
    // Simulate the cooldown elapsing while nothing was running.
    sqlx::query(
        "UPDATE provider_circuit_state SET cooldown_until_unix_ms = 0 WHERE provider_id = 1",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let restarted = AppState::new(state.pool.clone(), None);
    assert_eq!(
        crate::resilience_state::restore(&restarted).await.unwrap(),
        1
    );
    assert!(
        admit_provider_attempt_with_lease(&restarted, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok(),
        "the first request after the restart probes recovery"
    );
    assert!(
        admit_provider_attempt_with_lease(&restarted, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_err(),
        "a second request must not stampede a half-open provider"
    );
}

/// Persisted state expires on its own: a cooldown whose deadline has passed is
/// dropped, and a half-open marker older than the restore window is treated as
/// stale rather than gating a provider that has long since recovered.
#[tokio::test]
async fn expired_persisted_state_is_dropped_and_purged() {
    let (_directory, state) = restartable_circuit_state().await;
    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    sqlx::query(
        "INSERT INTO provider_circuit_state
             (provider_id, failure_streak, cooldown_until_unix_ms, updated_at_unix_ms)
         VALUES (1, 3, ?, ?)",
    )
    .bind(now_unix_ms - 1_000)
    .bind(now_unix_ms - (crate::resilience_state::HALF_OPEN_RESTORE_TTL_SECS + 60) * 1000)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO target_cooldown_state (provider_id, model, cooldown_until_unix_ms)
         VALUES (1, 'stale-model', ?)",
    )
    .bind(now_unix_ms - 1_000)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_key_cooldown_state (provider_key_id, cooldown_until_unix_ms)
         VALUES (11, ?)",
    )
    .bind(now_unix_ms - 1_000)
    .execute(&state.pool)
    .await
    .unwrap();

    let restarted = AppState::new(state.pool.clone(), None);
    assert_eq!(
        crate::resilience_state::restore(&restarted).await.unwrap(),
        0,
        "expired rows must not be restored"
    );
    assert!(restarted.provider_failure_streak.lock().await.is_empty());
    assert!(restarted.provider_cooldown.lock().await.is_empty());
    assert!(restarted.target_cooldown.lock().await.is_empty());
    assert!(restarted.provider_key_cooldown.lock().await.is_empty());
    assert_eq!(count_rows(&restarted, "provider_circuit_state").await, 0);
    assert_eq!(count_rows(&restarted, "target_cooldown_state").await, 0);
    assert_eq!(
        count_rows(&restarted, "provider_key_cooldown_state").await,
        0
    );
}

/// Model and credential cooldowns are restored too, so a restart does not
/// re-hammer a model or a credential that was just isolated.
#[tokio::test]
async fn persisted_model_and_key_cooldowns_survive_a_restart() {
    let (_directory, state) = restartable_circuit_state().await;
    let target = endpoint_test_target("openai", None);
    mark_target_error(&state, &target, StatusCode::TOO_MANY_REQUESTS, None).await;
    mark_provider_api_key_error(
        &state,
        Some(11),
        StatusCode::TOO_MANY_REQUESTS,
        "busy",
        None,
    )
    .await;
    crate::resilience_state::flush(&state).await.unwrap();
    assert_eq!(count_rows(&state, "target_cooldown_state").await, 1);
    assert_eq!(count_rows(&state, "provider_key_cooldown_state").await, 1);

    let restarted = AppState::new(state.pool.clone(), None);
    assert_eq!(
        crate::resilience_state::restore(&restarted).await.unwrap(),
        2
    );
    assert!(
        restarted
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, target.upstream_model.clone()))
    );
    assert!(
        restarted
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&11)
    );

    // Recovery must clear the persisted mirror as well, so the next restart
    // does not resurrect a cooldown that a successful request lifted.
    mark_target_success(&restarted, 1, &target.upstream_model).await;
    mark_provider_api_key_success(&restarted, Some(11)).await;
    crate::resilience_state::flush(&restarted).await.unwrap();
    assert_eq!(count_rows(&restarted, "target_cooldown_state").await, 0);
    assert_eq!(
        count_rows(&restarted, "provider_key_cooldown_state").await,
        0
    );
}

/// An operator clearing cooldowns must also clear the persisted mirror; a
/// restart afterwards must not bring the cooldowns back.
#[tokio::test]
async fn cleared_cooldowns_do_not_come_back_after_a_restart() {
    let (_directory, state) = restartable_circuit_state().await;
    for _ in 0..crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD {
        mark_provider_error(&state, 1, Some(StatusCode::SERVICE_UNAVAILABLE), None, None).await;
    }
    let target = endpoint_test_target("openai", None);
    mark_target_error(&state, &target, StatusCode::TOO_MANY_REQUESTS, None).await;
    mark_provider_api_key_error(
        &state,
        Some(11),
        StatusCode::TOO_MANY_REQUESTS,
        "busy",
        None,
    )
    .await;
    crate::resilience_state::flush(&state).await.unwrap();

    let axum::Json(result) = crate::api::clear_cooldowns(
        axum::extract::State(state.clone()),
        axum::Json(crate::api::ClearCooldownsInput {
            all: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(result.cleared_provider_cooldowns > 0);
    crate::resilience_state::flush(&state).await.unwrap();
    assert_eq!(count_rows(&state, "provider_circuit_state").await, 0);
    assert_eq!(count_rows(&state, "target_cooldown_state").await, 0);
    assert_eq!(count_rows(&state, "provider_key_cooldown_state").await, 0);

    let restarted = AppState::new(state.pool.clone(), None);
    assert_eq!(
        crate::resilience_state::restore(&restarted).await.unwrap(),
        0
    );
    assert!(
        admit_provider_attempt_with_lease(&restarted, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok()
    );
}

/// Persistence is opt-out, and the parser has to fail safe: a typo must keep
/// the protection on rather than silently making restarts forget a trip.
#[test]
fn resilience_state_persistence_parsing_fails_safe() {
    use crate::resilience_state::parse_resilience_state_persist;
    assert!(parse_resilience_state_persist(None));
    assert!(parse_resilience_state_persist(Some("")));
    assert!(parse_resilience_state_persist(Some("oops")));
    assert!(parse_resilience_state_persist(Some("1")));
    for disabled in ["0", "false", "FALSE", " no ", "off"] {
        assert!(
            !parse_resilience_state_persist(Some(disabled)),
            "{disabled} must disable the mirror"
        );
    }
}

/// Deleting a provider while its state is dirty must clean both the row (via
/// the cascade) and the in-memory entry, instead of failing the whole flush.
#[tokio::test]
async fn deleting_a_provider_drops_its_persisted_state() {
    let (_directory, state) = restartable_circuit_state().await;
    for _ in 0..crate::state::DEFAULT_PROVIDER_OPEN_THRESHOLD {
        mark_provider_error(&state, 1, Some(StatusCode::SERVICE_UNAVAILABLE), None, None).await;
    }
    crate::resilience_state::flush(&state).await.unwrap();
    sqlx::query("DELETE FROM providers WHERE id = 1")
        .execute(&state.pool)
        .await
        .unwrap();
    // A later mutation marks a provider that no longer exists.
    state.mark_provider_resilience_dirty(1);
    crate::resilience_state::flush(&state).await.unwrap();
    assert_eq!(count_rows(&state, "provider_circuit_state").await, 0);
    assert!(state.provider_failure_streak.lock().await.is_empty());
    assert!(state.provider_cooldown.lock().await.is_empty());
}
