use super::*;

#[test]
fn filters_targets_by_upstream_supported_endpoints() {
    let openai = endpoint_test_target("openai", Some(r#"["/chat/completions", "/responses"]"#));
    assert!(target_supports_endpoint(&openai, OPENAI_CHAT_COMPLETIONS));
    assert!(target_supports_endpoint(&openai, OPENAI_RESPONSES));
    assert!(!target_supports_endpoint(&openai, "/v1/embeddings"));

    let no_leading_slash = endpoint_test_target("openai", Some(r#"["chat/completions"]"#));
    assert!(target_supports_endpoint(
        &no_leading_slash,
        OPENAI_CHAT_COMPLETIONS
    ));

    let anthropic = endpoint_test_target("anthropic", Some(r#"["/messages"]"#));
    assert!(target_supports_endpoint(
        &anthropic,
        OPENAI_CHAT_COMPLETIONS
    ));
    assert!(target_supports_endpoint(&anthropic, ANTHROPIC_MESSAGES));
    // Anthropic targets serve Responses requests by translating them to
    // the Messages API, so they advertise Responses support too.
    assert!(target_supports_endpoint(&anthropic, OPENAI_RESPONSES));

    let unknown = endpoint_test_target("custom", None);
    assert!(target_supports_endpoint(&unknown, "/v1/embeddings"));

    let filtered =
        filter_targets_for_endpoint(vec![openai.clone(), anthropic.clone()], OPENAI_RESPONSES);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].provider_type, "openai");
    assert_eq!(filtered[1].provider_type, "anthropic");

    // Anthropic targets also serve the legacy completions endpoint.
    assert!(target_supports_endpoint(&anthropic, OPENAI_COMPLETIONS));
    let filtered =
        filter_targets_for_endpoint(vec![openai.clone(), anthropic.clone()], OPENAI_COMPLETIONS);
    // Both qualify: the OpenAI target falls back to chat, Anthropic to
    // Messages.
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].provider_type, "openai");
    assert_eq!(filtered[1].provider_type, "anthropic");
    assert_eq!(
        target_upstream_endpoint(&openai, OPENAI_COMPLETIONS),
        Some(OPENAI_CHAT_COMPLETIONS)
    );

    // A chat-only OpenAI-compatible target still advertises Responses
    // support because the gateway translates the request on the way out.
    let chat_only = endpoint_test_target("openai", Some(r#"["/chat/completions"]"#));
    assert!(target_supports_endpoint(&chat_only, OPENAI_RESPONSES));
    assert_eq!(
        target_upstream_endpoint(&chat_only, OPENAI_RESPONSES),
        Some(OPENAI_CHAT_COMPLETIONS)
    );
    // A target that genuinely supports Responses keeps the passthrough.
    assert_eq!(
        target_upstream_endpoint(&openai, OPENAI_RESPONSES),
        Some(OPENAI_RESPONSES)
    );

    // Conversely, a Responses-only target can serve chat callers.
    let responses_only = endpoint_test_target("openai", Some(r#"["/responses"]"#));
    assert!(target_supports_endpoint(
        &responses_only,
        OPENAI_CHAT_COMPLETIONS
    ));
    assert_eq!(
        target_upstream_endpoint(&responses_only, OPENAI_CHAT_COMPLETIONS),
        Some(OPENAI_RESPONSES)
    );
    // Anthropic callers can also reach a Responses-only target.
    assert!(target_supports_endpoint(
        &responses_only,
        ANTHROPIC_MESSAGES
    ));
    assert_eq!(
        target_upstream_endpoint(&responses_only, ANTHROPIC_MESSAGES),
        Some(OPENAI_RESPONSES)
    );
}

#[tokio::test]
async fn route_resolution_ignores_models_that_do_not_support_the_requested_endpoint() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
                (1, 'embeddings-only', 'openai', 'http://embeddings-only'),
                (2, 'responses', 'openai', 'http://responses')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'shared', '[\"/embeddings\"]'),
                (2, 'shared', '[\"/chat/completions\",\"/responses\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let resolved = resolve_route(&state, "shared", OPENAI_RESPONSES)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);
    assert_eq!(resolved.targets[0].provider_id, 2);

    // The embeddings-only provider is excluded from chat completions even
    // though it advertises a different OpenAI-compatible endpoint.
    let resolved = resolve_route(&state, "shared", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);
    assert_eq!(resolved.targets[0].provider_id, 2);
}

#[tokio::test]
async fn endpoint_override_controls_routing_and_public_metadata() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'override', 'openai', 'http://override', 'vendor/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints,
                supported_endpoints_override
             ) VALUES (
                1, 'model', '[\"/chat/completions\"]', '[\"/responses\"]'
             )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    // The override drops native chat support, but chat callers are still
    // served by translating to Responses.
    assert_eq!(
        resolve_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS)
            .await
            .unwrap()
            .targets
            .len(),
        1
    );
    // Endpoints the override does not imply stay unroutable.
    assert!(
        resolve_route(&state, "vendor/model", "/v1/embeddings")
            .await
            .is_err()
    );
    let resolved = resolve_route(&state, "vendor/model", OPENAI_RESPONSES)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);

    let models = crate::registry::synced_models(&state.pool).await.unwrap();
    // The override leaves the target Responses-only, so chat, completions
    // and Anthropic callers are all served through translation.
    assert_eq!(
        models[0].supported_endpoints,
        Some(vec![
            "/chat/completions".to_string(),
            "/completions".to_string(),
            "/messages".to_string(),
            "/responses".to_string(),
        ])
    );

    let uri: Uri = "/v1/models".parse().unwrap();
    let response = public_models_inner(&state, &HeaderMap::new(), &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["data"][0]["supported_endpoints"],
        json!([
            "/chat/completions",
            "/completions",
            "/messages",
            "/responses"
        ])
    );
}
