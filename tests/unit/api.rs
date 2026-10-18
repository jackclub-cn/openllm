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

/// Every exported sample must belong to a family that declared its type.
///
/// Prometheus tolerates a missing declaration by treating the family as
/// untyped, which silently breaks `rate()` / `increase()` semantics on counters
/// and drops the family's meaning in dashboards. Keeping the whole exposition
/// self-consistent is cheap; relying on reviewers to spot one missing header is
/// not.
fn assert_every_metric_family_declares_a_type(body: &str) {
    let declared = body
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    for line in body.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let family = line.split(['{', ' ']).next().unwrap_or_default();
        assert!(
            declared.contains(family),
            "metric family '{family}' is exported without a # TYPE declaration"
        );
    }
}

#[test]
#[should_panic(expected = "without a # TYPE declaration")]
fn metric_family_guard_detects_a_missing_type() {
    assert_every_metric_family_declares_a_type(
        "# HELP openllm_declared Declared family\n\
         # TYPE openllm_declared gauge\n\
         openllm_declared 1\n\
         openllm_undeclared 2\n",
    );
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
            id, name, provider_type, base_url, enabled, last_test_ok, max_concurrency
         ) VALUES
            (1, 'Healthy provider', 'openai', 'https://healthy.example/v1', 1, 1, 3),
            (2, 'Disabled provider', 'openai', 'https://disabled.example/v1', 0, 0, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled, max_concurrency)
         VALUES (1, 'model-a', 1, 2), (1, 'model-b', 0, NULL), (2, 'model-c', 1, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
         VALUES (1, 1, 'Primary', 'sk-primary', 1),
                (2, 1, 'Backup', 'sk-backup', 1)",
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
    let state = AppState::new(pool, Some("metrics-secret".to_string()));
    state
        .provider_concurrency
        .lock()
        .await
        .insert(1, std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
    state.model_concurrency.lock().await.insert(
        (1, "model-a".to_string()),
        std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    );
    state.target_cooldown.lock().await.insert(
        (1, "model-a".to_string()),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    state.provider_key_cooldown.lock().await.insert(
        1,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    state.failure_classifications.lock().await.insert(
        (
            "provider".to_string(),
            "next_target".to_string(),
            "upstream_unavailable".to_string(),
        ),
        2,
    );
    let router = crate::build_router(state);

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
    assert!(body.contains("# TYPE openllm_uptime_seconds gauge"));
    assert!(body.contains("# TYPE openllm_start_time_seconds gauge"));
    assert!(body.contains("# TYPE openllm_inflight_request_bytes gauge"));
    assert!(body.contains("# TYPE openllm_inflight_request_bytes_limit gauge"));
    assert!(body.contains("# TYPE openllm_request_bytes_shed_total counter"));
    assert!(body.contains("openllm_inflight_request_bytes 0"));
    assert!(body.contains("openllm_request_bytes_shed_total 0"));
    assert!(body.contains("# TYPE openllm_memory_limit_bytes gauge"));
    assert!(body.contains("# TYPE openllm_memory_pressure gauge"));
    assert!(body.contains("openllm_memory_pressure 0"));
    assert!(body.contains("openllm_memory_shed_total 0"));
    assert!(body.contains("# TYPE openllm_db_pool_connections gauge"));
    assert!(body.contains("# TYPE openllm_db_pool_idle gauge"));
    assert!(body.contains("# TYPE openllm_db_pool_max_connections gauge"));
    assert!(body.contains("# TYPE openllm_runtime_lag_millis gauge"));
    assert!(body.contains("openllm_runtime_lag_millis 0"));
    assert!(body.contains("# TYPE openllm_session_affinity_entries gauge"));
    assert!(body.contains("# TYPE openllm_session_affinity_hits_total counter"));
    assert!(body.contains("openllm_session_affinity_entries 0"));
    assert!(body.contains("# TYPE openllm_maintenance_runs_total counter"));
    assert!(body.contains("openllm_maintenance_runs_total 0"));
    assert!(body.contains("openllm_maintenance_last_run_timestamp_seconds 0"));
    assert!(body.contains("openllm_providers{state=\"healthy\"} 1"));
    assert!(body.contains("openllm_providers{state=\"disabled\"} 1"));
    assert!(
        body.contains("openllm_provider_health{provider=\"Healthy provider\",state=\"healthy\"} 1")
    );
    assert!(body.contains("openllm_provider_models{provider=\"Healthy provider\"} 1"));
    assert!(body.contains("openllm_provider_models_disabled{provider=\"Healthy provider\"} 1"));
    assert!(body.contains(
        "openllm_provider_circuit_state{provider=\"Healthy provider\",state=\"closed\"} 1"
    ));
    assert!(body.contains("openllm_provider_failure_streak{provider=\"Healthy provider\"} 0"));
    assert!(body.contains("openllm_provider_circuit_trips_total{provider=\"Healthy provider\"} 0"));
    assert!(body.contains("openllm_provider_concurrency_limit{provider=\"Healthy provider\"} 3"));
    assert!(body.contains("openllm_provider_inflight{provider=\"Healthy provider\"} 2"));
    assert!(
        body.contains("openllm_model_cooling{provider=\"Healthy provider\",model=\"model-a\"} 1")
    );
    assert!(
        body.contains("openllm_model_cooling{provider=\"Healthy provider\",model=\"model-b\"} 0")
    );
    assert!(body.contains(
        "openllm_model_concurrency_limit{provider=\"Healthy provider\",model=\"model-a\"} 2"
    ));
    assert!(
        body.contains("openllm_model_inflight{provider=\"Healthy provider\",model=\"model-a\"} 1")
    );
    assert!(body.contains(
        "openllm_provider_key_cooling{provider=\"Healthy provider\",key=\"Primary\",key_id=\"1\"} 1"
    ));
    assert!(body.contains(
        "openllm_provider_key_cooling{provider=\"Healthy provider\",key=\"Backup\",key_id=\"2\"} 0"
    ));
    assert!(body.contains("openllm_webhooks{state=\"enabled\"} 1"));
    assert!(body.contains("openllm_webhook_deliveries{state=\"succeeded\"} 1"));
    assert!(body.contains("openllm_webhook_deliveries{state=\"failed\"} 1"));
    assert!(body.contains("openllm_audit_logs 0"));
    assert!(body.contains("openllm_database_size_bytes"));
    assert!(body.contains("openllm_ready 1"));
    assert!(body.contains("# TYPE openllm_ready gauge"));
    assert!(body.contains("# TYPE openllm_pool_requests_window gauge"));
    assert!(body.contains("# TYPE openllm_pool_request_limit gauge"));
    assert!(body.contains("# TYPE openllm_pool_tokens_window gauge"));
    assert!(body.contains("# TYPE openllm_pool_token_limit gauge"));
    assert!(body.contains("# TYPE openllm_pool_saturated gauge"));
    assert!(body.contains("# TYPE openllm_pool_denials_total counter"));
    assert!(body.contains("# TYPE openllm_pool_key_requests_window gauge"));
    assert!(body.contains("# TYPE openllm_pool_key_request_share gauge"));
    assert!(body.contains("# TYPE openllm_pool_key_tokens_window gauge"));
    assert!(body.contains("# TYPE openllm_pool_key_token_share gauge"));
    assert!(body.contains("# TYPE openllm_provider_cooling gauge"));
    assert!(body.contains("# TYPE openllm_provider_circuit_state gauge"));
    assert!(body.contains("# TYPE openllm_provider_failure_streak gauge"));
    assert!(body.contains("# TYPE openllm_provider_circuit_trips_total counter"));
    assert!(body.contains("# TYPE openllm_upstream_failures_total counter"));
    assert!(body.contains(
        "openllm_upstream_failures_total{scope=\"provider\",retry=\"next_target\",reason=\"upstream_unavailable\"} 2"
    ));
    assert!(body.contains("# TYPE openllm_provider_requests_total counter"));
    assert!(body.contains("# TYPE openllm_provider_average_latency_ms gauge"));
    assert_every_metric_family_declares_a_type(&body);
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
        model_auto_include_rules: Vec::new(),
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
        timeout_seconds: None,
        configured_cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
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
        max_concurrency: None,
        requests_per_minute: None,
        queue_limit: None,
        queue_wait_ms: None,
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
        model_auto_include_rules: None,
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
        timeout_seconds: None,
        configured_cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
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
fn normalizes_and_validates_model_auto_include_rules() {
    let rules = normalize_model_auto_include_rules(&[
        " gpt-* ".to_string(),
        "gpt-*".to_string(),
        "re:^deepseek-".to_string(),
        String::new(),
    ])
    .unwrap();
    assert_eq!(rules, vec!["gpt-*", "re:^deepseek-"]);
    assert!(normalize_model_auto_include_rules(&["[".to_string()]).is_err());
    assert!(normalize_model_auto_include_rules(&["re:(".to_string()]).is_err());
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
    assert_eq!(command_code_monthly_total("individual-pro"), Some(30.0));
    assert_eq!(command_code_monthly_total("individual-pro-v1"), Some(80.0));
    assert_eq!(command_code_monthly_total("individual-max"), Some(150.0));
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
fn derives_command_code_monthly_total_from_the_ledger_and_snaps_races() {
    // Separate usage and remaining reads can race by a few cents during a
    // request; the published plan total must still win.
    assert_eq!(
        command_code_monthly_total_value(Some(62.53), Some(7.46), Some("individual-goat")),
        Some(70.0)
    );
    // A real plan or promotion change larger than the race tolerance is kept.
    assert_eq!(
        command_code_monthly_total_value(Some(72.5), Some(7.5), Some("individual-goat")),
        Some(80.0)
    );
    // An unlisted plan still gets a total from the live ledger.
    assert_eq!(
        command_code_monthly_total_value(Some(12.25), Some(7.75), Some("future-plan")),
        Some(20.0)
    );
    // With no ledger reads, the published fallback still works.
    assert_eq!(
        command_code_monthly_total_value(None, None, Some("individual-pro")),
        Some(30.0)
    );
}

#[test]
fn parses_command_code_model_monthly_credits() {
    let html = r#"
        <table>
          <thead>
            <tr><th>Model</th><th>Input</th><th>Monthly credits</th></tr>
          </thead>
          <tbody>
            <tr>
              <td><button>DeepSeek V4.1 Flash</button></td>
              <td>$0.15</td>
              <td><button>$80 <span>$100</span> through Oct 16th</button></td>
            </tr>
            <tr>
              <td>GLM-5.2</td>
              <td>$1.40</td>
              <td>$70</td>
            </tr>
          </tbody>
        </table>
    "#;
    let models = parse_command_code_plan_credits(html);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_name, "DeepSeek V4.1 Flash");
    assert_eq!(models[0].monthly_credits, Some(80.0));
    assert_eq!(models[0].promo_credits, Some(100.0));
    assert_eq!(models[0].note.as_deref(), Some("through Oct 16th"));
    assert_eq!(models[1].model_name, "GLM-5.2");
    assert_eq!(models[1].monthly_credits, Some(70.0));
    assert_eq!(models[1].promo_credits, None);
    assert_eq!(models[1].note, None);
}

#[test]
fn maps_command_code_model_names_to_provider_ids() {
    let mut models = vec![ProviderQuotaModel {
        model_name: "DeepSeek V4.1 Flash".to_string(),
        model_id: None,
        monthly_credits: Some(80.0),
        promo_credits: Some(100.0),
        note: Some("through Oct 16th".to_string()),
    }];
    apply_command_code_model_ids(
        &mut models,
        &json!({
            "data": [
                {
                    "id": "deepseek/deepseek-v4.1-flash",
                    "name": "DeepSeek V4.1 Flash"
                }
            ]
        }),
    );
    assert_eq!(
        models[0].model_id.as_deref(),
        Some("deepseek/deepseek-v4.1-flash")
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

#[test]
fn validates_provider_health_interval() {
    assert_eq!(normalize_health_interval(None).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(0)).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(30)).unwrap(), Some(30));
    assert!(normalize_health_interval(Some(-1)).is_err());
}

#[test]
fn validates_provider_request_policy() {
    assert_eq!(normalize_provider_timeout(None).unwrap(), None);
    assert_eq!(normalize_provider_timeout(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_timeout(Some(120)).unwrap(), Some(120));
    assert!(normalize_provider_timeout(Some(-1)).is_err());
    assert!(normalize_provider_timeout(Some(3601)).is_err());

    assert_eq!(normalize_provider_cooldown(None).unwrap(), None);
    assert_eq!(normalize_provider_cooldown(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_cooldown(Some(900)).unwrap(), Some(900));
    assert!(normalize_provider_cooldown(Some(-1)).is_err());
    assert!(normalize_provider_cooldown(Some(3601)).is_err());

    assert_eq!(normalize_provider_concurrency(None).unwrap(), None);
    assert_eq!(normalize_provider_concurrency(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_concurrency(Some(4)).unwrap(), Some(4));
    assert!(normalize_provider_concurrency(Some(-1)).is_err());
    assert!(normalize_provider_concurrency(Some(1001)).is_err());

    assert_eq!(normalize_provider_queue_timeout(None).unwrap(), None);
    assert_eq!(normalize_provider_queue_timeout(Some(0)).unwrap(), Some(0));
    assert_eq!(
        normalize_provider_queue_timeout(Some(30)).unwrap(),
        Some(30)
    );
    assert!(normalize_provider_queue_timeout(Some(-1)).is_err());
    assert!(normalize_provider_queue_timeout(Some(301)).is_err());
}

#[tokio::test]
async fn provider_request_policy_round_trips_and_can_be_cleared() {
    let state = provider_key_test_state().await;
    let mut input = provider_input(None, Vec::new());
    input.timeout_seconds = Some(180);
    input.configured_cooldown_seconds = Some(600);
    input.max_concurrency = Some(3);
    input.queue_timeout_seconds = Some(0);
    let (_, Json(created)) = create_provider(State(state.clone()), Json(input))
        .await
        .unwrap();
    assert_eq!(created.timeout_seconds, Some(180));
    assert_eq!(created.configured_cooldown_seconds, Some(600));
    assert_eq!(created.max_concurrency, Some(3));
    assert_eq!(created.queue_timeout_seconds, Some(0));
    assert_eq!(created.cooldown_seconds, None);
    state.provider_concurrency.lock().await.insert(
        created.id,
        std::sync::Arc::new(tokio::sync::Semaphore::new(3)),
    );

    let mut update = provider_update_with_keys(None);
    update.timeout_seconds = Some(0);
    update.configured_cooldown_seconds = Some(0);
    update.max_concurrency = Some(0);
    update.queue_timeout_seconds = Some(0);
    let Json(updated) = update_provider(State(state.clone()), Path(created.id), Json(update))
        .await
        .unwrap();
    assert_eq!(updated.timeout_seconds, None);
    assert_eq!(updated.configured_cooldown_seconds, None);
    assert_eq!(updated.max_concurrency, None);
    assert_eq!(updated.queue_timeout_seconds, Some(0));
    assert!(
        !state
            .provider_concurrency
            .lock()
            .await
            .contains_key(&created.id)
    );
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
        state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&first_id),
        "editing metadata without replacing the secret must preserve the key cooldown"
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
    assert!(
        !state
            .provider_key_cooldown
            .lock()
            .await
            .contains_key(&first_id),
        "removing a key must clear its runtime cooldown"
    );
    let mirrored: Option<String> = sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(mirrored, None);
}

#[tokio::test]
async fn provider_api_key_channel_limits_round_trip_through_the_api() {
    let state = provider_key_test_state().await;
    let mut primary = provider_key_input(None, "Primary", Some("sk-limited"), true);
    primary.max_concurrency = Some(3);
    primary.requests_per_minute = Some(120);
    primary.queue_limit = Some(10);
    primary.queue_wait_ms = Some(1_500);
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(None, vec![primary])),
    )
    .await
    .unwrap();
    assert_eq!(created.api_keys[0].max_concurrency, Some(3));
    assert_eq!(created.api_keys[0].requests_per_minute, Some(120));
    assert_eq!(created.api_keys[0].queue_limit, Some(10));
    assert_eq!(created.api_keys[0].queue_wait_ms, Some(1_500));

    let mut updated_key = provider_key_input(Some(created.api_keys[0].id), "Primary", None, true);
    updated_key.max_concurrency = Some(5);
    updated_key.requests_per_minute = Some(240);
    updated_key.queue_limit = Some(0);
    updated_key.queue_wait_ms = Some(0);
    let Json(updated) = update_provider(
        State(state.clone()),
        Path(created.id),
        Json(provider_update_with_keys(Some(vec![updated_key]))),
    )
    .await
    .unwrap();
    assert_eq!(updated.api_keys[0].max_concurrency, Some(5));
    assert_eq!(updated.api_keys[0].requests_per_minute, Some(240));
    assert_eq!(updated.api_keys[0].queue_limit, Some(0));
    assert_eq!(updated.api_keys[0].queue_wait_ms, Some(0));
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
async fn quota_snapshots_persist_manual_overrides_and_restore_to_cache() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'quota-provider', 'openai', 'https://opencode.ai/zen/go/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) \
         VALUES (11, 1, 'Primary', 'sk-quota')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let view = ProviderQuotaView {
        kind: "opencode_go".to_string(),
        title: "OpenCode Go 额度".to_string(),
        plan_name: Some("Go".to_string()),
        key_id: Some(11),
        key_name: Some("Primary".to_string()),
        key_suffix: Some("quota".to_string()),
        source_url: None,
        items: vec![ProviderQuotaItem {
            key: "rolling".to_string(),
            label: "5 小时".to_string(),
            used: None,
            limit: None,
            remaining: None,
            unit: "%".to_string(),
            percent: Some(95.0),
            reset_at: None,
        }],
        model_quotas: vec![ProviderQuotaModel {
            model_name: "Model A".to_string(),
            model_id: Some("model-a".to_string()),
            monthly_credits: Some(80.0),
            promo_credits: Some(100.0),
            note: None,
        }],
        details: Vec::new(),
        prices: Vec::new(),
        snapshots: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    };

    let snapshots = crate::quota::store_provider_quota_view(&state, 1, Some(11), &view)
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 2);
    let window = snapshots
        .iter()
        .find(|snapshot| snapshot.model.is_empty())
        .unwrap();
    assert_eq!(window.window_key, "5h");
    assert_eq!(window.state, "low");
    assert_eq!(window.percent, Some(95.0));
    let model = snapshots
        .iter()
        .find(|snapshot| snapshot.model == "model-a")
        .unwrap();
    assert_eq!(model.state, "unknown");
    assert_eq!(model.limit, Some(100.0));

    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_quota_snapshots WHERE provider_id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 2);

    let Json(listed) = list_provider_quota_snapshots(
        State(state.clone()),
        Path(1),
        Query(ProviderQuotaQuery { key_id: Some(11) }),
    )
    .await
    .unwrap();
    assert_eq!(listed.len(), 2);

    let Json(updated) = update_provider_quota_snapshots(
        State(state.clone()),
        Path(1),
        Json(ProviderQuotaSnapshotRequest {
            snapshots: vec![ProviderQuotaSnapshotInput {
                key_id: Some(11),
                model: None,
                window_key: "5h".to_string(),
                used: Some(20.0),
                limit: Some(100.0),
                remaining: Some(80.0),
                unit: "percent".to_string(),
                reset_at: None,
                valid_seconds: Some(3_600),
            }],
        }),
    )
    .await
    .unwrap();
    let window = updated
        .iter()
        .find(|snapshot| snapshot.model.is_empty())
        .unwrap();
    assert_eq!(window.source, "manual");
    assert_eq!(window.state, "healthy");
    assert_eq!(window.used, Some(20.0));

    let restarted = AppState::new(state.pool.clone(), None);
    crate::quota::restore(&restarted).await.unwrap();
    let cache = restarted.quota_snapshots.lock().await;
    assert!(cache.contains_key(&ProviderQuotaSnapshotKey {
        provider_id: 1,
        provider_key_id: Some(11),
        model: String::new(),
        window_key: "5h".to_string(),
        unit: "percent".to_string(),
        source: "manual".to_string(),
    }));
}

#[test]
fn provider_quota_refresh_interval_parses_and_clamps() {
    assert_eq!(parse_provider_quota_refresh_interval(Some("0")), None);
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("15")),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("invalid")),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("999999999")),
        Some(std::time::Duration::from_secs(7 * 24 * 60 * 60))
    );
}

