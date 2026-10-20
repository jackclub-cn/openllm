use super::*;

#[tokio::test]
async fn provider_cooldowns_skip_cooling_providers_when_alternatives_exist() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix, enabled
             ) VALUES
                (1, 'Cooling', 'openai', 'https://cooling.example/v1', '', 1),
                (2, 'Healthy', 'openai', 'https://healthy.example/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut cooling = endpoint_test_target("openai", None);
    cooling.id = 1;
    cooling.provider_id = 1;
    let mut healthy = endpoint_test_target("openai", None);
    healthy.id = 2;
    healthy.provider_id = 2;

    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() + Duration::from_secs(60));
    let available = order_targets(
        &state,
        1,
        "priority",
        vec![cooling.clone(), healthy.clone()],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        available
            .iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![2]
    );

    state
        .provider_cooldown
        .lock()
        .await
        .insert(2, Instant::now() + Duration::from_secs(60));
    let fallback = order_targets(&state, 1, "priority", vec![cooling, healthy], None, None)
        .await
        .unwrap();
    assert_eq!(
        fallback
            .iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn parses_retry_after_seconds_milliseconds_and_http_dates() {
    let mut seconds = HeaderMap::new();
    seconds.insert("retry-after", HeaderValue::from_static("42"));
    assert_eq!(
        retry_after_from_headers(&seconds),
        Some(Duration::from_secs(42))
    );

    let mut milliseconds = HeaderMap::new();
    milliseconds.insert("retry-after-ms", HeaderValue::from_static("1250"));
    assert_eq!(
        retry_after_from_headers(&milliseconds),
        Some(Duration::from_millis(1250))
    );

    let mut date = HeaderMap::new();
    let deadline = std::time::SystemTime::now() + Duration::from_secs(120);
    date.insert(
        "retry-after",
        HeaderValue::from_str(&httpdate::fmt_http_date(deadline)).unwrap(),
    );
    let parsed = retry_after_from_headers(&date).unwrap();
    assert!(parsed >= Duration::from_secs(118));
    assert!(parsed <= Duration::from_secs(120));

    let mut reset_after = HeaderMap::new();
    reset_after.insert("x-ratelimit-reset-after", HeaderValue::from_static("42"));
    assert_eq!(
        retry_after_from_headers(&reset_after),
        Some(Duration::from_secs(42))
    );

    let mut reset_durations = HeaderMap::new();
    reset_durations.insert(
        "x-ratelimit-reset-requests",
        HeaderValue::from_static("1m30s"),
    );
    reset_durations.insert("x-ratelimit-reset-tokens", HeaderValue::from_static("45s"));
    assert_eq!(
        retry_after_from_headers(&reset_durations),
        Some(Duration::from_secs(90)),
        "the latest active reset window must win"
    );

    let mut reset_timestamp = HeaderMap::new();
    let reset_at = chrono::Utc::now() + Duration::from_secs(120);
    reset_timestamp.insert(
        "anthropic-ratelimit-unified-reset",
        HeaderValue::from_str(&reset_at.to_rfc3339()).unwrap(),
    );
    let parsed = retry_after_from_headers(&reset_timestamp).unwrap();
    assert!(parsed >= Duration::from_secs(118));
    assert!(parsed <= Duration::from_secs(120));

    let mut unix_reset = HeaderMap::new();
    unix_reset.insert(
        "x-ratelimit-reset",
        HeaderValue::from_str(&(chrono::Utc::now().timestamp() + 60).to_string()).unwrap(),
    );
    let parsed = retry_after_from_headers(&unix_reset).unwrap();
    assert!(parsed >= Duration::from_secs(58));
    assert!(parsed <= Duration::from_secs(60));

    let mut stale_reset = HeaderMap::new();
    stale_reset.insert(
        "x-ratelimit-reset",
        HeaderValue::from_str(&(chrono::Utc::now().timestamp() - 60).to_string()).unwrap(),
    );
    assert_eq!(retry_after_from_headers(&stale_reset), None);

    let mut malformed = HeaderMap::new();
    malformed.insert(
        "x-ratelimit-reset-requests",
        HeaderValue::from_static("999999999999999999999999999999h"),
    );
    assert_eq!(retry_after_from_headers(&malformed), None);

    let mut capped = HeaderMap::new();
    capped.insert("retry-after", HeaderValue::from_static("99999"));
    assert_eq!(
        retry_after_from_headers(&capped),
        Some(MAX_UPSTREAM_RETRY_AFTER)
    );
}

#[test]
fn provider_cooldown_escalates_with_consecutive_failures_and_caps() {
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 1, None, None),
        Duration::from_secs(30)
    );
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 2, None, None),
        Duration::from_secs(60)
    );
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 9, None, None),
        MAX_PROVIDER_COOLDOWN
    );
    assert_eq!(
        provider_cooldown(
            Some(StatusCode::TOO_MANY_REQUESTS),
            2,
            Some(Duration::from_secs(5)),
            None
        ),
        Duration::from_secs(10)
    );
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 1, None, Some(900)),
        Duration::from_secs(900)
    );
    assert_eq!(
        provider_cooldown(
            Some(StatusCode::TOO_MANY_REQUESTS),
            1,
            Some(Duration::from_secs(1800)),
            Some(60)
        ),
        Duration::from_secs(1800)
    );
}

