use super::*;
use crate::models::{ContextBreakdown, ContextCategory, Usage};

fn completed_usage() -> Usage {
    Usage {
        prompt_tokens: 120,
        completion_tokens: 30,
        total_tokens: 150,
        cache_read_tokens: 40,
        cache_write_tokens: 5,
    }
}

#[test]
fn activity_lifecycle_tracks_attempts_context_and_cache() {
    let store = RequestActivityStore::for_test(16, Duration::from_secs(60));
    let started = store.start("request-1", "/v1/chat/completions").unwrap();
    assert_eq!(started.kind, "started");
    store.describe(
        "request-1",
        ActivityDescription {
            requested_model: "model-a".to_string(),
            streamed: true,
            session_id: Some("session-a".to_string()),
            api_key_id: Some(7),
            api_key_name: Some("desktop".to_string()),
            request_tokens_estimated: 100,
            context: Some(ContextBreakdown {
                window_tokens: None,
                input_tokens: 100,
                estimated: true,
                input_estimated: true,
                categories: vec![
                    ContextCategory {
                        key: "system".to_string(),
                        tokens: 20,
                        items: 1,
                    },
                    ContextCategory {
                        key: "conversation".to_string(),
                        tokens: 80,
                        items: 1,
                    },
                ],
                ..Default::default()
            }),
        },
    );
    store.set_route("request-1", Some(9), Some("Primary"), "priority", vec![]);
    store.begin_attempt(
        "request-1",
        ActivityAttemptStart {
            attempt_index: 1,
            round: 0,
            candidate_order: 1,
            target: ActivityTarget {
                provider_id: 2,
                provider_name: "Provider".to_string(),
                provider_api_key_id: Some(3),
                upstream_model: "upstream-a".to_string(),
                context_window_tokens: Some(1_000),
                input_limit: None,
            },
        },
    );
    store.finish_attempt(
        "request-1",
        ActivityAttemptFinish {
            attempt_index: 1,
            latency_ms: 25,
            status_code: Some(200),
            success: true,
            reason_code: "upstream_success".to_string(),
            failure_scope: None,
        },
    );
    store.first_token("request-1", 70);
    store.observe_usage("request-1", true, true);
    store.finish(
        "request-1",
        ActivityFinish {
            status_code: 200,
            success: true,
            error_code: None,
            latency_ms: 210,
            first_token_ms: Some(70),
            usage: completed_usage(),
        },
    );

    let snapshot = store.snapshot("request-1").unwrap();
    assert_eq!(snapshot.phase, "completed");
    assert_eq!(snapshot.outcome, "success");
    assert_eq!(snapshot.attempts.len(), 1);
    assert!(snapshot.attempts[0].success);
    assert_eq!(snapshot.target.unwrap().provider_id, 2);
    assert_eq!(snapshot.prompt_tokens, Some(120));
    assert_eq!(snapshot.cache.read_tokens, 40);
    assert_eq!(snapshot.cache.hit_rate, Some(40.0 / 120.0));
    assert_eq!(snapshot.timings.time_to_first_token_ms, Some(70));
    let context = snapshot.context.unwrap();
    assert!(context.estimated);
    assert!(!context.input_estimated);
    assert_eq!(context.window_tokens, Some(1_000));
    assert_eq!(
        context
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<i64>(),
        120
    );
}

#[test]
fn activity_terminal_updates_are_idempotent_and_revisioned() {
    let store = RequestActivityStore::for_test(16, Duration::from_secs(60));
    store.start("request-2", "/v1/responses");
    store.describe(
        "request-2",
        ActivityDescription {
            requested_model: "model-b".to_string(),
            streamed: false,
            session_id: None,
            api_key_id: None,
            api_key_name: None,
            request_tokens_estimated: 10,
            context: None,
        },
    );
    let finish = ActivityFinish {
        status_code: 200,
        success: true,
        error_code: None,
        latency_ms: 30,
        first_token_ms: Some(30),
        usage: Usage::new(10, 2),
    };
    assert!(store.finish("request-2", finish.clone()).is_some());
    let before = store.snapshot("request-2").unwrap();
    assert!(store.finish("request-2", finish).is_none());
    let after = store.snapshot("request-2").unwrap();
    assert_eq!(before.revision, after.revision);
    assert_eq!(after.outcome, "success");
}

#[tokio::test]
async fn finished_activity_is_bounded_while_retaining_the_latest_snapshot() {
    let store = RequestActivityStore::for_test(1, Duration::from_millis(20));
    for request_id in ["old", "new"] {
        store.start(request_id, "/v1/chat/completions");
        store.finish(
            request_id,
            ActivityFinish {
                status_code: 200,
                success: true,
                error_code: None,
                latency_ms: 1,
                first_token_ms: None,
                usage: Usage::new(1, 1),
            },
        );
    }
    let recent = store.recent(10);
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].request_id, "new");

    tokio::time::sleep(Duration::from_millis(30)).await;
    let recent = store.recent(10);
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].request_id, "new");
    assert!(store.snapshot("new").is_some());
    assert!(store.snapshot("old").is_none());
}

fn test_attempt(index: i64) -> ActivityAttemptStart {
    ActivityAttemptStart {
        attempt_index: index,
        round: 0,
        candidate_order: index,
        target: ActivityTarget {
            provider_id: index,
            provider_name: "供应商".repeat(100),
            provider_api_key_id: None,
            upstream_model: "model".repeat(100),
            context_window_tokens: None,
            input_limit: None,
        },
    }
}

