use super::*;

#[tokio::test]
async fn provider_list_batches_models_and_key_statistics() {
    let state = provider_key_test_state().await;
    for (id, name) in [(1, "First"), (2, "Second")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (provider_id, model_name) in [(1, "alpha"), (1, "beta"), (2, "gamma")] {
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
                 VALUES (?, ?, 1)",
        )
        .bind(provider_id)
        .bind(model_name)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (id, provider_id, name) in [(11, 1, "Primary"), (12, 1, "Backup"), (21, 2, "Only")] {
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
                 VALUES (?, ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(provider_id)
        .bind(name)
        .bind(format!("sk-{id}"))
        .execute(&state.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, requested_model,
                endpoint, prompt_tokens, completion_tokens, total_tokens,
                latency_ms, status_code, success, in_flight
             ) VALUES (
                'batched-provider-list', 1, 11, 'alpha',
                '/v1/chat/completions', 10, 5, 15,
                100, 200, 1, 0
             )",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    assert_eq!(providers.len(), 2);
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    assert_eq!(first.models, vec!["alpha", "beta"]);
    assert_eq!(first.api_keys.len(), 2);
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 1);
    assert_eq!(primary.success_rate, 100.0);
    assert_eq!(primary.avg_latency_ms, 100.0);
    assert_eq!(primary.prompt_tokens, 10);
    assert_eq!(primary.completion_tokens, 5);

    sqlx::query(
        "UPDATE usage_logs
             SET success = 0, latency_ms = 250,
                 prompt_tokens = 20, completion_tokens = 10, total_tokens = 30
             WHERE request_id = 'batched-provider-list'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 1);
    assert_eq!(primary.success_rate, 0.0);
    assert_eq!(primary.avg_latency_ms, 250.0);
    assert_eq!(primary.prompt_tokens, 20);
    assert_eq!(primary.completion_tokens, 10);

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'batched-provider-list'")
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(providers) = list_providers(State(state)).await.unwrap();
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 0);
    assert_eq!(primary.prompt_tokens, 0);
    assert_eq!(primary.completion_tokens, 0);

    let second = providers.iter().find(|provider| provider.id == 2).unwrap();
    assert_eq!(second.models, vec!["gamma"]);
    assert_eq!(second.api_keys[0].requests, 0);
}
