use super::*;

#[test]
fn validates_api_key_daily_limits() {
    assert_eq!(normalize_api_key_limit("token", None).unwrap(), None);
    assert_eq!(normalize_api_key_limit("token", Some(0)).unwrap(), None);
    assert_eq!(
        normalize_api_key_limit("token", Some(10_000)).unwrap(),
        Some(10_000)
    );
    assert!(normalize_api_key_limit("token", Some(-1)).is_err());
}

#[tokio::test]
async fn persists_and_reports_api_key_rate_limits() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    let (_, Json(created)) = create_api_key(
        State(state.clone()),
        Json(ApiKeyInput {
            name: "limited".to_string(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: Some(120),
            max_concurrency: Some(5),
            allowed_models: None,
            expires_at: None,
            routing_policy: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(created.item.requests_per_minute, Some(120));
    assert_eq!(created.item.max_concurrency, Some(5));

    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                status_code, in_flight, success, created_at
             ) VALUES (
                'active', ?, 'model', '/v1/chat/completions',
                0, 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .bind(created.item.id)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                prompt_tokens, completion_tokens, total_tokens,
                status_code, in_flight, success, created_at
             ) VALUES (
                'completed', ?, 'model', '/v1/chat/completions',
                100, 25, 125, 200, 0, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .bind(created.item.id)
    .execute(&pool)
    .await
    .unwrap();

    let Json(items) = list_api_keys(State(state.clone())).await.unwrap();
    assert_eq!(items[0].requests_per_minute, Some(120));
    assert_eq!(items[0].max_concurrency, Some(5));
    assert_eq!(items[0].requests_this_minute, 2);
    assert_eq!(items[0].current_in_flight, 1);
    assert_eq!(items[0].requests, 1);
    assert_eq!(items[0].tokens, 125);
    assert_eq!(items[0].prompt_tokens, 100);
    assert_eq!(items[0].completion_tokens, 25);
    assert_eq!(items[0].cost_micros, None);
    assert_eq!(items[0].unpriced_requests, 1);
    assert_eq!(items[0].today_prompt_tokens, 100);
    assert_eq!(items[0].today_completion_tokens, 25);

    sqlx::query(
        "UPDATE usage_logs
             SET prompt_tokens = 120, completion_tokens = 30, total_tokens = 150,
                 estimated_cost_micros = 1234
             WHERE request_id = 'completed'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let Json(items) = list_api_keys(State(state.clone())).await.unwrap();
    assert_eq!(items[0].requests, 1);
    assert_eq!(items[0].tokens, 150);
    assert_eq!(items[0].cost_micros, Some(1234));
    assert_eq!(items[0].unpriced_requests, 0);

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'completed'")
        .execute(&pool)
        .await
        .unwrap();
    let Json(items) = list_api_keys(State(state.clone())).await.unwrap();
    assert_eq!(items[0].requests, 0);
    assert_eq!(items[0].tokens, 0);
    assert_eq!(items[0].cost_micros, None);

    let Json(updated) = update_api_key(
        State(state),
        Path(created.item.id),
        Json(ApiKeyUpdate {
            enabled: true,
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: Some(0),
            max_concurrency: Some(2),
            allowed_models: None,
            expires_at: None,
            routing_policy: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.requests_per_minute, None);
    assert_eq!(updated.max_concurrency, Some(2));
}

#[tokio::test]
async fn api_key_routing_policy_round_trips_and_validates() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_api_key(
        State(state.clone()),
        Json(ApiKeyInput {
            name: "policy".to_string(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
            routing_policy: Some(ApiKeyRoutingPolicy {
                strategy: Some("least_used".to_string()),
                provider: Some("  beta  ".to_string()),
                exclude_providers: vec!["alpha".to_string(), "ALPHA".to_string()],
            }),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        created.item.routing_policy,
        ApiKeyRoutingPolicy {
            strategy: Some("least_used".to_string()),
            provider: Some("beta".to_string()),
            exclude_providers: vec!["alpha".to_string()],
        }
    );

    let stored: Option<String> =
        sqlx::query_scalar("SELECT routing_policy FROM api_keys WHERE id = ?")
            .bind(created.item.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(stored.unwrap().contains("least_used"));

    // The list view decodes the stored policy back into a typed object.
    let Json(items) = list_api_keys(State(state.clone())).await.unwrap();
    let listed = items
        .iter()
        .find(|item| item.id == created.item.id)
        .unwrap();
    assert_eq!(listed.routing_policy, created.item.routing_policy);

    // An unknown strategy is rejected instead of being stored and ignored.
    let error = update_api_key(
        State(state.clone()),
        Path(created.item.id),
        Json(ApiKeyUpdate {
            enabled: true,
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
            routing_policy: Some(ApiKeyRoutingPolicy {
                strategy: Some("telepathy".to_string()),
                ..Default::default()
            }),
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    // An empty policy clears the stored value.
    let Json(cleared) = update_api_key(
        State(state.clone()),
        Path(created.item.id),
        Json(ApiKeyUpdate {
            enabled: true,
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
            routing_policy: Some(ApiKeyRoutingPolicy::default()),
        }),
    )
    .await
    .unwrap();
    assert!(cleared.routing_policy.is_empty());
}

#[test]
fn generates_api_key_material() {
    let (raw, hash, prefix, suffix) = generate_api_key_material();
    assert!(raw.starts_with("sk-openllm-"));
    assert_eq!(hash.len(), 64);
    assert_eq!(prefix, raw[..12]);
    assert_eq!(suffix, raw[raw.len() - 4..]);
}

#[test]
fn validates_api_key_expiration() {
    assert_eq!(normalize_expiration(None).unwrap(), None);
    assert_eq!(normalize_expiration(Some("  ".to_string())).unwrap(), None);
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
    assert!(normalize_expiration(Some(future)).unwrap().is_some());
    assert!(normalize_expiration(Some("not-a-date".to_string())).is_err());
    assert!(normalize_expiration(Some((Utc::now() - Duration::hours(1)).to_rfc3339())).is_err());
}

#[test]
fn normalizes_api_key_model_permissions() {
    assert_eq!(normalize_allowed_models(None).unwrap(), None);
    assert_eq!(
        normalize_allowed_models(Some(vec!["  ".to_string()])).unwrap(),
        None
    );
    let stored = normalize_allowed_models(Some(vec![
        " gpt-* ".to_string(),
        "gpt-*".to_string(),
        "claude-*".to_string(),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(stored, r#"["gpt-*","claude-*"]"#);
    assert!(normalize_allowed_models(Some(vec!["unclosed[".to_string()])).is_err());
}
