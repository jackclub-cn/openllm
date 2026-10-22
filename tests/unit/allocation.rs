use super::*;

use crate::models::{ProviderPool, ProviderPoolAllocation, ProviderPoolInput};

fn pool_row(
    id: i64,
    request_limit: Option<i64>,
    token_limit: Option<i64>,
    threshold_pct: i64,
) -> ProviderPool {
    ProviderPool {
        id,
        provider_id: 1,
        name: format!("pool-{id}"),
        enabled: 1,
        window_seconds: 60,
        request_limit,
        token_limit,
        saturation_threshold_pct: threshold_pct,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn allocation_row(
    pool_id: i64,
    api_key_id: i64,
    weight_pct: i64,
    policy: &str,
) -> ProviderPoolAllocation {
    ProviderPoolAllocation {
        id: api_key_id,
        pool_id,
        api_key_id,
        weight_pct,
        policy: policy.to_string(),
    }
}

/// A pool of 10 requests per minute, saturated at 50%, where key 1 owns 20%.
fn hard_share_pool() -> PoolSpec {
    let pool = pool_row(1, Some(10), None, 50);
    let allocations = vec![
        allocation_row(1, 1, 20, "hard"),
        allocation_row(1, 2, 20, "hard"),
    ];
    PoolSpec::from_rows(&pool, &allocations)
}

/// Below the saturation threshold any key may borrow idle capacity, so a burst
/// that is still inside the pool headroom is not blocked by its own share.
#[test]
fn pool_allows_borrowing_below_the_saturation_threshold() {
    let spec = hard_share_pool();
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    // Key 1's share is 2 requests, but the pool is only 10% full at 4 used, so
    // borrowing the idle headroom is allowed.
    for index in 0..4 {
        assert_eq!(
            ledger.decide(&spec, 1, base + Duration::from_secs(index)),
            PoolDecision::Allow,
            "below the saturation threshold the idle capacity is borrowable"
        );
        ledger.charge_requests(&spec, 1, 1, base + Duration::from_secs(index));
    }
    assert_eq!(ledger.usage(&spec, 1, base).requests, 4);
}

/// The scenario the feature exists for: key A bursts the pool to its
/// saturation point, and key B still gets its reserved share instead of the
/// whole pool being consumed.
#[test]
fn saturated_pool_holds_a_hard_share_for_a_quiet_key() {
    let spec = hard_share_pool();
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    // Key 1 borrows up to the saturation threshold (5 of 10 requests).
    for index in 0..5 {
        assert_eq!(
            ledger.decide(&spec, 1, base + Duration::from_secs(index)),
            PoolDecision::Allow
        );
        ledger.charge_requests(&spec, 1, 1, base + Duration::from_secs(index));
    }

    // At saturation the burst key is over its 20% share (2 requests) and is
    // refused; it already borrowed 5, so the refusal is not a lock-out, it is
    // the pool protecting the rest of the capacity.
    match ledger.decide(&spec, 1, base + Duration::from_secs(6)) {
        PoolDecision::Deny {
            reason,
            retry_after,
        } => {
            assert!(
                reason.contains("fair share exceeded"),
                "the denial must name the fair share, got: {reason}"
            );
            assert!(retry_after >= Duration::from_secs(1));
        }
        other => panic!("a saturated pool must hold the burst key to its share, got {other:?}"),
    }

    // Key 2 has used nothing, so its own share is still intact: the burst did
    // not consume the pool on its behalf.
    assert_eq!(
        ledger.decide(&spec, 2, base + Duration::from_secs(6)),
        PoolDecision::Allow
    );
    for offset in 0..2 {
        ledger.charge_requests(&spec, 2, 1, base + Duration::from_secs(6 + offset));
    }
    assert!(
        matches!(
            ledger.decide(&spec, 2, base + Duration::from_secs(9)),
            PoolDecision::Deny { .. }
        ),
        "key 2 is held to its own share too"
    );
}

/// `soft` never blocks: it lets the request through and only marks the key so
/// the caller can try it after keys that are inside their share.
#[test]
fn soft_allocation_penalizes_without_blocking() {
    let pool = pool_row(1, Some(4), None, 50);
    let allocations = vec![allocation_row(1, 9, 25, "soft")];
    let spec = PoolSpec::from_rows(&pool, &allocations);
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    // Key 9 uses its single reserved request, then the rest of the pool is
    // consumed by an unallocated borrower.
    ledger.charge_requests(&spec, 9, 1, base);
    ledger.charge_requests(&spec, ANONYMOUS_API_KEY_ID, 2, base);
    match ledger.decide(&spec, 9, base + Duration::from_millis(1)) {
        PoolDecision::Penalize { reason } => assert!(
            reason.contains("soft share"),
            "the penalty must explain the soft share, got: {reason}"
        ),
        other => panic!("a soft allocation must not hard-block, got {other:?}"),
    }
}

/// `burst` may use whatever headroom is left, but it can never push the pool
/// past its limit.
#[test]
fn burst_allocation_uses_headroom_but_respects_the_pool_limit() {
    let pool = pool_row(1, Some(4), None, 50);
    let allocations = vec![allocation_row(1, 9, 10, "burst")];
    let spec = PoolSpec::from_rows(&pool, &allocations);
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    ledger.charge_requests(&spec, ANONYMOUS_API_KEY_ID, 3, base);
    assert_eq!(
        ledger.decide(&spec, 9, base + Duration::from_millis(1)),
        PoolDecision::Allow,
        "a burst key may still use the remaining pool headroom"
    );
    ledger.charge_requests(&spec, 9, 1, base + Duration::from_millis(1));

    match ledger.decide(&spec, 9, base + Duration::from_millis(2)) {
        PoolDecision::Deny { reason, .. } => assert!(
            reason.contains("request window is exhausted"),
            "an exhausted pool must be reported as such, got: {reason}"
        ),
        other => panic!("the pool limit must still apply, got {other:?}"),
    }
}

/// Tokens are a second dimension: a key may be inside its request share and
/// still be held back by token usage.
#[test]
fn token_limit_is_enforced_independently_of_requests() {
    let pool = pool_row(1, None, Some(1000), 50);
    let allocations = vec![allocation_row(1, 7, 50, "hard")];
    let spec = PoolSpec::from_rows(&pool, &allocations);
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    ledger.charge_tokens(&spec, 7, 600, base);
    match ledger.decide(&spec, 7, base + Duration::from_millis(1)) {
        PoolDecision::Deny { reason, .. } => assert!(
            reason.contains("token fair share exceeded"),
            "got: {reason}"
        ),
        other => panic!("an over-share token key must be denied, got {other:?}"),
    }

    // Another key with no allocation only uses the remaining headroom.
    ledger.charge_tokens(&spec, ANONYMOUS_API_KEY_ID, 400, base);
    match ledger.decide(&spec, ANONYMOUS_API_KEY_ID, base + Duration::from_millis(2)) {
        PoolDecision::Deny { reason, .. } => assert!(reason.contains("token window is exhausted")),
        other => panic!("an exhausted token pool must be denied, got {other:?}"),
    }
}

/// The rolling window has to forget old usage, or a pool would stay exhausted
/// forever after one burst.
#[test]
fn usage_expires_with_the_rolling_window() {
    let spec = hard_share_pool();
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);

    ledger.charge_requests(&spec, 1, 10, base);
    assert_eq!(
        ledger
            .usage(&spec, 1, base + Duration::from_secs(1))
            .requests,
        10
    );
    assert_eq!(
        ledger
            .usage(&spec, 1, base + Duration::from_secs(120))
            .requests,
        0,
        "usage older than the window must not count"
    );
    assert_eq!(
        ledger.decide(&spec, 1, base + Duration::from_secs(120)),
        PoolDecision::Allow
    );
}

/// A pool without limits has nothing to allocate, so it must not be loaded at
/// all (it would only cost a lookup per request).
#[test]
fn pools_without_limits_are_not_enforcing() {
    let pool = pool_row(1, None, None, 50);
    let spec = PoolSpec::from_rows(&pool, &[]);
    assert!(!spec.is_enforcing());
    let base = Instant::now();
    let mut ledger = AllocationLedger::new(base);
    ledger.charge_requests(&spec, 1, 100, base);
    assert_eq!(ledger.decide(&spec, 1, base), PoolDecision::Allow);
}

fn test_target(provider_id: i64, provider_name: &str) -> RouteTarget {
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
        reasoning_efforts: None,
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
        upstream_model: "m".to_string(),
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

async fn pool_test_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'primary', 'openai', 'https://primary.example/v1'),
            (2, 'backup', 'openai', 'https://backup.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_prefix, key_suffix, key_hash, enabled) VALUES
            (1, 'alpha', 'sk-a', 'aaaa', 'hash-a', 1),
            (2, 'beta', 'sk-b', 'bbbb', 'hash-b', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    AppState::new(pool, None)
}

async fn create_pool(
    state: &AppState,
    input: serde_json::Value,
) -> crate::models::ProviderPoolView {
    let axum::Json(view) = crate::api::create_provider_pool(
        axum::extract::State(state.clone()),
        axum::Json(serde_json::from_value::<ProviderPoolInput>(input).unwrap()),
    )
    .await
    .unwrap();
    view
}

/// A denied provider is dropped from the candidate list so the request can
/// still be served by a provider with headroom, and the decision is recorded
/// where an operator can read it.
#[tokio::test]
async fn denied_provider_is_excluded_and_the_decision_is_recorded() {
    let state = pool_test_state().await;
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "primary pool",
            "window_seconds": 60,
            "request_limit": 10,
            "saturation_threshold_pct": 50,
            "allocations": [{ "api_key_id": 1, "weight_pct": 20, "policy": "hard" }]
        }),
    )
    .await;

    let api_key = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    // Saturate the pool with this key's own burst.
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;

    let mut targets = vec![test_target(1, "primary"), test_target(2, "backup")];
    let outcome = crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut targets)
        .await
        .unwrap();
    assert_eq!(outcome.removed, 1, "the saturated provider is excluded");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].provider_id, 2, "the backup provider is kept");
    assert!(
        outcome
            .notes
            .iter()
            .any(|note| note.contains("fair share exceeded")),
        "the decision must be recorded for the request log: {:?}",
        outcome.notes
    );
    // A pool denial is a local routing decision, not a provider failure.
    assert!(state.provider_cooldown.lock().await.is_empty());
    assert!(state.provider_failure_streak.lock().await.is_empty());
}

