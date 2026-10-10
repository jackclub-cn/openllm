use super::*;
use tower::ServiceExt;

async fn provider_key_test_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

#[tokio::test]
async fn prometheus_metrics_render_and_require_admin_token() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
            id, name, provider_type, base_url, enabled, last_test_ok
         ) VALUES
            (1, 'Healthy provider', 'openai', 'https://healthy.example/v1', 1, 1),
            (2, 'Disabled provider', 'openai', 'https://disabled.example/v1', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
         VALUES (1, 'model-a', 1), (1, 'model-b', 0), (2, 'model-c', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO webhooks (id, name, url, enabled)
         VALUES (1, 'Audit hook', 'https://example.com/hook', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO webhook_deliveries (webhook_id, event_type, status_code, attempts)
         VALUES (1, 'request.completed', 200, 1), (1, 'request.failed', 500, 3)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let router = crate::build_router(AppState::new(pool, Some("metrics-secret".to_string())));

    let unauthorized = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("authorization", "Bearer metrics-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/plain"))
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("# TYPE openllm_requests_total counter"));
    assert!(body.contains("# TYPE openllm_requests_in_flight gauge"));
    assert!(body.contains("openllm_providers{state=\"healthy\"} 1"));
    assert!(body.contains("openllm_providers{state=\"disabled\"} 1"));
    assert!(
        body.contains("openllm_provider_health{provider=\"Healthy provider\",state=\"healthy\"} 1")
    );
    assert!(body.contains("openllm_provider_models{provider=\"Healthy provider\"} 1"));
    assert!(body.contains("openllm_provider_models_disabled{provider=\"Healthy provider\"} 1"));
    assert!(body.contains("openllm_webhooks{state=\"enabled\"} 1"));
    assert!(body.contains("openllm_webhook_deliveries{state=\"succeeded\"} 1"));
    assert!(body.contains("openllm_webhook_deliveries{state=\"failed\"} 1"));
    assert!(body.contains("openllm_database_size_bytes"));
}

fn provider_input(api_key: Option<&str>, api_keys: Vec<ProviderApiKeyInput>) -> ProviderInput {
    ProviderInput {
        name: "key-pool-test".to_string(),
        provider_type: ProviderType::Openai,
        base_url: "https://example.com/v1".to_string(),
        model_prefix: String::new(),
        api_key: api_key.map(ToOwned::to_owned),
        api_keys,
        headers: json!({}),
        enabled: true,
        auto_sync_models: false,
        models: Vec::new(),
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
    }
}

fn provider_key_input(
    id: Option<i64>,
    name: &str,
    api_key: Option<&str>,
    enabled: bool,
) -> ProviderApiKeyInput {
    ProviderApiKeyInput {
        id,
        name: name.to_string(),
        api_key: api_key.map(ToOwned::to_owned),
        enabled,
    }
}

fn provider_update_with_keys(api_keys: Option<Vec<ProviderApiKeyInput>>) -> ProviderUpdate {
    ProviderUpdate {
        name: None,
        provider_type: None,
        base_url: None,
        model_prefix: None,
        api_key: None,
        clear_api_key: None,
        api_keys,
        headers: None,
        enabled: None,
        auto_sync_models: None,
        models: None,
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
    }
}

#[test]
fn normalizes_model_prefix_with_trailing_slash() {
    assert_eq!(normalize_model_prefix("openai").unwrap(), "openai/");
    assert_eq!(normalize_model_prefix("openai/").unwrap(), "openai/");
    assert_eq!(normalize_model_prefix("  /local/  ").unwrap(), "local/");
    assert_eq!(normalize_model_prefix("").unwrap(), "");
}

#[test]
fn provider_test_urls_reuse_the_shared_joiner() {
    // The probe must hit the inference endpoint, which is what enforces
    // credentials, and must not double up `/v1`.
    assert_eq!(
        join_upstream_url(
            "https://api.commandcode.ai/provider/v1",
            "/v1/chat/completions"
        ),
        "https://api.commandcode.ai/provider/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("https://api.anthropic.com", "/v1/messages"),
        "https://api.anthropic.com/v1/messages"
    );
    // A base without `/v1` keeps the full path.
    assert_eq!(
        join_upstream_url("http://localhost:8000", "/v1/chat/completions"),
        "http://localhost:8000/v1/chat/completions"
    );
}

#[test]
fn recognizes_provider_quota_endpoints() {
    assert_eq!(
        provider_quota_kind("https://api.commandcode.ai/provider/v1"),
        Some("command_code")
    );
    assert_eq!(
        provider_quota_kind("https://opencode.ai/zen/go/v1"),
        Some("opencode_go")
    );
    assert_eq!(
        provider_quota_kind("https://api.deepseek.com/v1"),
        Some("deepseek")
    );
    assert_eq!(provider_quota_kind("https://api.openai.com/v1"), None);
}

