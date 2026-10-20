use super::*;

async fn capability_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'provider-1', 'openai', 'http://provider-1'),
            (2, 'provider-2', 'openai', 'http://provider-2')",
    )
    .execute(&pool)
    .await
    .unwrap();
    AppState::new(pool, None)
}

#[tokio::test]
async fn capability_profiles_are_scoped_prioritized_and_expire() {
    let state = capability_state().await;
    record_capability(
        &state,
        CapabilityKey {
            provider_id: 1,
            model: "model-a".to_string(),
            endpoint: "/v1/responses".to_string(),
            capability: CAPABILITY_REASONING_HISTORY.to_string(),
        },
        true,
        SOURCE_RUNTIME,
        "exact",
    )
    .await;
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-a",
            "/v1/responses",
            CAPABILITY_REASONING_HISTORY
        )
        .await,
        Some(true)
    );
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-b",
            "/v1/responses",
            CAPABILITY_REASONING_HISTORY
        )
        .await,
        None,
        "a model-scoped learning result must not leak to a sibling model"
    );
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-a",
            "/v1/chat/completions",
            CAPABILITY_REASONING_HISTORY
        )
        .await,
        None,
        "an endpoint-scoped learning result must not leak to another endpoint"
    );
    assert_eq!(
        capability_support(
            &state,
            2,
            "model-a",
            "/v1/responses",
            CAPABILITY_REASONING_HISTORY
        )
        .await,
        None,
        "a provider-scoped learning result must not leak to another provider"
    );

    record_capability(
        &state,
        CapabilityKey {
            provider_id: 1,
            model: "model-a".to_string(),
            endpoint: "/v1/responses".to_string(),
            capability: CAPABILITY_STRICT_TOOLS.to_string(),
        },
        false,
        SOURCE_RUNTIME,
        "exact",
    )
    .await;
    record_capability(
        &state,
        CapabilityKey {
            provider_id: 1,
            model: "model-a".to_string(),
            endpoint: "/v1/responses".to_string(),
            capability: CAPABILITY_STRICT_TOOLS.to_string(),
        },
        true,
        SOURCE_TEMPLATE,
        "exact",
    )
    .await;
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-a",
            "/v1/responses",
            CAPABILITY_STRICT_TOOLS
        )
        .await,
        Some(true),
        "template evidence should outrank a runtime inference"
    );

    record_capability(
        &state,
        CapabilityKey {
            provider_id: 1,
            model: String::new(),
            endpoint: String::new(),
            capability: CAPABILITY_TOOL_SEARCH.to_string(),
        },
        false,
        SOURCE_RUNTIME,
        "exact",
    )
    .await;
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-a",
            "/v1/chat/completions",
            CAPABILITY_TOOL_SEARCH
        )
        .await,
        Some(false),
        "a provider-wide profile should be the fallback for every model"
    );

    sqlx::query(
        "UPDATE provider_capability_profiles SET valid_until = '2000-01-01T00:00:00Z' \
         WHERE provider_id = 1 AND capability = ?",
    )
    .bind(CAPABILITY_REASONING_HISTORY)
    .execute(&state.pool)
    .await
    .unwrap();
    restore(&state).await.unwrap();
    assert_eq!(
        capability_support(
            &state,
            1,
            "model-a",
            "/v1/responses",
            CAPABILITY_REASONING_HISTORY
        )
        .await,
        None,
        "expired runtime evidence must be ignored"
    );
}
