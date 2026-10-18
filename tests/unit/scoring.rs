use super::*;

/// A minimal route target; this file has no access to the proxy test helpers,
/// so it builds its own to keep the scoring tests self-contained.
fn scoring_target(provider_id: i64, provider_name: &str) -> RouteTarget {
    RouteTarget {
        id: provider_id,
        route_id: None,
        provider_id,
        provider_name: provider_name.to_string(),
        provider_type: "openai".to_string(),
        base_url: "http://upstream".to_string(),
        model_prefix: String::new(),
        api_key: None,
        provider_headers: "{}".to_string(),
        supported_endpoints: None,
        cost: None,
        cost_input_override: None,
        cost_output_override: None,
        cost_cache_read_override: None,
        cost_cache_write_override: None,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        timeout_seconds: None,
        cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
        model_max_concurrency: None,
        model_queue_timeout_seconds: None,
        provider_health: None,
        upstream_model: "model".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        provider_key_max_concurrency: None,
        provider_key_requests_per_minute: None,
        provider_key_queue_limit: None,
        provider_key_queue_wait_ms: None,
        auth_retryable: false,
    }
}

fn cache_affinity_target(provider_id: i64, provider_name: &str, price: f64) -> RouteTarget {
    let mut target = scoring_target(provider_id, provider_name);
    target.cost = Some(
        json!({
            "input": price,
            "output": price
        })
        .to_string(),
    );
    target
}

async fn affinity_test_state() -> (
    AppState,
    std::sync::Arc<crate::session_affinity::SessionAffinityStore>,
) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled, last_test_ok)
         VALUES
            (1, 'provider-1', 'openai', 'http://upstream-1', 1, 1),
            (2, 'provider-2', 'openai', 'http://upstream-2', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut state = AppState::new(pool, None);
    let store = std::sync::Arc::new(crate::session_affinity::SessionAffinityStore::new(
        Duration::from_secs(60),
        16,
        false,
    ));
    state.session_affinity = Some(store.clone());
    (state, store)
}

async fn bind_affinity_pin(state: &AppState, provider_id: i64) {
    let store = state.session_affinity.as_ref().unwrap();
    let scope = crate::session_affinity::SessionAffinityScope::new(None, 7, "session-a");
    store
        .bind(
            &state.pool,
            &scope,
            crate::session_affinity::SessionAffinityTarget {
                provider_id,
                upstream_model: "model".to_string(),
            },
        )
        .await;
}

