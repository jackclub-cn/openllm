use super::*;

#[test]
fn validates_usage_retention_days() {
    assert_eq!(normalize_retention_days(None).unwrap(), None);
    assert_eq!(normalize_retention_days(Some(0)).unwrap(), None);
    assert_eq!(normalize_retention_days(Some(30)).unwrap(), Some(30));
    assert_eq!(normalize_retention_days(Some(3650)).unwrap(), Some(3650));
    assert!(normalize_retention_days(Some(-1)).is_err());
    assert!(normalize_retention_days(Some(3651)).is_err());
}

#[tokio::test]
async fn settings_expose_database_stats() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(settings) = get_settings(State(state)).await.unwrap();

    assert_eq!(settings.database, "sqlite");
    assert!(settings.database_stats.size_bytes > 0);
    assert_eq!(settings.database_stats.providers, 1);
    assert_eq!(settings.database_stats.provider_api_keys, 1);
    assert_eq!(settings.database_stats.audit_logs, 0);
    assert_eq!(settings.database_stats.in_flight_requests, 0);
}

#[tokio::test]
async fn vacuum_reports_reclaimed_space() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(result) = vacuum_database(State(state)).await.unwrap();

    assert!(result.reclaimed_bytes >= 0);
    assert_eq!(result.database_stats.providers, 1);
    assert_eq!(result.database_stats.in_flight_requests, 0);
}

#[tokio::test]
async fn background_database_writer_slots_are_bounded() {
    let state = provider_key_test_state().await;
    let mut permits = Vec::new();
    for _ in 0..crate::state::MAX_BACKGROUND_DB_TASKS {
        permits.push(state.acquire_background_db_slot().await);
    }

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            state.acquire_background_db_slot(),
        )
        .await
        .is_err()
    );

    permits.pop();
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state.acquire_background_db_slot(),
    )
    .await
    .expect("releasing one writer slot must wake a waiter");
    drop(permit);
}

#[tokio::test]
async fn backup_database_streams_a_sqlite_copy() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&source_path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let response = backup_database(State(state)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.sqlite3"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.starts_with(b"SQLite format 3\0"));
}

#[tokio::test]
async fn automatic_usage_retention_deletes_only_expired_logs() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('usage_retention_days', '30')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_capability_profiles (
            provider_id, model, endpoint, capability, supported, source,
            confidence, profile_version, observed_at, valid_until
         ) VALUES (
            1, 'old-model', '/v1/responses', 'tool_search', 0, 'runtime',
            'exact', 'runtime-v1', '2000-01-01T00:00:00Z', '2000-01-01T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_quota_snapshots (
            provider_id, model, window_key, unit, source, confidence,
            observed_at, valid_until
         ) VALUES (
            1, 'old-model', '5h', 'request', 'runtime_429', 'inferred',
            '2000-01-01T00:00:00Z', '2000-01-01T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let old = (Utc::now() - Duration::days(31)).to_rfc3339();
    let recent = (Utc::now() - Duration::days(2)).to_rfc3339();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, success, created_at
             ) VALUES (?, ?, '/v1/chat/completions', 200, 1, ?)",
    )
    .bind("expired")
    .bind("test-model")
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, success, created_at
             ) VALUES (?, ?, '/v1/chat/completions', 200, 1, ?)",
    )
    .bind("recent")
    .bind("test-model")
    .bind(recent)
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    state.quota_snapshots.lock().await.insert(
        ProviderQuotaSnapshotKey {
            provider_id: 1,
            provider_key_id: None,
            model: "old-model".to_string(),
            window_key: "5h".to_string(),
            unit: "request".to_string(),
            source: "runtime_429".to_string(),
        },
        ProviderQuotaSnapshotRecord {
            provider_id: 1,
            provider_key_id: None,
            model: "old-model".to_string(),
            window_key: "5h".to_string(),
            limit: None,
            used: None,
            remaining: Some(0.0),
            unit: "request".to_string(),
            reset_at: None,
            source: "runtime_429".to_string(),
            confidence: "inferred".to_string(),
            observed_at: "2000-01-01T00:00:00Z".to_string(),
            valid_until: "2000-01-01T00:00:00Z".to_string(),
        },
    );
    state
        .capability_profiles
        .lock()
        .await
        .record(crate::capabilities::CapabilityRecord {
            key: crate::capabilities::CapabilityKey {
                provider_id: 1,
                model: "old-model".to_string(),
                endpoint: "/v1/responses".to_string(),
                capability: crate::capabilities::CAPABILITY_TOOL_SEARCH.to_string(),
            },
            supported: false,
            source: crate::capabilities::SOURCE_RUNTIME.to_string(),
            confidence: "exact".to_string(),
            profile_version: "runtime-v1".to_string(),
            observed_at: "2000-01-01T00:00:00Z".to_string(),
            valid_until: "2000-01-01T00:00:00Z".to_string(),
        });
    assert!(state.quota_snapshots.lock().await.len() == 1);
    assert_eq!(state.capability_profiles.lock().await.len(), 0);
    run_due_usage_retention(state.clone()).await;

    assert!(state.quota_snapshots.lock().await.is_empty());
    assert_eq!(state.capability_profiles.lock().await.len(), 0);
    let request_ids =
        sqlx::query_scalar::<_, String>("SELECT request_id FROM usage_logs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(request_ids, vec!["recent"]);
    let expired_capabilities: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_capability_profiles")
            .fetch_one(&pool)
            .await
            .unwrap();
    let expired_quota_snapshots: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_quota_snapshots")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(expired_capabilities, 0);
    assert_eq!(expired_quota_snapshots, 0);
}

#[tokio::test]
async fn usage_completed_indexes_are_migrated() {
    let state = provider_key_test_state().await;
    let indexes = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master \
             WHERE type = 'index' AND name LIKE 'idx_usage_completed_%' \
             ORDER BY name",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();

    assert_eq!(
        indexes,
        vec![
            "idx_usage_completed_created",
            "idx_usage_completed_endpoint_created",
            "idx_usage_completed_model_created",
            "idx_usage_completed_provider_created",
            "idx_usage_completed_provider_key_created",
            "idx_usage_completed_route_created",
            "idx_usage_completed_success_created",
        ]
    );
}
