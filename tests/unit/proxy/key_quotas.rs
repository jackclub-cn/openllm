use super::*;

#[tokio::test]
async fn api_key_daily_token_quota_blocks_after_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (api_key_id, total_tokens, created_at)
             VALUES (1, 10, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(10),
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool, None);
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
}

#[tokio::test]
async fn api_key_daily_cost_quota_uses_estimated_cost() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (api_key_id, total_tokens, estimated_cost_micros, created_at)
             VALUES (1, 1, 2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: Some(2),
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool, None);
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
}

#[tokio::test]
async fn api_key_requests_per_minute_quota_blocks_after_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'rate-limited', 'rate-hash', 'sk-openllm', 'test', 1, 2, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "rate-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: Some(2),
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool.clone(), None);
    for request_id in ["first", "second"] {
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            request_id,
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "third",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));
    let in_flight: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(in_flight, 2);
}

#[tokio::test]
async fn api_key_concurrency_quota_blocks_at_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'concurrency-hash', 'sk-openllm', 'test', 1, NULL, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "concurrency-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: Some(1),
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool.clone(), None);
    reserve_api_key_rate_limit(
        &state,
        Some(&key),
        "active",
        None,
        "model",
        "/v1/chat/completions",
        10,
        false,
    )
    .await
    .unwrap();
    assert!(matches!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "blocked",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));

    sqlx::query("UPDATE usage_logs SET in_flight = 0 WHERE request_id = 'active'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "next",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn api_key_without_limits_skips_quota_queries() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "unlimited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool, None);
    assert!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "unlimited",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn api_key_rate_limit_rejection_is_logged_without_extra_in_flight() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'reject-hash', 'sk-openllm', 'test', 1, NULL, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "concurrency-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: Some(1),
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool.clone(), None);
    reserve_api_key_rate_limit(
        &state,
        Some(&key),
        "active",
        None,
        "model",
        "/v1/chat/completions",
        10,
        false,
    )
    .await
    .unwrap();

    assert!(matches!(
        enforce_api_key_rate_limit_or_log(
            &state,
            Some(&key),
            "blocked",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
            Instant::now(),
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));
    let blocked: (i64, i64) = sqlx::query_as(
        "SELECT status_code, in_flight FROM usage_logs WHERE request_id = 'blocked'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(blocked, (429, 0));
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(active, 1);
}

#[tokio::test]
async fn api_key_daily_quota_ignores_in_flight_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                api_key_id, total_tokens, estimated_cost_micros, in_flight, created_at
             ) VALUES (1, 1000, 1000000, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(10),
        daily_cost_limit_micros: Some(1),
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool.clone(), None);

    assert!(
        enforce_api_key_daily_quota(&state, Some(&key))
            .await
            .is_ok()
    );

    sqlx::query("UPDATE usage_logs SET in_flight = 0")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
}

#[tokio::test]
async fn quota_rejection_is_logged_as_zero_usage() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE usage_logs (
                request_id TEXT NOT NULL UNIQUE,
                api_key_id INTEGER,
                route_id INTEGER,
                provider_id INTEGER,
                requested_model TEXT NOT NULL,
                upstream_model TEXT,
                endpoint TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                first_token_ms INTEGER,
                status_code INTEGER NOT NULL,
                in_flight INTEGER NOT NULL DEFAULT 0,
                success INTEGER NOT NULL,
                streamed INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                response_preview TEXT,
                context_breakdown_json TEXT,
                cache_usage_reported INTEGER,
                performance_json TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                total_tokens, status_code, success, created_at
             ) VALUES (
                'seed', 1, 'seed', '/v1/chat/completions', 1, 200, 1,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(1),
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
        routing_policy: None,
    };
    let state = AppState::new(pool.clone(), None);
    assert!(
        enforce_policy_or_log(
            &state,
            Some(&key),
            "rejected",
            None,
            "model",
            "/v1/chat/completions",
            false,
            Instant::now(),
        )
        .await
        .is_err()
    );
    let row: (i64, i64, Option<i64>, i64) = sqlx::query_as(
        "SELECT total_tokens, status_code, estimated_cost_micros, success \
             FROM usage_logs WHERE request_id = 'rejected'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (0, 429, Some(0), 0));
}