/// When every candidate is over its allocation the request fails with a clear
/// `429` and a back-off hint, instead of pretending the upstream is broken.
#[tokio::test]
async fn every_candidate_denied_returns_429_with_retry_after() {
    let state = pool_test_state().await;
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "primary pool",
            "request_limit": 4,
            "saturation_threshold_pct": 25,
            "allocations": [{ "api_key_id": 1, "weight_pct": 25, "policy": "hard" }]
        }),
    )
    .await;
    let api_key = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..4 {
        crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    }

    let mut targets = vec![test_target(1, "primary")];
    let error = crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut targets)
        .await
        .expect_err("a fully denied request must fail");
    match error {
        AppError::UpstreamStatus {
            status,
            message,
            retry_after,
        } => {
            assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
            assert!(
                message.contains("provider pool"),
                "the error must name the pool, got: {message}"
            );
            assert!(retry_after.is_some(), "a 429 must carry a Retry-After hint");
        }
        other => panic!("expected a 429, got {other:?}"),
    }
    assert!(state.provider_cooldown.lock().await.is_empty());
}

/// A pool with no limits, a disabled pool, and a provider without a pool all
/// leave routing untouched.
#[tokio::test]
async fn pools_that_do_not_constrain_leave_routing_untouched() {
    let state = pool_test_state().await;
    let api_key = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();

    // No pools at all.
    let mut targets = vec![test_target(1, "primary"), test_target(2, "backup")];
    let outcome = crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut targets)
        .await
        .unwrap();
    assert_eq!(outcome.removed, 0);
    assert!(outcome.notes.is_empty());
    assert_eq!(targets.len(), 2);

    // A disabled pool must not enforce either.
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "disabled pool",
            "enabled": false,
            "request_limit": 1,
            "saturation_threshold_pct": 50,
            "allocations": [{ "api_key_id": 1, "weight_pct": 1, "policy": "hard" }]
        }),
    )
    .await;
    let mut targets = vec![test_target(1, "primary")];
    let outcome = crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut targets)
        .await
        .unwrap();
    assert_eq!(outcome.removed, 0);
    assert_eq!(targets.len(), 1);
}

