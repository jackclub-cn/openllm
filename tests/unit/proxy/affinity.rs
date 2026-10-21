use super::*;

#[test]
fn session_target_order_is_stable_and_spreads_weighted_sessions() {
    let base = (1..=4)
        .map(|id| {
            let mut target = endpoint_test_target("openai", None);
            target.id = id;
            target.provider_id = id;
            target.upstream_model = format!("model-{id}");
            target
        })
        .collect::<Vec<_>>();

    for strategy in [RouteStrategy::Weighted, RouteStrategy::RoundRobin] {
        let ordered_ids = |session_id: &str| {
            let mut targets = base.clone();
            order_targets_for_session(session_id, 7, strategy, &mut targets);
            targets
                .into_iter()
                .map(|target| target.id)
                .collect::<Vec<_>>()
        };

        let first = ordered_ids("session-a");
        assert_eq!(first, ordered_ids("session-a"));

        let mut primaries = Vec::new();
        for index in 0..64 {
            let primary = ordered_ids(&format!("session-{index}"))[0];
            if !primaries.contains(&primary) {
                primaries.push(primary);
            }
        }
        assert_eq!(
            primaries.len(),
            base.len(),
            "sessions should spread across every target for {strategy:?}"
        );
    }
}

#[test]
fn prompt_affinity_key_uses_explicit_ids_then_a_stable_first_prompt_prefix() {
    use crate::proxy::session_affinity_key;

    let mut headers = HeaderMap::new();
    let long_prompt = "x".repeat(96);
    let first = json!({
        "model": "model",
        "messages": [
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": long_prompt}
        ]
    });
    let later_turn = json!({
        "model": "model",
        "messages": [
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": long_prompt},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "follow up"}
        ]
    });

    let first_key = session_affinity_key(&headers, &first).unwrap();
    assert_eq!(
        first_key,
        session_affinity_key(&headers, &later_turn).unwrap()
    );
    assert!(first_key.starts_with("prompt:sha256:"));
    assert_ne!(
        first_key,
        session_affinity_key(
            &headers,
            &json!({"messages": [{"role": "user", "content": "y".repeat(96)}]})
        )
        .unwrap()
    );
    assert!(
        session_affinity_key(
            &headers,
            &json!({"messages": [{"role": "user", "content": "short"}]})
        )
        .is_none(),
        "short prompts should not create global affinity churn"
    );

    headers.insert("x-session-id", HeaderValue::from_static("session-a"));
    assert_eq!(
        session_affinity_key(&headers, &first).as_deref(),
        Some("session-a")
    );
}

#[test]
fn session_affinity_settings_parse_safe_bounds_and_keep_persistence_opt_in() {
    use crate::session_affinity::{
        MAX_SESSION_AFFINITY_CAPACITY, MAX_SESSION_AFFINITY_TTL_SECS,
        parse_session_affinity_capacity, parse_session_affinity_persist,
        parse_session_affinity_ttl,
    };

    assert_eq!(
        parse_session_affinity_ttl(None).unwrap(),
        Duration::from_secs(30 * 60)
    );
    assert_eq!(parse_session_affinity_ttl(Some("0")), None);
    assert_eq!(
        parse_session_affinity_ttl(Some("999999999")).unwrap(),
        Duration::from_secs(MAX_SESSION_AFFINITY_TTL_SECS)
    );
    assert_eq!(parse_session_affinity_capacity(None), 10_000);
    assert_eq!(
        parse_session_affinity_capacity(Some("999999999")),
        MAX_SESSION_AFFINITY_CAPACITY
    );
    assert!(!parse_session_affinity_persist(None));
    assert!(parse_session_affinity_persist(Some("true")));
    assert!(!parse_session_affinity_persist(Some("off")));
}

