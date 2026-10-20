use super::*;

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
