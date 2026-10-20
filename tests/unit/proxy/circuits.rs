use super::*;

#[test]
fn provider_open_threshold_parsing_keeps_a_safe_default() {
    use crate::state::{DEFAULT_PROVIDER_OPEN_THRESHOLD, parse_provider_open_threshold};

    // Missing, blank, and unparsable values keep the default so a typo cannot
    // silently disable the breaker.
    assert_eq!(
        parse_provider_open_threshold(None),
        DEFAULT_PROVIDER_OPEN_THRESHOLD
    );
    assert_eq!(
        parse_provider_open_threshold(Some("  ")),
        DEFAULT_PROVIDER_OPEN_THRESHOLD
    );
    assert_eq!(
        parse_provider_open_threshold(Some("nope")),
        DEFAULT_PROVIDER_OPEN_THRESHOLD
    );
    // An explicit zero disables the hard-open on purpose.
    assert_eq!(parse_provider_open_threshold(Some("0")), 0);
    assert_eq!(parse_provider_open_threshold(Some(" 5 ")), 5);
}

/// Below the threshold the cooldown only deprioritises a provider: the attempt
/// is still allowed, so one transient failure cannot turn into an outage.
#[tokio::test]
async fn provider_circuit_stays_soft_below_the_open_threshold() {
    let state = circuit_state().await;
    state.provider_failure_streak.lock().await.insert(1, 1);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() + Duration::from_secs(60));
    assert!(
        admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok(),
        "a provider below the threshold must still be attempted"
    );
}

/// At or above the threshold the circuit hard-opens: attempts are short-
/// circuited while the cooldown is active, and once it elapses exactly one
/// request probes recovery.
#[tokio::test]
async fn provider_circuit_hard_opens_then_admits_a_single_probe() {
    let state = circuit_state().await;
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() + Duration::from_secs(60));

    let retry_after = admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
        .await
        .expect_err("an open circuit must short-circuit the attempt");
    assert!(
        retry_after >= Duration::from_secs(1),
        "the block must carry a Retry-After hint, got {retry_after:?}"
    );

    // Cooldown elapsed: half-open, and only the first request is admitted.
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));
    assert!(
        admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok(),
        "the first request after the cooldown probes recovery"
    );
    assert!(
        admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_err(),
        "a second concurrent request must not stampede a recovering provider"
    );

    // A successful probe closes the circuit and frees the probe slot.
    mark_provider_success(&state, 1).await;
    assert!(
        admit_provider_attempt_with_lease(&state, 1, PROVIDER_PROBE_LEASE)
            .await
            .is_ok(),
        "after recovery the provider serves every request again"
    );
}

/// A half-open provider admits exactly one recovery probe. If that probe is
/// stopped before it ever reaches the upstream (a provider-pool denial), the
/// lease must be handed back, or the provider stays short-circuited for the whole
/// probe lease even though nothing tested it.
#[tokio::test]
async fn a_pool_denial_releases_the_half_open_probe() {
    let state = circuit_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'limited', 'openai', 'http://limited.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_pools (provider_id, name, enabled, window_seconds, \
             request_limit, token_limit, saturation_threshold_pct) \
         VALUES (1, 'cap', 1, 60, 1, NULL, 50)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    state.invalidate_pool_catalog().await;

    // Half-open: the streak is at the threshold and the cooldown has elapsed, so
    // the next attempt is admitted as the single recovery probe.
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));

    // Burn the pool's one request for the window so the probe attempt is denied.
    crate::allocation::charge_target_attempt(&state, 1, None).await;

    let target = endpoint_test_target("openai", None);
    let mut body = json!({"model": "model", "messages": []});
    let result = send_provider_request_with_compat_retry(
        &state,
        &target,
        None,
        "pool-denied-probe",
        OPENAI_CHAT_COMPLETIONS,
        UpstreamShape::Chat,
        false,
        &mut body,
        |_body: &Value| -> AppResult<RequestBuilder> {
            panic!("a pool denial must return before any upstream call")
        },
    )
    .await;

    match result {
        Err(AppError::UpstreamStatus { status, .. }) => {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        }
        Ok(_) => panic!("the saturated pool must deny the attempt"),
        Err(other) => panic!("expected a 429 pool denial, got {other:?}"),
    }
    assert!(
        !state.provider_probe.lock().await.contains_key(&1),
        "the denied probe must be released so the provider is not stuck half-open"
    );
}

