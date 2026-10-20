use super::*;

#[test]
fn api_key_routing_policy_merges_below_request_headers() {
    let key = ApiKeyRecord {
        id: 9,
        name: "policy".to_string(),
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
        routing_policy: Some(
            json!({
                "strategy": "priority",
                "provider": "alpha",
                "exclude_providers": ["beta"]
            })
            .to_string(),
        ),
    };
    let policy = routing_overrides_from_key(Some(&key));
    assert_eq!(policy.strategy.as_deref(), Some("priority"));
    assert_eq!(policy.provider.as_deref(), Some("alpha"));
    assert_eq!(policy.excluded_providers, vec!["beta"]);

    // A request that sets only the strategy keeps the key's provider pin and
    // exclusions, so the two layers compose instead of replacing each other.
    let merged = merge_routing_overrides(
        policy.clone(),
        RequestRoutingOverrides {
            strategy: Some("least_used".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(merged.strategy.as_deref(), Some("least_used"));
    assert_eq!(merged.provider.as_deref(), Some("alpha"));
    assert_eq!(merged.excluded_providers, vec!["beta"]);

    // Malformed stored JSON falls back to "no policy" instead of failing.
    let broken = ApiKeyRecord {
        routing_policy: Some("{".to_string()),
        ..key
    };
    assert!(routing_overrides_from_key(Some(&broken)).is_empty());
}

#[tokio::test]
async fn api_key_routing_policy_pins_the_provider_end_to_end() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn spawn_upstream() -> (
        std::net::SocketAddr,
        Arc<Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new().route(
            OPENAI_CHAT_COMPLETIONS,
            axum::routing::post({
                let requests = requests.clone();
                move |Json(body): Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        requests.lock().await.push(body);
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "chatcmpl-policy",
                                "object": "chat.completion",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 1,
                                    "total_tokens": 4
                                }
                            })),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (address, requests, server)
    }

    let (alpha_address, alpha_requests, alpha_server) = spawn_upstream().await;
    let (beta_address, beta_requests, beta_server) = spawn_upstream().await;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'alpha', 'openai', ?),
            (2, 'beta', 'openai', ?)",
    )
    .bind(format!("http://{alpha_address}"))
    .bind(format!("http://{beta_address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
            (1, 'm', '[\"/chat/completions\"]'),
            (2, 'm', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'shared', 'shared-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
            (1, 1, 'm', 0),
            (1, 2, 'm', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let key = ApiKeyRecord {
        id: 1,
        name: "pinned".to_string(),
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
        routing_policy: Some(
            json!({
                "strategy": "priority",
                "provider": "beta",
                "exclude_providers": []
            })
            .to_string(),
        ),
    };

    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "shared-model",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ),
        "key-policy-pin",
        Some(key.clone()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The key policy is not echoed back as if the caller had sent a header.
    assert!(response.headers().get(STRATEGY_HEADER).is_none());
    assert!(response.headers().get(PROVIDER_HEADER).is_none());
    assert_eq!(
        response.headers().get("x-openllm-routed-provider").unwrap(),
        "beta"
    );
    assert_eq!(beta_requests.lock().await.len(), 1);
    assert!(alpha_requests.lock().await.is_empty());

    // A request header that contradicts the key's pin leaves no target, which
    // is a client error rather than a silent fallback to the pinned provider.
    let mut headers = HeaderMap::new();
    headers.insert(EXCLUDE_PROVIDERS_HEADER, HeaderValue::from_static("beta"));
    let error = proxy_openai_inner(
        &state,
        &headers,
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "shared-model",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ),
        "key-policy-conflict",
        Some(key),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    alpha_server.abort();
    beta_server.abort();
}