#[tokio::test]
async fn routing_decision_reports_session_affinity_hits_and_misses() {
    let (state, _) = affinity_test_state().await;
    bind_affinity_pin(&state, 1).await;
    let targets = vec![
        scoring_target(1, "provider-1"),
        scoring_target(2, "provider-2"),
    ];

    let hit = order_targets_with_decision(
        &state,
        7,
        "priority",
        targets.clone(),
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(hit.session_decision, "hit");
    assert_eq!(hit.targets[0].provider_id, 1);

    let miss = order_targets_with_decision(
        &state,
        7,
        "priority",
        targets.clone(),
        Some("session-b"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(miss.session_decision, "miss");

    let no_affinity = order_targets_with_decision(&state, 7, "priority", targets, None, None)
        .await
        .unwrap();
    assert_eq!(no_affinity.session_decision, "no_affinity");
}

async fn record_cache_usage(
    pool: &sqlx::SqlitePool,
    request_id: &str,
    provider_id: i64,
    prompt_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) {
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, provider_id, upstream_model, requested_model, endpoint,
             prompt_tokens, cache_read_tokens, cache_write_tokens,
             latency_ms, status_code, success, in_flight
         ) VALUES (?, ?, 'model', 'route-model', '/v1/chat/completions', ?, ?, ?, 100, 200, 1, 0)",
    )
    .bind(request_id)
    .bind(provider_id)
    .bind(prompt_tokens)
    .bind(cache_read_tokens)
    .bind(cache_write_tokens)
    .execute(pool)
    .await
    .unwrap();
}

async fn put_quota_snapshot(state: &AppState, used: f64, reset_at: Option<String>) {
    let now = chrono::Utc::now();
    let record = crate::models::ProviderQuotaSnapshotRecord {
        provider_id: 1,
        provider_key_id: None,
        model: "model".to_string(),
        window_key: "monthly".to_string(),
        used: Some(used),
        limit: Some(100.0),
        remaining: Some((100.0 - used).max(0.0)),
        unit: "credit".to_string(),
        reset_at,
        source: "manual".to_string(),
        confidence: "exact".to_string(),
        observed_at: now.to_rfc3339(),
        valid_until: (now + chrono::Duration::minutes(10)).to_rfc3339(),
    };
    let key = record.key();
    state.quota_snapshots.lock().await.insert(key, record);
}

async fn pinned_provider_id(state: &AppState) -> Option<i64> {
    session_affinity_context(state, 7, Some("session-a"), None, false)
        .await?
        .pinned
        .map(|pinned| pinned.provider_id)
}

#[test]
fn quota_paced_strategy_storage_round_trips() {
    let strategy = "quota_paced".parse::<RouteStrategy>().unwrap();
    assert_eq!(strategy, RouteStrategy::QuotaPaced);
    assert_eq!(strategy.as_str(), "quota_paced");
    assert_eq!(strategy.storage_values(), ("priority", "quota_paced"));
}

/// The diagnostic preview must score every candidate with an explainable factor
/// breakdown and, above all, do it without touching an upstream.
#[tokio::test]
async fn route_diagnosis_scores_candidates_without_an_upstream_call() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    // Provider 1 points at a closed port: any upstream attempt would fail
    // loudly, which is exactly what this test asserts never happens.
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'reachable', 'openai', 'http://127.0.0.1:9/v1', 1, 1),
                (2, 'disabled', 'openai', 'http://127.0.0.1:9/v1', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'model', '[\"/chat/completions\"]'),
                (2, 'model', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority, enabled)
             VALUES (1, 1, 'model', 0, 1), (1, 2, 'model', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let diagnosis = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    assert_eq!(diagnosis.candidates.len(), 2);

    let candidate = &diagnosis.candidates[0];
    assert!(
        candidate.eligible,
        "the reachable target must be selectable"
    );
    assert!(candidate.exclusion.is_none());
    assert!(!candidate.deprioritized);
    assert!(
        candidate.score > 0.0 && candidate.score <= 100.0,
        "the score must stay inside the documented range, got {}",
        candidate.score
    );

    let names = candidate
        .factors
        .iter()
        .map(|factor| factor.name.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "circuit",
        "capacity",
        "health",
        "reliability",
        "latency",
        "cost",
        "strategy",
    ] {
        assert!(
            names.contains(&expected),
            "missing factor {expected}: {names:?}"
        );
    }
    // Health was tested, so it is known; everything unmeasured must stay
    // neutral instead of quietly penalising the candidate.
    let health = candidate
        .factors
        .iter()
        .find(|factor| factor.name == "health")
        .unwrap();
    assert_eq!(health.score, Some(1.0));
    let reliability = candidate
        .factors
        .iter()
        .find(|factor| factor.name == "reliability")
        .unwrap();
    assert!(
        reliability.score.is_none(),
        "an unmeasured success rate is unknown"
    );
    assert_eq!(reliability.contribution, reliability.weight * 0.5);
    let latency = candidate
        .factors
        .iter()
        .find(|factor| factor.name == "latency")
        .unwrap();
    assert!(latency.score.is_none());
    let allocation = candidate
        .factors
        .iter()
        .find(|factor| factor.name == "allocation");
    assert!(
        allocation.is_none(),
        "a provider without a pool must not get an allocation factor"
    );

    // The disabled target is still explained, just not as a candidate.
    let excluded = &diagnosis.candidates[1];
    assert!(!excluded.eligible);
    assert_eq!(excluded.score, 0.0);
    assert!(
        excluded
            .exclusion
            .as_deref()
            .is_some_and(|reason| reason.contains("provider is disabled")),
        "got {:?}",
        excluded.exclusion
    );
    assert!(excluded.order > candidate.order);

    // Proof that the preview made no upstream request.
    let usage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(usage, 0, "the preview must not record any request");

    // Active failure classifications are part of the diagnosis, so an
    // operator can see why a candidate is cooling without opening the
    // cooldown panel. A model-level classification wins over the provider's
    // broader fallback classification.
    let now = std::time::Instant::now();
    state.target_cooldown.lock().await.insert(
        (1, "model".to_string()),
        now + std::time::Duration::from_secs(30),
    );
    state.target_failure_meta.lock().await.insert(
        (1, "model".to_string()),
        crate::state::FailureMetadata {
            scope: "model",
            retry: "next_target",
            confidence: "exact",
            reason_code: "context_limit_exceeded",
            observed_at: now,
        },
    );
    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, now + std::time::Duration::from_secs(30));
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

    let diagnosis = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    let candidate = diagnosis
        .candidates
        .iter()
        .find(|candidate| candidate.provider_id == 1)
        .unwrap();
    assert_eq!(candidate.failure_scope, Some("model"));
    assert_eq!(candidate.failure_retry, Some("next_target"));
    assert_eq!(candidate.failure_confidence, Some("exact"));
    assert_eq!(
        candidate.failure_reason_code,
        Some("context_limit_exceeded")
    );
    assert!(candidate.failure_observed_seconds_ago.is_some());
}