#[test]
fn maps_command_code_plans() {
    assert_eq!(command_code_plan_name("individual-goat"), "GOAT");
    assert_eq!(command_code_monthly_total("individual-goat"), Some(70.0));
    assert_eq!(command_code_plan_name("individual-pro"), "Pro");
    assert_eq!(command_code_monthly_total("individual-pro"), Some(80.0));
    assert_eq!(command_code_plan_name("unknown"), "unknown");
    assert_eq!(
        quota_reset_at(Some(&json!(1790774552836_i64))),
        epoch_millis_to_rfc3339(1790774552836)
    );
    assert_eq!(
        quota_reset_at(Some(&json!("2026-10-01T00:00:00Z"))).as_deref(),
        Some("2026-10-01T00:00:00Z")
    );
}

#[test]
fn rejects_model_prefix_with_invalid_characters() {
    assert!(normalize_model_prefix("bad prefix").is_err());
    assert!(normalize_model_prefix("spaces/and").is_err());
}

#[test]
fn derives_ollama_root_from_root_and_v1_base_urls() {
    assert_eq!(
        ollama_root("http://localhost:11434"),
        "http://localhost:11434"
    );
    assert_eq!(
        ollama_root("http://localhost:11434/"),
        "http://localhost:11434"
    );
    assert_eq!(
        ollama_root("http://localhost:11434/v1"),
        "http://localhost:11434"
    );
}

#[test]
fn trims_trailing_slash_from_base_url() {
    assert_eq!(
        normalize_base_url("https://api.example.com/v1/"),
        "https://api.example.com/v1"
    );
    assert_eq!(
        normalize_base_url("  https://api.example.com/v1  "),
        "https://api.example.com/v1"
    );
}

#[test]
fn parses_openai_style_model_lists() {
    let value = json!({ "object": "list", "data": [
        { "id": "gpt-4.1" },
        { "id": "gpt-4.1-mini" },
        { "name": "fallback-name" }
    ]});
    let names = parse_openai_model_entries(&value)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["gpt-4.1", "gpt-4.1-mini", "fallback-name"]);
}

#[test]
fn captures_upstream_context_length_and_endpoints() {
    // CommandCode reports a flat `context_length` for every model, even ones
    // models.dev has never heard of.
    let value = json!({ "data": [
        { "id": "deepseek/deepseek-v4.1-flash", "context_length": 1000000,
          "supported_endpoints": ["/chat/completions", "/responses"] }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.context_limit, Some(1000000));
    assert_eq!(
        entries[0].1.supported_endpoints,
        vec!["/chat/completions", "/responses"]
    );
}

#[test]
fn prefers_stricter_upstream_input_limit() {
    // The live CallAI catalog advertises 400K input, while models.dev
    // reports a 1.05M window. Keeping the smaller value prevents Codex from
    // compacting too late and hitting an upstream 400.
    let value = json!({ "data": [
        {
            "id": "gpt-6-astra",
            "context_length": 1050000,
            "max_input_tokens": 400000
        }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries[0].1.context_limit, Some(400_000));
}

#[test]
fn accepts_max_input_tokens_without_context_length() {
    let value = json!({ "data": [
        { "id": "codex-auto-review", "max_input_tokens": 400000 }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries[0].1.context_limit, Some(400_000));
}

#[test]
fn validates_manual_model_limits() {
    assert!(validate_limit("context", None).is_ok());
    assert!(validate_limit("context", Some(400_000)).is_ok());
    assert!(validate_limit("context", Some(0)).is_err());
    assert!(validate_limit("input", Some(-1)).is_err());

    assert_eq!(serialize_endpoint_override(None).unwrap(), None);
    assert_eq!(
        serialize_endpoint_override(Some(&[
            "/v1/responses/".to_string(),
            "/v1/responses".to_string()
        ]))
        .unwrap()
        .as_deref(),
        Some(r#"["/v1/responses"]"#)
    );
    assert!(serialize_endpoint_override(Some(&["responses".to_string()])).is_err());

    assert!(validate_cost_override("input cost", None).is_ok());
    assert!(validate_cost_override("input cost", Some(0.0)).is_ok());
    assert!(validate_cost_override("input cost", Some(1.25)).is_ok());
    assert!(validate_cost_override("input cost", Some(-0.1)).is_err());
}

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
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.requests_per_minute, None);
    assert_eq!(updated.max_concurrency, Some(2));
}

#[test]
fn validates_provider_health_interval() {
    assert_eq!(normalize_health_interval(None).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(0)).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(30)).unwrap(), Some(30));
    assert!(normalize_health_interval(Some(-1)).is_err());
}

#[tokio::test]
async fn provider_api_key_pool_supports_create_update_and_clear() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "Primary", Some("sk-one"), true),
                provider_key_input(None, "Backup", Some("sk-two"), true),
            ],
        )),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys.len(), 2);
    assert_eq!(created.api_keys[0].name, "Primary");
    assert_eq!(created.api_keys[0].api_key_suffix, "-one");
    assert_eq!(created.api_keys[1].api_key_suffix, "-two");

    let first_id = created.api_keys[0].id;
    state.provider_key_cooldown.lock().await.insert(
        first_id,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![
            provider_key_input(Some(first_id), "Primary", None, false),
            provider_key_input(None, "Replacement", Some("sk-three"), true),
        ]))),
    )
    .await
    .unwrap();
    assert_eq!(updated.api_keys.len(), 2);
    assert!(!updated.api_keys[0].enabled);
    assert_eq!(updated.api_keys[1].name, "Replacement");
    assert!(
        !state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&first_id)
    );

    let retained_secret: String =
        sqlx::query_scalar("SELECT secret FROM provider_api_keys WHERE id = ?")
            .bind(first_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(retained_secret, "sk-one");
    let mirrored: Option<String> = sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(mirrored.as_deref(), Some("sk-three"));

    let Json(cleared) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(Vec::new()))),
    )
    .await
    .unwrap();
    assert!(cleared.api_keys.is_empty());
    assert!(!cleared.api_key_set);
    let mirrored: Option<String> = sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(mirrored, None);
}

