use super::*;

#[test]
fn resolves_stable_session_ids_from_headers_and_responses_body() {
    let mut headers = HeaderMap::new();
    headers.insert(
        OPENCODE_SESSION_HEADER,
        HeaderValue::from_static("opencode-session"),
    );
    headers.insert("session-id", HeaderValue::from_static("codex-session"));
    assert_eq!(
        upstream_session_id(&headers, &json!({})).as_deref(),
        Some("opencode-session")
    );

    let mut codex = HeaderMap::new();
    codex.insert("session-id", HeaderValue::from_static("codex-session"));
    assert_eq!(
        upstream_session_id(&codex, &json!({})).as_deref(),
        Some("codex-session")
    );
    assert_eq!(
        upstream_session_id(
            &HeaderMap::new(),
            &json!({"prompt_cache_key": "cache-session"})
        )
        .as_deref(),
        Some("cache-session")
    );
}

#[test]
fn adds_opencode_session_only_for_opencode_go_targets() {
    let mut opencode = endpoint_test_target("openai", None);
    opencode.provider_name = "OpenCode Go".to_string();
    let request = apply_opencode_session_header(
        reqwest::Client::new().post("http://upstream"),
        &opencode,
        Some("session-123"),
    )
    .build()
    .unwrap();
    assert_eq!(
        request.headers().get(OPENCODE_SESSION_HEADER).unwrap(),
        "session-123"
    );

    let other = endpoint_test_target("openai", None);
    let request = apply_opencode_session_header(
        reqwest::Client::new().post("http://upstream"),
        &other,
        Some("session-123"),
    )
    .build()
    .unwrap();
    assert!(request.headers().get(OPENCODE_SESSION_HEADER).is_none());
}

#[tokio::test]
async fn upstream_request_includes_the_resolved_session() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut opencode = endpoint_test_target("openai", None);
    opencode.provider_name = "OpenCode Go".to_string();

    let request = build_upstream_request(
        &state,
        "http://upstream/v1/chat/completions",
        ProviderType::Openai,
        &opencode,
        &json!({"model": "test"}),
        Some("session-123"),
    )
    .unwrap()
    .build()
    .unwrap();

    assert_eq!(
        request.headers().get(OPENCODE_SESSION_HEADER).unwrap(),
        "session-123"
    );
}

#[test]
fn configured_opencode_session_header_takes_precedence() {
    let mut opencode = endpoint_test_target("openai", None);
    opencode.base_url = "https://opencode.ai/zen/go/v1".to_string();
    opencode.provider_headers = r#"{"x-opencode-session":"configured-session"}"#.to_string();

    let request = apply_opencode_session_header(
        reqwest::Client::new().post("http://upstream"),
        &opencode,
        Some("request-session"),
    )
    .build()
    .unwrap();
    assert!(request.headers().get(OPENCODE_SESSION_HEADER).is_none());
}

#[tokio::test]
async fn prompt_affinity_keeps_round_robin_requests_on_one_upstream_without_a_session_header() {
    let (first_address, first_requests, first_server) = spawn_chat_upstream().await;
    let (second_address, second_requests, second_server) = spawn_chat_upstream().await;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
             id, name, provider_type, base_url, model_prefix, enabled
         ) VALUES
             (1, 'first', 'openai', ?, 'first/', 1),
             (2, 'second', 'openai', ?, 'second/', 1)",
    )
    .bind(format!("http://{first_address}"))
    .bind(format!("http://{second_address}"))
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
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled) VALUES
             (1, 'affinity route', 'affinity-model', 'round_robin', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
             route_id, provider_id, upstream_model, priority, weight, enabled
         ) VALUES
             (1, 1, 'model', 0, 100, 1),
             (1, 2, 'model', 0, 100, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let prefix = "stable prompt prefix ".repeat(8);
    for request_id in ["affinity-one", "affinity-two"] {
        let response = proxy_openai_inner(
            &state,
            &HeaderMap::new(),
            &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
            &Bytes::from(
                json!({
                    "model": "affinity-model",
                    "messages": [{"role": "user", "content": prefix.clone()}]
                })
                .to_string(),
            ),
            request_id,
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let first_count = first_requests.lock().await.len();
    let second_count = second_requests.lock().await.len();
    assert_eq!(
        first_count + second_count,
        2,
        "both requests must reach exactly one upstream"
    );
    assert!(
        first_count == 2 || second_count == 2,
        "the prompt prefix must keep round-robin calls on the same target, got {first_count}/{second_count}"
    );

    first_server.abort();
    second_server.abort();
}