#[tokio::test]
async fn provider_quota_overview_groups_database_snapshots_without_upstream_calls() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled) VALUES \
         (1, 'OpenCode Primary', 'openai', 'https://opencode.ai/zen/go/v1', 1), \
         (2, 'Unsupported', 'openai', 'https://api.openai.com/v1', 1), \
         (3, 'OpenCode Backup', 'openai', 'https://opencode.ai/zen/go/v1', 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) \
         VALUES (11, 1, 'Primary', 'sk-quota', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    crate::quota::upsert_manual_snapshots(
        &state,
        1,
        &[ProviderQuotaSnapshotInput {
            key_id: Some(11),
            model: None,
            window_key: "5h".to_string(),
            used: Some(20.0),
            limit: Some(100.0),
            remaining: Some(80.0),
            unit: "percent".to_string(),
            reset_at: None,
            valid_seconds: Some(3_600),
        }],
    )
    .await
    .unwrap();

    let Json(overview) = provider_quota_overview(State(state.clone())).await.unwrap();
    assert_eq!(
        overview
            .iter()
            .map(|item| item.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(overview[0].provider_name, "OpenCode Primary");
    assert!(overview[0].provider_enabled);
    assert_eq!(overview[0].snapshots.len(), 1);
    let snapshot = &overview[0].snapshots[0];
    assert_eq!(snapshot.key_id, Some(11));
    assert_eq!(snapshot.key_name.as_deref(), Some("Primary"));
    assert_eq!(snapshot.key_suffix.as_deref(), Some("uota"));
    assert_eq!(snapshot.state, "healthy");
    assert_eq!(snapshot.remaining, Some(80.0));
    assert!(!overview[1].provider_enabled);
    assert!(overview[1].snapshots.is_empty());
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
async fn guardrail_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    let Json(updated) = update_guardrails_settings(
        State(state.clone()),
        Json(GuardrailSettings {
            blocked_terms: vec![
                " secret ".to_string(),
                "SECRET".to_string(),
                "api-key".to_string(),
            ],
            max_prompt_tokens: Some(4096),
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.blocked_terms, vec!["secret", "api-key"]);
    assert_eq!(updated.max_prompt_tokens, Some(4096));

    let Json(loaded) = get_guardrails_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let error = update_guardrails_settings(
        State(state.clone()),
        Json(GuardrailSettings {
            blocked_terms: vec!["x".to_string()],
            max_prompt_tokens: None,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    let Json(cleared) =
        update_guardrails_settings(State(state.clone()), Json(GuardrailSettings::default()))
            .await
            .unwrap();
    assert!(cleared.is_empty());
    let Json(loaded) = get_guardrails_settings(State(state)).await.unwrap();
    assert!(loaded.is_empty());
}

#[tokio::test]
async fn inspector_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    let Json(updated) = update_inspector_settings(
        State(state.clone()),
        Json(InspectorSettings {
            capture_request_previews: true,
            request_preview_max_chars: 8192,
        }),
    )
    .await
    .unwrap();
    assert!(updated.capture_request_previews);
    assert_eq!(updated.request_preview_max_chars, 8192);

    let Json(loaded) = get_inspector_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let error = update_inspector_settings(
        State(state.clone()),
        Json(InspectorSettings {
            capture_request_previews: true,
            request_preview_max_chars: 64,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    let Json(defaults) =
        update_inspector_settings(State(state.clone()), Json(InspectorSettings::default()))
            .await
            .unwrap();
    assert_eq!(defaults, InspectorSettings::default());
}

#[tokio::test]
async fn resilience_settings_round_trip_validate_and_invalidate_cache() {
    let state = provider_key_test_state().await;
    assert_eq!(
        state.resilience_settings().await.unwrap(),
        ResilienceSettings::default()
    );

    let Json(updated) = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: true,
            stream_recovery_max_retries: 3,
            max_retries: 3,
            retry_backoff_ms: 250,
            retry_max_backoff_ms: 4_000,
        }),
    )
    .await
    .unwrap();
    assert!(updated.stream_recovery_enabled);
    assert_eq!(updated.stream_recovery_max_retries, 3);
    assert_eq!(updated.max_retries, 3);
    // The stale cache must be invalidated so the new policy takes effect.
    assert_eq!(state.resilience_settings().await.unwrap(), updated);
    let Json(loaded) = get_resilience_settings(State(state.clone())).await.unwrap();
    assert_eq!(loaded, updated);

    let too_many = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            max_retries: 9,
            ..ResilienceSettings::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(too_many, AppError::BadRequest(_)));

    let inverted = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: false,
            stream_recovery_max_retries: 2,
            max_retries: 2,
            retry_backoff_ms: 5_000,
            retry_max_backoff_ms: 100,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(inverted, AppError::BadRequest(_)));

    let out_of_range = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: true,
            stream_recovery_max_retries: STREAM_RECOVERY_MAX_RETRIES + 1,
            ..ResilienceSettings::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(out_of_range, AppError::BadRequest(_)));

    // Turning recovery off drops its budget back to the default so the stored
    // row does not keep a stale value for a disabled feature.
    let Json(off) = update_resilience_settings(
        State(state.clone()),
        Json(ResilienceSettings {
            stream_recovery_enabled: false,
            stream_recovery_max_retries: 4,
            max_retries: 1,
            retry_backoff_ms: 200,
            retry_max_backoff_ms: 2_000,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        off.stream_recovery_max_retries,
        ResilienceSettings::default().stream_recovery_max_retries
    );
    assert!(!off.stream_recovery_enabled);

    // Disabling retries falls back to the default backoff values and clears the
    // stored row so a fresh start uses defaults.
    let Json(cleared) =
        update_resilience_settings(State(state.clone()), Json(ResilienceSettings::default()))
            .await
            .unwrap();
    assert_eq!(cleared, ResilienceSettings::default());
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'resilience'")
            .fetch_optional(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, None);
}

#[tokio::test]
async fn cooldown_manager_lists_and_clears_scoped_cooldowns() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'alpha', 'openai', 'https://alpha.example/v1'),
            (2, 'beta', 'openai', 'https://beta.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) \
         VALUES (11, 1, 'main', 'sk-main')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let now = std::time::Instant::now();
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, now + std::time::Duration::from_secs(60));
    state.provider_failure_streak.lock().await.insert(1, 3);
    state.target_cooldown.lock().await.insert(
        (1, "model-a".to_string()),
        now + std::time::Duration::from_secs(30),
    );
    state.target_cooldown.lock().await.insert(
        (2, "model-b".to_string()),
        now + std::time::Duration::from_secs(30),
    );
    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, now + std::time::Duration::from_secs(45));
    state.provider_failure_meta.lock().await.insert(
        1,
        crate::state::FailureMetadata {
            scope: "provider",
            retry: "next_target",
            confidence: "inferred",
            reason_code: "upstream_unavailable",
            observed_at: now,
        },
    );
    state.target_failure_meta.lock().await.insert(
        (1, "model-a".to_string()),
        crate::state::FailureMetadata {
            scope: "model",
            retry: "next_target",
            confidence: "exact",
            reason_code: "context_limit_exceeded",
            observed_at: now,
        },
    );
    state.provider_key_failure_meta.lock().await.insert(
        11,
        crate::state::FailureMetadata {
            scope: "credential",
            retry: "next_target",
            confidence: "exact",
            reason_code: "quota_exhausted",
            observed_at: now,
        },
    );
    // An already-expired entry must not leak into the snapshot.
    state.target_cooldown.lock().await.insert(
        (2, "stale".to_string()),
        now - std::time::Duration::from_secs(5),
    );

    let Json(snapshot) = list_cooldowns(State(state.clone())).await.unwrap();
    assert_eq!(snapshot.provider_cooldowns.len(), 1);
    assert_eq!(
        snapshot.provider_cooldowns[0].provider_name.as_deref(),
        Some("alpha")
    );
    assert_eq!(snapshot.provider_cooldowns[0].failure_streak, 3);
    assert_eq!(snapshot.provider_cooldowns[0].remaining_seconds, 60);
    assert_eq!(snapshot.provider_cooldowns[0].state, "open");
    assert_eq!(snapshot.provider_cooldowns[0].scope, Some("provider"));
    assert_eq!(snapshot.provider_cooldowns[0].retry, Some("next_target"));
    assert_eq!(
        snapshot.provider_cooldowns[0].reason_code,
        Some("upstream_unavailable")
    );
    assert_eq!(snapshot.model_cooldowns.len(), 2);
    assert_eq!(snapshot.model_cooldowns[0].scope, Some("model"));
    assert_eq!(
        snapshot.model_cooldowns[0].reason_code,
        Some("context_limit_exceeded")
    );
    assert_eq!(snapshot.provider_key_cooldowns.len(), 1);
    assert_eq!(
        snapshot.provider_key_cooldowns[0].key_name.as_deref(),
        Some("main")
    );
    assert_eq!(
        snapshot.provider_key_cooldowns[0].provider_name.as_deref(),
        Some("alpha")
    );
    assert_eq!(snapshot.provider_key_cooldowns[0].scope, Some("credential"));
    assert_eq!(
        snapshot.provider_key_cooldowns[0].reason_code,
        Some("quota_exhausted")
    );

    // An empty body is rejected rather than silently clearing everything.
    let error = clear_cooldowns(State(state.clone()), Json(ClearCooldownsInput::default()))
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::BadRequest(_)));

    // Clearing one model leaves the sibling model and the provider circuit alone.
    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_id: Some(1),
            model: Some("model-a".to_string()),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_model_cooldowns, 1);
    assert_eq!(cleared.cleared_provider_cooldowns, 0);
    assert!(
        !state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "model-a".to_string()))
    );
    assert!(
        !state
            .target_failure_meta
            .lock()
            .await
            .contains_key(&(1, "model-a".to_string()))
    );
    assert!(state.provider_cooldown.lock().await.contains_key(&1));

    // A provider-scoped clear also resets the escalation streak.
    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_id: Some(1),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_provider_cooldowns, 1);
    assert!(!state.provider_cooldown.lock().await.contains_key(&1));
    assert!(!state.provider_failure_meta.lock().await.contains_key(&1));
    assert!(!state.provider_failure_streak.lock().await.contains_key(&1));

    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            provider_key_id: Some(11),
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_provider_key_cooldowns, 1);
    assert!(!state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(
        !state
            .provider_key_failure_meta
            .lock()
            .await
            .contains_key(&11)
    );

    let Json(cleared) = clear_cooldowns(
        State(state.clone()),
        Json(ClearCooldownsInput {
            all: true,
            ..ClearCooldownsInput::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared.cleared_model_cooldowns, 1);
    let Json(snapshot) = list_cooldowns(State(state.clone())).await.unwrap();
    assert!(snapshot.model_cooldowns.is_empty());
    assert!(snapshot.provider_cooldowns.is_empty());
    assert!(snapshot.provider_key_cooldowns.is_empty());
}

/// A provider whose cooldown elapsed but whose failure streak still trips the
/// breaker is half-open: no active cooldown, yet it must not be invisible to an
/// operator wondering why it keeps failing fast.
#[tokio::test]
async fn cooldown_snapshot_surfaces_half_open_circuits() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let now = std::time::Instant::now();
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, now - std::time::Duration::from_secs(5));

    let Json(snapshot) = list_cooldowns(State(state)).await.unwrap();
    assert_eq!(snapshot.provider_cooldowns.len(), 1);
    assert_eq!(snapshot.provider_cooldowns[0].state, "half_open");
    assert_eq!(snapshot.provider_cooldowns[0].remaining_seconds, 0);
    assert_eq!(
        snapshot.provider_open_threshold,
        crate::state::provider_open_threshold()
    );
}

