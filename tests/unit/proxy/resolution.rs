use super::*;

#[tokio::test]
async fn route_resolution_failures_are_logged() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    let error = resolve_route_or_log(
        &state,
        None,
        "route-rejection",
        None,
        "missing-model",
        OPENAI_RESPONSES,
        false,
        Instant::now(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(error, AppError::NotFound(_)));

    let row: (String, i64, i64, String) = sqlx::query_as(
        "SELECT request_id, status_code, success, error_message
             FROM usage_logs WHERE request_id = 'route-rejection'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "route-rejection");
    assert_eq!(row.1, 404);
    assert_eq!(row.2, 0);
    assert!(row.3.contains("missing-model"));
}

#[tokio::test]
async fn prefix_matching_is_literal_and_case_sensitive() {
    // A pooled `sqlite::memory:` database gives each connection its own
    // empty database, so pin the pool to a single connection.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                timeout_seconds INTEGER,
                cooldown_seconds INTEGER,
                max_concurrency INTEGER,
                queue_timeout_seconds INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL,
                max_concurrency INTEGER,
                queue_timeout_seconds INTEGER
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'underscore', 'openai', 'http://upstream', 'a_b/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO provider_models (provider_id, model_name) VALUES (1, 'x')")
        .execute(&pool)
        .await
        .unwrap();

    let state = AppState::new(pool, None);

    // `_` must be treated literally, not as a single-character wildcard.
    let literal = find_prefixed_targets(&state, "a_b/x", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    assert_eq!(literal.len(), 1);
    assert_eq!(literal[0].upstream_model, "x");
    assert!(
        find_prefixed_targets(&state, "aXb/x", OPENAI_CHAT_COMPLETIONS)
            .await
            .is_err()
    );

    // Prefix comparison must stay case-sensitive so `A_B/x` cannot reach a
    // provider registered as `a_b`.
    assert!(
        find_prefixed_targets(&state, "A_B/x", OPENAI_CHAT_COMPLETIONS)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disabled_provider_model_is_skipped_by_explicit_routes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                timeout_seconds INTEGER,
                cooldown_seconds INTEGER,
                max_concurrency INTEGER,
                queue_timeout_seconds INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL,
                max_concurrency INTEGER,
                queue_timeout_seconds INTEGER
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE route_targets (
                id INTEGER PRIMARY KEY,
                route_id INTEGER,
                provider_id INTEGER NOT NULL,
                upstream_model TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 100,
                priority INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE routes (
                id INTEGER PRIMARY KEY,
                model_pattern TEXT NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_override INTEGER,
                output_override INTEGER,
                created_at TEXT NOT NULL DEFAULT ''
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'upstream', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'disabled-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (id, route_id, provider_id, upstream_model)
             VALUES (1, 1, 1, 'disabled-model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, model_pattern, name)
             VALUES (1, 'route-model', 'route-model')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    assert!(load_targets(&state, 1).await.unwrap().is_empty());
    assert!(
        crate::registry::route_models(&state.pool)
            .await
            .unwrap()
            .is_empty()
    );

    sqlx::query("UPDATE provider_models SET enabled = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(load_targets(&state, 1).await.unwrap().len(), 1);
    let routes = crate::registry::route_models(&state.pool).await.unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].target_count, 1);
}

#[test]
fn health_ranking_prefers_known_good_then_unknown_then_failed() {
    assert_eq!(provider_health_rank(Some(1)), 0);
    assert_eq!(provider_health_rank(None), 1);
    assert_eq!(provider_health_rank(Some(0)), 2);
    assert_eq!(provider_health_rank(Some(7)), 0);
}