#[tokio::test]
async fn session_affinity_pins_the_target_and_rebinds_when_it_stops_being_healthy() {
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
        32,
        false,
    )));

    let mut preferred = endpoint_test_target("openai", None);
    preferred.id = 2;
    preferred.provider_id = 2;
    preferred.priority = 10;
    let mut fallback = endpoint_test_target("openai", None);
    fallback.id = 1;
    fallback.provider_id = 1;
    fallback.priority = 0;

    let first = order_targets(
        &state,
        7,
        "weighted",
        vec![preferred.clone()],
        Some("session-a"),
        Some(42),
    )
    .await
    .unwrap();
    assert_eq!(first[0].provider_id, 2);

    let repeated = order_targets(
        &state,
        7,
        "weighted",
        vec![fallback.clone(), preferred.clone()],
        Some("session-a"),
        Some(42),
    )
    .await
    .unwrap();
    assert_eq!(
        repeated[0].provider_id, 2,
        "the explicit session pin must keep weighted routing on its healthy target"
    );

    state
        .provider_cooldown
        .lock()
        .await
        .insert(2, Instant::now() + Duration::from_secs(60));
    let failed_over = order_targets(
        &state,
        7,
        "weighted",
        vec![fallback.clone(), preferred],
        Some("session-a"),
        Some(42),
    )
    .await
    .unwrap();
    assert_eq!(
        failed_over[0].provider_id, 1,
        "a cooling pin must not block failover"
    );
    let stats = state.session_affinity.as_ref().unwrap().stats().await;
    assert_eq!(stats.rebinds, 1);
}

#[tokio::test]
async fn priority_returns_to_primary_after_one_failure_in_the_same_session() {
    use crate::session_affinity::SessionAffinityStore;
    use std::sync::Arc;

    for model_failure in [false, true] {
        let mut state = circuit_state().await;
        let store = Arc::new(SessionAffinityStore::new(
            Duration::from_secs(60),
            32,
            false,
        ));
        state.session_affinity = Some(store.clone());
        let primary = endpoint_test_target("openai", None);
        let mut fallback = primary.clone();
        fallback.id = 2;
        fallback.priority = 1;
        if model_failure {
            fallback.upstream_model = "fallback-model".to_string();
        } else {
            fallback.provider_id = 2;
        }
        let targets = vec![fallback.clone(), primary.clone()];

        let first = order_targets_with_decision(
            &state,
            7,
            "priority",
            targets.clone(),
            Some("session-a"),
            Some(42),
        )
        .await
        .unwrap();
        assert_eq!(first.targets[0].id, primary.id);

        if model_failure {
            mark_target_error(&state, &primary, StatusCode::TOO_MANY_REQUESTS, None).await;
        } else {
            mark_provider_error(&state, primary.provider_id, None, None, None).await;
        }
        for _ in 0..3 {
            let cooling = order_targets_with_decision(
                &state,
                7,
                "priority",
                targets.clone(),
                Some("session-a"),
                Some(42),
            )
            .await
            .unwrap();
            assert_eq!(cooling.targets[0].id, fallback.id);
        }

        let expired = Instant::now() - Duration::from_secs(1);
        if model_failure {
            state.target_cooldown.lock().await.insert(
                (primary.provider_id, primary.upstream_model.clone()),
                expired,
            );
        } else {
            state
                .provider_cooldown
                .lock()
                .await
                .insert(primary.provider_id, expired);
        }
        for _ in 0..3 {
            let recovered = order_targets_with_decision(
                &state,
                7,
                "priority",
                targets.clone(),
                Some("session-a"),
                Some(42),
            )
            .await
            .unwrap();
            assert_eq!(
                recovered.targets[0].id, primary.id,
                "the same session must return to the primary once its cooldown expires"
            );
            assert_eq!(recovered.session_decision, "no_affinity");
        }
        let stats = store.stats().await;
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.hits + stats.misses + stats.rebinds, 0);
    }
}

