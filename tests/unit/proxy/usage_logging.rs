use super::*;
use crate::models::{ContextBreakdown, ContextCategory};

#[tokio::test]
async fn usage_cost_uses_manual_price_overrides() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'priced', 'openai', 'http://priced')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, enabled,
                cost_input_override, cost_output_override
             ) VALUES (1, 'model', 1, 2.0, 4.0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let cost = estimate_usage_cost(&state, 1, "model", Usage::new(1_000_000, 1_000_000))
        .await
        .unwrap();
    assert_eq!(cost, 6_000_000);
}

#[tokio::test]
async fn in_flight_usage_log_is_replaced_by_final_status() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    log_usage_started(
        &state,
        "request-in-flight",
        Some("session-123"),
        None,
        None,
        "test-model",
        "/v1/chat/completions",
        10,
        false,
        None,
    )
    .await;
    let pending: (i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT in_flight, status_code, prompt_tokens, total_tokens, session_id
             FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, (1, 0, 10, 10, Some("session-123".to_string())));

    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (7, 'test-provider', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 7, 'Primary', 'sk-primary', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    log_usage_target(
        &state,
        "request-in-flight",
        7,
        "upstream-test-model",
        Some(11),
    )
    .await;
    let target: (i64, String, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT provider_id, upstream_model, provider_api_key_id, provider_api_key_name
             FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        target,
        (
            7,
            "upstream-test-model".to_string(),
            Some(11),
            Some("Primary".to_string())
        )
    );

    log_usage(
        &state,
        UsageLogEntry {
            request_id: "request-in-flight",
            api_key_id: None,
            route_id: None,
            provider_id: Some(7),
            requested_model: "test-model",
            upstream_model: Some("upstream-test-model"),
            endpoint: "/v1/chat/completions",
            usage: Usage::new(10, 5),
            latency_ms: 120,
            first_token_ms: None,
            status_code: 200,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: None,
        },
    )
    .await;

    #[allow(clippy::type_complexity)]
    let completed: (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT in_flight, status_code, prompt_tokens, completion_tokens,
                    total_tokens, provider_id, upstream_model, provider_api_key_id,
                    provider_api_key_name, session_id
             FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        completed,
        (
            0,
            200,
            10,
            5,
            15,
            7,
            "upstream-test-model".to_string(),
            Some(11),
            Some("Primary".to_string()),
            Some("session-123".to_string())
        )
    );

    log_usage_target(&state, "request-in-flight", 8, "late-update", None).await;
    let provider_id: i64 = sqlx::query_scalar(
        "SELECT provider_id FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(provider_id, 7);
    let provider_api_key_id: Option<i64> = sqlx::query_scalar(
        "SELECT provider_api_key_id FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(provider_api_key_id, Some(11));
    let provider_api_key_name: Option<String> = sqlx::query_scalar(
        "SELECT provider_api_key_name FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(provider_api_key_name.as_deref(), Some("Primary"));

    sqlx::query("DELETE FROM provider_api_keys WHERE id = 11")
        .execute(&pool)
        .await
        .unwrap();
    let snapshot: (Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT provider_api_key_id, provider_api_key_name
             FROM usage_logs WHERE request_id = 'request-in-flight'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(snapshot, (None, Some("Primary".to_string())));
}

#[tokio::test]
async fn usage_warning_appends_once_per_unique_message() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    log_usage_started(
        &state,
        "warning-test",
        None,
        None,
        None,
        "requested-model",
        OPENAI_RESPONSES,
        10,
        false,
        None,
    )
    .await;
    log_usage_warning(&state, "warning-test", "first warning").await;
    log_usage_warning(&state, "warning-test", "first warning").await;
    log_usage_warning(&state, "warning-test", "second warning").await;

    let warning: Option<String> =
        sqlx::query_scalar("SELECT warning_message FROM usage_logs WHERE request_id = ?")
            .bind("warning-test")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(warning.as_deref(), Some("first warning\nsecond warning"));
}

#[tokio::test]
async fn usage_log_persists_and_calibrates_context_breakdown() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    state
        .activity
        .start("context-request", OPENAI_CHAT_COMPLETIONS);
    state.activity.describe(
        "context-request",
        crate::state::ActivityDescription {
            requested_model: "model-a".to_string(),
            streamed: false,
            session_id: None,
            api_key_id: None,
            api_key_name: None,
            request_tokens_estimated: 100,
            context: Some(ContextBreakdown {
                window_tokens: Some(1_000),
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
    log_usage_started(
        &state,
        "context-request",
        None,
        None,
        None,
        "model-a",
        OPENAI_CHAT_COMPLETIONS,
        100,
        false,
        None,
    )
    .await;
    let initial: Option<String> = sqlx::query_scalar(
        "SELECT context_breakdown_json FROM usage_logs WHERE request_id = 'context-request'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(initial.is_some());

    state.activity.observe_usage("context-request", true, false);
    log_usage(
        &state,
        UsageLogEntry {
            request_id: "context-request",
            api_key_id: None,
            route_id: None,
            provider_id: None,
            requested_model: "model-a",
            upstream_model: None,
            endpoint: OPENAI_CHAT_COMPLETIONS,
            usage: Usage::new(137, 5),
            latency_ms: 50,
            first_token_ms: Some(50),
            status_code: 200,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: None,
        },
    )
    .await;

    let raw: String = sqlx::query_scalar(
        "SELECT context_breakdown_json FROM usage_logs WHERE request_id = 'context-request'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let context: ContextBreakdown = serde_json::from_str(&raw).unwrap();
    assert!(context.estimated);
    assert!(!context.input_estimated);
    assert_eq!(context.input_tokens, 137);
    assert_eq!(
        context
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<i64>(),
        137
    );

    let Json(detail) = crate::api::get_usage_detail(
        axum::extract::State(state),
        axum::extract::Path("context-request".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(detail.context, Some(context));
    assert_eq!(detail.cache_usage_reported, Some(false));
}