#[tokio::test]
async fn session_affinity_factor_marks_the_pinned_target() {
    use crate::session_affinity::SessionAffinityStore;
    use std::sync::Arc;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    state.session_affinity = Some(Arc::new(SessionAffinityStore::new(
        Duration::from_secs(60),
        16,
        false,
    )));

    let first = scoring_target(1, "first");
    let second = scoring_target(2, "second");
    order_targets(
        &state,
        7,
        "priority",
        vec![second.clone()],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();

    let affinity = session_affinity_context(&state, 7, Some("session-a"), None, false)
        .await
        .expect("affinity is enabled in this test");
    let assessments = assess_targets(
        &state,
        RouteStrategy::Priority,
        &[first, second],
        None,
        Some(&affinity),
    )
    .await
    .unwrap();

    let pinned = assessments
        .get(&(2, "model".to_string()))
        .unwrap()
        .factors
        .iter()
        .find(|factor| factor.name == "session")
        .unwrap();
    assert_eq!(pinned.score, Some(1.0));
    assert!(pinned.detail.contains("命中会话粘性"));
    let other = assessments
        .get(&(1, "model".to_string()))
        .unwrap()
        .factors
        .iter()
        .find(|factor| factor.name == "session")
        .unwrap();
    assert_eq!(other.score, Some(0.0));
}

#[tokio::test]
async fn cache_backed_affinity_keeps_a_healthy_target_ahead_of_cheaper_ones() {
    let (state, _) = affinity_test_state().await;
    let cached = cache_affinity_target(1, "cached", 5.0);
    let cheaper = cache_affinity_target(2, "cheaper", 1.0);
    bind_affinity_pin(&state, 1).await;
    record_cache_usage(&state.pool, "cached-session", 1, 1000, 800, 50).await;

    let ordered = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cheaper.clone(), cached.clone()],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered[0].provider_id, 1,
        "cache reuse should outrank a modest short-term price difference"
    );

    let repeated = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cheaper, cached],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        repeated[0].provider_id, 1,
        "the same healthy session should stay on its cached target"
    );
}

#[tokio::test]
async fn missing_cache_evidence_does_not_override_cost_order() {
    let (state, _) = affinity_test_state().await;
    let pinned = cache_affinity_target(1, "pinned", 5.0);
    let cheaper = cache_affinity_target(2, "cheaper", 1.0);
    bind_affinity_pin(&state, 1).await;

    let ordered = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cheaper.clone(), pinned.clone()],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered[0].provider_id, 2,
        "without cache evidence, affinity must not block cost ordering"
    );

    let affinity = session_affinity_context(&state, 7, Some("session-a"), None, false)
        .await
        .expect("affinity is enabled");
    let assessments = assess_targets(
        &state,
        RouteStrategy::CostOptimized,
        &[pinned, cheaper],
        None,
        Some(&affinity),
    )
    .await
    .unwrap();
    let session = assessments
        .get(&(1, "model".to_string()))
        .unwrap()
        .factors
        .iter()
        .find(|factor| factor.name == "session")
        .unwrap();
    assert_eq!(session.score, Some(1.0));
    assert_eq!(session.weight, 0.25);
    assert!(session.detail.contains("暂无缓存收益证据"));
}

