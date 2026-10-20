use super::*;

#[test]
fn api_key_model_permissions_support_globs() {
    let key = ApiKeyRecord {
        id: 1,
        name: "scoped".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: Some(r#"["gpt-*","claude-sonnet-*"]"#.to_string()),
        expires_at: None,
        routing_policy: None,
    };
    assert!(enforce_api_key_model_access(Some(&key), "gpt-5.4").is_ok());
    assert!(enforce_api_key_model_access(Some(&key), "claude-sonnet-5").is_ok());
    assert!(matches!(
        enforce_api_key_model_access(Some(&key), "claude-opus-5"),
        Err(AppError::Forbidden(_))
    ));
    let malformed = ApiKeyRecord {
        allowed_models: Some("{".to_string()),
        ..key
    };
    assert!(matches!(
        enforce_api_key_model_access(Some(&malformed), "gpt-5.4"),
        Err(AppError::Forbidden(_))
    ));
}

#[tokio::test]
async fn public_model_list_respects_api_key_model_permissions() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Scoped', 'openai', 'https://example.com/v1', 'vendor/', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let raw_key = "sk-openllm-scoped-model-list";
    sqlx::query(
        "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-s', 'list', 1, '[\"vendor/gpt-*\"]')",
    )
    .bind(hash_secret(raw_key))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let mut openai_headers = HeaderMap::new();
    openai_headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
    );
    let uri: Uri = "/v1/models".parse().unwrap();
    let response = public_models_inner(&state, &openai_headers, &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    let ids = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["vendor/gpt-5"]);

    let response = public_model_inner(&state, &openai_headers, "vendor/gpt-5")
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
    assert_eq!(value["object"], "model");
    assert!(matches!(
        public_model_inner(&state, &openai_headers, "vendor/claude-4").await,
        Err(AppError::NotFound(_))
    ));

    let mut anthropic_headers = openai_headers.clone();
    anthropic_headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );
    let response = public_models_inner(&state, &anthropic_headers, &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    let ids = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["vendor/gpt-5"]);

    let response = public_model_inner(&state, &anthropic_headers, "vendor/gpt-5")
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
    assert_eq!(value["type"], "model");
}

#[tokio::test]
async fn model_retrieve_route_accepts_slashed_model_ids() {
    use axum::body::Body;
    use tower::ServiceExt;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Vendor', 'openai', 'https://example.com/v1', 'vendor/', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let response = crate::build_router(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/models/vendor/gpt-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
}

#[tokio::test]
async fn count_tokens_enforces_model_access_and_route_availability() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (name, provider_type, base_url, enabled)
             VALUES ('Scoped', 'anthropic', 'https://example.com', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let raw_key = "sk-openllm-count-tokens";
    sqlx::query(
        "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-c', 'kens', 1, '[\"gpt-*\"]')",
    )
    .bind(hash_secret(raw_key))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
    );

    let allowed =
        Bytes::from_static(br#"{"model":"gpt-5","messages":[{"role":"user","content":"hello"}]}"#);
    assert!(count_tokens_inner(&state, &headers, &allowed).await.is_ok());

    let forbidden = Bytes::from_static(br#"{"model":"claude-4","messages":[]}"#);
    assert!(matches!(
        count_tokens_inner(&state, &headers, &forbidden).await,
        Err(AppError::Forbidden(_))
    ));

    let missing = Bytes::from_static(br#"{"model":"gpt-missing","messages":[]}"#);
    assert!(matches!(
        count_tokens_inner(&state, &headers, &missing).await,
        Err(AppError::NotFound(_))
    ));
}
