use super::*;

async fn spawn_header_capturing_chat_upstream() -> (
    std::net::SocketAddr,
    std::sync::Arc<tokio::sync::Mutex<Vec<HeaderMap>>>,
    tokio::task::JoinHandle<()>,
) {
    let headers = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let headers = headers.clone();
            move |request_headers: HeaderMap, Json(_body): Json<Value>| {
                let headers = headers.clone();
                async move {
                    headers.lock().await.push(request_headers);
                    Json(json!({
                        "id": "chatcmpl-session",
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 3,
                            "completion_tokens": 1,
                            "total_tokens": 4
                        }
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, headers, server)
}

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
    let mut codex_underscored = HeaderMap::new();
    codex_underscored.insert("session_id", HeaderValue::from_static("codex-underscored"));
    assert_eq!(
        upstream_session_id(&codex_underscored, &json!({})).as_deref(),
        Some("codex-underscored")
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
fn opencode_session_falls_back_without_losing_conversation_affinity() {
    assert_eq!(
        opencode_session_id(Some("explicit"), Some("prompt-affinity"), "request-id"),
        "explicit"
    );
    assert_eq!(
        opencode_session_id(Some("  "), Some("prompt-affinity"), "request-id"),
        "prompt-affinity"
    );
    assert_eq!(opencode_session_id(None, None, "request-id"), "request-id");
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

    let mut inference = endpoint_test_target("openai", None);
    inference.base_url = "https://opencode.ai/inference/go/openai/v1".to_string();
    let request = apply_opencode_session_header(
        reqwest::Client::new().post("http://upstream"),
        &inference,
        Some("session-456"),
    )
    .build()
    .unwrap();
    assert_eq!(
        request.headers().get(OPENCODE_SESSION_HEADER).unwrap(),
        "session-456"
    );

    let mut not_go = endpoint_test_target("openai", None);
    not_go.base_url = "https://opencode.ai/zen/v1".to_string();
    let request = apply_opencode_session_header(
        reqwest::Client::new().post("http://upstream"),
        &not_go,
        Some("session-789"),
    )
    .build()
    .unwrap();
    assert!(request.headers().get(OPENCODE_SESSION_HEADER).is_none());
}

#[tokio::test]
async fn opencode_proxy_generates_a_session_from_prompt_affinity() {
    let (address, upstream_headers, server) = spawn_header_capturing_chat_upstream().await;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
             id, name, provider_type, base_url, model_prefix, enabled
         ) VALUES (1, 'Custom relay', 'openai', ?, 'opencode-go/', 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints)
         VALUES (1, 'model', '[\"/chat/completions\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let prefix = "stable prompt prefix ".repeat(8);

    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &Bytes::from(
            json!({
                "model": "opencode-go/model",
                "messages": [{"role": "user", "content": prefix}]
            })
            .to_string(),
        ),
        "session-fallback-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let headers = upstream_headers.lock().await;
    assert_eq!(headers.len(), 1);
    let session = headers[0]
        .get(OPENCODE_SESSION_HEADER)
        .expect("OpenCode Go must receive a session identifier")
        .to_str()
        .unwrap();
    assert!(
        session.starts_with("prompt:sha256:"),
        "the fallback should preserve prompt affinity, got {session}"
    );
    drop(headers);
    server.abort();
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