/// Configuration is validated, audited, and invalidates the cached catalog so
/// the next request sees the change.
#[tokio::test]
async fn pool_configuration_is_validated_audited_and_cache_invalidating() {
    let state = pool_test_state().await;

    for invalid in [
        serde_json::json!({"provider_id": 1, "name": "no limits"}),
        serde_json::json!({"provider_id": 1, "name": "zero window", "request_limit": 5, "window_seconds": 0}),
        serde_json::json!({"provider_id": 1, "name": "negative limit", "request_limit": -1}),
        serde_json::json!({"provider_id": 1, "name": "bad threshold", "request_limit": 5, "saturation_threshold_pct": 0}),
        serde_json::json!({"provider_id": 1, "name": "bad policy", "request_limit": 5, "allocations": [{"api_key_id": 1, "policy": "sometimes"}]}),
        serde_json::json!({"provider_id": 1, "name": "unknown key", "request_limit": 5, "allocations": [{"api_key_id": 99}]}),
        serde_json::json!({"provider_id": 1, "name": "duplicate key", "request_limit": 5, "allocations": [{"api_key_id": 1}, {"api_key_id": 1}]}),
        serde_json::json!({"provider_id": 404, "name": "unknown provider", "request_limit": 5}),
    ] {
        let error = crate::api::create_provider_pool(
            axum::extract::State(state.clone()),
            axum::Json(serde_json::from_value::<ProviderPoolInput>(invalid.clone()).unwrap()),
        )
        .await
        .expect_err("invalid pool configuration must be rejected");
        assert!(
            matches!(error, AppError::BadRequest(_)),
            "unexpected error for {invalid}: {error:?}"
        );
    }

    let view = create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "shared",
            "request_limit": 100,
            "token_limit": 1000,
            "allocations": [{ "api_key_id": 1, "weight_pct": 30, "policy": "hard" }]
        }),
    )
    .await;
    assert_eq!(view.provider_name.as_deref(), Some("primary"));
    assert_eq!(view.allocations.len(), 1);
    assert_eq!(view.allocations[0].api_key_name.as_deref(), Some("alpha"));
    assert_eq!(
        view.allocations[0].request_share,
        Some(30),
        "the reported share must match the hot path arithmetic"
    );
    assert_eq!(view.allocations[0].token_share, Some(300));

    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE entity = 'provider_pool' AND action = 'create'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1, "pool changes must be audited");

    // The catalog was invalidated by the create, so the provider now reports a
    // pool and a fresh load sees it.
    let catalog = state.pool_catalog().await.unwrap();
    assert_eq!(catalog.pools_for_provider(1).len(), 1);
    assert!(catalog.pools_for_provider(2).is_empty());

    // Deleting the pool removes it (and its allocations) from the catalog.
    crate::api::delete_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
    )
    .await
    .unwrap();
    let catalog = state.pool_catalog().await.unwrap();
    assert!(catalog.is_empty());
    let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_pool_allocations")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(orphans, 0, "allocations cascade with their pool");
}

