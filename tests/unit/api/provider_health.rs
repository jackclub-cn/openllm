use super::*;

#[tokio::test]
async fn persists_provider_health_check_model() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) =
        create_provider(State(state.clone()), Json(provider_input(None, Vec::new())))
            .await
            .unwrap();
    assert_eq!(created.health_check_model, None);

    let mut update = provider_update_with_keys(None);
    update.health_check_model = Some("  claude-haiku-4-5  ".to_string());
    let Json(updated) = update_provider(State(state.clone()), Path(created.id), Json(update))
        .await
        .unwrap();
    assert_eq!(
        updated.health_check_model.as_deref(),
        Some("claude-haiku-4-5")
    );

    let mut clear = provider_update_with_keys(None);
    clear.health_check_model = Some(String::new());
    let Json(cleared) = update_provider(State(state), Path(created.id), Json(clear))
        .await
        .unwrap();
    assert_eq!(cleared.health_check_model, None);
}

#[tokio::test]
async fn provider_health_check_uses_configured_model() {
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post({
            let seen = seen.clone();
            move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    *seen.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "health",
                            "choices": [],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 0,
                                "total_tokens": 1
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

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, health_check_model
             ) VALUES (1, 'mock', 'openai', ?, 'configured-model')",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'fallback-model', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let result = test_provider_inner(&state, 1).await.unwrap();
    assert!(result.ok);
    assert_eq!(result.checked, "inference");
    assert!(result.message.contains("configured-model"));
    assert_eq!(
        seen.lock()
            .await
            .as_ref()
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str),
        Some("configured-model")
    );
    server.abort();
}