/// The provider list and detail views must surface the circuit breaker state so
/// the console can show why a provider is short-circuited, not just that its
/// cooldown has some seconds left.
#[tokio::test]
async fn provider_views_expose_the_circuit_state() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let Json(providers) = list_providers(State(state.clone())).await.unwrap();
    assert_eq!(providers[0].circuit_state, "closed");

    let threshold = crate::state::provider_open_threshold();
    if threshold > 0 {
        state
            .provider_failure_streak
            .lock()
            .await
            .insert(1, threshold);
        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let Json(providers) = list_providers(State(state.clone())).await.unwrap();
        assert_eq!(providers[0].circuit_state, "open");
        assert_eq!(providers[0].cooldown_seconds, Some(60));

        // Once the cooldown elapses the provider is half-open, not healthy.
        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let Json(providers) = list_providers(State(state.clone())).await.unwrap();
        assert_eq!(providers[0].circuit_state, "half_open");
        assert!(providers[0].cooldown_seconds.is_none());

        // The single-provider hydration path agrees with the batched one.
        let single = get_provider(&state, 1).await.unwrap();
        assert_eq!(single.circuit_state, "half_open");
    }
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

#[tokio::test]
async fn route_trace_api_exposes_candidates_and_attempts() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, requested_model, endpoint, status_code, success, in_flight
         ) VALUES ('trace-api', 'model-a', '/v1/chat/completions', 200, 1, 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let candidates = serde_json::to_string(&vec![RouteTraceCandidate {
        order: 1,
        provider_id: 1,
        provider_api_key_id: Some(11),
        upstream_model: "model-a".to_string(),
        eligible: true,
        deprioritized: false,
        reason_code: "ordered".to_string(),
    }])
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_traces (
             request_id, route_id, strategy, session_id, session_decision,
             candidates_json, attempt_count, fallback_count,
             final_provider_id, final_provider_api_key_id,
             final_upstream_model, final_status_code, success
         ) VALUES ('trace-api', 7, 'priority', 'session-a', 'session_key_present',
                   ?, 1, 0, 1, 11, 'model-a', 200, 1)",
    )
    .bind(candidates)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routing_attempts (
             request_id, attempt_index, round, candidate_order,
             provider_id, provider_api_key_id, upstream_model,
             started_at, latency_ms, status_code, success, retryable,
             reason_code, failure_scope, failure_retry, failure_confidence
         ) VALUES ('trace-api', 1, 0, 1, 1, 11, 'model-a',
                   '2026-10-16T00:00:00Z', 42, 200, 1, 0,
                   'upstream_success', NULL, NULL, NULL)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(trace) = get_route_trace(State(state.clone()), Path("trace-api".to_string()))
        .await
        .unwrap();
    assert_eq!(trace.route_id, Some(7));
    assert_eq!(trace.session_decision, "session_key_present");
    assert_eq!(trace.candidates.len(), 1);
    assert_eq!(trace.candidates[0].provider_id, 1);
    assert!(trace.candidates[0].eligible);
    assert_eq!(trace.attempts.len(), 1);
    assert!(trace.attempts[0].success);
    assert_eq!(trace.attempts[0].reason_code, "upstream_success");

    let error = get_route_trace(State(state), Path("missing-trace".to_string()))
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)));
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
async fn audit_log_records_mutations_and_supports_filters() {
    use tower::ServiceExt;

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Audit provider', 'openai', 'https://audit.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let router = crate::build_router(state.clone());

    // A real mutation through the admin API must leave an audit trail.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/routes")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Audited route",
                        "model_pattern": "audit-*",
                        "strategy": "priority",
                        "enabled": true,
                        "targets": [
                            {
                                "provider_id": 1,
                                "upstream_model": "m",
                                "weight": 100,
                                "priority": 0,
                                "enabled": true,
                            }
                        ],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // A failed mutation must not be recorded as a change.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/routes/9999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/audit-logs?entity=route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["items"][0]["action"], "create");
    assert_eq!(page["items"][0]["entity"], "route");
    assert_eq!(page["items"][0]["summary"], "created route 'Audited route'");
    assert_eq!(page["items"][0]["detail"]["strategy"], "priority");

    // Search matches the summary and entity id fields.
    let Json(page) = list_audit_logs(
        State(state.clone()),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: None,
            action: None,
            search: Some("Audited".to_string()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 1);

    // The CSV export honours the same filters and carries a header row.
    let response = export_audit_logs(
        State(state.clone()),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: Some("route".to_string()),
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/csv; charset=utf-8"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let csv = String::from_utf8(body.to_vec()).unwrap();
    assert!(csv.starts_with('\u{feff}'));
    assert!(csv.contains("created_at,id,action,entity,entity_id,actor,summary,detail"));
    assert!(csv.contains("created route 'Audited route'"));
    assert!(
        !csv.contains("provider"),
        "the filter must exclude other entities"
    );

    let Json(page) = list_audit_logs(
        State(state),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: Some("provider".to_string()),
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 0);
}

/// A capped export must announce the truncation so the console warns instead of
/// handing the operator a silently incomplete file.
#[tokio::test]
async fn audit_export_flags_truncation_at_the_row_cap() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5001) \
         INSERT INTO audit_logs (action, entity, summary, actor) \
         SELECT 'update', 'settings', 'row ' || n, 'local' FROM seq",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let response = export_audit_logs(
        State(state),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: None,
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-openllm-export-truncated"], "true");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let csv = String::from_utf8(body.to_vec()).unwrap();
    // The header row plus exactly the row cap of data rows.
    assert_eq!(csv.lines().count(), 1 + 5000);
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
    assert_eq!(settings.database_stats.audit_logs, 0);
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
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: None,
            retry_jitter_ms: None,
            max_waiting_requests: None,
            keepalive_during_wait: None,
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
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: None,
            retry_jitter_ms: None,
            max_waiting_requests: None,
            keepalive_during_wait: None,
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
async fn updating_a_route_clears_its_session_affinity_pins() {
    use crate::session_affinity::{
        SessionAffinityScope, SessionAffinityStore, SessionAffinityTarget,
    };
    use std::sync::Arc;
    use std::time::Duration;

    let mut state = provider_key_test_state().await;
    let store = Arc::new(SessionAffinityStore::new(Duration::from_secs(60), 8, false));
    state.session_affinity = Some(store.clone());

    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Primary', 'openai', 'https://primary.example/v1'),
                (2, 'Backup', 'openai', 'https://backup.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
         VALUES (1, 'model-a', 1), (2, 'model-b', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let target = |provider_id, upstream_model: &str, priority| RouteTargetInput {
        id: None,
        provider_id,
        upstream_model: upstream_model.to_string(),
        weight: 100,
        priority,
        enabled: true,
    };
    let (_, created) = create_route(
        State(state.clone()),
        Json(RouteInput {
            name: "Sticky route".to_string(),
            model_pattern: "sticky-model".to_string(),
            strategy: RouteStrategy::Priority,
            enabled: true,
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: None,
            retry_jitter_ms: None,
            max_waiting_requests: None,
            keepalive_during_wait: None,
            targets: vec![target(1, "model-a", 0), target(2, "model-b", 1)],
        }),
    )
    .await
    .unwrap();

    let scope = SessionAffinityScope::new(None, created.0.id, "sticky-session");
    let pinned = SessionAffinityTarget {
        provider_id: 2,
        upstream_model: "model-b".to_string(),
    };
    store.bind(&state.pool, &scope, pinned.clone()).await;
    assert_eq!(store.peek(&state.pool, &scope).await, Some(pinned));

    let _ = update_route(
        State(state.clone()),
        Path(created.0.id),
        Json(RouteUpdate {
            name: None,
            model_pattern: None,
            strategy: None,
            enabled: None,
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: None,
            retry_jitter_ms: None,
            max_waiting_requests: None,
            keepalive_during_wait: None,
            targets: Some(vec![target(2, "model-b", 0), target(1, "model-a", 1)]),
        }),
    )
    .await
    .unwrap();

    assert_eq!(store.peek(&state.pool, &scope).await, None);
}

#[tokio::test]
async fn route_busy_wait_settings_round_trip_and_validate() {
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

    let target = || RouteTargetInput {
        id: None,
        provider_id: 1,
        upstream_model: "model".to_string(),
        weight: 100,
        priority: 0,
        enabled: true,
    };
    let (_, created) = create_route(
        State(state.clone()),
        Json(RouteInput {
            name: "Patient route".to_string(),
            model_pattern: "patient-model".to_string(),
            strategy: RouteStrategy::Priority,
            enabled: true,
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: Some(45),
            retry_jitter_ms: Some(800),
            max_waiting_requests: Some(9),
            keepalive_during_wait: Some(false),
            targets: vec![target()],
        }),
    )
    .await
    .unwrap();

    assert_eq!(created.0.all_targets_busy_wait_seconds, 45);
    assert_eq!(created.0.retry_jitter_ms, 800);
    assert_eq!(created.0.max_waiting_requests, 9);
    assert!(!created.0.keepalive_during_wait);

    let updated = update_route(
        State(state.clone()),
        Path(created.0.id),
        Json(RouteUpdate {
            name: None,
            model_pattern: None,
            strategy: None,
            enabled: None,
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: Some(10),
            retry_jitter_ms: Some(0),
            max_waiting_requests: Some(1),
            keepalive_during_wait: Some(true),
            targets: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(updated.0.all_targets_busy_wait_seconds, 10);
    assert_eq!(updated.0.retry_jitter_ms, 0);
    assert_eq!(updated.0.max_waiting_requests, 1);
    assert!(updated.0.keepalive_during_wait);

    let invalid = create_route(
        State(state.clone()),
        Json(RouteInput {
            name: "Invalid route".to_string(),
            model_pattern: "invalid-model".to_string(),
            strategy: RouteStrategy::Priority,
            enabled: true,
            context_override: None,
            output_override: None,
            all_targets_busy_wait_seconds: Some(-1),
            retry_jitter_ms: None,
            max_waiting_requests: None,
            keepalive_during_wait: None,
            targets: vec![target()],
        }),
    )
    .await;
    assert!(matches!(invalid, Err(AppError::BadRequest(_))));
}

#[tokio::test]
async fn route_capability_overrides_must_stay_within_the_target_barrel() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Primary', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
            provider_id, model_name, enabled, context_limit, input_limit, output_limit
         ) VALUES (1, 'model', 1, 100000, 80000, 20000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let input = |context_override: Option<i64>, output_override: Option<i64>| RouteInput {
        name: format!(
            "route-{}-{}",
            context_override.unwrap_or(0),
            output_override.unwrap_or(0)
        ),
        model_pattern: format!(
            "model-{}-{}",
            context_override.unwrap_or(0),
            output_override.unwrap_or(0)
        ),
        strategy: RouteStrategy::Priority,
        enabled: true,
        context_override,
        output_override,
        all_targets_busy_wait_seconds: None,
        retry_jitter_ms: None,
        max_waiting_requests: None,
        keepalive_during_wait: None,
        targets: vec![RouteTargetInput {
            id: None,
            provider_id: 1,
            upstream_model: "model".to_string(),
            weight: 100,
            priority: 0,
            enabled: true,
        }],
    };

    let (_, created) = create_route(State(state.clone()), Json(input(Some(64_000), Some(8_000))))
        .await
        .unwrap();
    assert_eq!(created.0.context_limit_base, Some(100_000));
    assert_eq!(created.0.output_limit_base, Some(20_000));
    assert_eq!(created.0.context_limit, Some(64_000));
    assert_eq!(created.0.input_limit, Some(64_000));
    assert_eq!(created.0.output_limit, Some(8_000));

    let too_large = create_route(
        State(state.clone()),
        Json(input(Some(120_000), Some(25_000))),
    )
    .await;
    assert!(
        matches!(too_large, Err(AppError::BadRequest(_))),
        "an override that widens the target barrel must be rejected"
    );
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
fn overview_query_deserializes_typed_filters() {
    let uri: axum::http::Uri = "http://localhost/api/overview\
        ?tz_offset_minutes=480&provider_id=1&api_key_id=2&route_id=3\
        &provider_api_key_id=4&success=false&gateway_adjusted=true&full_breakdown=true\
        &granularity=week"
        .parse()
        .unwrap();
    let Query(query) = Query::<OverviewQuery>::try_from_uri(&uri).unwrap();
    assert_eq!(query.tz_offset_minutes, 480);
    assert_eq!(query.provider_id, Some(1));
    assert_eq!(query.api_key_id, Some(2));
    assert_eq!(query.route_id, Some(3));
    assert_eq!(query.provider_api_key_id, Some(4));
    assert_eq!(query.success, Some(false));
    assert_eq!(query.gateway_adjusted, Some(true));
    assert!(query.full_breakdown);
    assert_eq!(query.granularity, Some(AnalyticsGranularity::Week));
    assert_eq!(query.filters().provider_id, Some(1));
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
                max_concurrency: Some(4),
                queue_timeout_seconds: Some(0),
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
    assert_eq!(rows[0].max_concurrency, Some(4));
    assert_eq!(rows[0].queue_timeout_seconds, Some(0));
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
            "idx_usage_completed_endpoint_created",
            "idx_usage_completed_model_created",
            "idx_usage_completed_provider_created",
            "idx_usage_completed_provider_key_created",
            "idx_usage_completed_route_created",
            "idx_usage_completed_success_created",
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
            ..Default::default()
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
    assert_eq!(view.range_latency_p50_ms, Some(200));
    assert_eq!(view.range_latency_p95_ms, Some(300));
    assert_eq!(view.range_latency_p99_ms, Some(300));
    assert!(view.range_first_token_p50_ms.is_none());
    assert!((view.range_output_tps.unwrap() - 166.666_666).abs() < 0.001);
    assert_eq!(view.status_breakdown.len(), 1);
    assert_eq!(view.status_breakdown[0].status_code, 200);
    assert_eq!(view.status_breakdown[0].count, 3);
    assert_eq!(view.error_reasons.len(), 1);
    assert_eq!(view.error_reasons[0].label, "无错误信息");
    assert_eq!(view.error_reasons[0].count, 1);
    assert_eq!(view.gateway_adjustments.len(), 1);
    assert_eq!(view.gateway_adjustments[0].category, "other");
    assert_eq!(view.gateway_adjustments[0].count, 1);
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
    assert_eq!(view.granularity, "day");
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
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.cost_micros)
            .sum::<i64>(),
        view.range_cost_micros
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
    assert_eq!(view.model_usage[0].cache_read, 25);
    assert!((view.model_usage[0].cache_hit_rate - 17.857_142).abs() < 0.001);
    assert_eq!(
        view.daily_usage
            .iter()
            .map(|row| row.cache_read)
            .sum::<i64>(),
        view.range_cache_read
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        view.range_requests
    );
    assert!(
        view.daily_model_requests
            .iter()
            .all(|row| row.label == "test-model")
    );
    // Requests with no routed provider still count toward the daily totals, so
    // the share chart keeps the same denominator as the trend line.
    assert_eq!(
        view.daily_provider_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        view.range_requests
    );
    assert!(
        view.daily_provider_requests
            .iter()
            .all(|row| row.label == "其他")
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.tokens)
            .sum::<i64>(),
        view.range_tokens
    );

    let Json(view_without_sessions) = overview(
        State(state),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            include_session_metrics: false,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(view_without_sessions.range_requests, view.range_requests);
    assert_eq!(view_without_sessions.range_sessions, 0);
    assert_eq!(view_without_sessions.range_session_coverage, 0.0);
}

#[tokio::test]
async fn overview_buckets_a_single_day_by_hour() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    for (request_id, hour) in [("morning", 9_i64), ("evening", 21)] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, status_code, success, created_at
                 ) VALUES (
                    ?, 'test-model', '/v1/chat/completions', 10, 5, 15, 200, 1, ?
                 )",
        )
        .bind(request_id)
        .bind((day_start + Duration::hours(hour)).to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let Json(view) = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            tz_offset_minutes: 0,
            from: Some(day_start.to_rfc3339()),
            to: Some((day_start + Duration::days(1)).to_rfc3339()),
            include_session_metrics: false,
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let day = day_start.format("%Y-%m-%d").to_string();
    assert_eq!(view.granularity, "hour");
    assert_eq!(view.daily_usage.len(), 24);
    assert_eq!(view.daily_usage[0].day, format!("{day} 00:00"));
    assert_eq!(view.daily_usage[23].day, format!("{day} 23:00"));

    let busy: Vec<_> = view
        .daily_usage
        .iter()
        .filter(|row| row.requests > 0)
        .collect();
    assert_eq!(busy.len(), 2);
    assert_eq!(busy[0].day, format!("{day} 09:00"));
    assert_eq!(busy[0].requests, 1);
    assert_eq!(busy[1].day, format!("{day} 21:00"));
    // The model and provider breakdowns share the same hourly buckets.
    assert!(
        view.daily_model_requests
            .iter()
            .all(|row| row.day.starts_with(&format!("{day} ")))
    );
    assert_eq!(
        view.daily_model_requests
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        2
    );
}

#[tokio::test]
async fn overview_applies_dimension_filters() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled) VALUES
            (1, 'alpha-provider', 'openai', 'https://alpha.example/v1', 1),
            (2, 'beta-provider', 'openai', 'https://beta.example/v1', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let now = Utc::now().to_rfc3339();
    for (request_id, model, provider_id, endpoint, tokens, latency, status, success, error) in [
        (
            "alpha-ok",
            "alpha",
            1_i64,
            "/v1/chat/completions",
            10_i64,
            100_i64,
            200_i64,
            1_i64,
            None,
        ),
        (
            "alpha-failed",
            "alpha",
            1,
            "/v1/responses",
            20,
            300,
            500,
            0,
            Some("upstream failed"),
        ),
        (
            "beta-ok",
            "beta",
            2,
            "/v1/chat/completions",
            40,
            500,
            200,
            1,
            None,
        ),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, provider_id, endpoint,
                    prompt_tokens, completion_tokens, total_tokens, latency_ms,
                    status_code, success, error_message, created_at
                 ) VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(model)
        .bind(provider_id)
        .bind(endpoint)
        .bind(tokens)
        .bind(tokens)
        .bind(latency)
        .bind(status)
        .bind(success)
        .bind(error)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let call = |filters: UsageFilters| {
        let state = state.clone();
        let from = from.clone();
        let to = to.clone();
        async move {
            overview(
                State(state),
                Query(OverviewQuery {
                    tz_offset_minutes: 0,
                    from: Some(from),
                    to: Some(to),
                    include_session_metrics: false,
                    provider_id: filters.provider_id,
                    provider_api_key_id: filters.provider_api_key_id,
                    api_key_id: filters.api_key_id,
                    route_id: filters.route_id,
                    model: filters.model,
                    request_id: filters.request_id,
                    session_id: filters.session_id,
                    endpoint: filters.endpoint,
                    success: filters.success,
                    in_flight: filters.in_flight,
                    gateway_adjusted: filters.gateway_adjusted,
                    full_breakdown: false,
                    granularity: None,
                }),
            )
            .await
            .unwrap()
        }
    };

    let Json(alpha) = call(UsageFilters {
        model: Some("alpha".to_string()),
        ..Default::default()
    })
    .await;
    assert_eq!(alpha.range_requests, 2);
    assert_eq!(alpha.range_tokens, 30);
    assert!((alpha.range_success_rate - 50.0).abs() < 0.001);
    assert_eq!(alpha.model_usage.len(), 1);
    assert_eq!(alpha.model_usage[0].model, "alpha");
    assert_eq!(alpha.provider_usage.len(), 1);
    assert_eq!(alpha.provider_usage[0].provider_name, "alpha-provider");
    assert_eq!(alpha.status_breakdown.len(), 2);
    assert_eq!(
        alpha
            .status_breakdown
            .iter()
            .find(|row| row.status_code == 500)
            .map(|row| row.count),
        Some(1)
    );

    let Json(beta_provider) = call(UsageFilters {
        provider_id: Some(2),
        ..Default::default()
    })
    .await;
    assert_eq!(beta_provider.range_requests, 1);
    assert_eq!(beta_provider.range_tokens, 40);
    assert_eq!(beta_provider.model_usage[0].model, "beta");

    let Json(failed) = call(UsageFilters {
        success: Some(false),
        ..Default::default()
    })
    .await;
    assert_eq!(failed.range_requests, 1);
    assert_eq!(failed.error_reasons.len(), 1);
    assert_eq!(failed.error_reasons[0].label, "upstream failed");
    assert_eq!(failed.error_reasons[0].count, 1);
}

#[tokio::test]
async fn analytics_dimensions_groups_keys_routes_endpoints_and_sessions() {
    async fn insert_usage(
        pool: &sqlx::SqlitePool,
        request_id: &str,
        api_key_id: Option<i64>,
        route_id: Option<i64>,
        provider_id: Option<i64>,
        provider_api_key_id: Option<i64>,
        model: &str,
        endpoint: &str,
        session_id: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read: i64,
        cache_write: i64,
        latency_ms: i64,
        status_code: i64,
        success: i64,
        cost_micros: Option<i64>,
        created_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, api_key_id, route_id, provider_id, provider_api_key_id,
                    requested_model, provider_api_key_name, endpoint, session_id,
                    prompt_tokens, completion_tokens, total_tokens,
                    cache_read_tokens, cache_write_tokens, latency_ms,
                    status_code, success, estimated_cost_micros, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(api_key_id)
        .bind(route_id)
        .bind(provider_id)
        .bind(provider_api_key_id)
        .bind(model)
        .bind(provider_api_key_id.map(|_| "Primary"))
        .bind(endpoint)
        .bind(session_id)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(prompt_tokens + completion_tokens)
        .bind(cache_read)
        .bind(cache_write)
        .bind(latency_ms)
        .bind(status_code)
        .bind(success)
        .bind(cost_micros)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'Alpha', 'openai', 'https://alpha.example/v1'),
            (2, 'Beta', 'openai', 'https://beta.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, key_suffix) VALUES
            (1, 'Console', 'dimension-hash-1', 'sk-console', 'a1b2'),
            (2, 'CI', 'dimension-hash-2', 'sk-ci', 'c3d4')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy) VALUES
            (1, 'Chat', 'gpt-*', 'priority'),
            (2, 'Responses', 'claude-*', 'priority')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) VALUES
            (3, 1, 'Primary', 'dimension-alpha-key'),
            (4, 2, 'Primary', 'dimension-beta-key')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let base = Utc::now() - Duration::hours(1);
    let t1 = (base - Duration::minutes(30)).to_rfc3339();
    let t2 = base.to_rfc3339();
    let t3 = (base + Duration::minutes(10)).to_rfc3339();
    let t4 = (base + Duration::minutes(20)).to_rfc3339();
    insert_usage(
        &pool,
        "r1",
        Some(1),
        Some(1),
        Some(1),
        Some(3),
        "gpt-5.1",
        "/v1/chat/completions",
        Some("s1"),
        100,
        50,
        40,
        10,
        1000,
        200,
        1,
        Some(1000),
        &t1,
    )
    .await;
    insert_usage(
        &pool,
        "r2",
        Some(1),
        Some(1),
        Some(1),
        Some(3),
        "gpt-5.1",
        "/v1/chat/completions",
        Some("s1"),
        200,
        0,
        0,
        0,
        2000,
        500,
        0,
        None,
        &t2,
    )
    .await;
    insert_usage(
        &pool,
        "r3",
        Some(2),
        Some(2),
        Some(2),
        Some(4),
        "claude-sonnet-4",
        "/v1/responses",
        Some("s2"),
        100,
        100,
        50,
        0,
        3000,
        200,
        1,
        Some(2000),
        &t3,
    )
    .await;
    insert_usage(
        &pool,
        "r4",
        None,
        None,
        None,
        None,
        "local-model",
        "/v1/responses",
        None,
        50,
        0,
        0,
        0,
        500,
        200,
        1,
        None,
        &t4,
    )
    .await;

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let Json(dimensions) = analytics_dimensions(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(dimensions.api_key_usage.len(), 3);
    let console = dimensions
        .api_key_usage
        .iter()
        .find(|row| row.id == Some(1))
        .unwrap();
    assert_eq!(console.label, "Console");
    assert_eq!(console.requests, 2);
    assert_eq!(console.tokens, 350);
    assert_eq!(console.cache_read, 40);
    assert!((console.cache_hit_rate - 13.333_333).abs() < 0.001);
    assert!((console.success_rate - 50.0).abs() < 0.001);
    assert_eq!(console.cost_micros, Some(1000));

    assert_eq!(dimensions.provider_api_key_usage.len(), 3);
    let alpha_key = dimensions
        .provider_api_key_usage
        .iter()
        .find(|row| row.id == Some(3))
        .unwrap();
    assert_eq!(alpha_key.label, "Alpha · Primary");
    assert_eq!(alpha_key.requests, 2);
    assert!((alpha_key.cache_hit_rate - 13.333_333).abs() < 0.001);

    assert_eq!(dimensions.route_usage.len(), 3);
    let chat = dimensions
        .route_usage
        .iter()
        .find(|row| row.id == Some(1))
        .unwrap();
    assert_eq!(chat.label, "Chat");
    assert_eq!(chat.requests, 2);

    assert_eq!(dimensions.endpoint_usage.len(), 2);
    let responses = dimensions
        .endpoint_usage
        .iter()
        .find(|row| row.label == "/v1/responses")
        .unwrap();
    assert_eq!(responses.id, None);
    assert_eq!(responses.requests, 2);

    assert_eq!(dimensions.session_usage.len(), 2);
    let session_one = dimensions
        .session_usage
        .iter()
        .find(|row| row.session_id == "s1")
        .unwrap();
    assert_eq!(session_one.requests, 2);
    assert_eq!(session_one.last_seen, t2);
    assert!((session_one.cache_hit_rate - 13.333_333).abs() < 0.001);
    let session_two = dimensions
        .session_usage
        .iter()
        .find(|row| row.session_id == "s2")
        .unwrap();
    assert_eq!(session_two.requests, 1);
    assert_eq!(session_two.last_seen, t3);

    let Json(filtered) = analytics_dimensions(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            provider_id: Some(2),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(filtered.api_key_usage.len(), 1);
    assert_eq!(filtered.api_key_usage[0].id, Some(2));
    assert_eq!(filtered.route_usage.len(), 1);
    assert_eq!(filtered.route_usage[0].label, "Responses");
    assert_eq!(filtered.session_usage.len(), 1);
    assert_eq!(filtered.session_usage[0].session_id, "s2");
}

#[tokio::test]
async fn overview_full_breakdown_lifts_model_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let now = Utc::now().to_rfc3339();
    for index in 0..10 {
        sqlx::query(
            "INSERT INTO usage_logs (
                    request_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, latency_ms, status_code,
                    success, created_at
                 ) VALUES (?, ?, '/v1/chat/completions', 10, 5, 15, 100, 200, 1, ?)",
        )
        .bind(format!("model-{index}"))
        .bind(format!("model-{index:02}"))
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::days(1)).to_rfc3339();
    let to = (Utc::now() + Duration::days(1)).to_rfc3339();
    let Json(limited) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(limited.model_usage.len(), 8);

    let Json(full) = overview(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            full_breakdown: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(full.model_usage.len(), 10);
}

#[tokio::test]
async fn overview_compares_previous_equal_length_range() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let range_end = Utc::now() - Duration::hours(1);
    let range_start = range_end - Duration::hours(2);
    let previous_start = range_start - Duration::hours(2);
    for (request_id, created_at, tokens) in [
        ("previous-1", previous_start + Duration::minutes(30), 10),
        ("previous-2", previous_start + Duration::minutes(90), 20),
        ("current-1", range_start + Duration::minutes(30), 30),
        ("current-2", range_start + Duration::minutes(90), 40),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, 'test-model', '/v1/chat/completions', ?, 0, ?, 100, 200, 1, ?)",
        )
        .bind(request_id)
        .bind(tokens)
        .bind(tokens)
        .bind(created_at.to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let Json(view) = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Day),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(view.granularity, "day");
    assert_eq!(view.range_requests, 2);
    assert_eq!(view.range_tokens, 70);
    assert_eq!(view.previous_range.from, previous_start.to_rfc3339());
    assert_eq!(view.previous_range.to, range_start.to_rfc3339());
    assert_eq!(view.previous_range.requests, 2);
    assert_eq!(view.previous_range.tokens, 30);
    assert!((view.previous_range.success_rate - 100.0).abs() < 0.001);
}

