use super::*;

#[test]
fn budget_header_requires_a_positive_number() {
    let mut headers = HeaderMap::new();
    assert_eq!(budget_usd_from_headers(&headers).unwrap(), None);

    headers.insert(BUDGET_USD_HEADER, HeaderValue::from_static("  "));
    assert_eq!(budget_usd_from_headers(&headers).unwrap(), None);

    headers.insert(BUDGET_USD_HEADER, HeaderValue::from_static("0.25"));
    assert_eq!(budget_usd_from_headers(&headers).unwrap(), Some(0.25));

    // A ceiling the caller cannot have meant is rejected instead of ignored,
    // because dropping it silently would leave them believing it applied.
    for invalid in ["0", "-1", "abc", "NaN", "inf"] {
        headers.insert(BUDGET_USD_HEADER, HeaderValue::from_str(invalid).unwrap());
        assert!(
            budget_usd_from_headers(&headers).is_err(),
            "{invalid} should be rejected"
        );
    }
}

#[test]
fn budget_filter_keeps_affordable_targets_and_drops_unknown_pricing() {
    let mut cheap = endpoint_test_target("openai", None);
    cheap.id = 1;
    cheap.provider_id = 1;
    cheap.cost = Some(json!({"input": 1.0, "output": 2.0}).to_string());
    let mut expensive = endpoint_test_target("openai", None);
    expensive.id = 2;
    expensive.provider_id = 2;
    expensive.cost = Some(json!({"input": 100.0, "output": 200.0}).to_string());
    let mut unpriced = endpoint_test_target("openai", None);
    unpriced.id = 3;
    unpriced.provider_id = 3;

    // 10k prompt tokens at $1/M plus 1k output at $2/M is $0.012 on the cheap
    // target and $1.2 on the expensive one.
    assert_eq!(estimate_target_cost_usd(&cheap, 10_000, 1_000), Some(0.012));
    assert_eq!(
        estimate_target_cost_usd(&expensive, 10_000, 1_000),
        Some(1.2)
    );
    assert_eq!(estimate_target_cost_usd(&unpriced, 10_000, 1_000), None);

    let mut targets = vec![cheap.clone(), expensive, unpriced.clone()];
    let removed = apply_budget_filter(&mut targets, 0.05, 10_000, 1_000).unwrap();
    // The expensive target busts the ceiling and the unpriced one cannot be
    // proven to fit it.
    assert_eq!(removed, 2);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, 1);

    // When nothing survives, the error names the budget and the cheapest
    // estimate so an operator can see what went wrong.
    let mut targets = vec![unpriced.clone()];
    let error = apply_budget_filter(&mut targets, 0.05, 10_000, 1_000).unwrap_err();
    assert!(error.to_string().contains("publishes pricing"));

    let mut targets = vec![cheap];
    let error = apply_budget_filter(&mut targets, 0.0001, 10_000, 1_000).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("$0.000100"), "{message}");
    assert!(message.contains("$0.012000"), "{message}");
}

#[tokio::test]
async fn over_budget_requests_are_rejected_and_logged() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled)
         VALUES (1, 'pricey', 'openai', 'http://127.0.0.1:9/v1', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled, cost)
         VALUES (1, 'budget-model', 1, '{\"input\":1000,\"output\":1000}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'budget route', 'budget-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (id, route_id, provider_id, upstream_model, enabled)
         VALUES (1, 1, 1, 'budget-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let body = json!({
        "model": "budget-model",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 1000
    })
    .to_string();
    let response = crate::build_router(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(BUDGET_USD_HEADER, "0.000001")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("budget"), "{body}");

    let (success, message): (i64, Option<String>) = sqlx::query_as(
        "SELECT success, error_message FROM usage_logs
         WHERE requested_model = 'budget-model'
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(success, 0);
    assert!(
        message
            .as_deref()
            .is_some_and(|value| value.contains("budget")),
        "{message:?}"
    );

    // The same request without a ceiling still reaches the (unreachable) mock
    // upstream, proving the ceiling is what rejected it.
    let unconstrained = crate::build_router(AppState::new(pool, None))
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "budget-model",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(unconstrained.status(), StatusCode::BAD_REQUEST);
}