/// Completed responses charge their tokens to the pool, which is the dimension
/// token quotas are expressed in.
#[tokio::test]
async fn completed_usage_is_charged_to_the_pool() {
    let state = pool_test_state().await;
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "token pool",
            "token_limit": 1000,
            "saturation_threshold_pct": 50,
            "allocations": [{ "api_key_id": 1, "weight_pct": 50, "policy": "hard" }]
        }),
    )
    .await;

    crate::allocation::charge_token_usage(&state, Some(1), Some(1), 400).await;
    crate::allocation::charge_token_usage(&state, Some(1), Some(1), -5).await;
    crate::allocation::charge_token_usage(&state, Some(2), Some(1), 900).await;
    let usage = crate::allocation::pool_usage(&state, 1).await.unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].tokens, 400, "negative token counts are ignored");
    assert_eq!(usage[0].requests, 0);
    assert!(!usage[0].saturated);
    assert_eq!(usage[0].allocations[0].token_share, Some(500));
    assert_eq!(usage[0].allocations[0].key_tokens, 400);
}

/// Updating a pool keeps its allocations by default and can replace them.
#[tokio::test]
async fn pool_update_keeps_or_replaces_allocations() {
    let state = pool_test_state().await;
    let view = create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "shared",
            "request_limit": 10,
            "allocations": [{ "api_key_id": 1, "weight_pct": 40, "policy": "soft" }]
        }),
    )
    .await;

    let axum::Json(updated) = crate::api::update_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
        axum::Json(
            serde_json::from_value::<crate::models::ProviderPoolUpdate>(
                serde_json::json!({ "request_limit": 20 }),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(updated.request_limit, Some(20));
    assert_eq!(updated.allocations.len(), 1, "omitted allocations are kept");
    assert_eq!(updated.allocations[0].weight_pct, 40);
    assert_eq!(updated.allocations[0].request_share, Some(8));

    let axum::Json(replaced) = crate::api::update_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
        axum::Json(serde_json::from_value::<crate::models::ProviderPoolUpdate>(
            serde_json::json!({ "allocations": [{ "api_key_id": 2, "weight_pct": 60, "policy": "burst" }] }),
        )
        .unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(replaced.allocations.len(), 1);
    assert_eq!(replaced.allocations[0].api_key_id, 2);
    assert_eq!(replaced.allocations[0].policy, "burst");

    let catalog = state.pool_catalog().await.unwrap();
    let spec = &catalog.pools_for_provider(1)[0];
    assert_eq!(spec.allocations.len(), 1);
    assert_eq!(
        spec.allocations.get(&2).map(|allocation| allocation.policy),
        Some(AllocationPolicy::Burst)
    );
}

/// An explicit `null` clears a pool limit, while omitting the field keeps it.
///
/// The console sends the whole pool back on save, so "clear the request limit
/// but keep the token limit" has to round-trip. Before the `double_option`
/// deserializer both an explicit `null` and an absent field collapsed to
/// "keep the current value", so an operator's cleared ceiling silently came
/// back on the next read.
#[tokio::test]
async fn pool_update_can_clear_a_limit_with_an_explicit_null() {
    let state = pool_test_state().await;
    let view = create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "shared",
            "request_limit": 10,
            "token_limit": 1000
        }),
    )
    .await;

    let update = |body: serde_json::Value| {
        serde_json::from_value::<crate::models::ProviderPoolUpdate>(body).unwrap()
    };

    let axum::Json(cleared) = crate::api::update_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
        axum::Json(update(serde_json::json!({ "request_limit": null }))),
    )
    .await
    .unwrap();
    assert_eq!(
        cleared.request_limit, None,
        "an explicit null clears the request limit"
    );
    assert_eq!(
        cleared.token_limit,
        Some(1000),
        "the untouched token limit is kept"
    );

    // An omitted field must still mean "keep", not "clear".
    let axum::Json(kept) = crate::api::update_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
        axum::Json(update(serde_json::json!({ "name": "renamed" }))),
    )
    .await
    .unwrap();
    assert_eq!(kept.name, "renamed");
    assert_eq!(kept.token_limit, Some(1000));

    // Clearing every limit leaves a pool that can allocate nothing; the API
    // rejects it instead of silently keeping a stale ceiling.
    let all_cleared = crate::api::update_provider_pool(
        axum::extract::State(state.clone()),
        axum::extract::Path(view.id),
        axum::Json(update(serde_json::json!({ "token_limit": null }))),
    )
    .await;
    assert!(all_cleared.is_err(), "a pool must keep at least one limit");
}

