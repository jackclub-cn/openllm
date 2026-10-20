use super::*;

#[tokio::test]
async fn model_limit_overrides_survive_resync() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
             VALUES (1, 'CallAI', 'openai', 'https://sub.callai.one/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, input_limit,
                output_limit, context_override, input_override, output_override,
                supported_endpoints_override, cost, cost_input_override,
                cost_output_override
             ) VALUES
                (1, 'gpt-6-astra', 0, 1050000, 922000, 128000, 400000, NULL, 64000,
                 '[\"/responses\"]', '{\"input\":2,\"output\":10,\"cache_read\":0.2}', 1.5, 12.0),
                (1, 'gpt-6-luna', 1, 1050000, 922000, 128000, 400000, 400000, 64000,
                 NULL, NULL, NULL, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    replace_provider_models(
        &mut tx,
        1,
        &[
            (
                "gpt-6-astra".to_string(),
                UpstreamModelInfo {
                    context_limit: Some(1_050_000),
                    supported_endpoints: vec!["/chat/completions".to_string()],
                    ..Default::default()
                },
            ),
            (
                "gpt-6-luna".to_string(),
                UpstreamModelInfo {
                    context_limit: Some(1_050_000),
                    supported_endpoints: vec![
                        "/chat/completions".to_string(),
                        "/responses".to_string(),
                    ],
                    ..Default::default()
                },
            ),
        ],
        None,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = AppState::new(pool, None);
    let limits = provider_model_limits(&state, 1).await.unwrap();
    assert_eq!(limits.len(), 2);
    let astra = limits
        .iter()
        .find(|model| model.model_name == "gpt-6-astra")
        .unwrap();
    assert!(!astra.enabled);
    assert_eq!(astra.context_limit, Some(400_000));
    assert_eq!(astra.input_limit, Some(400_000));
    assert_eq!(astra.output_limit, Some(64_000));
    assert_eq!(astra.context_override, Some(400_000));
    assert_eq!(astra.supported_endpoints, vec!["/responses"]);
    assert_eq!(
        astra.supported_endpoints_override,
        Some(vec!["/responses".to_string()])
    );
    assert_eq!(astra.cost_input, Some(1.5));
    assert_eq!(astra.cost_output, Some(12.0));
    assert_eq!(astra.cost_input_override, Some(1.5));
    assert_eq!(astra.cost_output_override, Some(12.0));

    let luna = limits
        .iter()
        .find(|model| model.model_name == "gpt-6-luna")
        .unwrap();
    assert_eq!(
        luna.supported_endpoints,
        vec!["/chat/completions", "/responses"]
    );
    assert_eq!(luna.supported_endpoints_override, None);

    let models = crate::registry::synced_models(&state.pool).await.unwrap();
    let model = models
        .iter()
        .find(|model| model.upstream_model == "gpt-6-luna")
        .unwrap();
    assert_eq!(models.len(), 1);
    let capabilities = model.capabilities.as_ref().unwrap();
    assert_eq!(capabilities.context_limit, Some(400_000));
    assert_eq!(capabilities.input_limit, Some(400_000));
    assert_eq!(capabilities.output_limit, Some(64_000));
    // The model declares chat + responses, so every message protocol is
    // reachable through translation.
    assert_eq!(
        model.supported_endpoints,
        Some(vec![
            "/chat/completions".to_string(),
            "/completions".to_string(),
            "/messages".to_string(),
            "/responses".to_string()
        ])
    );
}

#[tokio::test]
async fn model_inventory_exposes_effective_limits_and_prices() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled) \
             VALUES (1, 'Inventory', 'openai', 'https://inventory.example/v1', 'inv/', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, input_limit, output_limit,
                context_override, input_override, output_override, supported_endpoints,
                supported_endpoints_override, cost, cost_input_override
             ) VALUES
                (1, 'model-a', 1, 100000, 90000, 8000, 50000, NULL, 4000,
                 '[\"/chat/completions\"]', NULL, '{\"input\":1,\"output\":2}', 1.5)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let Json(rows) = list_model_inventory(State(state)).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.provider_id, 1);
    assert_eq!(row.provider_name, "Inventory");
    assert!(row.provider_enabled);
    assert_eq!(row.model_prefix, "inv/");
    assert_eq!(row.model_name, "model-a");
    assert_eq!(row.context_limit, Some(50_000));
    assert_eq!(row.input_limit, Some(50_000));
    assert_eq!(row.output_limit, Some(4_000));
    assert_eq!(row.supported_endpoints, vec!["/chat/completions"]);
    // The catalog also reports what the gateway serves, which includes the
    // endpoints reachable through protocol translation.
    assert_eq!(
        row.served_endpoints,
        vec![
            "/chat/completions",
            "/completions",
            "/messages",
            "/responses"
        ]
    );
    assert_eq!(row.cost_input, Some(1.5));
    assert_eq!(row.cost_output, Some(2.0));
}

#[tokio::test]
async fn updates_model_cost_overrides() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Priced', 'openai', 'https://priced.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let Json(rows) = update_provider_model_limits(
        State(state),
        Path(1),
        Json(ProviderModelLimitsUpdate {
            models: vec![ProviderModelLimitInput {
                model_name: "model".to_string(),
                enabled: true,
                supported_endpoints_override: None,
                context_limit: None,
                input_limit: None,
                output_limit: None,
                max_concurrency: Some(4),
                queue_timeout_seconds: Some(0),
                cost_input_override: Some(1.25),
                cost_output_override: Some(5.0),
                cost_cache_read_override: Some(0.1),
                cost_cache_write_override: Some(2.0),
            }],
        }),
    )
    .await
    .unwrap();

    assert_eq!(rows[0].cost_input, Some(1.25));
    assert_eq!(rows[0].cost_output, Some(5.0));
    assert_eq!(rows[0].cost_cache_read, Some(0.1));
    assert_eq!(rows[0].cost_cache_write, Some(2.0));
    assert_eq!(rows[0].cost_input_override, Some(1.25));
    assert_eq!(rows[0].max_concurrency, Some(4));
    assert_eq!(rows[0].queue_timeout_seconds, Some(0));
}
