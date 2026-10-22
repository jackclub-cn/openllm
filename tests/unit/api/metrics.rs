use super::*;

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
    assert!(!body.contains("openllm_webhook"));
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