/// Denials counted on the request path surface through the usage snapshot and
/// the metrics endpoint, so an operator can alert on a key being throttled by
/// its fair share instead of only reading the request log.
#[tokio::test]
async fn pool_denials_are_counted_for_metrics_and_snapshots() {
    let state = pool_test_state().await;
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "shared",
            "window_seconds": 60,
            "request_limit": 10,
            "saturation_threshold_pct": 50,
            "allocations": [{ "api_key_id": 1, "weight_pct": 20, "policy": "hard" }]
        }),
    )
    .await;

    let api_key = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..5 {
        crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;
    }

    // The diagnose preview also calls `decide`, and must not inflate the counter.
    let diagnose_targets = vec![test_target(1, "primary"), test_target(2, "backup")];
    let mut preview = diagnose_targets.clone();
    let _ = crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut preview)
        .await
        .unwrap();

    let mut targets = vec![test_target(1, "primary"), test_target(2, "backup")];
    crate::allocation::apply_allocation_filter(&state, Some(&api_key), &mut targets)
        .await
        .unwrap();

    let usage = crate::allocation::all_pool_usage(&state).await.unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].name, "shared");
    assert!(usage[0].saturated);
    assert_eq!(usage[0].denials, 2, "each denial is counted once");
    assert_eq!(usage[0].request_limit, Some(10));

    let scoped = crate::allocation::pool_usage(&state, 1).await.unwrap();
    assert_eq!(scoped[0].denials, 2);
}

