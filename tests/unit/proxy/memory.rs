use super::*;

#[test]
fn memory_pressure_threshold_math() {
    let pressure = MemoryPressure::with_usage(1_000, 0.9, 0);

    assert_eq!(pressure.limit_bytes(), 1_000);
    assert_eq!(pressure.threshold_bytes(), 900);
    assert!(!pressure.is_shedding_at(899));
    assert!(pressure.is_shedding_at(900));
    assert!(pressure.is_shedding_at(1_000));
}

#[test]
fn memory_pressure_uses_the_sampled_usage() {
    let calm = MemoryPressure::with_usage(1_000, 0.9, 500);
    assert_eq!(calm.used_bytes(), Some(500));
    assert!(!calm.is_shedding());

    let critical = MemoryPressure::with_usage(1_000, 0.9, 950);
    assert!(critical.is_shedding());
}

/// Under memory pressure the gateway must refuse new proxied work with a
/// retryable `503` while the probes that keep it in rotation stay reachable.
#[tokio::test]
async fn memory_pressure_sheds_proxied_requests_but_not_probes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    state.memory_pressure = Some(std::sync::Arc::new(MemoryPressure::with_usage(
        1_000, 0.9, 950,
    )));
    let shed = state.memory_shed.clone();
    let router = crate::build_router(state);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .body(Body::from("{\"model\":\"m\",\"messages\":[]}"))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "5"
    );
    assert_eq!(
        shed.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a pressure shed is counted for metrics"
    );

    let probe = axum::http::Request::builder()
        .method("GET")
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(probe).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "probes must stay reachable while proxied work is shed"
    );
}

#[test]
fn memory_limit_resolution_prefers_env_then_tighter_cgroup_or_default() {
    let gib = 1024 * 1024 * 1024;
    let default = crate::state::DEFAULT_MEMORY_LIMIT_MIB * 1024 * 1024;
    assert_eq!(
        resolve_memory_limit(Some("512"), Some(gib)),
        (Some(512 * 1024 * 1024), "env")
    );
    assert_eq!(resolve_memory_limit(Some("0"), Some(gib)), (None, "off"));
    assert_eq!(
        resolve_memory_limit(None, Some(gib)),
        (Some(default), "default")
    );
    assert_eq!(
        resolve_memory_limit(None, Some(512 * 1024 * 1024)),
        (Some(512 * 1024 * 1024), "cgroup")
    );
    assert_eq!(resolve_memory_limit(None, None), (Some(default), "default"));
    assert_eq!(
        resolve_memory_limit(Some("  "), Some(gib)),
        (Some(default), "default"),
        "a blank value falls back to the bounded default"
    );
    assert_eq!(
        resolve_memory_limit(Some("oops"), Some(gib)),
        (Some(default), "default"),
        "a typo must not silently disable the guard"
    );
    assert_eq!(
        resolve_memory_limit(Some("oops"), None),
        (Some(default), "default")
    );
}

#[test]
fn cgroup_memory_limit_parsing() {
    assert_eq!(
        parse_cgroup_memory_max(Some("1073741824")),
        Some(1_073_741_824)
    );
    assert_eq!(parse_cgroup_memory_max(Some(" max \n")), None);
    assert_eq!(parse_cgroup_memory_max(Some("")), None);
    // cgroup v1's "unlimited" sentinel must not arm the guard.
    assert_eq!(parse_cgroup_memory_max(Some("9223372036854771712")), None);
    assert_eq!(parse_cgroup_memory_max(None), None);
}