#[tokio::test]
async fn provider_quota_credential_uses_selected_or_first_enabled_key() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "Primary", Some("sk-one"), true),
                provider_key_input(None, "Backup", Some("sk-two"), true),
            ],
        )),
    )
    .await
    .unwrap();
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

    let selected = provider_quota_credential(&state, &provider, Some(created.api_keys[1].id))
        .await
        .unwrap();
    assert_eq!(selected.key_id, Some(created.api_keys[1].id));
    assert_eq!(selected.key_name, "Backup");
    assert_eq!(selected.secret, "sk-two");

    let default = provider_quota_credential(&state, &provider, None)
        .await
        .unwrap();
    assert_eq!(default.key_id, Some(created.api_keys[0].id));
    assert_eq!(default.key_name, "Primary");

    sqlx::query("UPDATE provider_api_keys SET enabled = 0 WHERE id = ?")
        .bind(created.api_keys[0].id)
        .execute(&state.pool)
        .await
        .unwrap();
    let fallback = provider_quota_credential(&state, &provider, None)
        .await
        .unwrap();
    assert_eq!(fallback.key_id, Some(created.api_keys[1].id));
    assert_eq!(fallback.key_name, "Backup");

    assert!(
        provider_quota_credential(&state, &provider, Some(999_999))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provider_api_key_rotation_resets_stale_health_state() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![provider_key_input(None, "Primary", Some("sk-old"), true)],
        )),
    )
    .await
    .unwrap();
    let key_id = created.api_keys[0].id;
    sqlx::query(
        "UPDATE provider_api_keys \
             SET last_used_at = '2026-01-01T00:00:00Z', \
                 last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale 401', \
                 last_test_at = '2026-01-01T00:00:00Z', last_test_ok = 0, \
                 last_test_latency_ms = 42, last_test_checked = 'inference', \
                 last_test_message = 'old failure' \
             WHERE id = ?",
    )
    .bind(key_id)
    .execute(&state.pool)
    .await
    .unwrap();
    state.provider_key_cooldown.lock().await.insert(
        key_id,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );

    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![provider_key_input(
            Some(key_id),
            "Primary",
            Some("sk-rotated"),
            true,
        )]))),
    )
    .await
    .unwrap();

    assert_eq!(updated.api_keys[0].api_key_suffix, "ated");
    assert_eq!(updated.api_keys[0].last_test_ok, None);
    assert_eq!(updated.api_keys[0].last_error, None);
    assert!(
        !state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&key_id)
    );
    let stale_fields: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_api_keys \
             WHERE id = ? AND ( \
                 last_used_at IS NOT NULL OR last_error_at IS NOT NULL OR last_error IS NOT NULL \
                 OR last_test_at IS NOT NULL OR last_test_ok IS NOT NULL \
                 OR last_test_latency_ms IS NOT NULL OR last_test_checked IS NOT NULL \
                 OR last_test_message IS NOT NULL \
             )",
    )
    .bind(key_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(stale_fields, 0);
}

#[tokio::test]
async fn provider_api_key_pool_rejects_duplicate_secrets() {
    let state = provider_key_test_state().await;
    let error = create_provider(
        State(state),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "One", Some("sk-same"), true),
                provider_key_input(None, "Two", Some("sk-same"), true),
            ],
        )),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("must be unique"));
}

