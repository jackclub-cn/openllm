use super::*;

#[tokio::test]
async fn per_request_routing_overrides_apply_and_echo() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'fast', 'openai', 'https://fast.example/v1'),
            (2, 'cheap', 'openai', 'https://cheap.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
            (1, 'small', '[\"/chat/completions\"]'),
            (2, 'tiny', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled) VALUES
            (1, 'override route', 'override-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority, weight, enabled) VALUES
            (1, 1, 'small', 0, 100, 1),
            (1, 2, 'tiny', 0, 100, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let mut resolved = resolve_route(&state, "override-model", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    assert_eq!(resolved.strategy, "priority");
    assert_eq!(resolved.targets.len(), 2);

    // A strategy override replaces the route's configured strategy.
    let overrides = RequestRoutingOverrides {
        strategy: Some("cost_optimized".to_string()),
        ..Default::default()
    };
    apply_routing_overrides(&mut resolved, "override-model", &overrides).unwrap();
    assert_eq!(resolved.strategy, "cost_optimized");

    // Pinning by numeric id keeps only that provider.
    let mut pinned = resolve_route(&state, "override-model", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    let overrides = RequestRoutingOverrides {
        provider: Some("2".to_string()),
        ..Default::default()
    };
    apply_routing_overrides(&mut pinned, "override-model", &overrides).unwrap();
    assert_eq!(pinned.targets.len(), 1);
    assert_eq!(pinned.targets[0].provider_id, 2);

    // Excluding one provider removes exactly its target.
    let mut excluded = resolve_route(&state, "override-model", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    let overrides = RequestRoutingOverrides {
        excluded_providers: vec!["cheap".to_string()],
        ..Default::default()
    };
    let removed = apply_routing_overrides(&mut excluded, "override-model", &overrides).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(excluded.targets.len(), 1);
    assert!(
        excluded
            .targets
            .iter()
            .all(|target| target.provider_name != "cheap")
    );

    // An unmatched pin is a client error, not a silent fallback.
    let overrides = RequestRoutingOverrides {
        provider: Some("nope".to_string()),
        ..Default::default()
    };
    let error = apply_routing_overrides(&mut excluded, "override-model", &overrides).unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    // Excluding everything is rejected too.
    let overrides = RequestRoutingOverrides {
        excluded_providers: vec!["1".to_string()],
        ..Default::default()
    };
    let error = apply_routing_overrides(&mut excluded, "override-model", &overrides).unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));
}

#[tokio::test]
async fn proxy_openai_applies_per_request_provider_pin_and_echoes_headers() {
    let (fast_address, fast_requests, fast_server) = spawn_chat_upstream().await;
    let (cheap_address, cheap_requests, cheap_server) = spawn_chat_upstream().await;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix) VALUES
            (1, 'fast', 'openai', ?, 'fast/'),
            (2, 'cheap', 'openai', ?, 'cheap/')",
    )
    .bind(format!("http://{fast_address}"))
    .bind(format!("http://{cheap_address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
            (1, 'small', '[\"/chat/completions\"]'),
            (2, 'tiny', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled) VALUES
            (1, 'override route', 'override-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority, weight, enabled) VALUES
            (1, 1, 'small', 0, 100, 1),
            (1, 2, 'tiny', 0, 100, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let mut headers = HeaderMap::new();
    headers.insert(STRATEGY_HEADER, HeaderValue::from_static("cost_optimized"));
    headers.insert(PROVIDER_HEADER, HeaderValue::from_static("cheap"));
    headers.insert(EXCLUDE_PROVIDERS_HEADER, HeaderValue::from_static("fast"));
    let response = proxy_openai_inner(
        &state,
        &headers,
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "override-model",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ),
        "routing-override-e2e",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(STRATEGY_HEADER).unwrap(),
        "cost_optimized"
    );
    assert_eq!(response.headers().get(PROVIDER_HEADER).unwrap(), "cheap");
    assert_eq!(
        response.headers().get(EXCLUDE_PROVIDERS_HEADER).unwrap(),
        "fast"
    );
    assert_eq!(
        response.headers().get("x-openllm-routed-provider").unwrap(),
        "cheap"
    );

    let cheap_requests = cheap_requests.lock().await;
    assert_eq!(cheap_requests.len(), 1);
    assert_eq!(cheap_requests[0]["model"], "tiny");
    assert!(fast_requests.lock().await.is_empty());

    fast_server.abort();
    cheap_server.abort();
}

#[test]
fn per_request_routing_headers_are_parsed_and_validated() {
    let mut headers = HeaderMap::new();
    assert!(request_routing_overrides(&headers).unwrap().is_empty());

    headers.insert(STRATEGY_HEADER, HeaderValue::from_static("least_used"));
    headers.insert(PROVIDER_HEADER, HeaderValue::from_static("openai"));
    headers.insert(
        EXCLUDE_PROVIDERS_HEADER,
        HeaderValue::from_static(" a , b ,, "),
    );
    let overrides = request_routing_overrides(&headers).unwrap();
    assert_eq!(overrides.strategy.as_deref(), Some("least_used"));
    assert_eq!(overrides.provider.as_deref(), Some("openai"));
    assert_eq!(overrides.excluded_providers, vec!["a", "b"]);

    // An unknown strategy would silently change behavior, so it is rejected.
    let mut headers = HeaderMap::new();
    headers.insert(STRATEGY_HEADER, HeaderValue::from_static("telepathy"));
    assert!(request_routing_overrides(&headers).is_err());

    // Blank values mean "not supplied" rather than an empty selector.
    let mut headers = HeaderMap::new();
    headers.insert(PROVIDER_HEADER, HeaderValue::from_static("   "));
    assert!(request_routing_overrides(&headers).unwrap().is_empty());
}

#[test]
fn routing_headers_report_attempts_and_the_selected_target() {
    let mut response = StatusCode::OK.into_response();
    apply_routing_headers(&mut response, 3, 2, "Provider X", "model-y");
    // No ceiling was requested, so the budget headers stay absent rather than
    // advertising a limit that was never applied.
    apply_budget_headers(&mut response, None, 0);

    assert_eq!(response.headers()["x-openllm-routing-attempts"], "3");
    assert_eq!(response.headers()["x-openllm-fallback-count"], "2");
    assert_eq!(
        response.headers()["x-openllm-routed-provider"],
        "Provider X"
    );
    assert_eq!(response.headers()["x-openllm-routed-model"], "model-y");
    assert!(response.headers().get(BUDGET_USD_HEADER).is_none());
    assert!(
        response
            .headers()
            .get("x-openllm-budget-excluded-targets")
            .is_none()
    );

    let mut budgeted = StatusCode::OK.into_response();
    apply_budget_headers(&mut budgeted, Some(0.25), 2);
    assert_eq!(budgeted.headers()[BUDGET_USD_HEADER], "0.250000");
    assert_eq!(budgeted.headers()["x-openllm-budget-excluded-targets"], "2");
}