#[tokio::test]
async fn overview_supports_week_and_month_buckets() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let range_start = DateTime::parse_from_rfc3339("2026-01-05T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let range_end = DateTime::parse_from_rfc3339("2026-02-02T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (request_id, created_at) in [
        ("week-1", "2026-01-06T12:00:00Z"),
        ("week-2", "2026-01-12T12:00:00Z"),
        ("week-3", "2026-02-01T12:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, 'test-model', '/v1/chat/completions', 10, 5, 15, 100, 200, 1, ?)",
        )
        .bind(request_id)
        .bind(created_at)
        .execute(&pool)
        .await
        .unwrap();
    }

    let state = AppState::new(pool, None);
    let Json(weekly) = overview(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Week),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(weekly.granularity, "week");
    assert_eq!(
        weekly
            .daily_usage
            .iter()
            .map(|row| row.day.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-01-05", "2026-01-12", "2026-01-19", "2026-01-26"]
    );
    assert_eq!(
        weekly
            .daily_usage
            .iter()
            .map(|row| row.requests)
            .sum::<i64>(),
        3
    );

    let Json(monthly) = overview(
        State(state),
        Query(OverviewQuery {
            from: Some(range_start.to_rfc3339()),
            to: Some(range_end.to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Month),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(monthly.granularity, "month");
    assert_eq!(
        monthly
            .daily_usage
            .iter()
            .map(|row| row.day.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-01-01", "2026-02-01"]
    );
}

#[tokio::test]
async fn overview_rejects_oversized_hourly_range() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let start = Utc::now() - Duration::days(40);
    let result = overview(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            from: Some(start.to_rfc3339()),
            to: Some(Utc::now().to_rfc3339()),
            granularity: Some(AnalyticsGranularity::Hour),
            ..Default::default()
        }),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn analytics_route_quality_groups_strategies_routes_sessions_and_failures() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, strategy_ext) VALUES
            (1, 'Priority route', 'gpt-*', 'priority', ''),
            (2, 'Cost route', 'claude-*', 'priority', 'cost_optimized')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let created_at = (Utc::now() - Duration::minutes(10)).to_rfc3339();
    for (request_id, route_id, model, success, latency) in [
        ("quality-1", 1_i64, "gpt-5.1", 1_i64, 100_i64),
        ("quality-2", 2, "claude-sonnet-4", 1, 250),
        ("quality-3", 1, "gpt-5.1", 0, 800),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                 request_id, route_id, requested_model, endpoint, prompt_tokens,
                 completion_tokens, total_tokens, latency_ms, status_code,
                 success, created_at
             ) VALUES (?, ?, ?, '/v1/chat/completions', 10, 5, 15, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(route_id)
        .bind(model)
        .bind(latency)
        .bind(if success == 1 { 200 } else { 500 })
        .bind(success)
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (
        request_id,
        route_id,
        strategy,
        session_decision,
        attempt_count,
        fallback_count,
        success,
    ) in [
        ("quality-1", 1_i64, "priority", "hit", 1_i64, 0_i64, 1_i64),
        ("quality-2", 2, "cost_optimized", "miss", 2, 1, 1),
        ("quality-3", 1, "priority", "no_affinity", 1, 0, 0),
    ] {
        sqlx::query(
            "INSERT INTO routing_traces (
                 request_id, route_id, strategy, session_id, session_decision,
                 attempt_count, fallback_count, success
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(route_id)
        .bind(strategy)
        .bind((request_id != "quality-3").then_some("session-a"))
        .bind(session_decision)
        .bind(attempt_count)
        .bind(fallback_count)
        .bind(success)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO routing_attempts (
             request_id, attempt_index, round, candidate_order, started_at,
             latency_ms, status_code, success, retryable, reason_code,
             failure_scope, failure_retry, failure_confidence
         ) VALUES
            ('quality-1', 1, 0, 1, ?, 100, 200, 1, 0, 'upstream_success', NULL, NULL, 'exact'),
            ('quality-2', 1, 0, 1, ?, 120, 429, 0, 1, 'upstream_rate_limited', 'provider', 'next_target', 'inferred'),
            ('quality-2', 2, 0, 2, ?, 130, 200, 1, 0, 'upstream_success', NULL, NULL, 'exact'),
            ('quality-3', 1, 0, 1, ?, 800, 500, 0, 0, 'internal_error', 'endpoint', 'do_not_retry', 'exact')",
    )
    .bind(&created_at)
    .bind(&created_at)
    .bind(&created_at)
    .bind(&created_at)
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let from = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let to = Utc::now().to_rfc3339();
    let Json(quality) = analytics_route_quality(
        State(state.clone()),
        Query(OverviewQuery {
            from: Some(from.clone()),
            to: Some(to.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(quality.traces, 3);
    assert!((quality.fallback_rate - 33.333_333).abs() < 0.001);
    assert!((quality.avg_attempts - 1.333_333).abs() < 0.001);
    assert_eq!(quality.session_requests, 2);
    assert_eq!(quality.session_hits, 1);
    assert!((quality.session_hit_rate - 50.0).abs() < 0.001);
    assert!((quality.success_rate - 66.666_666).abs() < 0.001);
    assert_eq!(quality.strategies.len(), 2);
    let priority = quality
        .strategies
        .iter()
        .find(|row| row.strategy == "priority")
        .unwrap();
    assert_eq!(priority.traces, 2);
    assert!((priority.success_rate - 50.0).abs() < 0.001);
    let cost = quality
        .strategies
        .iter()
        .find(|row| row.strategy == "cost_optimized")
        .unwrap();
    assert_eq!(cost.fallback_rate, 100.0);
    assert_eq!(quality.routes.len(), 2);
    assert_eq!(
        quality
            .routes
            .iter()
            .find(|row| row.route_id == Some(1))
            .map(|row| row.route_name.as_str()),
        Some("Priority route")
    );
    assert_eq!(
        quality
            .session_decisions
            .iter()
            .find(|row| row.decision == "hit")
            .map(|row| row.count),
        Some(1)
    );
    assert_eq!(
        quality
            .failure_reasons
            .iter()
            .find(|row| row.reason_code == "upstream_rate_limited")
            .map(|row| row.failure_scope.as_str()),
        Some("provider")
    );

    let Json(filtered) = analytics_route_quality(
        State(state),
        Query(OverviewQuery {
            from: Some(from),
            to: Some(to),
            route_id: Some(1),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(filtered.traces, 2);
    assert_eq!(filtered.routes.len(), 1);
    assert_eq!(filtered.routes[0].route_id, Some(1));
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

#[tokio::test]
async fn runtime_settings_expose_effective_limits() {
    let state = provider_key_test_state().await;

    let expected_concurrency = state.request_capacity_limit;
    let expected_inflight_mib = state.request_bytes_limit / (1024 * 1024);
    let expected_admission_wait_ms = state.admission_wait.as_millis() as u64;
    let expected_body_read_timeout = state.body_read_timeout.map(|timeout| timeout.as_secs());
    let expected_request_timeout = state.request_timeout.map(|timeout| timeout.as_secs());
    let expected_memory_limit_mib = state
        .memory_pressure
        .as_ref()
        .map(|pressure| pressure.limit_bytes() / (1024 * 1024))
        .unwrap_or(0);
    let expected_memory_ratio_pct = (state.memory_shed_ratio * 100.0).round() as u64;
    let expected_memory_source = state.memory_limit_source;
    let expected_affinity_ttl = state
        .session_affinity
        .as_ref()
        .map(|store| store.ttl_secs());
    let expected_affinity_capacity = state
        .session_affinity
        .as_ref()
        .map(|store| store.capacity())
        .unwrap_or(crate::session_affinity::DEFAULT_SESSION_AFFINITY_CAPACITY);
    let expected_affinity_persist = state
        .session_affinity
        .as_ref()
        .is_some_and(|store| store.persist());
    let Json(view) = get_runtime_settings(State(state)).await.unwrap();
    assert!(view.limits.max_request_body_mib >= 1);
    assert!(view.limits.max_upstream_body_mib >= 1);
    assert!(view.limits.upstream_idle_timeout_secs >= 1);
    assert_eq!(view.limits.max_concurrent_requests, expected_concurrency);
    assert_eq!(view.limits.max_inflight_request_mib, expected_inflight_mib);
    assert_eq!(view.limits.admission_wait_ms, expected_admission_wait_ms);
    assert_eq!(
        view.limits.body_read_timeout_secs,
        expected_body_read_timeout
    );
    assert_eq!(view.limits.request_timeout_secs, expected_request_timeout);
    assert_eq!(
        view.limits.provider_open_threshold,
        crate::state::provider_open_threshold()
    );
    assert_eq!(
        view.limits.stream_max_secs,
        crate::state::max_stream_lifetime().map(|lifetime| lifetime.as_secs())
    );
    assert_eq!(view.limits.memory_limit_mib, expected_memory_limit_mib);
    assert_eq!(view.limits.memory_limit_source, expected_memory_source);
    assert_eq!(view.limits.memory_shed_ratio_pct, expected_memory_ratio_pct);
    assert_eq!(view.limits.session_affinity_ttl_secs, expected_affinity_ttl);
    assert_eq!(
        view.limits.session_affinity_capacity,
        expected_affinity_capacity
    );
    assert_eq!(
        view.limits.session_affinity_persist,
        expected_affinity_persist
    );
    assert_eq!(
        view.limits.db_max_connections,
        crate::db::db_max_connections()
    );
    assert_eq!(
        view.limits.db_busy_timeout_secs,
        crate::db::db_busy_timeout_secs()
    );
    assert_eq!(
        view.limits.db_acquire_timeout_secs,
        crate::db::db_acquire_timeout_secs()
    );
}

/// The on-demand maintenance endpoint runs a full cycle and records that it
/// happened, so an operator can both trigger and confirm maintenance.
#[tokio::test]
async fn run_maintenance_runs_a_cycle_and_records_it() {
    let state = provider_key_test_state().await;
    let runs = state.maintenance_runs.clone();
    let last_run = state.maintenance_last_run_unix.clone();
    assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 0);

    let Json(body) = run_maintenance(State(state)).await.unwrap();
    assert_eq!(body["status"], "completed");
    assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(
        last_run.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "the completion timestamp must be recorded"
    );
}

#[tokio::test]
async fn probes_stay_reachable_when_the_request_cap_is_saturated() {
    let mut state = provider_key_test_state().await;
    // Zero permits: the admission cap would reject any proxied request.
    state.request_capacity = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
    state.request_capacity_limit = 1;
    let router = crate::build_router(state);

    for path in ["/api/health", "/api/ready"] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} must stay reachable while the proxy cap is saturated"
        );
    }
}

#[tokio::test]
async fn readiness_reports_ready_when_the_database_answers() {
    let state = provider_key_test_state().await;
    let response = ready(State(state)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "ready");
}

/// Once a shutdown signal arrives the readiness probe must report `not_ready`
/// so a load balancer drains this instance before its connections close.
#[tokio::test]
async fn readiness_reports_draining_after_the_shutdown_signal() {
    let state = provider_key_test_state().await;
    let response = ready(State(state.clone())).await;
    assert_eq!(response.status(), StatusCode::OK);

    state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let response = ready(State(state)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "draining");
}

/// The consolidated status endpoint must report the live runtime signals an
/// operator needs in one place: draining, admission, the database pool, and the
/// circuit summary.
#[tokio::test]
async fn runtime_status_reports_circuits_and_runtime_signals() {
    let state = provider_key_test_state().await;
    let Json(status) = get_status(State(state.clone())).await;
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
    assert!(!status.draining);
    assert_eq!(status.admission.in_flight, 0);
    assert_eq!(
        status.database.max_connections,
        crate::db::db_max_connections()
    );
    assert_eq!(
        status.circuits.threshold,
        crate::state::provider_open_threshold()
    );
    assert_eq!(status.circuits.open, 0);
    assert_eq!(status.circuits.half_open, 0);

    // A provider that crossed the threshold is reported as open, then as
    // half-open once its cooldown elapses.
    let threshold = crate::state::provider_open_threshold();
    if threshold > 0 {
        state
            .provider_failure_streak
            .lock()
            .await
            .insert(1, threshold);
        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let Json(status) = get_status(State(state.clone())).await;
        assert_eq!(status.circuits.open, 1);

        state.provider_cooldown.lock().await.insert(
            1,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let Json(status) = get_status(State(state.clone())).await;
        assert_eq!(status.circuits.open, 0);
        assert_eq!(status.circuits.half_open, 1);
    }

    // A shutting-down instance reports draining even before the DB is closed.
    state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let Json(status) = get_status(State(state)).await;
    assert!(status.draining);
}

#[tokio::test]
async fn readiness_reports_unavailable_when_the_database_is_unreachable() {
    let state = provider_key_test_state().await;
    state.pool.close().await;
    let response = ready(State(state)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "not_ready");
    assert!(value["error"].is_string());
}