#[tokio::test]
async fn provider_api_key_pool_accepts_legacy_single_key_input() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state),
        Json(provider_input(Some("sk-legacy"), Vec::new())),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys.len(), 1);
    assert_eq!(created.api_keys[0].name, "Default");
    assert_eq!(created.api_keys[0].api_key_suffix, "gacy");
}

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
    state.provider_key_cooldown.lock().await.insert(
        11,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );

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
async fn usage_views_expose_provider_api_key_name() {
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
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model,
                endpoint, prompt_tokens, completion_tokens, total_tokens,
                latency_ms, status_code, success, warning_message
             ) VALUES (
                'request-with-provider-key', 1, 11, 'Primary', 'gpt-test',
                '/v1/chat/completions', 10, 4, 14, 123, 200, 1, 'repaired request'
             )",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
    let Json(page) = list_usage(State(state.clone()), Query(query))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].provider_api_key_id, Some(11));
    assert_eq!(
        page.items[0].provider_api_key_name.as_deref(),
        Some("Primary")
    );

    let Json(detail) = get_usage_detail(
        State(state.clone()),
        Path("request-with-provider-key".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(detail.provider_api_key_id, Some(11));
    assert_eq!(detail.provider_api_key_name.as_deref(), Some("Primary"));
    assert_eq!(detail.warning_message.as_deref(), Some("repaired request"));

    let provider = get_provider(&state, 1).await.unwrap();
    assert_eq!(provider.api_keys[0].requests, 1);
    assert_eq!(provider.api_keys[0].success_rate, 100.0);
    assert_eq!(provider.api_keys[0].avg_latency_ms, 123.0);
    assert_eq!(provider.api_keys[0].prompt_tokens, 10);
    assert_eq!(provider.api_keys[0].completion_tokens, 4);
    state.provider_key_cooldown.lock().await.insert(
        11,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    );
    let provider = get_provider(&state, 1).await.unwrap();
    assert!(provider.api_keys[0].cooldown_seconds.is_some());
    assert!(provider.api_keys[0].cooldown_seconds.unwrap() > 0);

    state.provider_cooldown.lock().await.insert(
        1,
        std::time::Instant::now() + std::time::Duration::from_secs(90),
    );
    let provider = get_provider(&state, 1).await.unwrap();
    assert!(provider.cooldown_seconds.is_some());
    assert!(provider.cooldown_seconds.unwrap() > 0);
}

#[tokio::test]
async fn unfiltered_usage_total_combines_lifetime_and_in_flight_counts() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success
             ) VALUES
                ('pending', 'm', '/v1/chat/completions', 0, 1, 0),
                ('finished', 'm', '/v1/chat/completions', 200, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();

    let Json(page) = list_usage(State(state.clone()), Query(query))
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'finished'")
        .execute(&state.pool)
        .await
        .unwrap();
    let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
    let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "pending");
}

#[tokio::test]
async fn usage_filter_can_select_provider_api_key() {
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
             VALUES (11, 1, 'Primary', 'sk-primary', 1),
                    (12, 1, 'Backup', 'sk-backup', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model, endpoint, status_code, success
             ) VALUES
                ('primary', 1, 11, 'Primary', 'gpt-test', '/v1/chat/completions', 200, 1),
                ('backup', 1, 12, 'Backup', 'gpt-test', '/v1/chat/completions', 200, 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let query: UsageQuery = serde_json::from_value(json!({
        "provider_api_key_id": 12
    }))
    .unwrap();
    let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "backup");
    assert_eq!(
        page.items[0].provider_api_key_name.as_deref(),
        Some("Backup")
    );
}

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
    assert_eq!(settings.database_stats.webhooks, 0);
    assert_eq!(settings.database_stats.webhook_deliveries, 0);
    assert_eq!(settings.database_stats.in_flight_requests, 0);
}