#[test]
fn provider_probe_lease_covers_the_configured_timeout() {
    assert_eq!(provider_probe_lease(None), PROVIDER_PROBE_LEASE);
    assert_eq!(provider_probe_lease(Some(0)), PROVIDER_PROBE_LEASE);
    assert_eq!(
        provider_probe_lease(Some(3600)),
        Duration::from_secs(3630),
        "a provider allowed to wait an hour must reserve the probe for that long"
    );
}

#[tokio::test]
async fn a_long_provider_timeout_keeps_the_half_open_probe_reserved() {
    let state = circuit_state().await;
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));
    state
        .provider_probe
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(200));

    let lease = provider_probe_lease(Some(3600));
    assert!(
        admit_provider_attempt_with_lease(&state, 1, lease)
            .await
            .is_err(),
        "an in-flight slow probe must keep the provider half-open"
    );
    assert!(
        admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok(),
        "the old lease still expires for direct callers using the minimum"
    );
}

#[tokio::test]
async fn concurrent_failures_never_shorten_key_or_model_cooldowns() {
    let state = circuit_state().await;
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.upstream_model = "model-a".to_string();

    mark_target_error(
        &state,
        &target,
        StatusCode::TOO_MANY_REQUESTS,
        Some(Duration::from_secs(120)),
    )
    .await;
    let target_deadline = *state
        .target_cooldown
        .lock()
        .await
        .get(&(7, "model-a".to_string()))
        .unwrap();
    mark_target_error(&state, &target, StatusCode::SERVICE_UNAVAILABLE, None).await;
    assert_eq!(
        state
            .target_cooldown
            .lock()
            .await
            .get(&(7, "model-a".to_string()))
            .copied(),
        Some(target_deadline),
        "a shorter model failure must not cut an active cooldown short"
    );

    mark_provider_api_key_error(
        &state,
        Some(11),
        StatusCode::TOO_MANY_REQUESTS,
        "busy",
        Some(Duration::from_secs(120)),
    )
    .await;
    let key_deadline = *state.provider_key_cooldown.lock().await.get(&11).unwrap();
    mark_provider_api_key_error(
        &state,
        Some(11),
        StatusCode::SERVICE_UNAVAILABLE,
        "boom",
        None,
    )
    .await;
    assert_eq!(
        state.provider_key_cooldown.lock().await.get(&11).copied(),
        Some(key_deadline),
        "a shorter key failure must not cut an active cooldown short"
    );
}

#[tokio::test]
async fn provider_cooldown_is_set_for_retryable_failures_and_cleared_on_success() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);

    mark_provider_error(&state, 7, Some(StatusCode::TOO_MANY_REQUESTS), None, None).await;
    let until = state
        .provider_cooldown
        .lock()
        .await
        .get(&7)
        .copied()
        .unwrap();
    assert!(until > Instant::now() + Duration::from_secs(25));

    mark_provider_success(&state, 7).await;
    assert!(!state.provider_cooldown.lock().await.contains_key(&7));
}

#[tokio::test]
async fn provider_key_success_clears_recovered_error_state() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Recovered', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (
                id, provider_id, name, secret, enabled, last_error_at, last_error
             ) VALUES (
                11, 1, 'Primary', 'sk-primary', 1,
                '2026-01-01T00:00:00Z', 'stale unauthorized'
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    state.load_provider_key_error_state().await.unwrap();
    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, Instant::now() + Duration::from_secs(60));

    mark_provider_api_key_success(&state, Some(11)).await;

    assert!(!state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(!state.provider_key_error_state.lock().await.contains(&11));
    let error: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT last_error_at, last_error FROM provider_api_keys WHERE id = 11")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(error, (None, None));
}
