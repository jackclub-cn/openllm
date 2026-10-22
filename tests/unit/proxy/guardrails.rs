use super::*;

#[test]
fn text_estimate_ignores_json_scaffolding() {
    // A short message must not be inflated by keys, braces and quotes.
    let body = json!({
        "model": "claude-x",
        "messages": [{"role": "user", "content": "hello"}]
    });
    let estimate = estimate_request_tokens(&body);
    // 5 chars of content -> 1 token, plus one message of framing (4).
    assert_eq!(estimate, 5);
    // Sanity: far below the old "stringify the whole payload" behaviour.
    let stringified = body.to_string().chars().count() / 4;
    assert!(estimate < stringified as i64, "{estimate} vs {stringified}");
}

#[test]
fn text_estimate_counts_nested_content_blocks() {
    let body = json!({
        "model": "claude-x",
        "system": "be brief",
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "abcdefgh"},
                {"type": "text", "text": "ijkl"}
            ]}
        ]
    });
    // 8 ("be brief") + 12 (content) = 20 chars -> 5 tokens + 4 framing.
    assert_eq!(estimate_request_tokens(&body), 9);
}

#[test]
fn text_estimate_counts_responses_instructions_reasoning_and_tool_outputs() {
    let body = json!({
        "instructions": "a".repeat(400),
        "input": [
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "b".repeat(100)}]
            },
            {
                "type": "function_call",
                "name": "shell",
                "arguments": "{\"command\":\"cargo test\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "c1",
                "output": "c".repeat(200)
            }
        ]
    });
    // 729 text-bearing characters -> 182 tokens, plus 12 framing tokens.
    assert_eq!(estimate_request_tokens(&body), 194);
}

#[test]
fn text_estimate_skips_image_payloads() {
    // A base64 image must not be metered as prompt text.
    let body = json!({
        "model": "claude-x",
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "describe"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAAAAAABBBBBBBBCCCCCCCC"}}
        ]}]
    });
    let estimate = estimate_request_tokens(&body);
    // Only "describe" (8 chars -> 2) plus one message of framing (4).
    assert_eq!(estimate, 6);
}

#[test]
fn text_estimate_is_deterministic_and_grows_with_content() {
    let small = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
    let large = json!({"model": "m", "messages": [{"role": "user", "content": "hi".repeat(200)}]});
    assert_eq!(
        estimate_request_tokens(&small),
        estimate_request_tokens(&small)
    );
    assert!(estimate_request_tokens(&large) > estimate_request_tokens(&small));
}

#[test]
fn detects_anthropic_clients_from_the_version_header() {
    let mut anthropic = HeaderMap::new();
    anthropic.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    assert!(wants_anthropic_models(&anthropic));

    // OpenAI clients authenticate with a bearer token and send no
    // anthropic-version header, so they keep the OpenAI shape.
    let mut openai = HeaderMap::new();
    openai.insert("authorization", HeaderValue::from_static("Bearer sk-test"));
    assert!(!wants_anthropic_models(&openai));
    assert!(!wants_anthropic_models(&HeaderMap::new()));
}

#[tokio::test]
async fn gateway_errors_expose_the_request_id_header() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);

    let response = proxy_openai(
        State(state.clone()),
        Extension(GatewayAuth(None)),
        HeaderMap::new(),
        "/v1/chat/completions".parse().unwrap(),
        Bytes::from_static(b"{"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let request_id = response
        .headers()
        .get("x-openllm-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(response.headers().get("x-request-id").unwrap(), request_id);

    let response = proxy_anthropic(
        State(state),
        Extension(GatewayAuth(None)),
        HeaderMap::new(),
        "/v1/messages".parse().unwrap(),
        Bytes::from_static(b"{"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let request_id = response
        .headers()
        .get("x-openllm-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(response.headers().get("x-request-id").unwrap(), request_id);
}

#[tokio::test]
async fn proxy_openai_rejects_blocked_guardrail_terms_before_routing() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?)")
        .bind(crate::state::SETTING_GUARDRAILS)
        .bind(
            json!({
                "blocked_terms": ["classified"],
                "max_prompt_tokens": null
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "unconfigured-model",
                "messages": [{"role": "user", "content": "read the classified file"}]
            })
            .to_string(),
        ),
        "guardrail-rejection",
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        response,
        AppError::BadRequest(message) if message.contains("guardrail")
    ));

    let (status, error_message): (i64, Option<String>) =
        sqlx::query_as("SELECT status_code, error_message FROM usage_logs WHERE request_id = ?")
            .bind("guardrail-rejection")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(status, 400);
    assert!(error_message.unwrap().contains("guardrail"));
}

#[tokio::test]
async fn proxy_anthropic_rejects_prompt_above_guardrail_token_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?)")
        .bind(crate::state::SETTING_GUARDRAILS)
        .bind(
            json!({
                "blocked_terms": [],
                "max_prompt_tokens": 1
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let error = proxy_anthropic_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/messages".parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "unconfigured-model",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "a long prompt"}]
            })
            .to_string(),
        ),
        "anthropic-guardrail-rejection",
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        AppError::BadRequest(message) if message.contains("guardrail")
    ));
}

#[tokio::test]
async fn console_api_key_selection_uses_id_without_exposing_secret() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE api_keys (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                key_suffix TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                last_used_at TEXT,
                created_at TEXT NOT NULL,
                daily_token_limit INTEGER,
                daily_cost_limit_micros INTEGER,
                requests_per_minute INTEGER,
                max_concurrency INTEGER,
                allowed_models TEXT,
                expires_at TEXT,
                routing_policy TEXT
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix, enabled, created_at
             ) VALUES (7, 'Console', 'hash', 'sk-con', 'sole', 1, '2026-10-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let error = selected_console_api_key(&state, &HeaderMap::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AppError::BadRequest(message) if message.contains("select a gateway API key")
    ));

    let mut headers = HeaderMap::new();
    headers.insert(CONSOLE_API_KEY_ID_HEADER, HeaderValue::from_static("7"));
    let key = selected_console_api_key(&state, &headers)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key.id, 7);
    assert_eq!(key.name, "Console");
    assert_eq!(key.key_suffix, "sole");
}

#[test]
fn requires_max_tokens_like_the_anthropic_api() {
    // Missing entirely: the live API answers 400, so the gateway must too
    // rather than silently defaulting and hiding a client bug.
    assert!(validate_anthropic_max_tokens(&json!({"messages": []})).is_err());
    // Present and positive is the only accepted form.
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 1})).is_ok());
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 4096})).is_ok());
    // Zero, negative and non-numeric values are rejected.
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 0})).is_err());
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": -5})).is_err());
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": "1024"})).is_err());
    assert!(validate_anthropic_max_tokens(&json!({"max_tokens": null})).is_err());
}

#[test]
fn forwards_anthropic_beta_flags_verbatim() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "anthropic-beta",
        HeaderValue::from_static("prompt-caching-2024-07-31"),
    );
    assert_eq!(
        anthropic_beta_of(&headers).as_deref(),
        Some("prompt-caching-2024-07-31")
    );

    // Absent or blank values must not produce a header, so the gateway
    // never invents feature flags the caller did not ask for.
    assert_eq!(anthropic_beta_of(&HeaderMap::new()), None);
    let mut blank = HeaderMap::new();
    blank.insert("anthropic-beta", HeaderValue::from_static("   "));
    assert_eq!(anthropic_beta_of(&blank), None);
}