/// The back-off on a denial must cover the whole window, not just the bucket the
/// charge landed in: buckets are 1/12 of the window, so reporting the current
/// bucket's remainder told clients to retry ~12× too early.
#[tokio::test]
async fn a_denial_asks_the_client_to_wait_for_the_whole_window() {
    let state = pool_test_state().await;
    create_pool(
        &state,
        serde_json::json!({
            "provider_id": 1,
            "name": "shared",
            "window_seconds": 60,
            "request_limit": 1,
            "saturation_threshold_pct": 50,
            "allocations": [{ "api_key_id": 1, "weight_pct": 100, "policy": "hard" }]
        }),
    )
    .await;
    // One request fills the only slot in the window.
    crate::allocation::charge_target_attempt(&state, 1, Some(1)).await;

    let catalog = state.pool_catalog().await.unwrap();
    let spec = catalog.pools()[0];
    let decision = {
        let mut ledger = state.allocation_ledger.lock().await;
        ledger.decide(spec, 1, std::time::Instant::now())
    };
    let crate::allocation::PoolDecision::Deny { retry_after, .. } = decision else {
        panic!("the window is full, so the next request must be denied: {decision:?}");
    };
    assert!(
        retry_after >= std::time::Duration::from_secs(55),
        "the client would retry far too early: {retry_after:?}"
    );
    assert!(
        retry_after <= std::time::Duration::from_secs(60),
        "the wait must not exceed the window: {retry_after:?}"
    );
}