#[tokio::test]
async fn retained_snapshot_preserves_attempt_results_reported_after_completion() {
    let store = RequestActivityStore::for_test(1, Duration::from_millis(20));
    store.start("late-attempt", "/v1/chat/completions");
    store.begin_attempt("late-attempt", test_attempt(1));
    store.finish(
        "late-attempt",
        ActivityFinish {
            status_code: 200,
            success: true,
            error_code: None,
            latency_ms: 25,
            first_token_ms: None,
            usage: Usage::new(1, 1),
        },
    );
    let event = store
        .finish_attempt(
            "late-attempt",
            ActivityAttemptFinish {
                attempt_index: 1,
                latency_ms: 20,
                status_code: Some(200),
                success: true,
                reason_code: "upstream_success".to_string(),
                failure_scope: None,
            },
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let snapshot = store.snapshot("late-attempt").unwrap();
    assert_eq!(snapshot.revision, event.snapshot.revision);
    assert_eq!(snapshot.attempts[0].status_code, Some(200));
    assert!(snapshot.attempts[0].success);
    assert_eq!(
        snapshot.attempts[0].reason_code.as_deref(),
        Some("upstream_success")
    );
    assert_eq!(store.recent(10).items[0].revision, snapshot.revision);
}

#[test]
fn activity_attempt_counts_survive_reordering_and_truncation() {
    let store = RequestActivityStore::for_test(1, Duration::from_secs(60));
    store.start("attempts", "/v1/messages");
    for index in 1..=40 {
        store.set_route("attempts", None, None, "priority", vec![]);
        store.begin_attempt("attempts", test_attempt(index));
        assert!(
            store
                .begin_attempt("attempts", test_attempt(index))
                .is_none()
        );
        store.finish_attempt(
            "attempts",
            ActivityAttemptFinish {
                attempt_index: index,
                latency_ms: 10,
                status_code: Some(503),
                success: false,
                reason_code: "provider_short_circuit".to_string(),
                failure_scope: None,
            },
        );
    }
    let candidates = (1..=1000)
        .map(|order| ActivityCandidate {
            order,
            provider_id: order,
            provider_name: Some("供应商".repeat(100)),
            provider_api_key_id: None,
            upstream_model: "model".repeat(100),
            eligible: true,
            deprioritized: false,
            reason_code: "reason".repeat(100),
        })
        .collect();
    store.set_route("attempts", None, None, "priority", candidates);
    let snapshot = store.snapshot("attempts").unwrap();
    assert_eq!(snapshot.attempt_count, 40);
    assert_eq!(snapshot.fallback_count, 39);
    assert_eq!(snapshot.attempts.len(), 32);
    assert_eq!(snapshot.attempts.first().unwrap().attempt_index, 9);
    assert_eq!(snapshot.attempts.last().unwrap().attempt_index, 40);
    assert!(snapshot.attempts_truncated);
    assert_eq!(snapshot.candidate_count, 1000);
    assert_eq!(snapshot.candidates.len(), 32);
    assert!(snapshot.candidates_truncated);
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < 65_536);
    store.start("active", "/v1/messages");
    store.cancel("attempts", "client_cancelled");
    assert_eq!(store.recent(200).active_count, 1);
    assert_eq!(store.snapshot("active").unwrap().outcome, "running");
}

#[test]
fn activity_reports_cache_before_completion_and_distinguishes_zero() {
    let store = RequestActivityStore::for_test(1, Duration::from_secs(60));
    store.start("cache", "/v1/messages");
    assert_eq!(store.snapshot("cache").unwrap().cache.hit_rate, None);
    store.report_usage("cache", completed_usage(), true, true);
    let snapshot = store.snapshot("cache").unwrap();
    assert_eq!(snapshot.outcome, "running");
    assert_eq!(snapshot.cache.hit_rate, Some(40.0 / 120.0));
    store.report_usage("cache", Usage::new(120, 0), true, true);
    assert_eq!(store.snapshot("cache").unwrap().cache.hit_rate, Some(0.0));
    assert!(
        store
            .report_usage("cache", Usage::new(120, 0), true, true)
            .is_none()
    );
}

#[test]
fn activity_broadcast_sequences_follow_publication_order() {
    let store = RequestActivityStore::for_test(1, Duration::from_secs(60));
    let (events, mut receiver) = broadcast::channel(2048);
    store.lock().events = Some(events);
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let store = &store;
            scope.spawn(move || {
                for index in 0..32 {
                    let id = format!("{worker}-{index}");
                    store.start(&id, "/v1/responses");
                    store.cancel(&id, "client_cancelled");
                }
            });
        }
    });
    let mut sequence = 0;
    while let Ok(event) = receiver.try_recv() {
        sequence += 1;
        assert_eq!(event.sequence, sequence);
    }
    assert_eq!(sequence, 512);
}

#[test]
fn activity_disabled_and_nonstreamed_states_are_explicit() {
    let mut store = RequestActivityStore::for_test(1, Duration::from_secs(60));
    store.enabled = false;
    assert!(store.start("disabled", "/v1/responses").is_none());
    assert!(!store.recent(50).enabled);
    store.enabled = true;
    store.start("nonstream", "/v1/responses");
    assert!(store.first_token("nonstream", 10).is_none());
    store.finish(
        "nonstream",
        ActivityFinish {
            success: true,
            status_code: 200,
            error_code: None,
            latency_ms: 10,
            first_token_ms: Some(10),
            usage: Usage::new(0, 0),
        },
    );
    let snapshot = store.snapshot("nonstream").unwrap();
    assert_eq!(snapshot.timings.time_to_first_token_ms, None);
    assert_eq!(snapshot.timings.generation_ms, None);
    assert!(store.waiting("nonstream", "late").is_none());
}
