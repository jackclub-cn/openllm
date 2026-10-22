use super::*;

#[tokio::test]
async fn analytics_context_aggregates_samples_and_agent_dimensions() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, key_suffix) VALUES
            (1, 'Codex', 'context-hash-1', 'sk-codex', '1111'),
            (2, 'CI', 'context-hash-2', 'sk-ci', '2222')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let now = Utc::now();
    let measured = json!({
        "input_tokens": 120,
        "estimated": true,
        "input_estimated": false,
        "categories": [
            {"key": "system", "tokens": 20, "items": 1},
            {"key": "conversation", "tokens": 40, "items": 3},
            {"key": "tool_results", "tokens": 60, "items": 2}
        ],
        "unmetered_items": 0,
        "opaque_reasoning_items": 0
    })
    .to_string();
    let estimated = json!({
        "input_tokens": 80,
        "estimated": true,
        "input_estimated": true,
        "categories": [
            {"key": "system", "tokens": 10, "items": 1},
            {"key": "conversation", "tokens": 50, "items": 2},
            {"key": "tools", "tokens": 20, "items": 4}
        ],
        "unmetered_items": 1,
        "opaque_reasoning_items": 2
    })
    .to_string();

    for (request_id, api_key_id, model, session_id, prompt, cache_read, context, age) in [
        (
            "measured",
            Some(1_i64),
            "gpt-5.1",
            Some("s1"),
            120_i64,
            80_i64,
            Some(measured),
            10_i64,
        ),
        (
            "estimated",
            Some(1_i64),
            "gpt-5.1",
            Some("s1"),
            80,
            0,
            Some(estimated),
            5,
        ),
        (
            "other-model",
            Some(1_i64),
            "deepseek-v4",
            Some("s2"),
            50,
            10,
            None,
            1,
        ),
        (
            "ci",
            Some(2_i64),
            "claude-sonnet-4",
            Some("s3"),
            30,
            0,
            None,
            3,
        ),
        ("anonymous", None, "local-model", None, 10, 0, None, 0),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint, session_id,
                prompt_tokens, completion_tokens, total_tokens, cache_read_tokens,
                latency_ms, status_code, success, context_breakdown_json, created_at
             ) VALUES (?, ?, ?, '/v1/responses', ?, ?, 0, ?, ?, 100, 200, 1, ?, ?)",
        )
        .bind(request_id)
        .bind(api_key_id)
        .bind(model)
        .bind(session_id)
        .bind(prompt)
        .bind(prompt)
        .bind(cache_read)
        .bind(context)
        .bind((now - Duration::minutes(age)).to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let Json(view) = analytics_context(
        State(AppState::new(pool, None)),
        Query(OverviewQuery {
            from: Some((now - Duration::days(1)).to_rfc3339()),
            to: Some((now + Duration::days(1)).to_rfc3339()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(view.requests, 5);
    assert_eq!(view.sessions, 3);
    assert_eq!(view.context_requests, 2);
    assert_eq!(view.measured_requests, 1);
    assert_eq!(view.estimated_requests, 1);
    assert_eq!(view.agents.len(), 3);

    let codex = view
        .agents
        .iter()
        .find(|agent| agent.api_key_id == Some(1))
        .unwrap();
    assert_eq!(codex.label, "Codex");
    assert_eq!(codex.requests, 3);
    assert_eq!(codex.sessions, 2);
    assert_eq!(codex.context_samples, 2);
    assert_eq!(codex.measured_samples, 1);
    assert_eq!(codex.estimated_samples, 1);
    assert_eq!(codex.prompt_tokens, 250);
    assert_eq!(codex.max_prompt_tokens, 120);
    assert_eq!(codex.models, vec!["gpt-5.1", "deepseek-v4"]);
    assert_eq!(codex.unmetered_items, 1);
    assert_eq!(codex.opaque_reasoning_items, 2);
    assert_eq!(
        codex
            .category_tokens
            .iter()
            .map(|category| (category.key.as_str(), category.tokens))
            .collect::<Vec<_>>(),
        vec![
            ("system", 30),
            ("conversation", 90),
            ("tool_results", 60),
            ("tools", 20),
        ]
    );
    assert!((codex.cache_hit_rate - 36.0).abs() < 0.001);
}