/// A request that fails to build never reaches the upstream, so the half-open
/// probe the caller admitted must be handed back instead of being held for the
/// whole lease.
#[tokio::test]
async fn a_build_failure_releases_the_half_open_probe() {
    let state = circuit_state().await;
    // Half-open: the streak is at the threshold and the cooldown has elapsed, so
    // the attempt is admitted as the single recovery probe.
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));

    let target = endpoint_test_target("openai", None);
    let mut body = json!({"model": "model", "messages": []});
    let result = send_provider_request_with_compat_retry(
        &state,
        &target,
        None,
        "build-failure-probe",
        OPENAI_CHAT_COMPLETIONS,
        UpstreamShape::Chat,
        false,
        &mut body,
        |_body: &Value| -> AppResult<RequestBuilder> {
            Err(AppError::BadRequest("boom".to_string()))
        },
    )
    .await;

    match result {
        Err(AppError::BadRequest(message)) => assert_eq!(message, "boom"),
        Ok(_) => panic!("a build failure must propagate as an error"),
        Err(other) => panic!("expected the build error, got {other:?}"),
    }
    assert!(
        !state.provider_probe.lock().await.contains_key(&1),
        "a request that was never built must not hold the probe lease"
    );
}

/// The trip counter must count each hard-open so a flapping provider stands
/// out, but must not count failures that stay below the threshold.
#[tokio::test]
async fn provider_circuit_trip_counter_registers_hard_opens() {
    let state = circuit_state().await;
    let mut circuit_events = state.circuit_events.subscribe();
    let failure = StatusCode::SERVICE_UNAVAILABLE;

    // Two failures sit below the default threshold of three.
    for _ in 0..2 {
        mark_provider_error(&state, 1, Some(failure), None, None).await;
    }
    assert_eq!(
        state
            .provider_circuit_trips
            .lock()
            .await
            .get(&1)
            .copied()
            .unwrap_or(0),
        0,
        "failures below the threshold must not count as trips"
    );

    // The third crosses the threshold and trips the circuit.
    mark_provider_error(&state, 1, Some(failure), None, None).await;
    assert_eq!(
        state
            .provider_circuit_trips
            .lock()
            .await
            .get(&1)
            .copied()
            .unwrap_or(0),
        1,
        "crossing the threshold must register a trip"
    );
    let opened = circuit_events
        .try_recv()
        .expect("crossing the threshold must publish a circuit event");
    assert_eq!(opened.provider_id, 1);
    assert_eq!(opened.state, "open");
    assert_eq!(opened.reason, "failure_threshold");
    assert_eq!(opened.failure_streak, 3);
    assert!(opened.cooldown_seconds > 0);

    // A further failure from a request that was already in flight when the
    // circuit opened must not extend the streak or move the cooldown deadline.
    let opened_streak = state
        .provider_failure_streak
        .lock()
        .await
        .get(&1)
        .copied()
        .unwrap();
    let opened_until = state
        .provider_cooldown
        .lock()
        .await
        .get(&1)
        .copied()
        .unwrap();
    mark_provider_error(&state, 1, Some(failure), None, None).await;
    assert_eq!(
        state.provider_failure_streak.lock().await.get(&1).copied(),
        Some(opened_streak),
        "concurrent follower failures must not inflate the streak"
    );
    assert_eq!(
        state.provider_cooldown.lock().await.get(&1).copied(),
        Some(opened_until),
        "concurrent follower failures must not extend the cooldown"
    );
    assert_eq!(
        state
            .provider_circuit_trips
            .lock()
            .await
            .get(&1)
            .copied()
            .unwrap_or(0),
        1,
        "staying open across failures is not a new trip"
    );

    // Recovery resets the streak, so a fresh streak trips the circuit again.
    mark_provider_success(&state, 1).await;
    let closed = circuit_events
        .try_recv()
        .expect("recovery must publish a circuit event");
    assert_eq!(closed.provider_id, 1);
    assert_eq!(closed.state, "closed");
    assert_eq!(closed.reason, "successful_request");
    assert_eq!(closed.failure_streak, 0);
    for _ in 0..3 {
        mark_provider_error(&state, 1, Some(failure), None, None).await;
    }
    assert_eq!(
        state
            .provider_circuit_trips
            .lock()
            .await
            .get(&1)
            .copied()
            .unwrap_or(0),
        2,
        "a fresh streak after recovery trips the circuit again"
    );
    assert_eq!(
        circuit_events
            .try_recv()
            .expect("the fresh streak must publish another event")
            .state,
        "open"
    );
    assert!(circuit_events.try_recv().is_err());
}

#[tokio::test]
async fn a_failed_half_open_probe_still_escalates_the_open_circuit() {
    let state = circuit_state().await;
    state.provider_failure_streak.lock().await.insert(1, 3);
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() - Duration::from_secs(1));
    state.provider_probe.lock().await.insert(1, Instant::now());

    mark_provider_error(&state, 1, Some(StatusCode::SERVICE_UNAVAILABLE), None, None).await;

    assert_eq!(
        state.provider_failure_streak.lock().await.get(&1).copied(),
        Some(4),
        "a failed half-open probe must advance the streak"
    );
    assert!(
        state
            .provider_cooldown
            .lock()
            .await
            .get(&1)
            .is_some_and(|until| *until > Instant::now() + Duration::from_secs(60)),
        "a failed half-open probe must install an escalated cooldown"
    );
    assert!(!state.provider_probe.lock().await.contains_key(&1));
}
