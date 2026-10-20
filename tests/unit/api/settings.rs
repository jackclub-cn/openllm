use super::*;

#[tokio::test]
async fn guardrail_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    let Json(updated) = update_guardrails_settings(
        State(state.clone()),
        Json(GuardrailSettings {
            blocked_terms: vec![
                " secret ".to_string(),
                "SECRET".to_string(),
                "api-key".to_string(),
            ],
            max_prompt_tokens: Some(4096),
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.blocked_terms, vec!["secret", "api-key"]);
    assert_eq!(updated.max_prompt_tokens, Some(4096));

    let Json(loaded) = get_guardrails_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let error = update_guardrails_settings(
        State(state.clone()),
        Json(GuardrailSettings {
            blocked_terms: vec!["x".to_string()],
            max_prompt_tokens: None,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    let Json(cleared) =
        update_guardrails_settings(State(state.clone()), Json(GuardrailSettings::default()))
            .await
            .unwrap();
    assert!(cleared.is_empty());
    let Json(loaded) = get_guardrails_settings(State(state)).await.unwrap();
    assert!(loaded.is_empty());
}

#[tokio::test]
async fn inspector_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    let Json(updated) = update_inspector_settings(
        State(state.clone()),
        Json(InspectorSettings {
            capture_request_previews: true,
            request_preview_max_chars: 8192,
        }),
    )
    .await
    .unwrap();
    assert!(updated.capture_request_previews);
    assert_eq!(updated.request_preview_max_chars, 8192);

    let Json(loaded) = get_inspector_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let error = update_inspector_settings(
        State(state.clone()),
        Json(InspectorSettings {
            capture_request_previews: true,
            request_preview_max_chars: 64,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    let Json(defaults) =
        update_inspector_settings(State(state.clone()), Json(InspectorSettings::default()))
            .await
            .unwrap();
    assert_eq!(defaults, InspectorSettings::default());
}

#[tokio::test]
async fn resilience_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    assert_eq!(
        state.resilience_settings().await.unwrap(),
        ResilienceSettings::default()
    );

    let Json(updated) = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: true,
            stream_recovery_max_retries: 3,
            max_retries: 3,
            retry_backoff_ms: 250,
            retry_max_backoff_ms: 4_000,
        }),
    )
    .await
    .unwrap();
    assert!(updated.stream_recovery_enabled);
    assert_eq!(updated.stream_recovery_max_retries, 3);
    assert_eq!(updated.max_retries, 3);
    // The stale cache must be invalidated so the new policy takes effect.
    assert_eq!(state.resilience_settings().await.unwrap(), updated);
    let Json(loaded) = get_resilience_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let too_many = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            max_retries: 9,
            ..ResilienceSettings::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(too_many, AppError::BadRequest(_)));

    let inverted = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: false,
            stream_recovery_max_retries: 2,
            max_retries: 2,
            retry_backoff_ms: 5_000,
            retry_max_backoff_ms: 100,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(inverted, AppError::BadRequest(_)));

    let out_of_range = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: true,
            stream_recovery_max_retries: STREAM_RECOVERY_MAX_RETRIES + 1,
            ..ResilienceSettings::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(out_of_range, AppError::BadRequest(_)));

    // Turning recovery off drops its budget back to the default so the stored
    // row does not keep a stale value for a disabled feature.
    let Json(off) = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: false,
            stream_recovery_max_retries: 4,
            max_retries: 1,
            retry_backoff_ms: 200,
            retry_max_backoff_ms: 2_000,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        off.stream_recovery_max_retries,
        ResilienceSettings::default().stream_recovery_max_retries
    );
    assert!(!off.stream_recovery_enabled);

    // Disabling retries falls back to the default backoff values and clears the
    // stored row so a fresh start uses defaults.
    let Json(cleared) =
        update_resilience_settings(State(state.clone()), Json(ResilienceSettings::default()))
            .await
            .unwrap();
    assert_eq!(cleared, ResilienceSettings::default());
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'resilience'")
            .fetch_optional(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, None);
}
