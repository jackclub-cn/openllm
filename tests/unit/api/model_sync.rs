use super::*;

#[test]
fn builds_model_sync_preview() {
    let row = |enabled: i64| ProviderModelPreviewRow {
        model_name: String::new(),
        enabled,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        supported_endpoints: None,
        cost: None,
        display_name: None,
    };
    let entries = vec![
        ("new-model".to_string(), UpstreamModelInfo::default()),
        ("kept-model".to_string(), UpstreamModelInfo::default()),
        ("disabled-model".to_string(), UpstreamModelInfo::default()),
    ];
    let existing = HashMap::from([
        ("kept-model".to_string(), row(1)),
        ("disabled-model".to_string(), row(0)),
        ("removed-model".to_string(), row(1)),
    ]);
    let changed = vec![ModelSyncChange {
        model_name: "kept-model".to_string(),
        fields: vec!["context_limit".to_string()],
    }];
    let preview = build_model_sync_preview(7, &entries, &existing, changed);
    assert_eq!(preview.provider_id, 7);
    assert_eq!(preview.added, vec!["new-model"]);
    assert_eq!(preview.removed, vec!["removed-model"]);
    assert_eq!(preview.changed[0].model_name, "kept-model");
    assert_eq!(preview.retained, 2);
    assert_eq!(preview.disabled_retained, 1);
}

#[test]
fn detects_metadata_changes_during_sync_preview() {
    let entries = vec![(
        "model".to_string(),
        UpstreamModelInfo {
            context_limit: Some(128_000),
            supported_endpoints: vec!["/responses".to_string()],
            display_name: Some("Model".to_string()),
        },
    )];
    let existing = HashMap::from([(
        "model".to_string(),
        ProviderModelPreviewRow {
            model_name: "model".to_string(),
            enabled: 1,
            context_limit: Some(64_000),
            input_limit: Some(64_000),
            output_limit: Some(8_000),
            supported_endpoints: Some(r#"["/chat/completions"]"#.to_string()),
            cost: Some(r#"{"input":1.0}"#.to_string()),
            display_name: None,
        },
    )]);

    let changed = detect_model_sync_changes(&entries, &existing, None, None);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].model_name, "model");
    assert!(changed[0].fields.contains(&"context_limit".to_string()));
    assert!(
        changed[0]
            .fields
            .contains(&"supported_endpoints".to_string())
    );
    assert!(changed[0].fields.contains(&"cost".to_string()));
    assert!(changed[0].fields.contains(&"display_name".to_string()));
}

#[tokio::test]
async fn automatic_model_sync_adds_only_matching_models_and_preserves_manual_state() {
    let upstream = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async {
            (
                StatusCode::OK,
                Json(json!({
                    "data": [
                        {"id": "gpt-5"},
                        {"id": "deepseek-v3"},
                        {"id": "not-selected"}
                    ]
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_auto_include_rules) \
         VALUES (1, 'auto-include', 'openai', ?, ?)",
    )
    .bind(format!("http://{address}/v1"))
    .bind(r#"["gpt-*","re:^deepseek-"]"#)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) VALUES \
         (1, 'manual-model', 1), \
         (1, 'disabled-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let result = sync_provider_auto(state, 1).await.unwrap();
    assert_eq!(result.count, 2);

    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT model_name, enabled FROM provider_models WHERE provider_id = 1 ORDER BY model_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("deepseek-v3".to_string(), 1),
            ("disabled-model".to_string(), 0),
            ("gpt-5".to_string(), 1),
            ("manual-model".to_string(), 1),
        ]
    );

    server.abort();
}

#[tokio::test]
async fn automatic_model_sync_failure_keeps_the_previous_catalog() {
    let upstream = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_auto_include_rules) \
         VALUES (1, 'auto-include-failure', 'openai', ?, '[\"gpt-*\"]')",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) \
         VALUES (1, 'existing-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    assert!(sync_provider_auto(state, 1).await.is_err());
    let models = sqlx::query_scalar::<_, String>(
        "SELECT model_name FROM provider_models WHERE provider_id = 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(models, vec!["existing-model"]);
    let error: Option<String> =
        sqlx::query_scalar("SELECT models_sync_error FROM providers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(error.is_some());

    server.abort();
}

#[tokio::test]
async fn provider_model_sync_rejects_duplicate_runs() {
    let state = provider_key_test_state().await;
    state.provider_model_sync.lock().await.insert(1);

    let error = sync_provider(state, 1).await.unwrap_err();
    assert!(
        matches!(error, AppError::Conflict(message) if message.contains("already in progress"))
    );
}

#[tokio::test]
async fn reconciles_interrupted_provider_model_syncs() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url,
                models_sync_attempted_at, models_synced_at, models_sync_error
             ) VALUES
                (1, 'interrupted', 'openai', 'https://example.com/v1',
                 '2026-10-01T10:00:00Z', '2026-10-01T09:00:00Z', NULL),
                (2, 'completed', 'openai', 'https://example.com/v1',
                 '2026-10-01T09:00:00Z', '2026-10-01T10:00:00Z', NULL),
                (3, 'failed', 'openai', 'https://example.com/v1',
                 '2026-10-01T10:00:00Z', NULL, 'upstream failed')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    assert_eq!(
        reconcile_interrupted_provider_model_syncs(&state)
            .await
            .unwrap(),
        1
    );
    let error: Option<String> =
        sqlx::query_scalar("SELECT models_sync_error FROM providers WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        error.as_deref(),
        Some("gateway restarted before model synchronization completed")
    );
    let completed_error: Option<String> =
        sqlx::query_scalar("SELECT models_sync_error FROM providers WHERE id = 2")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(completed_error.is_none());
}