#[tokio::test]
async fn provider_list_batches_models_and_key_statistics() {
    let state = provider_key_test_state().await;
    for (id, name) in [(1, "First"), (2, "Second")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (provider_id, model_name) in [(1, "alpha"), (1, "beta"), (2, "gamma")] {
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
                 VALUES (?, ?, 1)",
        )
        .bind(provider_id)
        .bind(model_name)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (id, provider_id, name) in [(11, 1, "Primary"), (12, 1, "Backup"), (21, 2, "Only")] {
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
                 VALUES (?, ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(provider_id)
        .bind(name)
        .bind(format!("sk-{id}"))
        .execute(&state.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, requested_model,
                endpoint, prompt_tokens, completion_tokens, total_tokens,
                latency_ms, status_code, success, in_flight
             ) VALUES (
                'batched-provider-list', 1, 11, 'alpha',
                '/v1/chat/completions', 10, 5, 15,
                100, 200, 1, 0
             )",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    assert_eq!(providers.len(), 2);
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    assert_eq!(first.models, vec!["alpha", "beta"]);
    assert_eq!(first.api_keys.len(), 2);
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 1);
    assert_eq!(primary.success_rate, 100.0);
    assert_eq!(primary.avg_latency_ms, 100.0);
    assert_eq!(primary.prompt_tokens, 10);
    assert_eq!(primary.completion_tokens, 5);

    sqlx::query(
        "UPDATE usage_logs
             SET success = 0, latency_ms = 250,
                 prompt_tokens = 20, completion_tokens = 10, total_tokens = 30
             WHERE request_id = 'batched-provider-list'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 1);
    assert_eq!(primary.success_rate, 0.0);
    assert_eq!(primary.avg_latency_ms, 250.0);
    assert_eq!(primary.prompt_tokens, 20);
    assert_eq!(primary.completion_tokens, 10);

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'batched-provider-list'")
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(providers) = list_providers(State(state)).await.unwrap();
    let first = providers.iter().find(|provider| provider.id == 1).unwrap();
    let primary = first.api_keys.iter().find(|key| key.id == 11).unwrap();
    assert_eq!(primary.requests, 0);
    assert_eq!(primary.prompt_tokens, 0);
    assert_eq!(primary.completion_tokens, 0);

    let second = providers.iter().find(|provider| provider.id == 2).unwrap();
    assert_eq!(second.models, vec!["gamma"]);
    assert_eq!(second.api_keys[0].requests, 0);
}

#[tokio::test]
async fn route_list_batches_targets() {
    let state = provider_key_test_state().await;
    for (id, name) in [(1, "First"), (2, "Second")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (id, pattern) in [(1, "route-one"), (2, "route-two")] {
        sqlx::query(
            "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
                 VALUES (?, ?, ?, 'priority', 1)",
        )
        .bind(id)
        .bind(format!("Route {id}"))
        .bind(pattern)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (route_id, provider_id, upstream_model, priority) in [
        (1, 1, "alpha", 0),
        (1, 2, "alpha-backup", 1),
        (2, 2, "beta", 0),
    ] {
        sqlx::query(
            "INSERT INTO route_targets (
                    route_id, provider_id, upstream_model, weight, priority, enabled
                 ) VALUES (?, ?, ?, 100, ?, 1)",
        )
        .bind(route_id)
        .bind(provider_id)
        .bind(upstream_model)
        .bind(priority)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    for (provider_id, model_name, context, input, output) in [
        (1, "alpha", 100_000, 80_000, 40_000),
        (2, "alpha-backup", 128_000, 100_000, 20_000),
    ] {
        sqlx::query(
            "INSERT INTO provider_models (
                    provider_id, model_name, enabled, context_limit, input_limit, output_limit
                 ) VALUES (?, ?, 1, ?, ?, ?)",
        )
        .bind(provider_id)
        .bind(model_name)
        .bind(context)
        .bind(input)
        .bind(output)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    let Json(routes) = list_routes(State(state.clone())).await.unwrap();
    assert_eq!(routes.len(), 2);
    let first = routes.iter().find(|route| route.id == 1).unwrap();
    assert_eq!(first.context_limit, Some(100_000));
    assert_eq!(first.input_limit, Some(80_000));
    assert_eq!(first.output_limit, Some(20_000));
    assert!(first.limits_verified);
    assert_eq!(first.targets.len(), 2);
    assert_eq!(first.targets[0].upstream_model, "alpha");
    assert_eq!(first.targets[1].upstream_model, "alpha-backup");
    assert!(first.targets[0].provider_enabled);
    assert!(first.targets[0].model_enabled);
    let second = routes.iter().find(|route| route.id == 2).unwrap();
    assert_eq!(second.targets.len(), 1);
    assert_eq!(second.targets[0].upstream_model, "beta");

    sqlx::query("UPDATE providers SET enabled = 0 WHERE id = 2")
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(routes) = list_routes(State(state.clone())).await.unwrap();
    let first = routes.iter().find(|route| route.id == 1).unwrap();
    assert_eq!(first.input_limit, Some(80_000));
    assert_eq!(first.output_limit, Some(40_000));
    assert!(first.limits_verified);
    assert!(!first.targets[1].provider_enabled);
}

#[tokio::test]
async fn advanced_route_strategy_round_trips_through_storage() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Primary', 'openai', 'https://example.com/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
         VALUES (1, 'model', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let (_, created) = create_route(
        State(state.clone()),
        Json(RouteInput {
            name: "Cost route".to_string(),
            model_pattern: "cost-model".to_string(),
            strategy: RouteStrategy::CostOptimized,
            enabled: true,
            targets: vec![RouteTargetInput {
                id: None,
                provider_id: 1,
                upstream_model: "model".to_string(),
                weight: 100,
                priority: 0,
                enabled: true,
            }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(created.0.strategy, "cost_optimized");
    let (stored, extension): (String, String) =
        sqlx::query_as("SELECT strategy, strategy_ext FROM routes WHERE id = ?")
            .bind(created.0.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, "priority");
    assert_eq!(extension, "cost_optimized");

    let updated = update_route(
        State(state.clone()),
        Path(created.0.id),
        Json(RouteUpdate {
            name: None,
            model_pattern: None,
            strategy: Some(RouteStrategy::LatencyOptimized),
            enabled: None,
            targets: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.0.strategy, "latency_optimized");
    let (stored, extension): (String, String) =
        sqlx::query_as("SELECT strategy, strategy_ext FROM routes WHERE id = ?")
            .bind(created.0.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, "priority");
    assert_eq!(extension, "latency_optimized");
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
async fn provider_update_resets_tool_search_probe_only_when_context_changes() {
    let state = provider_key_test_state().await;
    sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url,
                tool_search_supported, tool_search_checked_at
             ) VALUES (1, 'Provider', 'openai', 'https://example.com/v1', 0, '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

    let mut status_update = provider_update_with_keys(None);
    status_update.enabled = Some(false);
    let _ = update_provider(State(state.clone()), Path(1), Json(status_update))
        .await
        .unwrap();
    let (supported, checked_at): (i64, Option<String>) = sqlx::query_as(
        "SELECT tool_search_supported, tool_search_checked_at FROM providers WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(supported, 0);
    assert_eq!(checked_at.as_deref(), Some("2026-01-01T00:00:00Z"));

    let mut context_update = provider_update_with_keys(None);
    context_update.headers = Some(json!({"X-Test": "changed"}));
    let _ = update_provider(State(state.clone()), Path(1), Json(context_update))
        .await
        .unwrap();
    let (supported, checked_at): (i64, Option<String>) = sqlx::query_as(
        "SELECT tool_search_supported, tool_search_checked_at FROM providers WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(supported, 1);
    assert_eq!(checked_at, None);
}

#[test]
fn validates_overview_ranges() {
    let default_start = Utc::now() - Duration::days(13);
    let default_end = Utc::now() + Duration::days(1);
    assert_eq!(
        normalize_overview_range(None, None, default_start, default_end).unwrap(),
        (default_start, default_end)
    );

    let start = Utc::now() - Duration::days(7);
    let end = Utc::now();
    assert_eq!(
        normalize_overview_range(
            Some(&start.to_rfc3339()),
            Some(&end.to_rfc3339()),
            default_start,
            default_end,
        )
        .unwrap(),
        (start, end)
    );
    assert!(
        normalize_overview_range(Some("not-a-date"), None, default_start, default_end).is_err()
    );
    assert!(
        normalize_overview_range(
            Some(&end.to_rfc3339()),
            Some(&start.to_rfc3339()),
            default_start,
            default_end,
        )
        .is_err()
    );
    assert!(
        normalize_overview_range(
            Some(&(Utc::now() - Duration::days(367)).to_rfc3339()),
            Some(&Utc::now().to_rfc3339()),
            default_start,
            default_end,
        )
        .is_err()
    );
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
async fn provider_model_sync_rejects_duplicate_runs() {
    let state = provider_key_test_state().await;
    state.provider_model_sync.lock().await.insert(1);

    let error = sync_provider(state, 1).await.unwrap_err();
    assert!(
        matches!(error, AppError::Conflict(message) if message.contains("already in progress"))
    );
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

#[test]
fn escapes_csv_fields() {
    assert_eq!(csv_field("plain"), "plain");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
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

#[test]
fn parses_ollama_style_model_lists() {
    let value = json!({ "models": [
        { "name": "llama3:8b", "model": "llama3:8b" },
        { "model": "qwen2:7b" }
    ]});
    assert_eq!(parse_ollama_models(&value), vec!["llama3:8b", "qwen2:7b"]);
}

#[test]
fn ignores_empty_model_entries() {
    let value = json!({ "data": [{ "id": "  " }, { "id": "real-model" }] });
    let names = parse_openai_model_entries(&value)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["real-model"]);
}

#[test]
fn accepts_valid_glob_model_patterns() {
    assert!(validate_model_pattern("gpt-*").is_ok());
    assert!(validate_model_pattern("*").is_ok());
    assert!(validate_model_pattern("vendor/model-?").is_ok());
    assert!(validate_model_pattern("exact-model").is_ok());
}

#[test]
fn rejects_uncompilable_glob_model_patterns() {
    assert!(validate_model_pattern("unclosed[").is_err());
}

#[test]
fn rejects_route_when_every_target_is_disabled() {
    let disabled = vec![RouteTargetInput {
        id: None,
        provider_id: 1,
        upstream_model: "m".to_string(),
        weight: 100,
        priority: 0,
        enabled: false,
    }];
    assert!(validate_targets(&disabled).is_err());

    let enabled = vec![RouteTargetInput {
        id: None,
        provider_id: 1,
        upstream_model: "m".to_string(),
        weight: 100,
        priority: 0,
        enabled: true,
    }];
    assert!(validate_targets(&enabled).is_ok());
}

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
    run_due_usage_retention(state).await;

    let request_ids =
        sqlx::query_scalar::<_, String>("SELECT request_id FROM usage_logs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(request_ids, vec!["recent"]);
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
            "idx_usage_completed_model_created",
            "idx_usage_completed_provider_created",
            "idx_usage_completed_provider_key_created",
            "idx_usage_completed_route_created",
        ]
    );
}

#[tokio::test]
async fn overview_range_filters_dashboard_usage() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let old = (Utc::now() - Duration::days(20)).to_rfc3339();
    let recent = (Utc::now() - Duration::days(1)).to_rfc3339();
    let today = Utc::now().to_rfc3339();
    for (request_id, session_id, prompt, completion, cache_read, latency_ms, success, created_at) in [
        (
            "old",
            Some("old-session"),
            700_i64,
            200_i64,
            0_i64,
            500_i64,
            1_i64,
            old,
        ),
        (
            "recent",
            Some("shared-session"),
            60,
            40,
            20,
            100,
            1,
            recent.clone(),
        ),
        (
            "recent-2",
            Some("shared-session"),
            30,
            10,
            5,
            200,
            1,
            recent,
        ),
        ("today", None, 50, 0, 0, 300, 0, today),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, session_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, cache_read_tokens, latency_ms,
                    status_code, success, created_at
                 ) VALUES (?, ?, 'test-model', '/v1/chat/completions', ?, ?, ?, ?, ?, 200, ?, ?)",
        )
        .bind(request_id)
        .bind(session_id)
        .bind(prompt)
        .bind(completion)
        .bind(prompt + completion)
        .bind(cache_read)
        .bind(latency_ms)
        .bind(success)
        .bind(created_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                total_tokens, status_code, in_flight, success, created_at
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 25, 25, 0, 1, 0,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE usage_logs SET warning_message = 'repaired request'
             WHERE request_id = 'recent-2'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (name, provider_type, base_url, enabled, last_test_ok)
             VALUES
                ('healthy', 'openai', 'https://healthy.example/v1', 1, 1),
                ('failed', 'openai', 'https://failed.example/v1', 1, 0),
                ('untested', 'openai', 'https://untested.example/v1', 1, NULL),
                ('disabled', 'openai', 'https://disabled.example/v1', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (
                provider_id, name, secret, enabled, last_test_ok, last_error
             ) VALUES
                (1, 'Healthy', 'sk-healthy', 1, 1, NULL),
                (2, 'Failed', 'sk-failed', 1, 0, 'upstream 401'),
                (3, 'Untested', 'sk-untested', 1, NULL, NULL),
                (4, 'Disabled provider', 'sk-disabled-provider', 1, 1, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    state.provider_key_cooldown.lock().await.insert(
        99,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    );
    state.provider_cooldown.lock().await.insert(
        2,
        std::time::Instant::now() + std::time::Duration::from_secs(90),
    );
    let range_start = Utc::now() - Duration::days(2);
    let range_end = Utc::now() + Duration::days(1);
    let Json(view) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            include_session_metrics: true,
        }),
    )
    .await
    .unwrap();

    assert_eq!(view.requests_today, 1);
    assert_eq!(view.requests_total, 4);
    assert_eq!(view.prompt_tokens_today, 50);
    assert_eq!(view.completion_tokens_today, 0);
    assert_eq!(view.prompt_tokens_total, 840);
    assert_eq!(view.completion_tokens_total, 250);
    assert_eq!(view.range_requests, 3);
    assert_eq!(view.range_tokens, 190);
    assert_eq!(view.range_prompt_tokens, 140);
    assert_eq!(view.range_completion_tokens, 50);
    assert_eq!(view.range_cache_read, 25);
    assert_eq!(view.range_sessions, 1);
    assert!((view.range_session_coverage - 66.666_666).abs() < 0.001);
    assert_eq!(view.range_avg_requests_per_session, 2.0);
    assert!((view.range_session_cache_hit_rate - 27.777_777).abs() < 0.001);
    assert!((view.range_success_rate - 66.666_666).abs() < 0.001);
    assert_eq!(view.range_avg_latency_ms, 200.0);
    assert_eq!(view.range_gateway_adjusted, 1);
    assert_eq!(view.active_providers, 3);
    assert_eq!(view.healthy_providers, 1);
    assert_eq!(view.failed_providers, 1);
    assert_eq!(view.untested_providers, 1);
    assert_eq!(view.provider_keys_total, 3);
    assert_eq!(view.healthy_provider_keys, 1);
    assert_eq!(view.failed_provider_keys, 1);
    assert_eq!(view.untested_provider_keys, 1);
    assert_eq!(view.runtime_error_provider_keys, 1);
    assert_eq!(view.cooling_providers, 1);
    assert_eq!(view.cooling_provider_keys, 1);
    assert_eq!(view.in_flight_requests, 1);
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.prompt_tokens)
            .sum::<i64>(),
        view.range_prompt_tokens
    );
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.completion_tokens)
            .sum::<i64>(),
        view.range_completion_tokens
    );
    assert_eq!(view.recent_requests.len(), 4);
    assert!(view.recent_requests.iter().any(|row| row.in_flight));
    assert!(
        view.recent_requests
            .iter()
            .all(|row| row.request_id != "old")
    );
    assert_eq!(view.model_usage[0].requests, 3);
    assert_eq!(view.model_usage[0].tokens, 190);
    assert_eq!(view.model_usage[0].prompt_tokens, 140);
    assert_eq!(view.model_usage[0].completion_tokens, 50);

    let Json(view_without_sessions) = overview(
        State(state),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            include_session_metrics: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(view_without_sessions.range_requests, view.range_requests);
    assert_eq!(view_without_sessions.range_sessions, 0);
    assert_eq!(view_without_sessions.range_session_coverage, 0.0);
}

#[tokio::test]
async fn usage_lifetime_stats_tracks_completion_and_cleanup() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                completion_tokens, total_tokens, cache_read_tokens,
                cache_write_tokens, latency_ms, status_code, in_flight, success
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 10,
                20, 30, 5, 7, 100, 0, 1, 1
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let pending: i64 = sqlx::query_scalar("SELECT requests FROM usage_lifetime_stats WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);

    sqlx::query(
        "UPDATE usage_logs
             SET in_flight = 0, status_code = 200, success = 1,
                 estimated_cost_micros = 42
             WHERE request_id = 'pending'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let completed: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT requests, tokens, prompt_tokens, completion_tokens,
                    cache_read_tokens, cache_write_tokens, cost_micros,
                    successful_requests, latency_ms_sum
             FROM usage_lifetime_stats WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completed, (1, 30, 10, 20, 5, 7, 42, 1, 100));

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'pending'")
        .execute(&pool)
        .await
        .unwrap();
    let cleaned: (i64, i64, i64) = sqlx::query_as(
        "SELECT requests, tokens, cost_micros
             FROM usage_lifetime_stats WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cleaned, (0, 0, 0));
}

#[tokio::test]
async fn usage_filter_can_select_in_flight_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens, total_tokens,
                latency_ms, status_code, in_flight, success, created_at
             ) VALUES
                ('pending', 'm', '/v1/chat/completions', 10, 10, 0, 0, 1, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('finished', 'm', '/v1/chat/completions', 10, 20, 100, 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: Some(true),
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "pending");
    assert!(page.items[0].in_flight);
}

#[tokio::test]
async fn usage_filter_can_select_endpoint() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('chat', 'm', '/v1/chat/completions', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('responses', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: Some("/v1/responses".to_string()),
            success: None,
            in_flight: None,
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "responses");
}

#[tokio::test]
async fn usage_filter_can_select_gateway_adjusted_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success, warning_message, created_at
             ) VALUES
                ('adjusted', 'm', '/v1/chat/completions', 200, 0, 1,
                 'repaired request', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('original', 'm', '/v1/chat/completions', 200, 0, 1,
                 NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(adjusted) = list_usage(
        State(state.clone()),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: Some(true),
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(adjusted.total, 1);
    assert_eq!(adjusted.items[0].request_id, "adjusted");

    let Json(original) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: Some(false),
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(original.total, 1);
    assert_eq!(original.items[0].request_id, "original");
}

#[tokio::test]
async fn usage_filter_can_select_session() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, session_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('session-a', 'codex-session-a', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('session-b', 'codex-session-b', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: Some("session-a".to_string()),
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "session-a");
    assert_eq!(page.items[0].session_id.as_deref(), Some("codex-session-a"));
}

#[tokio::test]
async fn reconciles_stale_and_interrupted_usage_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let stale = (Utc::now() - Duration::days(2)).to_rfc3339();
    let recent = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at
             ) VALUES
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('recent', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('done', 'm', '/v1/chat/completions', 200, 0, 1, ?)",
    )
    .bind(stale)
    .bind(recent)
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let cutoff = (Utc::now() - Duration::days(1)).to_rfc3339();
    let stale_updated = finish_interrupted_usage_requests(
        &state,
        Some(&cutoff),
        "request was interrupted before completion",
    )
    .await
    .unwrap();
    assert_eq!(stale_updated, 1);
    let stale_row: (i64, i64, String) = sqlx::query_as(
        "SELECT in_flight, status_code, error_message
             FROM usage_logs WHERE request_id = 'stale'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale_row.0, 0);
    assert_eq!(stale_row.1, 499);
    assert_eq!(stale_row.2, "request was interrupted before completion");

    let startup_updated = reconcile_interrupted_usage_requests(&state).await.unwrap();
    assert_eq!(startup_updated, 1);
    let statuses = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("done".to_string(), 0, 200),
            ("recent".to_string(), 0, 499),
            ("stale".to_string(), 0, 499),
        ]
    );
}

#[tokio::test]
async fn stale_reconciliation_uses_last_activity_for_long_streams() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let old_created = (Utc::now() - Duration::hours(2)).to_rfc3339();
    let active = Utc::now().to_rfc3339();
    let stale_activity = (Utc::now() - Duration::minutes(20)).to_rfc3339();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at, last_activity_at
             ) VALUES
                ('active', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?),
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?)",
    )
    .bind(&old_created)
    .bind(active)
    .bind(&old_created)
    .bind(stale_activity)
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    reconcile_stale_usage_requests(state).await;
    let rows = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![("active".to_string(), 1, 0), ("stale".to_string(), 0, 499)]
    );
}