#[tokio::test]
async fn priority_ignores_existing_session_pins_in_routing_and_diagnosis() {
    use crate::session_affinity::{
        SessionAffinityScope, SessionAffinityStore, SessionAffinityTarget,
    };
    use std::sync::Arc;

    let mut state = circuit_state().await;
    for sql in [
        "INSERT INTO providers (id, name, provider_type, base_url, last_test_ok) VALUES
             (1, 'primary', 'openai', 'http://primary', 1),
             (2, 'fallback', 'openai', 'http://fallback', 1)",
        "INSERT INTO provider_models (provider_id, model_name) VALUES (1, 'model'), (2, 'model')",
        "INSERT INTO routes (id, name, model_pattern, strategy)
             VALUES (7, 'priority route', 'shared', 'priority')",
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES
             (7, 1, 'model', 0), (7, 2, 'model', 1)",
    ] {
        sqlx::query(sql).execute(&state.pool).await.unwrap();
    }
    let store = Arc::new(SessionAffinityStore::new(Duration::from_secs(60), 32, true));
    let scope = SessionAffinityScope::new(None, 7, "session-a");
    store
        .bind(
            &state.pool,
            &scope,
            SessionAffinityTarget {
                provider_id: 2,
                upstream_model: "model".to_string(),
            },
        )
        .await;
    assert_eq!(store.stats().await.persistence_errors, 0);

    for active_store in [
        store,
        Arc::new(SessionAffinityStore::new(Duration::from_secs(60), 32, true)),
    ] {
        state.session_affinity = Some(active_store.clone());
        let resolved = resolve_route(&state, "shared", OPENAI_CHAT_COMPLETIONS)
            .await
            .unwrap();
        let ordered = order_targets_with_decision(
            &state,
            7,
            "priority",
            resolved.targets,
            Some("session-a"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(ordered.targets[0].provider_id, 1);
        assert_eq!(ordered.session_decision, "no_affinity");

        let diagnosis =
            diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
                .await
                .unwrap();
        let runtime = diagnosis.runtime_targets.as_ref().unwrap();
        assert_eq!(runtime[0].provider_id, 1);
        assert_eq!(runtime[0].decision_reason, "priority_order");
        assert!(diagnosis.candidates.iter().all(|candidate| {
            candidate
                .factors
                .iter()
                .all(|factor| factor.name != "session")
        }));
        let stats = active_store.stats().await;
        assert_eq!(stats.hits + stats.misses + stats.rebinds, 0);
    }
}

#[tokio::test]
async fn session_affinity_ttl_capacity_and_sqlite_mirror_are_bounded() {
    use crate::session_affinity::{
        SessionAffinityScope, SessionAffinityStore, SessionAffinityTarget,
    };

    let ttl_store = SessionAffinityStore::new(Duration::from_millis(5), 1, false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let scope = SessionAffinityScope::new(Some(1), 7, "session-a");
    let target = SessionAffinityTarget {
        provider_id: 1,
        upstream_model: "model".to_string(),
    };
    ttl_store.bind(&pool, &scope, target.clone()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(ttl_store.lookup(&pool, &scope).await.is_none());

    let capacity_store = SessionAffinityStore::new(Duration::from_secs(60), 1, false);
    capacity_store.bind(&pool, &scope, target.clone()).await;
    let other = SessionAffinityScope::new(Some(1), 7, "session-b");
    capacity_store.bind(&pool, &other, target.clone()).await;
    let stats = capacity_store.stats().await;
    assert_eq!(stats.entries, 1);
    assert!(stats.evictions >= 1);

    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Mirror provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let persistent_store = SessionAffinityStore::new(Duration::from_secs(60), 8, true);
    persistent_store.bind(&pool, &scope, target.clone()).await;
    let restarted_store = SessionAffinityStore::new(Duration::from_secs(60), 8, true);
    assert_eq!(
        restarted_store.lookup(&pool, &scope).await,
        Some(target),
        "the optional SQLite mirror should restore a live affinity pin"
    );
}
