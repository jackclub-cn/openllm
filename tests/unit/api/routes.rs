use super::*;

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
