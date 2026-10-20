use super::*;

#[tokio::test]
async fn provider_request_policy_round_trips_and_can_be_cleared() {
    let state = provider_key_test_state().await;
    let mut input = provider_input(None, Vec::new());
    input.timeout_seconds = Some(180);
    input.configured_cooldown_seconds = Some(600);
    input.max_concurrency = Some(3);
    input.queue_timeout_seconds = Some(0);
    let (_, Json(created)) = create_provider(State(state.clone()), Json(input))
        .await
        .unwrap();
    assert_eq!(created.timeout_seconds, Some(180));
    assert_eq!(created.configured_cooldown_seconds, Some(600));
    assert_eq!(created.max_concurrency, Some(3));
    assert_eq!(created.queue_timeout_seconds, Some(0));
    assert_eq!(created.cooldown_seconds, None);
    state.provider_concurrency.lock().await.insert(
        created.id,
        std::sync::Arc::new(tokio::sync::Semaphore::new(3)),
    );

    let mut update = provider_update_with_keys(None);
    update.timeout_seconds = Some(0);
    update.configured_cooldown_seconds = Some(0);
    update.max_concurrency = Some(0);
    update.queue_timeout_seconds = Some(0);
    let Json(updated) = update_provider(State(state.clone()), Path(created.id), Json(update))
        .await
        .unwrap();
    assert_eq!(updated.timeout_seconds, None);
    assert_eq!(updated.configured_cooldown_seconds, None);
    assert_eq!(updated.max_concurrency, None);
    assert_eq!(updated.queue_timeout_seconds, Some(0));
    assert!(
        !state
            .provider_concurrency
            .lock()
            .await
            .contains_key(&created.id)
    );
}

#[tokio::test]
async fn provider_update_resets_tool_search_probe_only_when_context_changes() {
    let state = provider_key_test_state().await;
    sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url,
                tool_search_supported, tool_search_checked_at
             ) VALUES (1, 'Provider', 'openai', 'https://example.com/v1', 0, '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

    let mut status_update = provider_update_with_keys(None);
    status_update.enabled = Some(false);
    let _ = update_provider(State(state.clone()), Path(1), Json(status_update))
        .await
        .unwrap();
    let (supported, checked_at): (i64, Option<String>) = sqlx::query_as(
        "SELECT tool_search_supported, tool_search_checked_at FROM providers WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(supported, 0);
    assert_eq!(checked_at.as_deref(), Some("2026-01-01T00:00:00Z"));

    let mut context_update = provider_update_with_keys(None);
    context_update.headers = Some(json!({"X-Test": "changed"}));
    let _ = update_provider(State(state.clone()), Path(1), Json(context_update))
        .await
        .unwrap();
    let (supported, checked_at): (i64, Option<String>) = sqlx::query_as(
        "SELECT tool_search_supported, tool_search_checked_at FROM providers WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(supported, 1);
    assert_eq!(checked_at, None);
}