#[tokio::test]
async fn provider_health_probe_learns_tool_search_compatibility() {
    let reject_tool_search = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post({
            let reject_tool_search = reject_tool_search.clone();
            move |Json(body): Json<Value>| {
                let reject_tool_search = reject_tool_search.clone();
                async move {
                    let includes_tool_search = body
                        .get("tools")
                        .and_then(Value::as_array)
                        .is_some_and(|tools| {
                            tools.iter().any(|tool| tool["type"] == "tool_search")
                        });
                    if includes_tool_search
                        && reject_tool_search.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "unknown tool type: tool_search"
                                }
                            })),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "health",
                            "choices": [],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 0,
                                "total_tokens": 1
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

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, tool_search_supported
             ) VALUES (1, 'mock', 'openai', ?, 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
             VALUES (1, 'Primary', 'sk-test', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'probe-model', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let result = test_provider_inner(&state, 1).await.unwrap();
    assert!(result.ok);
    let checked_after_rejection: Option<String> =
        sqlx::query_scalar("SELECT tool_search_checked_at FROM providers WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(checked_after_rejection.is_some());
    let unsupported: i64 =
        sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(unsupported, 0);

    reject_tool_search.store(false, std::sync::atomic::Ordering::SeqCst);
    let result = test_provider_inner(&state, 1).await.unwrap();
    assert!(result.ok);
    let supported: i64 =
        sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(supported, 1);
    server.abort();
}

#[tokio::test]
async fn provider_health_check_uses_responses_for_responses_only_model() {
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post({
            let seen = seen.clone();
            move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    *seen.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "resp_health",
                            "object": "response",
                            "usage": {
                                "input_tokens": 1,
                                "output_tokens": 0,
                                "total_tokens": 1
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

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, enabled, supported_endpoints
             ) VALUES (1, 'responses-model', 1, '[\"/responses\"]')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let result = test_provider_inner(&state, 1).await.unwrap();
    assert!(result.ok);
    assert_eq!(result.checked, "inference");
    let body = seen.lock().await;
    let body = body.as_ref().unwrap();
    assert_eq!(body["model"], "responses-model");
    assert_eq!(body["input"], "ping");
    assert!(body.get("max_output_tokens").is_none());
    server.abort();
}

#[tokio::test]
async fn provider_key_health_check_tests_every_enabled_key() {
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post({
            let seen = seen.clone();
            move |headers: HeaderMap, Json(_body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    let authorization = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    seen.lock().await.push(authorization.clone());
                    let ok = authorization == "Bearer sk-good";
                    (
                        if ok {
                            StatusCode::OK
                        } else {
                            StatusCode::UNAUTHORIZED
                        },
                        Json(json!({
                            "id": "key-health",
                            "choices": [],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 0,
                                "total_tokens": 1
                            },
                            "error": if ok {
                                Value::Null
                            } else {
                                json!({"message": "invalid API key"})
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

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'multi-key', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'probe-model', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) VALUES
                (11, 1, 'Broken', 'sk-bad', 1),
                (12, 1, 'Healthy', 'sk-good', 1),
                (13, 1, 'Disabled', 'sk-disabled', 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE provider_api_keys \
             SET last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale unauthorized' \
             WHERE id = 12",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let cooldown_until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    {
        let mut cooldowns = state.provider_key_cooldown.lock().await;
        cooldowns.insert(11, cooldown_until);
        cooldowns.insert(12, cooldown_until);
    }
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, cooldown_until);
    state.provider_failure_streak.lock().await.insert(1, 3);
    let mut circuit_events = state.circuit_events.subscribe();

    let result = test_provider_keys_inner(&state, 1).await.unwrap();

    assert_eq!(result.provider_name, "multi-key");
    assert_eq!(result.model.as_deref(), Some("probe-model"));
    assert_eq!(result.total, 2);
    assert_eq!(result.ok, 1);
    assert_eq!(result.failed, 1);
    assert_eq!(result.results[0].key_id, Some(11));
    assert_eq!(result.results[0].key_name, "Broken");
    assert_eq!(result.results[0].api_key_suffix, "-bad");
    assert!(!result.results[0].ok);
    assert!(result.results[0].message.contains("401"));
    assert_eq!(result.results[1].key_id, Some(12));
    assert_eq!(result.results[1].key_name, "Healthy");
    assert_eq!(result.results[1].api_key_suffix, "good");
    assert!(result.results[1].ok);
    assert_eq!(result.results[1].checked, "inference");
    assert_eq!(
        seen.lock().await.as_slice(),
        &["Bearer sk-bad".to_string(), "Bearer sk-good".to_string()]
    );
    assert!(state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(!state.provider_key_cooldown.lock().await.contains_key(&12));
    assert!(!state.provider_cooldown.lock().await.contains_key(&1));
    assert!(!state.provider_failure_streak.lock().await.contains_key(&1));
    let closed = circuit_events
        .try_recv()
        .expect("a successful key probe must publish a circuit recovery event");
    assert_eq!(closed.state, "closed");
    assert_eq!(closed.reason, "health_check");
    let failed: (Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT last_test_ok, last_test_checked, last_test_message \
             FROM provider_api_keys WHERE id = 11",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(failed.0, Some(0));
    assert_eq!(failed.1.as_deref(), Some("inference"));
    assert!(failed.2.unwrap().contains("401"));
    let recovered: (Option<i64>, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT last_test_ok, last_test_checked, last_test_message, last_error \
                 FROM provider_api_keys WHERE id = 12",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(recovered.0, Some(1));
    assert_eq!(recovered.1.as_deref(), Some("inference"));
    assert!(recovered.2.is_some());
    assert_eq!(recovered.3, None);
    let provider = get_provider(&state, 1).await.unwrap();
    assert_eq!(provider.api_keys[0].last_test_ok, Some(false));
    assert_eq!(provider.api_keys[1].last_test_ok, Some(true));
    assert!(provider.api_keys[1].last_test_at.is_some());

    sqlx::query(
        "UPDATE provider_api_keys \
             SET last_test_at = NULL, last_test_ok = NULL, last_test_latency_ms = NULL, \
                 last_test_checked = NULL, last_test_message = NULL, \
                 last_error_at = NULL, last_error = NULL \
             WHERE id IN (11, 12)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE provider_api_keys \
             SET last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale unauthorized' \
             WHERE id = 12",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let aggregate = test_provider_inner(&state, 1).await.unwrap();
    assert!(aggregate.ok);
    let persisted: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT \
                COALESCE(SUM(CASE WHEN id IN (11, 12) AND last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN id IN (11, 12) AND last_test_ok = 1 THEN 1 ELSE 0 END), 0) \
             FROM provider_api_keys",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(persisted, (Some(1), Some(1)));
    let recovered_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM provider_api_keys WHERE id = 12")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(recovered_error, None);

    let Json(all) = test_all_provider_keys(State(state.clone())).await.unwrap();
    assert_eq!(all.total_providers, 1);
    assert_eq!(all.tested_providers, 1);
    assert_eq!(all.healthy_providers, 0);
    assert_eq!(all.failed_providers, 1);
    assert_eq!(all.total_keys, 2);
    assert_eq!(all.healthy_keys, 1);
    assert_eq!(all.failed_keys, 1);
    server.abort();
}

#[tokio::test]
async fn provider_health_check_rejects_duplicate_runs() {
    let state = provider_key_test_state().await;
    state.provider_health_check.lock().await.insert(1);

    let error = test_provider_inner(&state, 1).await.unwrap_err();
    assert!(
        matches!(error, AppError::Conflict(message) if message.contains("already in progress"))
    );
}

#[tokio::test]
async fn persists_provider_test_result() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                last_test_at TEXT,
                last_test_ok INTEGER,
                last_test_latency_ms INTEGER,
                last_test_checked TEXT,
                last_test_message TEXT
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO providers (id) VALUES (1)")
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::new(pool.clone(), None);
    persist_provider_test(
        &state,
        1,
        &ProviderTestResult {
            ok: true,
            latency_ms: 42,
            message: "ok".to_string(),
            checked: "inference".to_string(),
        },
    )
    .await
    .unwrap();
    let row: (Option<i64>, Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT last_test_ok, last_test_latency_ms, last_test_checked, last_test_message \
             FROM providers WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            Some(1),
            Some(42),
            Some("inference".to_string()),
            Some("ok".to_string())
        )
    );
}