#[tokio::test]
async fn low_or_exhausted_quota_breaks_cache_affinity() {
    for (case, used, reset_at) in [
        ("low", 95.0, None),
        (
            "exhausted",
            100.0,
            Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
        ),
    ] {
        let (state, _) = affinity_test_state().await;
        let cached = cache_affinity_target(1, "cached", 5.0);
        let cheaper = cache_affinity_target(2, "cheaper", 1.0);
        bind_affinity_pin(&state, 1).await;
        record_cache_usage(&state.pool, "cached-session", 1, 1000, 800, 50).await;
        put_quota_snapshot(&state, used, reset_at).await;

        let ordered = order_targets(
            &state,
            7,
            "cost_optimized",
            vec![cheaper, cached],
            Some("session-a"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            ordered[0].provider_id, 2,
            "{case} quota must break the cache-backed pin"
        );
    }
}

#[tokio::test]
async fn sink_cooldown_and_saturation_rebind_an_unhealthy_pin() {
    // A temporary rate-limit sink makes the pinned target non-primary without
    // removing it from the route, so the session should move to the sibling.
    let (state, _) = affinity_test_state().await;
    let cached = cache_affinity_target(1, "cached", 5.0);
    let fallback = cache_affinity_target(2, "fallback", 1.0);
    bind_affinity_pin(&state, 1).await;
    record_cache_usage(&state.pool, "cached-session", 1, 1000, 800, 50).await;
    mark_target_sink(&state, &cached, "rate_limit", None).await;
    let ordered = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cached, fallback.clone()],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(ordered[0].provider_id, 2);
    assert_eq!(pinned_provider_id(&state).await, Some(2));

    // A hard model cooldown removes the target before affinity is considered.
    let (state, _) = affinity_test_state().await;
    let cached = cache_affinity_target(1, "cached", 5.0);
    let fallback = cache_affinity_target(2, "fallback", 1.0);
    bind_affinity_pin(&state, 1).await;
    record_cache_usage(&state.pool, "cached-session", 1, 1000, 800, 50).await;
    state.target_cooldown.lock().await.insert(
        (1, "model".to_string()),
        std::time::Instant::now() + Duration::from_secs(60),
    );
    let ordered = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cached, fallback.clone()],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(ordered[0].provider_id, 2);
    assert_eq!(pinned_provider_id(&state).await, Some(2));

    // A saturated pinned target is deprioritized and the session is rebound.
    let (state, _) = affinity_test_state().await;
    let mut cached = cache_affinity_target(1, "cached", 5.0);
    cached.max_concurrency = Some(1);
    let fallback = cache_affinity_target(2, "fallback", 1.0);
    bind_affinity_pin(&state, 1).await;
    record_cache_usage(&state.pool, "cached-session", 1, 1000, 800, 50).await;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let held = semaphore.clone().acquire_owned().await.unwrap();
    state.provider_concurrency.lock().await.insert(1, semaphore);
    let ordered = order_targets(
        &state,
        7,
        "cost_optimized",
        vec![cached, fallback],
        Some("session-a"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(ordered[0].provider_id, 2);
    assert_eq!(pinned_provider_id(&state).await, Some(2));
    drop(held);
}

/// A provider at its concurrency cap ranks behind one with headroom, but is
/// never dropped: a single-target route must still be able to queue for it.
#[tokio::test]
async fn saturated_capacity_is_deprioritized_but_not_dropped() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);

    let busy = {
        let mut target = scoring_target(1, "busy");
        target.max_concurrency = Some(1);
        target
    };
    let roomy = scoring_target(2, "roomy");
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let held = semaphore.clone().acquire_owned().await.unwrap();
    state.provider_concurrency.lock().await.insert(1, semaphore);

    let ordered = order_targets(
        &state,
        1,
        "priority",
        vec![busy.clone(), roomy.clone()],
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(ordered.len(), 2, "a saturated candidate is never dropped");
    assert_eq!(
        ordered[0].provider_id, 2,
        "the candidate with headroom is tried first"
    );

    let assessments = assess_targets(&state, RouteStrategy::Priority, &ordered, None, None)
        .await
        .unwrap();
    let busy_assessment = assessments
        .get(&(1, busy.upstream_model.clone()))
        .expect("the busy provider is still scored");
    assert!(busy_assessment.deprioritized);
    assert!(
        busy_assessment.exclusion.is_none(),
        "saturation is a preference, not an exclusion"
    );
    let capacity = busy_assessment
        .factors
        .iter()
        .find(|factor| factor.name == "capacity")
        .unwrap();
    assert_eq!(capacity.score, Some(0.0));
    assert!(capacity.detail.contains("并发余量 0/1"));

    drop(held);
}

/// A candidate over its provider-pool share is excluded with the reason, and it
/// is ordered after candidates the request path would actually use.
#[tokio::test]
async fn diagnosis_excludes_a_candidate_over_its_pool_share() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled, last_test_ok) VALUES
                (1, 'pooled', 'openai', 'http://127.0.0.1:9/v1', 1, 1),
                (2, 'plain', 'openai', 'http://127.0.0.1:9/v1', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'model', '[\"/chat/completions\"]'),
                (2, 'model', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Provider 1 is the preferred target, provider 2 the fallback.
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority, enabled)
             VALUES (1, 1, 'model', 0, 1), (1, 2, 'model', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_prefix, key_suffix, key_hash, enabled)
             VALUES (1, 'alpha', 'sk-a', 'aaaa', 'hash-a', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let pool_input = serde_json::from_value::<crate::models::ProviderPoolInput>(json!({
        "provider_id": 1,
        "name": "shared",
        "request_limit": 2,
        "saturation_threshold_pct": 50,
        "allocations": [{ "api_key_id": 1, "weight_pct": 50, "policy": "hard" }]
    }))
    .unwrap();
    let created_pool = crate::api::create_provider_pool(
        axum::extract::State(state.clone()),
        axum::Json(pool_input),
    )
    .await
    .unwrap();
    assert_eq!(created_pool.0.provider_id, 1);
    // One request already used by that key saturates the pool (50% of 2) and
    // exhausts its single reserved request.
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;

    let diagnosis =
        diagnose_route_for_key(&state, "shared", OPENAI_CHAT_COMPLETIONS, None, Some(1))
            .await
            .unwrap();

    let denied = diagnosis
        .candidates
        .iter()
        .find(|candidate| candidate.provider_id == 1)
        .unwrap();
    assert!(
        !denied.eligible,
        "the over-share candidate must be excluded"
    );
    assert!(
        denied
            .exclusion
            .as_deref()
            .is_some_and(|reason| reason.contains("配额池") && reason.contains("fair share")),
        "got {:?}",
        denied.exclusion
    );
    let allowed = diagnosis
        .candidates
        .iter()
        .find(|candidate| candidate.provider_id == 2)
        .unwrap();
    assert!(allowed.eligible);
    assert!(
        allowed.order < denied.order,
        "an excluded candidate is listed after the ones that can serve"
    );
    // The exclusion is visible as a zero-scoring allocation factor too.
    let allocation = denied
        .factors
        .iter()
        .find(|factor| factor.name == "allocation")
        .unwrap();
    assert_eq!(allocation.score, Some(0.0));

    // A denyless key that borrows leftover capacity is not excluded.
    let borrower = diagnose_route_for_key(&state, "shared", OPENAI_CHAT_COMPLETIONS, None, None)
        .await
        .unwrap();
    let borrowed = borrower
        .candidates
        .iter()
        .find(|candidate| candidate.provider_id == 1)
        .unwrap();
    assert!(
        borrowed.eligible,
        "a key without an allocation only borrows headroom, so it stays eligible"
    );
}

/// A `soft` allocation is only a nudge: the key that is over its own share is
/// kept but tried last. Regression guard for the request path using the real
/// gateway key — it used to pass the anonymous id, so a soft over-share was
/// never deprioritised on a real request while the diagnose preview said it was.
#[tokio::test]
async fn a_soft_pool_over_share_moves_the_target_last_for_the_real_key() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled, last_test_ok) VALUES
                (1, 'pooled', 'openai', 'http://127.0.0.1:9/v1', 1, 1),
                (2, 'plain', 'openai', 'http://127.0.0.1:9/v1', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'model', '[\"/chat/completions\"]'),
                (2, 'model', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_prefix, key_suffix, key_hash, enabled)
             VALUES (1, 'alpha', 'sk-a', 'aaaa', 'hash-a', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let _ = crate::api::create_provider_pool(
        axum::extract::State(state.clone()),
        axum::Json(
            serde_json::from_value::<crate::models::ProviderPoolInput>(json!({
                "provider_id": 1,
                "name": "shared",
                "request_limit": 2,
                "saturation_threshold_pct": 50,
                "allocations": [{ "api_key_id": 1, "weight_pct": 50, "policy": "soft" }]
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    // One request saturates the pool (50% of 2) and puts the key at its share of
    // one request, which is exactly when a soft allocation starts penalising.
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;

    let preferred = scoring_target(1, "pooled");
    let fallback = RouteTarget {
        priority: 1,
        ..scoring_target(2, "plain")
    };

    let for_key = order_targets(
        &state,
        1,
        "priority",
        vec![preferred.clone(), fallback.clone()],
        None,
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        for_key[0].provider_id, 2,
        "the over-share target must be tried last for the key that is over it"
    );

    let without_key = order_targets(&state, 1, "priority", vec![preferred, fallback], None, None)
        .await
        .unwrap();
    assert_eq!(
        without_key[0].provider_id, 1,
        "without a gateway key nothing is over its share"
    );
}
