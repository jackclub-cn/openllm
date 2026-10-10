use super::*;
use tower::ServiceExt;

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
                expires_at TEXT
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

/// Builds a page of model entries with the given ids, matching the shape
/// `anthropic_models` produces.
fn model_page(ids: &[&str]) -> Vec<Value> {
    ids.iter()
        .map(|id| json!({"type": "model", "id": id, "display_name": id, "created_at": Value::Null}))
        .collect()
}

fn ids_of(models: &[Value]) -> Vec<String> {
    models
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn paginates_forward_with_after_id() {
    let models = model_page(&["a", "b", "c", "d", "e"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: Some("b".to_string()),
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["c", "d"]);
    assert!(has_more, "e remains beyond the window");
}

#[test]
fn paginates_backward_with_before_id() {
    let models = model_page(&["a", "b", "c", "d", "e"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: None,
        before_id: Some("d".to_string()),
    };
    let (window, has_more) = paginate_models(models, &query);
    // The page ends just before the cursor, keeping the last two entries.
    assert_eq!(ids_of(&window), vec!["b", "c"]);
    // `d`/`e` still sit after `last_id`, so the client can page forward.
    assert!(has_more);
}

#[test]
fn reports_has_more_on_a_partial_first_page() {
    let models = model_page(&["a", "b", "c"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: None,
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["a", "b"]);
    assert!(has_more);
}

#[test]
fn returns_last_page_without_claiming_more() {
    let models = model_page(&["a", "b", "c"]);
    let query = ModelPageQuery {
        limit: Some(10),
        after_id: None,
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["a", "b", "c"]);
    assert!(!has_more);
}

#[test]
fn unknown_cursor_yields_an_empty_page() {
    let models = model_page(&["a", "b"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: Some("missing".to_string()),
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert!(window.is_empty());
    assert!(!has_more);
}

#[test]
fn page_size_defaults_and_clamps_to_anthropic_bounds() {
    assert_eq!(
        ModelPageQuery::default().page_size(),
        ANTHROPIC_DEFAULT_PAGE_SIZE
    );
    let tiny = ModelPageQuery {
        limit: Some(0),
        ..Default::default()
    };
    assert_eq!(tiny.page_size(), 1, "zero clamps up");
    let huge = ModelPageQuery {
        limit: Some(99999),
        ..Default::default()
    };
    assert_eq!(huge.page_size(), ANTHROPIC_MAX_PAGE_SIZE, "clamps down");
}

#[test]
fn parses_pagination_params_including_slashes() {
    // Model ids contain '/', which clients percent-encode.
    let uri: Uri = "/v1/models?limit=5&after_id=cmd%2Fdeepseek%2Fv4&ignored=x"
        .parse()
        .unwrap();
    let query = ModelPageQuery::parse(&uri);
    assert_eq!(query.limit, Some(5));
    assert_eq!(query.after_id.as_deref(), Some("cmd/deepseek/v4"));
    assert_eq!(query.before_id, None);

    // A malformed limit is ignored rather than rejecting the request.
    let bad: Uri = "/v1/models?limit=abc".parse().unwrap();
    assert_eq!(ModelPageQuery::parse(&bad).limit, None);
}

#[test]
fn joins_base_and_path_without_double_v1() {
    assert_eq!(
        join_upstream_url("https://api.openai.com/v1", "/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("https://api.openai.com", "/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("http://localhost:11434/v1", "/models"),
        "http://localhost:11434/v1/models"
    );
}

fn endpoint_test_target(provider_type: &str, supported_endpoints: Option<&str>) -> RouteTarget {
    RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "test".to_string(),
        provider_type: provider_type.to_string(),
        base_url: "http://upstream".to_string(),
        model_prefix: String::new(),
        api_key: None,
        provider_headers: "{}".to_string(),
        supported_endpoints: supported_endpoints.map(ToOwned::to_owned),
        cost: None,
        cost_input_override: None,
        cost_output_override: None,
        cost_cache_read_override: None,
        cost_cache_write_override: None,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        provider_health: None,
        upstream_model: "model".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        auth_retryable: false,
    }
}

#[test]
fn filters_targets_by_upstream_supported_endpoints() {
    let openai = endpoint_test_target("openai", Some(r#"["/chat/completions", "/responses"]"#));
    assert!(target_supports_endpoint(&openai, OPENAI_CHAT_COMPLETIONS));
    assert!(target_supports_endpoint(&openai, OPENAI_RESPONSES));
    assert!(!target_supports_endpoint(&openai, "/v1/embeddings"));

    let no_leading_slash = endpoint_test_target("openai", Some(r#"["chat/completions"]"#));
    assert!(target_supports_endpoint(
        &no_leading_slash,
        OPENAI_CHAT_COMPLETIONS
    ));

    let anthropic = endpoint_test_target("anthropic", Some(r#"["/messages"]"#));
    assert!(target_supports_endpoint(
        &anthropic,
        OPENAI_CHAT_COMPLETIONS
    ));
    assert!(target_supports_endpoint(&anthropic, ANTHROPIC_MESSAGES));
    // Anthropic targets serve Responses requests by translating them to
    // the Messages API, so they advertise Responses support too.
    assert!(target_supports_endpoint(&anthropic, OPENAI_RESPONSES));

    let unknown = endpoint_test_target("custom", None);
    assert!(target_supports_endpoint(&unknown, "/v1/embeddings"));

    let filtered =
        filter_targets_for_endpoint(vec![openai.clone(), anthropic.clone()], OPENAI_RESPONSES);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].provider_type, "openai");
    assert_eq!(filtered[1].provider_type, "anthropic");

    // Anthropic targets also serve the legacy completions endpoint.
    assert!(target_supports_endpoint(&anthropic, OPENAI_COMPLETIONS));
    let filtered =
        filter_targets_for_endpoint(vec![openai.clone(), anthropic.clone()], OPENAI_COMPLETIONS);
    // Both qualify: the OpenAI target falls back to chat, Anthropic to
    // Messages.
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].provider_type, "openai");
    assert_eq!(filtered[1].provider_type, "anthropic");
    assert_eq!(
        target_upstream_endpoint(&openai, OPENAI_COMPLETIONS),
        Some(OPENAI_CHAT_COMPLETIONS)
    );

    // A chat-only OpenAI-compatible target still advertises Responses
    // support because the gateway translates the request on the way out.
    let chat_only = endpoint_test_target("openai", Some(r#"["/chat/completions"]"#));
    assert!(target_supports_endpoint(&chat_only, OPENAI_RESPONSES));
    assert_eq!(
        target_upstream_endpoint(&chat_only, OPENAI_RESPONSES),
        Some(OPENAI_CHAT_COMPLETIONS)
    );
    // A target that genuinely supports Responses keeps the passthrough.
    assert_eq!(
        target_upstream_endpoint(&openai, OPENAI_RESPONSES),
        Some(OPENAI_RESPONSES)
    );

    // Conversely, a Responses-only target can serve chat callers.
    let responses_only = endpoint_test_target("openai", Some(r#"["/responses"]"#));
    assert!(target_supports_endpoint(
        &responses_only,
        OPENAI_CHAT_COMPLETIONS
    ));
    assert_eq!(
        target_upstream_endpoint(&responses_only, OPENAI_CHAT_COMPLETIONS),
        Some(OPENAI_RESPONSES)
    );
    // Anthropic callers can also reach a Responses-only target.
    assert!(target_supports_endpoint(
        &responses_only,
        ANTHROPIC_MESSAGES
    ));
    assert_eq!(
        target_upstream_endpoint(&responses_only, ANTHROPIC_MESSAGES),
        Some(OPENAI_RESPONSES)
    );
}

#[test]
fn transient_upstream_4xx_only_matches_opaque_aggregator_errors() {
    // The trace-id-only aggregator error is transient and worth a retry.
    assert!(transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"{\"type\":\"invalid_request_error\",\"code\":\"\",\"message\":\"invalid request error trace_id: 63eeeadddaa2d5224a4acf42a78dd1de\"}\n","type":"invalid_request_error"}}"#
        ));
    // Actionable client errors carry a `param` and must not be retried.
    assert!(!transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"Too small: expected array to have >=1 items","type":"invalid_request_error","param":"input"}}"#
        ));
    // Success and other status classes are never treated as transient.
    assert!(!transient_upstream_4xx(
        StatusCode::OK,
        br#"{"error":{"message":"invalid request error trace_id: abc"}}"#
    ));
    assert!(!transient_upstream_4xx(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"context length exceeded"}}"#
    ));
}

#[test]
fn extracts_trace_id_from_wrapped_upstream_error() {
    let body = br#"{"error":{"message":"{\"type\":\"invalid_request_error\",\"message\":\"invalid request error trace_id: b25620737c4014bef3c7d9d4b56e6854\"}\n","type":"invalid_request_error"}}"#;
    assert_eq!(
        upstream_trace_id(body).as_deref(),
        Some("b25620737c4014bef3c7d9d4b56e6854")
    );
    assert_eq!(
        upstream_trace_id(br#"{"error":{"message":"bad input"}}"#),
        None
    );
}

#[tokio::test]
async fn auto_route_ignores_models_that_do_not_support_the_requested_endpoint() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
                (1, 'embeddings-only', 'openai', 'http://embeddings-only'),
                (2, 'responses', 'openai', 'http://responses')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'shared', '[\"/embeddings\"]'),
                (2, 'shared', '[\"/chat/completions\",\"/responses\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let resolved = resolve_route(&state, "shared", OPENAI_RESPONSES)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);
    assert_eq!(resolved.targets[0].provider_id, 2);

    // The embeddings-only provider is excluded from chat completions even
    // though it advertises a different OpenAI-compatible endpoint.
    let resolved = resolve_route(&state, "shared", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);
    assert_eq!(resolved.targets[0].provider_id, 2);
}

async fn virtual_auto_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix) VALUES
            (1, 'fast', 'openai', 'https://fast.example/v1', 'fast/'),
            (2, 'cheap', 'openai', 'https://cheap.example/v1', 'cheap/'),
            (3, 'responses', 'openai', 'https://responses.example/v1', 'responses/'),
            (4, 'embeddings', 'openai', 'https://embeddings.example/v1', 'embed/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
            provider_id, model_name, supported_endpoints, cost
         ) VALUES
            (1, 'small', '[\"/chat/completions\"]', '{\"input\":1,\"output\":2}'),
            (2, 'tiny', '[\"/chat/completions\"]', '{\"input\":0.1,\"output\":0.2}'),
            (3, 'only', '[\"/responses\"]', '{\"input\":0.5,\"output\":0.5}'),
            (4, 'vector', '[\"/embeddings\"]', '{\"input\":0.01,\"output\":0}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    AppState::new(pool, None)
}

#[tokio::test]
async fn virtual_auto_routes_across_providers_and_respects_api_key_target_patterns() {
    let state = virtual_auto_state().await;

    let resolved = resolve_route_with_patterns(&state, "auto/fast", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    assert_eq!(resolved.strategy, "latency_optimized");
    assert_eq!(resolved.targets.len(), 3);
    assert!(
        resolved
            .targets
            .iter()
            .all(|target| target.upstream_model != "vector"),
        "embeddings-only models must not enter the chat auto pool"
    );

    let patterns = vec!["auto/fast".to_string(), "fast/*".to_string()];
    let restricted = resolve_route_with_patterns(
        &state,
        "auto/fast",
        OPENAI_CHAT_COMPLETIONS,
        Some(&patterns),
    )
    .await
    .unwrap();
    assert_eq!(restricted.targets.len(), 1);
    assert_eq!(restricted.targets[0].provider_id, 1);
    assert_eq!(restricted.targets[0].upstream_model, "small");

    let denied = vec!["auto/fast".to_string()];
    let error =
        resolve_route_with_patterns(&state, "auto/fast", OPENAI_CHAT_COMPLETIONS, Some(&denied))
            .await
            .unwrap_err();
    assert!(matches!(error, AppError::Forbidden(_)));
}

#[tokio::test]
async fn virtual_auto_models_are_public_and_diagnosable() {
    let state = virtual_auto_state().await;
    let models = openai_public_models(&state, None).await.unwrap();
    for (id, display_name, _) in crate::registry::AUTO_MODELS {
        let model = models.iter().find(|model| model.id == id).unwrap();
        assert_eq!(model.display_name.as_deref(), Some(display_name));
        assert_eq!(model.target_count, Some(3));
        assert_eq!(model.limits_verified, Some(false));
    }

    let patterns = vec!["auto/cheap".to_string(), "cheap/*".to_string()];
    let models = openai_public_models(&state, Some(&patterns)).await.unwrap();
    let ids = models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"auto/cheap"));
    assert!(ids.contains(&"cheap/tiny"));
    assert!(!ids.contains(&"auto/fast"));
    assert!(!ids.contains(&"fast/small"));

    let diagnosis = diagnose_route(&state, "auto/fast", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    assert!(diagnosis.matched);
    assert!(diagnosis.resolved);
    assert_eq!(diagnosis.match_type, "auto");
    assert_eq!(diagnosis.route_name.as_deref(), Some("Auto"));
    assert_eq!(diagnosis.strategy.as_deref(), Some("latency_optimized"));
    assert_eq!(
        diagnosis
            .targets
            .iter()
            .filter(|target| target.eligible)
            .count(),
        3
    );
    assert_eq!(diagnosis.targets.len(), 4);
}

#[tokio::test]
async fn endpoint_override_controls_routing_and_public_metadata() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'override', 'openai', 'http://override', 'vendor/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints,
                supported_endpoints_override
             ) VALUES (
                1, 'model', '[\"/chat/completions\"]', '[\"/responses\"]'
             )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    // The override drops native chat support, but chat callers are still
    // served by translating to Responses.
    assert_eq!(
        resolve_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS)
            .await
            .unwrap()
            .targets
            .len(),
        1
    );
    // Endpoints the override does not imply stay unroutable.
    assert!(
        resolve_route(&state, "vendor/model", "/v1/embeddings")
            .await
            .is_err()
    );
    let resolved = resolve_route(&state, "vendor/model", OPENAI_RESPONSES)
        .await
        .unwrap();
    assert_eq!(resolved.targets.len(), 1);

    let models = crate::registry::synced_models(&state.pool).await.unwrap();
    // The override leaves the target Responses-only, so chat, completions
    // and Anthropic callers are all served through translation.
    assert_eq!(
        models[0].supported_endpoints,
        Some(vec![
            "/chat/completions".to_string(),
            "/completions".to_string(),
            "/messages".to_string(),
            "/responses".to_string(),
        ])
    );

    let uri: Uri = "/v1/models".parse().unwrap();
    let response = public_models_inner(&state, &HeaderMap::new(), &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["data"][0]["supported_endpoints"],
        json!([
            "/chat/completions",
            "/completions",
            "/messages",
            "/responses"
        ])
    );
}

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
async fn route_diagnosis_explains_each_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'embeddings-only', 'openai', 'http://embeddings-only', 1, 1),
                (2, 'responses', 'openai', 'http://responses', 1, 1),
                (3, 'disabled', 'openai', 'http://disabled', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints
             ) VALUES
                (1, 'model', '[\"/embeddings\"]'),
                (2, 'model', '[\"/chat/completions\",\"/responses\"]'),
                (3, 'model', '[\"/responses\"]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 0, 1),
                (2, 1, 2, 'model', 1, 1),
                (3, 1, 3, 'model', 2, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let diagnosis = diagnose_route(&state, "shared", OPENAI_RESPONSES, None)
        .await
        .unwrap();
    assert!(diagnosis.matched);
    assert!(diagnosis.resolved);
    assert_eq!(diagnosis.match_type, "explicit_route");
    assert_eq!(diagnosis.targets.len(), 3);
    assert!(!diagnosis.targets[0].eligible);
    assert!(diagnosis.targets[0].reason.contains("does not declare"));
    assert!(diagnosis.targets[1].eligible);
    assert!(!diagnosis.targets[2].eligible);
    assert!(diagnosis.targets[2].reason.contains("provider is disabled"));
    assert!(diagnosis.runtime_targets.is_none());
}

#[tokio::test]
async fn route_diagnosis_returns_sticky_runtime_order_and_key_names() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'primary', 'openai', 'http://primary', 1, 1),
                (2, 'secondary', 'openai', 'http://secondary', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model'), (2, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'primary-a', 'sk-a', 1),
                    (12, 1, 'primary-b', 'sk-b', 1),
                    (21, 2, 'secondary-a', 'sk-c', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'sticky route', 'shared', 'weighted', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, weight, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 1, 0, 1),
                (2, 1, 2, 'model', 100, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let first = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let repeated = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();

    let first_runtime = first.runtime_targets.as_ref().unwrap();
    let repeated_runtime = repeated.runtime_targets.as_ref().unwrap();
    assert_eq!(first.session_id.as_deref(), Some("session-a"));
    assert_eq!(first_runtime.len(), 3);
    assert_eq!(
        first_runtime
            .iter()
            .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
            .collect::<Vec<_>>(),
        repeated_runtime
            .iter()
            .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
            .collect::<Vec<_>>()
    );
    assert!(
        first_runtime
            .iter()
            .any(|target| target.provider_api_key_name.as_deref() == Some("primary-a"))
    );
    assert!(
        first_runtime
            .iter()
            .all(|target| target.order > 0 && target.order <= first_runtime.len())
    );
}

#[tokio::test]
async fn route_diagnosis_explains_advanced_strategy_metrics() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'cheap', 'openai', 'http://cheap', 1, 1),
                (2, 'fast', 'openai', 'http://fast', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, cost)
             VALUES
                (1, 'model', '{\"input\":0.5,\"output\":1}'),
                (2, 'model', '{\"input\":5,\"output\":10}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (request_id, provider_id, latency) in [
        ("cheap-1", 1, 2_400),
        ("cheap-2", 1, 2_500),
        ("cheap-3", 1, 2_600),
        ("cheap-4", 1, 2_500),
        ("fast-1", 2, 500),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', ?, 200, 1, 0)",
        )
        .bind(request_id)
        .bind(provider_id)
        .bind(latency)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO routes (
                id, name, model_pattern, strategy, strategy_ext, enabled
             ) VALUES (1, 'smart route', 'shared', 'priority', 'cost_optimized', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, weight, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 100, 10, 1),
                (2, 1, 2, 'model', 100, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let cost = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let cost_targets = cost.runtime_targets.as_ref().unwrap();
    assert_eq!(cost_targets[0].provider_id, 1);
    assert_eq!(cost_targets[0].decision_reason, "lowest_known_cost");
    assert_eq!(cost_targets[0].input_cost_per_million, Some(0.5));
    assert_eq!(cost_targets[0].output_cost_per_million, Some(1.0));
    assert_eq!(cost_targets[0].recent_requests, Some(4));

    sqlx::query("UPDATE routes SET strategy_ext = 'latency_optimized' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let latency = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
        .await
        .unwrap();
    let latency_targets = latency.runtime_targets.as_ref().unwrap();
    assert_eq!(latency_targets[0].provider_id, 2);
    assert_eq!(latency_targets[0].decision_reason, "lowest_recent_latency");
    assert_eq!(latency_targets[0].avg_latency_ms, Some(500.0));
    assert_eq!(latency_targets[0].recent_requests, Some(1));
}

#[tokio::test]
async fn disabled_route_does_not_hide_direct_prefix_diagnosis() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'prefixed', 'openai', 'http://prefixed', 'vendor/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'disabled exact', 'vendor/model', 'priority', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let diagnosis = diagnose_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS, None)
        .await
        .unwrap();
    assert!(diagnosis.matched);
    assert!(diagnosis.resolved);
    assert_eq!(diagnosis.match_type, "prefix");
    assert_eq!(diagnosis.route_id, None);
}

#[tokio::test]
async fn route_resolution_failures_are_logged() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), None);

    let error = resolve_route_or_log(
        &state,
        None,
        "route-rejection",
        None,
        "missing-model",
        OPENAI_RESPONSES,
        false,
        Instant::now(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(error, AppError::NotFound(_)));

    let row: (String, i64, i64, String) = sqlx::query_as(
        "SELECT request_id, status_code, success, error_message
             FROM usage_logs WHERE request_id = 'route-rejection'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "route-rejection");
    assert_eq!(row.1, 404);
    assert_eq!(row.2, 0);
    assert!(row.3.contains("missing-model"));
}

#[tokio::test]
async fn prefix_matching_is_literal_and_case_sensitive() {
    // A pooled `sqlite::memory:` database gives each connection its own
    // empty database, so pin the pool to a single connection.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'underscore', 'openai', 'http://upstream', 'a_b/')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO provider_models (provider_id, model_name) VALUES (1, 'x')")
        .execute(&pool)
        .await
        .unwrap();

    let state = AppState::new(pool, None);

    // `_` must be treated literally, not as a single-character wildcard.
    let literal = find_prefixed_targets(&state, "a_b/x", OPENAI_CHAT_COMPLETIONS)
        .await
        .unwrap();
    assert_eq!(literal.len(), 1);
    assert_eq!(literal[0].upstream_model, "x");
    assert!(
        find_prefixed_targets(&state, "aXb/x", OPENAI_CHAT_COMPLETIONS)
            .await
            .is_err()
    );

    // Prefix comparison must stay case-sensitive so `A_B/x` cannot reach a
    // provider registered as `a_b`.
    assert!(
        find_prefixed_targets(&state, "A_B/x", OPENAI_CHAT_COMPLETIONS)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disabled_provider_model_is_skipped_by_explicit_routes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE route_targets (
                id INTEGER PRIMARY KEY,
                route_id INTEGER,
                provider_id INTEGER NOT NULL,
                upstream_model TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 100,
                priority INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE routes (
                id INTEGER PRIMARY KEY,
                model_pattern TEXT NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT ''
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'upstream', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'disabled-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (id, route_id, provider_id, upstream_model)
             VALUES (1, 1, 1, 'disabled-model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, model_pattern, name)
             VALUES (1, 'route-model', 'route-model')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    assert!(load_targets(&state, 1).await.unwrap().is_empty());
    assert!(
        crate::registry::route_models(&state.pool)
            .await
            .unwrap()
            .is_empty()
    );

    sqlx::query("UPDATE provider_models SET enabled = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(load_targets(&state, 1).await.unwrap().len(), 1);
    let routes = crate::registry::route_models(&state.pool).await.unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].target_count, 1);
}

#[tokio::test]
async fn api_key_daily_token_quota_blocks_after_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (api_key_id, total_tokens, created_at)
             VALUES (1, 10, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(10),
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool, None);
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
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
async fn api_key_daily_cost_quota_uses_estimated_cost() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (api_key_id, total_tokens, estimated_cost_micros, created_at)
             VALUES (1, 1, 2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: Some(2),
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool, None);
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
}

#[tokio::test]
async fn api_key_requests_per_minute_quota_blocks_after_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'rate-limited', 'rate-hash', 'sk-openllm', 'test', 1, 2, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "rate-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: Some(2),
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool.clone(), None);
    for request_id in ["first", "second"] {
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            request_id,
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "third",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));
    let in_flight: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(in_flight, 2);
}

#[tokio::test]
async fn api_key_concurrency_quota_blocks_at_limit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'concurrency-hash', 'sk-openllm', 'test', 1, NULL, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "concurrency-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: Some(1),
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool.clone(), None);
    reserve_api_key_rate_limit(
        &state,
        Some(&key),
        "active",
        None,
        "model",
        "/v1/chat/completions",
        10,
        false,
    )
    .await
    .unwrap();
    assert!(matches!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "blocked",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));

    sqlx::query("UPDATE usage_logs SET in_flight = 0 WHERE request_id = 'active'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "next",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn api_key_without_limits_skips_quota_queries() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "unlimited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool, None);
    assert!(
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "unlimited",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn api_key_rate_limit_rejection_is_logged_without_extra_in_flight() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'reject-hash', 'sk-openllm', 'test', 1, NULL, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "concurrency-limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: Some(1),
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool.clone(), None);
    reserve_api_key_rate_limit(
        &state,
        Some(&key),
        "active",
        None,
        "model",
        "/v1/chat/completions",
        10,
        false,
    )
    .await
    .unwrap();

    assert!(matches!(
        enforce_api_key_rate_limit_or_log(
            &state,
            Some(&key),
            "blocked",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
            Instant::now(),
        )
        .await,
        Err(AppError::TooManyRequests(_))
    ));
    let blocked: (i64, i64) = sqlx::query_as(
        "SELECT status_code, in_flight FROM usage_logs WHERE request_id = 'blocked'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(blocked, (429, 0));
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(active, 1);
}

#[tokio::test]
async fn api_key_daily_quota_ignores_in_flight_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                api_key_id, total_tokens, estimated_cost_micros, in_flight, created_at
             ) VALUES (1, 1000, 1000000, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(10),
        daily_cost_limit_micros: Some(1),
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool.clone(), None);

    assert!(
        enforce_api_key_daily_quota(&state, Some(&key))
            .await
            .is_ok()
    );

    sqlx::query("UPDATE usage_logs SET in_flight = 0")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        enforce_api_key_daily_quota(&state, Some(&key)).await,
        Err(AppError::TooManyRequests(_))
    ));
}

#[tokio::test]
async fn quota_rejection_is_logged_as_zero_usage() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TABLE usage_logs (
                request_id TEXT NOT NULL UNIQUE,
                api_key_id INTEGER,
                route_id INTEGER,
                provider_id INTEGER,
                requested_model TEXT NOT NULL,
                upstream_model TEXT,
                endpoint TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                first_token_ms INTEGER,
                status_code INTEGER NOT NULL,
                in_flight INTEGER NOT NULL DEFAULT 0,
                success INTEGER NOT NULL,
                streamed INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                response_preview TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )
            "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                total_tokens, status_code, success, created_at
             ) VALUES (
                'seed', 1, 'seed', '/v1/chat/completions', 1, 200, 1,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = ApiKeyRecord {
        id: 1,
        name: "limited".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: Some(1),
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: None,
        expires_at: None,
    };
    let state = AppState::new(pool.clone(), None);
    assert!(
        enforce_policy_or_log(
            &state,
            Some(&key),
            "rejected",
            None,
            "model",
            "/v1/chat/completions",
            false,
            Instant::now(),
        )
        .await
        .is_err()
    );
    let row: (i64, i64, Option<i64>, i64) = sqlx::query_as(
        "SELECT total_tokens, status_code, estimated_cost_micros, success \
             FROM usage_logs WHERE request_id = 'rejected'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (0, 429, Some(0), 0));
}

#[test]
fn api_key_model_permissions_support_globs() {
    let key = ApiKeyRecord {
        id: 1,
        name: "scoped".to_string(),
        key_prefix: "sk-openllm".to_string(),
        key_suffix: "test".to_string(),
        enabled: 1,
        last_used_at: None,
        created_at: String::new(),
        daily_token_limit: None,
        daily_cost_limit_micros: None,
        requests_per_minute: None,
        max_concurrency: None,
        allowed_models: Some(r#"["gpt-*","claude-sonnet-*"]"#.to_string()),
        expires_at: None,
    };
    assert!(enforce_api_key_model_access(Some(&key), "gpt-5.4").is_ok());
    assert!(enforce_api_key_model_access(Some(&key), "claude-sonnet-5").is_ok());
    assert!(matches!(
        enforce_api_key_model_access(Some(&key), "claude-opus-5"),
        Err(AppError::Forbidden(_))
    ));
    let malformed = ApiKeyRecord {
        allowed_models: Some("{".to_string()),
        ..key
    };
    assert!(matches!(
        enforce_api_key_model_access(Some(&malformed), "gpt-5.4"),
        Err(AppError::Forbidden(_))
    ));
}

#[tokio::test]
async fn public_model_list_respects_api_key_model_permissions() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Scoped', 'openai', 'https://example.com/v1', 'vendor/', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let raw_key = "sk-openllm-scoped-model-list";
    sqlx::query(
        "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-s', 'list', 1, '[\"vendor/gpt-*\"]')",
    )
    .bind(hash_secret(raw_key))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let mut openai_headers = HeaderMap::new();
    openai_headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
    );
    let uri: Uri = "/v1/models".parse().unwrap();
    let response = public_models_inner(&state, &openai_headers, &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    let ids = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["vendor/gpt-5"]);

    let response = public_model_inner(&state, &openai_headers, "vendor/gpt-5")
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
    assert_eq!(value["object"], "model");
    assert!(matches!(
        public_model_inner(&state, &openai_headers, "vendor/claude-4").await,
        Err(AppError::NotFound(_))
    ));

    let mut anthropic_headers = openai_headers.clone();
    anthropic_headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );
    let response = public_models_inner(&state, &anthropic_headers, &uri)
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    let ids = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["vendor/gpt-5"]);

    let response = public_model_inner(&state, &anthropic_headers, "vendor/gpt-5")
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
    assert_eq!(value["type"], "model");
}

#[tokio::test]
async fn model_retrieve_route_accepts_slashed_model_ids() {
    use axum::body::Body;
    use tower::ServiceExt;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Vendor', 'openai', 'https://example.com/v1', 'vendor/', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let response = crate::build_router(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/models/vendor/gpt-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "vendor/gpt-5");
}

#[test]
fn retries_only_transient_statuses() {
    for status in [408, 409, 425, 429, 500, 502, 503] {
        assert!(
            retryable_status(StatusCode::from_u16(status).unwrap()),
            "{status} should be retryable"
        );
    }
    for status in [400, 401, 403, 404, 422] {
        assert!(
            !retryable_status(StatusCode::from_u16(status).unwrap()),
            "{status} should not be retryable"
        );
    }
}

#[test]
fn health_ranking_prefers_known_good_then_unknown_then_failed() {
    assert_eq!(provider_health_rank(Some(1)), 0);
    assert_eq!(provider_health_rank(None), 1);
    assert_eq!(provider_health_rank(Some(0)), 2);
    assert_eq!(provider_health_rank(Some(7)), 0);
}

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

#[tokio::test]
async fn cost_optimized_orders_known_prices_before_unknown_targets() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let mut expensive = endpoint_test_target("openai", None);
    expensive.id = 1;
    expensive.provider_id = 1;
    expensive.cost = Some(json!({"input": 10.0, "output": 30.0}).to_string());
    let mut cheap = endpoint_test_target("openai", None);
    cheap.id = 2;
    cheap.provider_id = 2;
    cheap.cost = Some(json!({"input": 1.0, "output": 2.0}).to_string());
    let mut unknown = endpoint_test_target("openai", None);
    unknown.id = 3;
    unknown.provider_id = 3;

    let ordered = order_targets(
        &state,
        1,
        "cost_optimized",
        vec![expensive, unknown, cheap],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        vec![2, 1, 3]
    );
}

#[tokio::test]
async fn latency_optimized_prefers_recent_successful_samples() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    for (id, name) in [(1, "Fast"), (2, "Slow"), (3, "New")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (request_id, provider_id, latency) in [
        ("fast-1", 1, 400),
        ("fast-2", 1, 600),
        ("slow-1", 2, 1_200),
        ("slow-2", 2, 1_400),
    ] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', ?, 200, 1, 0)",
        )
        .bind(request_id)
        .bind(provider_id)
        .bind(latency)
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = AppState::new(pool, None);
    let targets = (1..=3)
        .map(|id| {
            let mut target = endpoint_test_target("openai", None);
            target.id = id;
            target.provider_id = id;
            target.upstream_model = "model".to_string();
            target
        })
        .collect::<Vec<_>>();

    let ordered = order_targets(&state, 1, "latency_optimized", targets, None)
        .await
        .unwrap();
    assert_eq!(
        ordered
            .into_iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn least_used_prefers_the_target_with_less_recent_traffic() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    for (id, name) in [(1, "Busy"), (2, "Quiet")] {
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (?, ?, 'openai', 'https://example.com/v1')",
        )
        .bind(id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (request_id, provider_id) in [("busy-1", 1), ("busy-2", 1), ("busy-3", 1), ("quiet-1", 2)] {
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, requested_model, upstream_model, endpoint,
                latency_ms, status_code, success, streamed
             ) VALUES (?, ?, 'model', 'model', '/v1/chat/completions', 500, 200, 1, 0)",
        )
        .bind(request_id)
        .bind(provider_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = AppState::new(pool, None);
    let mut busy = endpoint_test_target("openai", None);
    busy.id = 1;
    busy.provider_id = 1;
    let mut quiet = endpoint_test_target("openai", None);
    quiet.id = 2;
    quiet.provider_id = 2;

    let ordered = order_targets(&state, 1, "least_used", vec![busy, quiet], None)
        .await
        .unwrap();
    assert_eq!(ordered[0].provider_id, 2);
}

#[tokio::test]
async fn provider_keys_are_sticky_per_session_without_advancing_rotation() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Sticky', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1),
                    (13, 1, 'Third', 'sk-third', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;
    let keys = |targets: Vec<RouteTarget>| {
        targets
            .into_iter()
            .map(|target| target.api_key.unwrap())
            .collect::<Vec<_>>()
    };

    let first = keys(
        order_targets(
            &state,
            1,
            "priority",
            vec![target.clone()],
            Some("session-a"),
        )
        .await
        .unwrap(),
    );
    let repeated = keys(
        order_targets(
            &state,
            1,
            "priority",
            vec![target.clone()],
            Some("session-a"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(first, repeated);

    let no_session_first = keys(
        order_targets(&state, 1, "priority", vec![target.clone()], None)
            .await
            .unwrap(),
    );
    let no_session_second = keys(
        order_targets(&state, 1, "priority", vec![target], None)
            .await
            .unwrap(),
    );
    assert_eq!(no_session_first[0], "sk-first");
    assert_eq!(no_session_second[0], "sk-second");
}

#[tokio::test]
async fn provider_keys_rotate_and_expand_a_route_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Rotating', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
             VALUES (1, 'First', 'sk-first', 1), (1, 'Second', 'sk-second', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;

    let first = order_targets(&state, 1, "priority", vec![target.clone()], None)
        .await
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-first"), Some("sk-second")]
    );
    assert!(first[0].auth_retryable);
    assert!(!first[1].auth_retryable);
    assert!(
        first
            .iter()
            .all(|target| target.provider_api_key_id.is_some())
    );

    let second = order_targets(&state, 1, "priority", vec![target], None)
        .await
        .unwrap();
    assert_eq!(
        second
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-second"), Some("sk-first")]
    );
}

#[tokio::test]
async fn provider_keys_skip_cooling_candidates_when_alternatives_exist() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Cooling', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.id = 1;

    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, Instant::now() + Duration::from_secs(60));
    let available = order_targets(&state, 1, "priority", vec![target.clone()], None)
        .await
        .unwrap();
    assert_eq!(
        available
            .iter()
            .map(|target| target.api_key.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sk-second")]
    );

    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(12, Instant::now() + Duration::from_secs(60));
    let fallback = order_targets(&state, 1, "priority", vec![target], None)
        .await
        .unwrap();
    assert_eq!(fallback.len(), 2);
    assert!(
        fallback
            .iter()
            .all(|target| target.provider_api_key_id.is_some())
    );
}

#[tokio::test]
async fn provider_cooldowns_skip_cooling_providers_when_alternatives_exist() {
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
                (1, 'Cooling', 'openai', 'https://cooling.example/v1', '', 1),
                (2, 'Healthy', 'openai', 'https://healthy.example/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut cooling = endpoint_test_target("openai", None);
    cooling.id = 1;
    cooling.provider_id = 1;
    let mut healthy = endpoint_test_target("openai", None);
    healthy.id = 2;
    healthy.provider_id = 2;

    state
        .provider_cooldown
        .lock()
        .await
        .insert(1, Instant::now() + Duration::from_secs(60));
    let available = order_targets(
        &state,
        1,
        "priority",
        vec![cooling.clone(), healthy.clone()],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        available
            .iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![2]
    );

    state
        .provider_cooldown
        .lock()
        .await
        .insert(2, Instant::now() + Duration::from_secs(60));
    let fallback = order_targets(&state, 1, "priority", vec![cooling, healthy], None)
        .await
        .unwrap();
    assert_eq!(
        fallback
            .iter()
            .map(|target| target.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn parses_retry_after_seconds_milliseconds_and_http_dates() {
    let mut seconds = HeaderMap::new();
    seconds.insert("retry-after", HeaderValue::from_static("42"));
    assert_eq!(
        retry_after_from_headers(&seconds),
        Some(Duration::from_secs(42))
    );

    let mut milliseconds = HeaderMap::new();
    milliseconds.insert("retry-after-ms", HeaderValue::from_static("1250"));
    assert_eq!(
        retry_after_from_headers(&milliseconds),
        Some(Duration::from_millis(1250))
    );

    let mut date = HeaderMap::new();
    let deadline = std::time::SystemTime::now() + Duration::from_secs(120);
    date.insert(
        "retry-after",
        HeaderValue::from_str(&httpdate::fmt_http_date(deadline)).unwrap(),
    );
    let parsed = retry_after_from_headers(&date).unwrap();
    assert!(parsed >= Duration::from_secs(118));
    assert!(parsed <= Duration::from_secs(120));

    let mut capped = HeaderMap::new();
    capped.insert("retry-after", HeaderValue::from_static("99999"));
    assert_eq!(
        retry_after_from_headers(&capped),
        Some(MAX_UPSTREAM_RETRY_AFTER)
    );
}

#[test]
fn provider_cooldown_escalates_with_consecutive_failures_and_caps() {
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 1, None),
        Duration::from_secs(30)
    );
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 2, None),
        Duration::from_secs(60)
    );
    assert_eq!(
        provider_cooldown(Some(StatusCode::TOO_MANY_REQUESTS), 9, None),
        MAX_PROVIDER_COOLDOWN
    );
    assert_eq!(
        provider_cooldown(
            Some(StatusCode::TOO_MANY_REQUESTS),
            2,
            Some(Duration::from_secs(5))
        ),
        Duration::from_secs(10)
    );
}

#[tokio::test]
async fn rate_limits_isolate_models_before_opening_the_provider_circuit() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut first = endpoint_test_target("openai", None);
    first.provider_id = 7;
    first.upstream_model = "model-a".to_string();
    let mut second = first.clone();
    second.upstream_model = "model-b".to_string();

    let headers = HeaderMap::new();
    record_upstream_failure(
        &state,
        &first,
        StatusCode::TOO_MANY_REQUESTS,
        &headers,
        "rate limited",
    )
    .await;
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(7, "model-a".to_string()))
    );
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&7),
        "one model-specific 429 must not remove healthy sibling models"
    );

    record_upstream_failure(
        &state,
        &second,
        StatusCode::TOO_MANY_REQUESTS,
        &headers,
        "rate limited",
    )
    .await;
    assert!(
        state.provider_cooldown.lock().await.contains_key(&7),
        "multiple cooling models should open the provider circuit"
    );
}

#[tokio::test]
async fn model_cooldowns_rank_healthy_sibling_models_first() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let mut cooling = endpoint_test_target("openai", None);
    cooling.id = 1;
    cooling.provider_id = 7;
    cooling.upstream_model = "model-a".to_string();
    let mut healthy = cooling.clone();
    healthy.id = 2;
    healthy.upstream_model = "model-b".to_string();
    state.target_cooldown.lock().await.insert(
        (7, "model-a".to_string()),
        Instant::now() + Duration::from_secs(60),
    );

    let ordered = order_targets(&state, 1, "priority", vec![cooling, healthy], None)
        .await
        .unwrap();

    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].upstream_model, "model-b");
}

#[test]
fn routing_headers_report_attempts_and_the_selected_target() {
    let mut response = StatusCode::OK.into_response();
    apply_routing_headers(&mut response, 3, 2, "Provider X", "model-y");
    // No ceiling was requested, so the budget headers stay absent rather than
    // advertising a limit that was never applied.
    apply_budget_headers(&mut response, None, 0);

    assert_eq!(response.headers()["x-openllm-routing-attempts"], "3");
    assert_eq!(response.headers()["x-openllm-fallback-count"], "2");
    assert_eq!(
        response.headers()["x-openllm-routed-provider"],
        "Provider X"
    );
    assert_eq!(response.headers()["x-openllm-routed-model"], "model-y");
    assert!(response.headers().get(BUDGET_USD_HEADER).is_none());
    assert!(
        response
            .headers()
            .get("x-openllm-budget-excluded-targets")
            .is_none()
    );

    let mut budgeted = StatusCode::OK.into_response();
    apply_budget_headers(&mut budgeted, Some(0.25), 2);
    assert_eq!(budgeted.headers()[BUDGET_USD_HEADER], "0.250000");
    assert_eq!(budgeted.headers()["x-openllm-budget-excluded-targets"], "2");
}

#[tokio::test]
async fn provider_cooldown_is_set_for_retryable_failures_and_cleared_on_success() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);

    mark_provider_error(&state, 7, Some(StatusCode::TOO_MANY_REQUESTS), None).await;
    let until = state
        .provider_cooldown
        .lock()
        .await
        .get(&7)
        .copied()
        .unwrap();
    assert!(until > Instant::now() + Duration::from_secs(25));

    mark_provider_success(&state, 7).await;
    assert!(!state.provider_cooldown.lock().await.contains_key(&7));
}

#[tokio::test]
async fn provider_key_success_clears_recovered_error_state() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Recovered', 'openai', 'https://example.com/v1', '', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (
                id, provider_id, name, secret, enabled, last_error_at, last_error
             ) VALUES (
                11, 1, 'Primary', 'sk-primary', 1,
                '2026-01-01T00:00:00Z', 'stale unauthorized'
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    state.load_provider_key_error_state().await.unwrap();
    state
        .provider_key_cooldown
        .lock()
        .await
        .insert(11, Instant::now() + Duration::from_secs(60));

    mark_provider_api_key_success(&state, Some(11)).await;

    assert!(!state.provider_key_cooldown.lock().await.contains_key(&11));
    assert!(!state.provider_key_error_state.lock().await.contains(&11));
    let error: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT last_error_at, last_error FROM provider_api_keys WHERE id = 11")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(error, (None, None));
}

#[test]
fn auth_failures_only_fall_through_when_another_candidate_exists() {
    let mut target = endpoint_test_target("openai", None);
    assert!(!should_try_next_target(&target, StatusCode::UNAUTHORIZED));
    target.auth_retryable = true;
    assert!(should_try_next_target(&target, StatusCode::UNAUTHORIZED));
    assert!(should_try_next_target(&target, StatusCode::FORBIDDEN));
    assert!(!should_try_next_target(&target, StatusCode::BAD_REQUEST));
}

#[test]
fn rejects_estimated_input_above_route_context_limit() {
    let barrel = BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            context_limit: Some(100),
            ..Default::default()
        }),
        incomplete: false,
        target_count: 1,
    };
    assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
    assert!(matches!(
        enforce_context_capacity(101, Some(&barrel)),
        Err(AppError::BadRequest(_))
    ));
    assert!(enforce_context_capacity(101, None).is_ok());
}

#[test]
fn rejects_estimated_input_above_model_input_limit_without_context_limit() {
    let barrel = BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            input_limit: Some(100),
            ..Default::default()
        }),
        incomplete: false,
        target_count: 1,
    };
    assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
    assert!(matches!(
        enforce_context_capacity(101, Some(&barrel)),
        Err(AppError::BadRequest(_))
    ));
}

#[test]
fn effective_input_limit_prefers_the_stricter_value() {
    let capabilities = ModelCapabilities {
        context_limit: Some(100),
        input_limit: Some(200),
        ..Default::default()
    };
    assert_eq!(effective_input_limit(&capabilities), Some(100));
}

#[test]
fn capability_headers_include_input_and_context_without_output_limit() {
    let mut response = Response::new(Body::empty());
    let receipt = Some(json!({
        "context_limit": 100,
        "input_limit": 80
    }));
    apply_capability_headers(&mut response, &receipt);
    assert_eq!(
        response
            .headers()
            .get("x-openllm-max-context-tokens")
            .and_then(|value| value.to_str().ok()),
        Some("100")
    );
    assert_eq!(
        response
            .headers()
            .get("x-openllm-max-input-tokens")
            .and_then(|value| value.to_str().ok()),
        Some("80")
    );
    assert!(
        response
            .headers()
            .get("x-openllm-max-output-tokens")
            .is_none()
    );
}

fn barrel_with_output_limit(output_limit: Option<i64>) -> BarrelEnvelope {
    BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            output_limit,
            ..Default::default()
        }),
        incomplete: false,
        target_count: 2,
    }
}

#[test]
fn clamps_requested_output_to_barrel_limit() {
    let barrel = barrel_with_output_limit(Some(8000));
    let mut body = json!({"model": "m", "max_tokens": 32000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(8000));
    assert_eq!(body["max_tokens"], 8000);
}

#[test]
fn leaves_requests_within_the_barrel_untouched() {
    let barrel = barrel_with_output_limit(Some(8000));
    let mut body = json!({"model": "m", "max_tokens": 4096});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), None);
    assert_eq!(body["max_tokens"], 4096);

    // No known limit means there is nothing to clamp against.
    let unknown = barrel_with_output_limit(None);
    let mut body = json!({"model": "m", "max_tokens": 999999});
    assert_eq!(clamp_output_request(&mut body, Some(&unknown)), None);
    assert_eq!(body["max_tokens"], 999999);
}

#[test]
fn clamps_max_completion_tokens_too() {
    let barrel = barrel_with_output_limit(Some(1000));
    let mut body = json!({"model": "m", "max_completion_tokens": 5000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
    assert_eq!(body["max_completion_tokens"], 1000);
}

#[test]
fn clamps_responses_max_output_tokens_too() {
    let barrel = barrel_with_output_limit(Some(1000));
    let mut body = json!({"model": "m", "max_output_tokens": 5000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
    assert_eq!(body["max_output_tokens"], 1000);
    assert_eq!(
        requested_output_tokens_of(&json!({"max_output_tokens": 5000})),
        Some(5000)
    );
}

#[test]
fn receipt_reports_clamping_and_target_count() {
    let barrel = barrel_with_output_limit(Some(8000));
    let receipt = capability_receipt(Some(&barrel), Some(32000), Some(8000)).unwrap();
    assert_eq!(receipt["output_limit"], 8000);
    assert_eq!(receipt["requested_output_tokens"], 32000);
    assert_eq!(receipt["clamped_output_tokens"], 8000);
    assert_eq!(receipt["target_count"], 2);
    assert_eq!(receipt["limits_verified"], true);
}

#[test]
fn receipt_injected_only_into_json_objects() {
    let receipt = Some(json!({"output_limit": 8000}));
    let injected = inject_capability_receipt(br#"{"id":"x"}"#, &receipt).unwrap();
    let value: Value = serde_json::from_slice(&injected).unwrap();
    assert_eq!(value["capabilities"]["output_limit"], 8000);
    // Non-JSON bodies must be left alone rather than replaced.
    assert!(inject_capability_receipt(b"not json", &receipt).is_none());
    assert!(inject_capability_receipt(b"{}", &None).is_none());
}

#[test]
fn strips_tool_search_for_compat_retry() {
    let body = json!({
        "model": "m",
        "tools": [
            {"type": "function", "name": "lookup"},
            {"type": "tool_search", "execution": "client"}
        ]
    });
    let stripped = strip_tool_search_tools(&body).unwrap();
    assert_eq!(stripped["tools"].as_array().unwrap().len(), 1);
    assert_eq!(stripped["tools"][0]["name"], "lookup");

    let only_tool_search = json!({
        "tools": [{"type": "tool_search", "execution": "client"}]
    });
    let stripped = strip_tool_search_tools(&only_tool_search).unwrap();
    assert!(stripped.get("tools").is_none());

    let standard_tools = json!({
        "tools": [{"type": "function", "name": "lookup"}]
    });
    assert!(strip_tool_search_tools(&standard_tools).is_none());
}

#[test]
fn repairs_orphaned_responses_tool_items() {
    let mut body = json!({
        "input": [
            {"type": "function_call", "call_id": "call_paired", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_paired", "output": "ok"},
            {"type": "function_call", "call_id": "call_missing", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_orphan", "output": "stale"},
            {"role": "user", "content": "continue"}
        ]
    });

    assert_eq!(sanitize_responses_tool_history(&mut body), 2);
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["call_id"], "call_paired");
    assert_eq!(input[1]["call_id"], "call_paired");
    assert_eq!(input[2]["role"], "user");
}

#[test]
fn repairs_orphaned_chat_tool_messages() {
    let mut body = json!({
        "messages": [
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_paired",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"}
                    },
                    {
                        "id": "call_missing",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{}"}
                    }
                ]
            },
            {"role": "tool", "tool_call_id": "call_paired", "content": "ok"},
            {"role": "tool", "tool_call_id": "call_orphan", "content": "stale"},
            {"role": "user", "content": "continue"}
        ]
    });

    assert_eq!(sanitize_chat_tool_history(&mut body), 2);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["tool_calls"][0]["id"], "call_paired");
    assert_eq!(messages[1]["tool_call_id"], "call_paired");
    assert_eq!(messages[2]["role"], "user");
}

#[test]
fn normalizes_command_code_output_caps_and_reasoning_effort() {
    let mut responses = json!({
        "input": "hi",
        "max_output_tokens": 500_000,
        "reasoning": {"effort": "none"}
    });
    assert_eq!(normalize_command_code_request(&mut responses), 1);
    assert_eq!(responses["max_output_tokens"], 200_000);
    assert_eq!(responses["reasoning"]["effort"], "none");

    let mut chat = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": -1,
        "reasoning_effort": "minimal"
    });
    assert_eq!(normalize_command_code_request(&mut chat), 2);
    assert!(chat.get("max_tokens").is_none());
    assert_eq!(chat["reasoning_effort"], "low");
}

#[test]
fn identifies_command_code_targets_by_base_url_or_prefix() {
    let mut target = endpoint_test_target("openai", None);
    target.base_url = "https://api.commandcode.ai/provider/v1".to_string();
    assert!(is_command_code_target(&target));

    target.base_url = "https://example.com/v1".to_string();
    target.model_prefix = "commandcode/".to_string();
    assert!(is_command_code_target(&target));

    target.model_prefix = "gateway/".to_string();
    assert!(!is_command_code_target(&target));
}

#[test]
fn recognizes_unsupported_tool_search_error() {
    for error in [
        r#"{
                "type": "BadRequest",
                "code": "InvalidParameter",
                "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
            }"#,
        r#"{"error":{"message":"unsupported tool type: tool_search"}}"#,
        r#"{"error":{"message":"tool_search is not supported by this model"}}"#,
        r#"{"error":{"message":"unknown tool: tool_search"}}"#,
    ] {
        assert!(upstream_rejects_tool_search(error.as_bytes()));
    }
    for error in [
        r#"{"error":{"message":"rate limit exceeded"}}"#,
        r#"{"error":{"message":"tool_search returned an invalid result"}}"#,
    ] {
        assert!(!upstream_rejects_tool_search(error.as_bytes()));
    }
}

#[tokio::test]
async fn retries_openai_requests_without_tool_search_when_upstream_rejects_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (endpoint, request_json, success_body) in [
        (
            OPENAI_RESPONSES,
            json!({
                "model": "requested-model",
                "input": "hello",
                "tools": [{"type": "tool_search", "execution": "client"}]
            }),
            json!({
                "id": "ok",
                "object": "response",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "total_tokens": 12
                }
            }),
        ),
        (
            OPENAI_CHAT_COMPLETIONS,
            json!({
                "model": "requested-model",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{"type": "tool_search", "execution": "client"}]
            }),
            json!({
                "id": "ok",
                "object": "chat.completion",
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2,
                    "total_tokens": 12
                }
            }),
        ),
    ] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let success_body = success_body.clone();
        let app = axum::Router::new().route(
                endpoint,
                axum::routing::post({
                    let attempts = attempts.clone();
                    move |Json(body): Json<Value>| {
                        let attempts = attempts.clone();
                        let success_body = success_body.clone();
                        async move {
                            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                                assert!(
                                    body["tools"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|tool| tool["type"] == "tool_search")
                                );
                                (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({
                                        "type": "BadRequest",
                                        "code": "InvalidParameter",
                                        "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
                                    })),
                                )
                            } else {
                                assert!(body.get("tools").is_none());
                                (StatusCode::OK, Json(success_body))
                            }
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (1, 'mock', 'openai', ?)",
        )
        .bind(format!("http://{address}"))
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool.clone(), None);
        let mut target = RouteTarget {
            id: 1,
            route_id: None,
            provider_id: 1,
            provider_name: "mock".to_string(),
            provider_type: "openai".to_string(),
            base_url: format!("http://{address}"),
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
            provider_enabled: None,
            model_enabled: None,
            tool_search_supported: 1,
            provider_health: None,
            upstream_model: "upstream".to_string(),
            weight: 100,
            priority: 0,
            enabled: 1,
            provider_api_key_id: None,
            provider_api_key_name: None,
            auth_retryable: false,
        };

        let response = forward_to_target(
            &state,
            "tool-search-retry",
            endpoint,
            "requested-model",
            &request_json,
            &Bytes::new(),
            None,
            target.clone(),
            false,
            10,
            None,
            Instant::now(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let tool_search_supported: i64 =
            sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tool_search_supported, 0);
        let tool_search_checked_at: Option<String> =
            sqlx::query_scalar("SELECT tool_search_checked_at FROM providers WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(tool_search_checked_at.is_some());

        target.tool_search_supported = 0;
        let response = forward_to_target(
            &state,
            "tool-search-cached",
            endpoint,
            "requested-model",
            &request_json,
            &Bytes::new(),
            None,
            target,
            false,
            10,
            None,
            Instant::now(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        server.abort();
    }
}

#[tokio::test]
async fn forward_repairs_tool_history_and_normalizes_command_code_request() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured = Arc::new(Mutex::new(None));
    let app = axum::Router::new().route(
        OPENAI_RESPONSES,
        axum::routing::post({
            let captured = captured.clone();
            move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "resp_ok",
                            "object": "response",
                            "output": [],
                            "usage": {
                                "input_tokens": 1,
                                "output_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'command-code', 'openai', ?, 'commandcode/')",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let request_json = json!({
        "model": "commandcode/deepseek/deepseek-v4.1-flash",
        "input": [
            {"type": "function_call", "call_id": "call_pair", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_pair", "output": "ok"},
            {"type": "function_call", "call_id": "call_missing", "name": "lookup"},
            {"type": "function_call_output", "call_id": "call_orphan", "output": "stale"},
            {"role": "user", "content": "continue"}
        ],
        "max_output_tokens": 500_000,
        "reasoning": {"effort": "none"}
    });
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "command-code".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
        model_prefix: "commandcode/".to_string(),
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
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        provider_health: None,
        upstream_model: "deepseek/deepseek-v4.1-flash".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        auth_retryable: false,
    };

    let response = forward_to_target(
        &state,
        "tool-history-repair",
        OPENAI_RESPONSES,
        "commandcode/deepseek/deepseek-v4.1-flash",
        &request_json,
        &Bytes::new(),
        None,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await.clone().unwrap();
    assert_eq!(captured["model"], "deepseek/deepseek-v4.1-flash");
    assert_eq!(captured["max_output_tokens"], 200_000);
    assert_eq!(captured["reasoning"]["effort"], "none");
    let input = captured["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["call_id"], "call_pair");
    assert_eq!(input[1]["call_id"], "call_pair");
    assert_eq!(input[2]["role"], "user");

    server.abort();
}

#[test]
fn classifies_upstream_shape_from_body_and_provider() {
    assert_eq!(
        classify_upstream_shape(ProviderType::Openai, &json!({"input": "hi"})),
        UpstreamShape::Responses
    );
    assert_eq!(
        classify_upstream_shape(
            ProviderType::Openai,
            &json!({"messages": [{"role": "user", "content": "hi"}]})
        ),
        UpstreamShape::Chat
    );
    // An Anthropic provider keeps the Anthropic repair rules even though
    // the body also carries a `messages` array.
    assert_eq!(
        classify_upstream_shape(
            ProviderType::Anthropic,
            &json!({"messages": [{"role": "user", "content": []}]})
        ),
        UpstreamShape::Anthropic
    );
}

#[tokio::test]
async fn anthropic_forward_repairs_orphan_tool_calls_for_openai_upstream() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured = Arc::new(Mutex::new(None));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let captured = captured.clone();
            move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-1",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "done"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 3,
                                "completion_tokens": 1,
                                "total_tokens": 4
                            }
                        })),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "mock".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
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
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        provider_health: None,
        upstream_model: "upstream".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        auth_retryable: false,
    };
    // OpenAI chat shape, as produced from an Anthropic caller, carrying an
    // assistant tool call whose result never came back.
    let request_json = json!({
        "model": "requested-model",
        "messages": [
            {"role": "user", "content": "hi"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_missing",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]
            },
            {"role": "user", "content": "continue"}
        ]
    });

    let response = forward_openai_as_anthropic(
        &state,
        "anthropic-tool-repair",
        "requested-model",
        &request_json,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await.clone().unwrap();
    let messages = captured["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|message| message.get("tool_calls").is_none()),
        "orphan tool_calls must be removed before the upstream call: {captured}"
    );
    assert_eq!(messages.len(), 2);

    server.abort();
}

#[tokio::test]
async fn anthropic_forward_retries_openai_upstream_without_tool_search() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move |Json(body): Json<Value>| {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        assert!(
                            body["tools"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|tool| tool["type"] == "tool_search")
                        );
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "unknown tool type: tool_search"
                                }
                            })),
                        )
                    } else {
                        assert!(body.get("tools").is_none());
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "chatcmpl-1",
                                "object": "chat.completion",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "done"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 1,
                                    "total_tokens": 4
                                }
                            })),
                        )
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool.clone(), None);
    let target = RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "mock".to_string(),
        provider_type: "openai".to_string(),
        base_url: format!("http://{address}"),
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
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        provider_health: None,
        upstream_model: "upstream".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        auth_retryable: false,
    };
    let request_json = json!({
        "model": "requested-model",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{"type": "tool_search", "execution": "client"}]
    });

    let response = forward_openai_as_anthropic(
        &state,
        "anthropic-tool-search-retry",
        "requested-model",
        &request_json,
        target,
        false,
        10,
        None,
        Instant::now(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let tool_search_supported: i64 =
        sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tool_search_supported, 0);
    let tool_search_checked_at: Option<String> =
        sqlx::query_scalar("SELECT tool_search_checked_at FROM providers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(tool_search_checked_at.is_some());

    server.abort();
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

#[test]
fn converts_anthropic_request_into_openai_shape() {
    let inbound = json!({
        "model": "claude-x",
        "system": "be brief",
        "max_tokens": 256,
        "temperature": 0.3,
        "stop_sequences": ["STOP"],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        ],
        "tools": [{
            "name": "get_weather",
            "description": "weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }],
        "tool_choice": {"type": "tool", "name": "get_weather"}
    });
    let openai = anthropic_request_to_openai(&inbound, "claude-x");

    assert_eq!(openai["max_tokens"], 256);
    assert_eq!(openai["temperature"], 0.3);
    assert_eq!(openai["stop"], json!(["STOP"]));
    // The system prompt becomes a leading system message.
    assert_eq!(openai["messages"][0]["role"], "system");
    assert_eq!(openai["messages"][0]["content"], "be brief");
    // A text-only user turn collapses to a plain string.
    assert_eq!(openai["messages"][1]["role"], "user");
    assert_eq!(openai["messages"][1]["content"], "hi");
    // Anthropic tool schema maps onto the OpenAI function shape.
    assert_eq!(openai["tools"][0]["type"], "function");
    assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        openai["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
    assert_eq!(openai["tool_choice"]["function"]["name"], "get_weather");
}

#[test]
fn converts_anthropic_tool_use_and_results_round_trip() {
    // Assistant asks for a tool, then the user returns its result.
    let inbound = json!({
        "model": "claude-x",
        "max_tokens": 128,
        "messages": [
            {"role": "assistant", "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C"}
            ]}
        ]
    });
    let openai = anthropic_request_to_openai(&inbound, "claude-x");
    let assistant = &openai["messages"][0];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "checking");
    assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    // The tool result becomes a dedicated role: tool message.
    let tool = &openai["messages"][1];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "toolu_1");
    assert_eq!(tool["content"], "18C");
}

#[test]
fn converts_openai_completion_into_anthropic_message() {
    let upstream = json!({
        "id": "chatcmpl-abc",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
    });
    let (message, usage) = openai_response_to_anthropic(&upstream, "claude-x");
    assert_eq!(message["type"], "message");
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["model"], "claude-x");
    assert_eq!(message["content"][0]["type"], "text");
    assert_eq!(message["content"][0]["text"], "hello");
    assert_eq!(message["stop_reason"], "end_turn");
    assert_eq!(message["usage"]["input_tokens"], 4);
    assert_eq!(message["usage"]["output_tokens"], 2);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
}

#[test]
fn converts_openai_tool_call_into_anthropic_tool_use() {
    let upstream = json!({
        "id": "chatcmpl-abc",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8}
    });
    let (message, _) = openai_response_to_anthropic(&upstream, "claude-x");
    // No text block: a tool-only answer must not emit an empty text block.
    assert_eq!(message["content"].as_array().unwrap().len(), 1);
    assert_eq!(message["content"][0]["type"], "tool_use");
    assert_eq!(message["content"][0]["id"], "call_1");
    assert_eq!(message["content"][0]["name"], "get_weather");
    assert_eq!(message["content"][0]["input"]["city"], "Paris");
    assert_eq!(message["stop_reason"], "tool_use");
}

#[test]
fn maps_openai_finish_reasons_to_anthropic_stop_reasons() {
    assert_eq!(anthropic_stop_reason(Some("length"), false), "max_tokens");
    assert_eq!(anthropic_stop_reason(Some("tool_calls"), false), "tool_use");
    assert_eq!(anthropic_stop_reason(Some("stop"), false), "end_turn");
    // A tool_use block is authoritative even without a matching reason.
    assert_eq!(anthropic_stop_reason(None, true), "tool_use");
}

#[tokio::test]
async fn rewrites_openai_stream_into_anthropic_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "claude-x".to_string(),
        input_tokens: 7,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();

    let lines: [&[u8]; 5] = [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n",
            b"data: [DONE]\n",
        ];
    let mut saw_done = false;
    for line in lines {
        if !process_openai_line_for_anthropic(line, &mut state, &context, &tx).await {
            saw_done = true;
        }
    }
    assert!(saw_done, "[DONE] must terminate the stream");
    // Exercise the same closing sequence the live handler uses.
    finish_anthropic_stream(&mut state, &context, &tx).await;
    drop(tx);

    let mut frames = Vec::new();
    while let Some(item) = rx.recv().await {
        frames.push(String::from_utf8(item.unwrap().to_vec()).unwrap());
    }
    let all = frames.join("");

    assert!(all.contains("event: message_start"), "{all}");
    assert!(all.contains("event: content_block_start"), "{all}");
    assert!(all.contains("\"type\":\"text_delta\""), "{all}");
    // Text deltas must not be re-joined; each chunk passes through.
    assert!(all.contains("\"text\":\"Hel\""), "{all}");
    assert!(all.contains("\"text\":\"lo\""), "{all}");
    assert!(all.contains("event: content_block_stop"), "{all}");
    assert!(all.contains("event: message_delta"), "{all}");
    assert!(all.contains("\"stop_reason\":\"end_turn\""), "{all}");
    assert!(all.contains("event: message_stop"), "{all}");
    assert_eq!(state.output_tokens, 2);
    assert_eq!(state.text, "Hello");
    assert!(state.first_token_ms.is_some());
}

#[tokio::test]
async fn anthropic_stream_maps_tool_calls_to_tool_use_blocks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "claude-x".to_string(),
        input_tokens: 5,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    // Build the frames with serde so nested JSON escaping stays correct.
    let frames = [
        json!({"choices":[{"index":0,"delta":{"content":"thinking"}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}),
    ];
    for frame in frames {
        let line = format!("data: {frame}\n");
        process_openai_line_for_anthropic(line.as_bytes(), &mut state, &context, &tx).await;
    }
    process_openai_line_for_anthropic(b"data: [DONE]\n", &mut state, &context, &tx).await;
    drop(tx);

    let mut all = String::new();
    while let Some(item) = rx.recv().await {
        all.push_str(std::str::from_utf8(&item.unwrap()).unwrap());
    }
    assert!(all.contains("\"type\":\"tool_use\""), "{all}");
    assert!(all.contains("\"name\":\"get_weather\""), "{all}");
    assert!(all.contains("\"type\":\"input_json_delta\""), "{all}");
    assert!(all.contains("partial_json"), "{all}");
    assert!(state.has_tool_use);
    assert!(state.first_token_ms.is_some());
    assert_eq!(state.finish_reason, None);
}

#[test]
fn anthropic_request_requires_a_model() {
    let missing = json!({"max_tokens": 10, "messages": []});
    assert!(requested_model_of(&missing).is_err());
    let present = json!({"model": "claude-x"});
    assert_eq!(requested_model_of(&present).unwrap(), "claude-x");
}

#[tokio::test]
async fn count_tokens_enforces_model_access_and_route_availability() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (name, provider_type, base_url, enabled)
             VALUES ('Scoped', 'anthropic', 'https://example.com', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let raw_key = "sk-openllm-count-tokens";
    sqlx::query(
        "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-c', 'kens', 1, '[\"gpt-*\"]')",
    )
    .bind(hash_secret(raw_key))
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
    );

    let allowed =
        Bytes::from_static(br#"{"model":"gpt-5","messages":[{"role":"user","content":"hello"}]}"#);
    assert!(count_tokens_inner(&state, &headers, &allowed).await.is_ok());

    let forbidden = Bytes::from_static(br#"{"model":"claude-4","messages":[]}"#);
    assert!(matches!(
        count_tokens_inner(&state, &headers, &forbidden).await,
        Err(AppError::Forbidden(_))
    ));

    let missing = Bytes::from_static(br#"{"model":"gpt-missing","messages":[]}"#);
    assert!(matches!(
        count_tokens_inner(&state, &headers, &missing).await,
        Err(AppError::NotFound(_))
    ));
}

#[test]
fn stream_usage_merges_anthropic_start_and_delta() {
    // Anthropic sends input tokens in `message_start`...
    let mut parser = UsageParser::new(Instant::now());
    parser.push(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":0}}}\n\n",
        );
    // ...and output tokens later in `message_delta`.
    parser.push(
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        );
    let usage = parser.finish().expect("usage should be parsed");
    // Both sides must survive; replacing would leave one of them at zero.
    assert_eq!(usage.prompt_tokens, 11);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.total_tokens, 14);
}

#[test]
fn extracts_openai_usage() {
    let usage = usage_from_value(&json!({
        "usage": { "prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19 }
    }))
    .expect("usage should parse");
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        ),
        (12, 7, 19)
    );
}

#[test]
fn reads_openai_cache_tokens_without_double_counting() {
    // Shape verified against a live CommandCode response: `cached_tokens`
    // lives under prompt_tokens_details and is *already included* in
    // prompt_tokens (638 stays constant while cached_tokens rises 0 -> 512).
    let usage = usage_from_value(&json!({
        "usage": {
            "prompt_tokens": 638,
            "completion_tokens": 32,
            "total_tokens": 670,
            "prompt_tokens_details": { "cached_tokens": 512 }
        }
    }))
    .expect("usage should parse");
    assert_eq!(usage.prompt_tokens, 638, "must not add cached on top");
    assert_eq!(usage.cache_read_tokens, 512);
    assert_eq!(usage.total_tokens, 670);
}

#[test]
fn reads_anthropic_cache_tokens_as_additional_input() {
    // Anthropic reports input_tokens *excluding* cache traffic, so the
    // cache counts must be folded in for the total input to be correct.
    let usage = usage_from_value(&json!({
        "usage": {
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 50
        }
    }))
    .expect("usage should parse");
    assert_eq!(usage.cache_read_tokens, 800);
    assert_eq!(usage.cache_write_tokens, 50);
    assert_eq!(usage.prompt_tokens, 950, "100 fresh + 800 read + 50 write");
}

#[test]
fn cache_fields_default_to_zero_when_absent() {
    let usage = usage_from_value(&json!({
        "usage": { "prompt_tokens": 10, "completion_tokens": 2 }
    }))
    .unwrap();
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(usage.prompt_tokens, 10);
}

#[test]
fn negative_cache_counts_are_clamped() {
    let usage = Usage {
        prompt_tokens: 5,
        completion_tokens: 1,
        total_tokens: 6,
        cache_read_tokens: -10,
        cache_write_tokens: -3,
    }
    .normalized();
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn extracts_nested_responses_usage() {
    let usage = usage_from_value(&json!({
        "type": "response.completed",
        "response": { "usage": { "input_tokens": 88, "output_tokens": 42, "total_tokens": 130 } }
    }))
    .expect("nested responses usage should parse");
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        ),
        (88, 42, 130)
    );
}

#[test]
fn derives_total_when_upstream_omits_it() {
    let usage = usage_from_value(&json!({
        "usage": { "prompt_tokens": 5, "completion_tokens": 3 }
    }))
    .expect("usage should parse");
    assert_eq!(usage.total_tokens, 8);
}

#[test]
fn stream_parser_counts_chat_completion_text() {
    let mut parser = UsageParser::new(Instant::now());
    parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
    parser.push(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"total_tokens\":4}}\n\n",
        );
    let usage = parser.finish().expect("usage should be parsed");
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (2, 2));
}

#[test]
fn stream_parser_records_first_token_once_on_real_content() {
    let mut parser = UsageParser::new(Instant::now());
    // A role-only opening frame and an empty delta must not start the
    // clock; otherwise TTFT would be reported as roughly zero even though
    // the model has not produced anything yet.
    parser.push(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
    parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n");
    assert_eq!(parser.first_token_ms, None);
    parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
    let first = parser.first_token_ms.expect("first token should be timed");
    parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n");
    assert_eq!(
        parser.first_token_ms,
        Some(first),
        "the timestamp must be captured once and not overwritten"
    );
    parser.finish();
}

#[test]
fn stream_parser_counts_responses_api_deltas() {
    let mut parser = UsageParser::new(Instant::now());
    parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n");
    parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n");
    assert!(parser.first_token_ms.is_some());
    let usage = parser
        .finish()
        .expect("estimated usage should be produced from streamed text");
    assert!(
        usage.completion_tokens > 0,
        "estimated completion tokens should be non-zero"
    );
}

#[test]
fn stream_parser_times_tool_call_and_reasoning_output() {
    let mut parser = UsageParser::new(Instant::now());
    parser.push(
        b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n",
    );
    let first = parser
        .first_token_ms
        .expect("reasoning output should start the first-token clock");
    parser.push(b"data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"path\\\":\"}\n\n");
    assert_eq!(parser.first_token_ms, Some(first));

    let mut chat = UsageParser::new(Instant::now());
    chat.push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
        );
    assert!(
        chat.first_token_ms.is_some(),
        "tool-call argument deltas should count as output"
    );
    let usage = chat
        .finish()
        .expect("tool-call arguments should produce an estimate");
    assert!(usage.completion_tokens > 0);
}

#[test]
fn stream_parser_times_native_anthropic_content() {
    let mut text = UsageParser::new(Instant::now());
    text.push(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        );
    assert!(text.first_token_ms.is_some());

    let mut tool = UsageParser::new(Instant::now());
    tool.push(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"name\":\"get_weather\"}}\n\n",
        );
    assert!(
        tool.first_token_ms.is_some(),
        "tool block starts should be treated as the beginning of output"
    );

    let mut thinking = UsageParser::new(Instant::now());
    thinking.push(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
        );
    assert!(thinking.first_token_ms.is_some());
}

#[test]
fn stream_parser_counts_legacy_completions_text() {
    let mut parser = UsageParser::new(Instant::now());
    parser.push(b"data: {\"choices\":[{\"text\":\"hello \"}]}\n\n");
    parser.push(b"data: {\"choices\":[{\"text\":\"world\"}]}\n\n");
    let usage = parser
        .finish()
        .expect("legacy completions text should produce an estimate");
    assert!(
        usage.completion_tokens >= 2,
        "legacy completions text should be counted, got {}",
        usage.completion_tokens
    );
}

#[test]
fn stream_preview_is_bounded() {
    let mut parser = UsageParser::new(Instant::now());
    // Feed far more text than the preview budget across many frames.
    for _ in 0..200 {
        let frame = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
            "x".repeat(100)
        );
        parser.push(frame.as_bytes());
    }
    let preview = parser.preview().expect("preview should exist");
    assert!(
        preview.chars().count() <= PREVIEW_CHAR_LIMIT,
        "preview should be capped at {PREVIEW_CHAR_LIMIT}, got {}",
        preview.chars().count()
    );
}

#[test]
fn anthropic_response_converts_to_openai_shape() {
    let (converted, usage) = convert_anthropic_response(&json!({
        "id": "msg_1",
        "model": "claude-x",
        "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": "hi there" }],
        "usage": { "input_tokens": 30, "output_tokens": 12 }
    }));
    assert_eq!(converted["object"], "chat.completion");
    assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
}

#[test]
fn converts_openai_tool_calls_and_results_to_anthropic() {
    let converted = convert_request_to_anthropic(
        &json!({
            "messages": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "18C" }
            ]
        }),
        "claude-x",
        false,
    );

    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);

    let assistant = &messages[1]["content"];
    let blocks = assistant.as_array().expect("assistant content blocks");
    assert_eq!(blocks.len(), 1, "empty text block should be dropped");
    assert_eq!(blocks[0]["type"], "tool_use");
    assert_eq!(blocks[0]["id"], "call_1");
    assert_eq!(blocks[0]["name"], "get_weather");
    assert_eq!(blocks[0]["input"]["city"], "Paris");

    let tool_result = &messages[2]["content"][0];
    assert_eq!(tool_result["type"], "tool_result");
    assert_eq!(tool_result["tool_use_id"], "call_1");
    assert_eq!(tool_result["content"], "18C");
}

#[test]
fn converts_max_output_tokens_to_anthropic_max_tokens() {
    let converted = convert_request_to_anthropic(
        &json!({
            "max_output_tokens": 1200,
            "messages": [{ "role": "user", "content": "hello" }]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["max_tokens"], 1200);
}

#[test]
fn responses_request_maps_instructions_and_input_to_anthropic() {
    let converted = responses_request_to_anthropic(
        &json!({
            "instructions": "be terse",
            "max_output_tokens": 512,
            "input": [
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hello" }]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "parameters": { "type": "object", "properties": {} }
            }]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["system"], "be terse");
    assert_eq!(converted["max_tokens"], 512);
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hello");
    assert_eq!(converted["tools"][0]["name"], "get_weather");
}

#[test]
fn responses_accepts_plain_string_input() {
    let converted =
        responses_request_to_anthropic(&json!({ "input": "hello world" }), "claude-x", false);
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hello world");
}

#[test]
fn responses_function_call_items_round_trip_to_anthropic() {
    let converted = responses_request_to_anthropic(
        &json!({
            "input": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "18C"
                }
            ]
        }),
        "claude-x",
        false,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);
    let assistant = messages[1]["content"].as_array().expect("assistant blocks");
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["type"], "tool_use");
    assert_eq!(assistant[0]["id"], "call_1");
    assert_eq!(assistant[0]["name"], "get_weather");
    assert_eq!(assistant[0]["input"]["city"], "Paris");
    let tool_result = &messages[2]["content"][0];
    assert_eq!(tool_result["type"], "tool_result");
    assert_eq!(tool_result["tool_use_id"], "call_1");
    assert_eq!(tool_result["content"], "18C");
}

#[test]
fn anthropic_response_converts_to_responses_output() {
    let (converted, usage) = anthropic_response_to_responses(
        &json!({
            "id": "msg_1",
            "model": "claude-x",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "let me check" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": { "city": "Paris" }
                }
            ],
            "usage": { "input_tokens": 30, "output_tokens": 12 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "response");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["status"], "completed");
    assert_eq!(converted["output"][0]["type"], "message");
    assert_eq!(converted["output"][0]["content"][0]["text"], "let me check");
    assert_eq!(converted["output"][1]["type"], "function_call");
    assert_eq!(converted["output"][1]["call_id"], "toolu_1");
    assert_eq!(converted["output"][1]["name"], "get_weather");
    assert_eq!(converted["output"][1]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(converted["usage"]["input_tokens"], 30);
    assert_eq!(converted["usage"]["output_tokens"], 12);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
}

#[tokio::test]
async fn anthropic_stream_converts_to_responses_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut event_name = String::new();
    for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n".as_slice(),
            b"event: message_stop\n".as_slice(),
            b"data: {\"type\":\"message_stop\"}\n".as_slice(),
        ] {
            stream.handle_line(line, &mut event_name, &tx).await;
        }
    stream.complete(&tx).await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(chunk) = rx.recv().await {
        let chunk = chunk.expect("stream chunk");
        events.push(String::from_utf8_lossy(&chunk).to_string());
    }
    let joined = events.join("");
    assert!(joined.contains("event: response.created"));
    assert!(joined.contains("event: response.output_text.delta"));
    assert!(joined.contains("\"delta\":\"hello\""));
    assert!(joined.contains("event: response.output_text.done"));
    assert!(joined.contains("event: response.completed"));
    assert_eq!(stream.usage.prompt_tokens, 7);
    assert_eq!(stream.usage.completion_tokens, 3);
}

#[tokio::test]
async fn chat_stream_converts_to_responses_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut tool_index = None;
    for line in [
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hel\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n".as_slice(),
            b"data: [DONE]\n".as_slice(),
        ] {
            process_chat_chunk_line(&mut stream, line, &mut tool_index, &tx).await;
        }
    stream.ensure_created(&tx).await;
    stream.complete(&tx).await;
    drop(tx);

    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("event: response.created"));
    assert!(joined.contains("event: response.output_text.delta"));
    assert!(joined.contains("\"delta\":\"hel\""));
    assert!(joined.contains("\"delta\":\"lo\""));
    assert!(joined.contains("event: response.output_text.done"));
    assert!(joined.contains("event: response.completed"));
    assert_eq!(stream.usage.prompt_tokens, 5);
    assert_eq!(stream.usage.completion_tokens, 2);
}

#[test]
fn completions_request_maps_prompt_to_anthropic() {
    let converted = completions_request_to_anthropic(
        &json!({
            "prompt": "write a haiku",
            "max_tokens": 64,
            "temperature": 0.5,
            "stop": ["\n\n"]
        }),
        "claude-x",
        false,
    );
    assert_eq!(converted["max_tokens"], 64);
    assert_eq!(converted["temperature"], 0.5);
    assert_eq!(converted["stop_sequences"][0], "\n\n");
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "write a haiku");
}

#[test]
fn completions_request_joins_prompt_array() {
    let converted = completions_request_to_anthropic(
        &json!({ "prompt": ["hello ", "world"] }),
        "claude-x",
        false,
    );
    let messages = converted["messages"].as_array().expect("messages array");
    assert_eq!(messages[0]["content"][0]["text"], "hello world");
}

#[test]
fn completions_request_converts_to_chat() {
    let converted = completions_request_to_chat(
        &json!({
            "prompt": "summarize",
            "max_tokens": 128,
            "temperature": 0.3,
            "stop": ["\n"]
        }),
        "gpt-x",
        true,
    );
    assert_eq!(converted["model"], "gpt-x");
    assert_eq!(converted["stream"], true);
    assert_eq!(converted["max_tokens"], 128);
    assert_eq!(converted["temperature"], 0.3);
    assert_eq!(converted["stop"][0], "\n");
    assert_eq!(converted["messages"][0]["role"], "user");
    assert_eq!(converted["messages"][0]["content"], "summarize");
}

#[test]
fn chat_response_converts_to_completions() {
    let (converted, usage) = chat_response_to_completions(
        &json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "done" },
                "finish_reason": "length"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 6, "total_tokens": 11 }
        }),
        "gpt-x",
    );
    assert_eq!(converted["object"], "text_completion");
    assert_eq!(converted["id"], "cmpl-abc");
    assert_eq!(converted["choices"][0]["text"], "done");
    assert_eq!(converted["choices"][0]["finish_reason"], "length");
    assert_eq!(converted["usage"]["completion_tokens"], 6);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (5, 6));
}

#[tokio::test]
async fn chat_stream_converts_to_completions_chunks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut text = String::new();
    let mut usage = Usage::default();
    let mut finish_reason = None;
    let mut first_token_ms = None;
    for line in [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n".as_slice(),
            b"data: [DONE]\n".as_slice(),
        ] {
            process_chat_line_for_completions(
                line,
                &mut text,
                &mut usage,
                &mut finish_reason,
                &mut first_token_ms,
                Instant::now(),
                "cmpl_test",
                "gpt-x",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"object\":\"text_completion\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert_eq!(text, "hi!");
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(finish_reason.as_deref(), Some("stop"));
}

#[test]
fn anthropic_response_converts_to_text_completion() {
    let (converted, usage) = anthropic_response_to_completions(
        &json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "a haiku" }],
            "usage": { "input_tokens": 9, "output_tokens": 4 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "text_completion");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["choices"][0]["text"], "a haiku");
    assert_eq!(converted["choices"][0]["finish_reason"], "stop");
    assert_eq!(converted["usage"]["completion_tokens"], 4);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (9, 4));
}

#[tokio::test]
async fn anthropic_stream_converts_to_completions_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut text = String::new();
    let mut stop_reason = None;
    let mut first_token_ms = None;
    for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n".as_slice(),
        ] {
            process_completions_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut text,
                &mut stop_reason,
                &mut first_token_ms,
                Instant::now(),
                "cmpl_test",
                "coding",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut events = Vec::new();
    while let Some(chunk) = rx.recv().await {
        events.push(String::from_utf8_lossy(&chunk.expect("chunk")).to_string());
    }
    let joined = events.join("");
    assert!(joined.contains("\"object\":\"text_completion\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert_eq!(text, "hi");
    assert_eq!(usage.prompt_tokens, 5);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
}

#[tokio::test]
async fn anthropic_stream_tool_use_becomes_openai_tool_calls() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut output_chars = 0usize;
    let mut text = String::new();
    let mut sent_role = false;
    let mut saw_tool_use = false;
    let mut next_tool_index = 0usize;
    let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
    let mut first_token_ms: Option<i64> = None;

    // Anthropic emits a tool_use block start followed by JSON argument deltas.
    for line in [
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
    for line in [
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
    drop(tx);

    let mut frames = Vec::new();
    while let Some(Ok(bytes)) = rx.recv().await {
        frames.push(String::from_utf8_lossy(&bytes).to_string());
    }
    let joined = frames.join("");

    assert!(saw_tool_use, "tool_use block start should be detected");
    assert!(
        joined.contains("\"tool_calls\""),
        "should emit tool_calls delta: {joined}"
    );
    assert!(
        joined.contains("toolu_1"),
        "should carry the tool call id: {joined}"
    );
    assert!(
        joined.contains("get_weather"),
        "should carry the function name: {joined}"
    );
    assert!(
        joined.contains("Paris"),
        "should carry streamed arguments: {joined}"
    );
}

#[tokio::test]
async fn parallel_anthropic_tool_calls_keep_distinct_indices() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut output_chars = 0usize;
    let mut text = String::new();
    let mut sent_role = false;
    let mut saw_tool_use = false;
    let mut next_tool_index = 0usize;
    let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
    let mut first_token_ms: Option<i64> = None;

    let lines: [&[u8]; 8] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n",
        ];
    for line in lines {
        process_anthropic_line(
            line,
            &mut event_name,
            &mut usage,
            &mut output_chars,
            &mut text,
            &mut sent_role,
            &mut saw_tool_use,
            &mut next_tool_index,
            &mut tool_indices,
            &mut first_token_ms,
            Instant::now(),
            "chatcmpl_test",
            "claude-x",
            &tx,
        )
        .await;
    }
    drop(tx);

    let mut joined = String::new();
    while let Some(Ok(bytes)) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&bytes));
    }

    assert!(joined.contains("toolu_a"), "first tool call should appear");
    assert!(joined.contains("toolu_b"), "second tool call should appear");
    assert!(
        joined.contains("\"index\":1"),
        "second tool call should keep index 1 rather than collapsing to 0: {joined}"
    );
    assert!(
        joined.contains("get_time"),
        "second tool name should appear"
    );
}

#[tokio::test]
async fn text_block_before_tools_does_not_shift_tool_indices() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut output_chars = 0usize;
    let mut text = String::new();
    let mut sent_role = false;
    let mut saw_tool_use = false;
    let mut next_tool_index = 0usize;
    let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
    let mut first_token_ms: Option<i64> = None;

    // Anthropic block 0 is text; the tool calls are blocks 1 and 2.
    // OpenAI must renumber the tool calls to 0 and 1.
    let lines: [&[u8]; 10] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n",
        ];
    for line in lines {
        process_anthropic_line(
            line,
            &mut event_name,
            &mut usage,
            &mut output_chars,
            &mut text,
            &mut sent_role,
            &mut saw_tool_use,
            &mut next_tool_index,
            &mut tool_indices,
            &mut first_token_ms,
            Instant::now(),
            "chatcmpl_test",
            "claude-x",
            &tx,
        )
        .await;
    }
    drop(tx);

    let mut joined = String::new();
    while let Some(Ok(bytes)) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&bytes));
    }

    assert!(
        joined.contains("toolu_a") && joined.contains("get_weather"),
        "first tool call should appear: {joined}"
    );
    assert!(
        joined.contains("\"index\":0") && joined.contains("\"index\":1"),
        "tool calls should be renumbered to 0 and 1: {joined}"
    );
    assert!(
        !joined.contains("\"index\":2"),
        "OpenAI tool indices must not include Anthropic text-block offsets: {joined}"
    );
}

#[test]
fn chat_request_converts_to_responses_shape() {
    let converted = chat_request_to_responses(
        &json!({
            "model": "coding",
            "max_tokens": 256,
            "temperature": 0.2,
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": [
                    { "type": "text", "text": "guess 2+2" }
                ] },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "calc", "arguments": "{\"expr\":\"2+2\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "4" }
            ],
            "tools": [{
                "type": "function",
                "function": { "name": "calc", "parameters": { "type": "object" } }
            }]
        }),
        "deepseek/x",
        true,
    );
    assert_eq!(converted["model"], "deepseek/x");
    assert_eq!(converted["stream"], true);
    assert_eq!(converted["instructions"], "be terse");
    assert_eq!(converted["max_output_tokens"], 256);
    let input = converted["input"].as_array().expect("input array");
    // user message, function_call, function_call_output (assistant text was empty)
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(input[0]["content"][0]["text"], "guess 2+2");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_1");
    assert_eq!(input[1]["name"], "calc");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["output"], "4");
    assert_eq!(converted["tools"][0]["name"], "calc");
}

#[test]
fn responses_result_converts_to_chat_completion() {
    let (converted, usage) = responses_response_to_chat(
        &json!({
            "id": "resp_abc",
            "status": "completed",
            "output": [
                { "type": "reasoning", "summary": [] },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi there" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_9",
                    "name": "calc",
                    "arguments": "{\"expr\":\"2+2\"}"
                }
            ],
            "usage": { "input_tokens": 11, "output_tokens": 5, "total_tokens": 16 }
        }),
        "coding",
    );
    assert_eq!(converted["object"], "chat.completion");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
    assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_9"
    );
    assert_eq!(converted["usage"]["prompt_tokens"], 11);
    assert_eq!(converted["usage"]["completion_tokens"], 5);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (11, 5));
}

#[tokio::test]
async fn responses_stream_converts_to_chat_chunks() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut event_name = String::new();
    let mut state = ChatStreamState::default();
    for line in [
            b"event: response.reasoning_summary_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"plan\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" there\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3},\"status\":\"completed\"}}\n".as_slice(),
        ] {
            process_responses_line_for_chat(
                line,
                &mut event_name,
                &mut state,
                "chatcmpl_test",
                "coding",
                Instant::now(),
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"content\":\"hi\""));
    assert!(joined.contains("\"content\":\" there\""));
    assert!(joined.contains("\"reasoning_content\":\"plan\""));
    assert_eq!(state.text, "hi there");
    assert_eq!(state.prompt_tokens, 7);
    assert_eq!(state.completion_tokens, 3);
    assert!(state.response_terminated);
    assert!(state.response_error.is_none());
}

#[tokio::test]
async fn responses_stream_failure_is_not_a_successful_chat_or_anthropic_turn() {
    let (tx, _rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let failed = b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"quota exhausted\"},\"usage\":{\"input_tokens\":6,\"output_tokens\":0}}}\n";
    let mut chat = ChatStreamState::default();
    process_responses_line_for_chat(
        failed,
        &mut String::new(),
        &mut chat,
        "chatcmpl_test",
        "coding",
        Instant::now(),
        &tx,
    )
    .await;
    assert!(chat.response_terminated);
    assert_eq!(chat.response_error.as_deref(), Some("quota exhausted"));
    assert_eq!(chat.prompt_tokens, 6);

    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 6,
        started: Instant::now(),
    };
    let mut anthropic = AnthropicStreamState::default();
    process_responses_line_for_anthropic(
        failed,
        &mut String::new(),
        &mut anthropic,
        &mut std::collections::HashMap::new(),
        &context,
        &tx,
    )
    .await;
    assert!(anthropic.response_terminated);
    assert_eq!(anthropic.response_error.as_deref(), Some("quota exhausted"));
    assert_eq!(anthropic.input_tokens, 6);
}

#[tokio::test]
async fn incomplete_responses_stream_has_length_finish_reason() {
    let (tx, _rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let incomplete = b"data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n";
    let mut chat = ChatStreamState::default();
    process_responses_line_for_chat(
        incomplete,
        &mut String::new(),
        &mut chat,
        "chatcmpl_test",
        "coding",
        Instant::now(),
        &tx,
    )
    .await;
    assert_eq!(chat.finish_reason(), "length");
    assert!(chat.response_error.is_none());
}

#[test]
fn responses_result_converts_to_anthropic_message() {
    let (converted, usage) = responses_response_to_anthropic(
        &json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "calc",
                    "arguments": "{\"x\":1}"
                }
            ],
            "usage": { "input_tokens": 4, "output_tokens": 2 }
        }),
        "coding",
    );
    assert_eq!(converted["type"], "message");
    assert_eq!(converted["model"], "coding");
    assert_eq!(converted["content"][0]["type"], "text");
    assert_eq!(converted["content"][0]["text"], "hi");
    assert_eq!(converted["content"][1]["type"], "tool_use");
    assert_eq!(converted["content"][1]["name"], "calc");
    assert_eq!(converted["content"][1]["input"]["x"], 1);
    assert_eq!(converted["stop_reason"], "tool_use");
    assert_eq!(converted["usage"]["output_tokens"], 2);
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
}

#[tokio::test]
async fn responses_stream_converts_to_anthropic_events() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 5,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    let mut tool_indices = std::collections::HashMap::new();
    let mut event_name = String::new();
    for line in [
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":6,\"output_tokens\":2}}}\n".as_slice(),
        ] {
            process_responses_line_for_anthropic(
                line,
                &mut event_name,
                &mut state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
    finish_anthropic_stream(&mut state, &context, &tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("event: message_start"));
    assert!(joined.contains("event: content_block_start"));
    assert!(joined.contains("\"text_delta\""));
    assert!(joined.contains("\"text\":\"hi\""));
    assert!(joined.contains("event: message_stop"));
    assert_eq!(state.text, "hi!");
    assert_eq!(state.output_tokens, 2);
}

#[test]
fn anthropic_thinking_becomes_responses_reasoning() {
    let (converted, _) = anthropic_response_to_responses(
        &json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "let me think", "signature": "sig" },
                { "type": "text", "text": "answer" }
            ],
            "usage": { "input_tokens": 3, "output_tokens": 4 }
        }),
        "coding",
    );
    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(converted["output"][0]["summary"][0]["text"], "let me think");
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][1]["content"][0]["text"], "answer");
}

#[tokio::test]
async fn anthropic_thinking_stream_becomes_responses_reasoning() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    let mut event_name = String::new();
    for line in [
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n".as_slice(),
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":1}\n".as_slice(),
        ] {
            stream.handle_line(line, &mut event_name, &tx).await;
        }
    stream.complete(&tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("response.reasoning_summary_text.delta"));
    assert!(joined.contains("\"delta\":\"plan\""));
    assert!(joined.contains("response.output_text.delta"));
    assert_eq!(stream.output[0]["type"], "reasoning");
    assert_eq!(stream.output[0]["summary"][0]["text"], "plan");
    assert_eq!(stream.output[1]["type"], "message");
}

#[tokio::test]
async fn anthropic_max_tokens_emits_incomplete_responses_event() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
    stream.stop_reason = Some("max_tokens".to_string());
    stream.complete(&tx).await;
    drop(tx);
    let mut events = String::new();
    while let Some(chunk) = rx.recv().await {
        events.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    assert!(events.contains("event: response.incomplete"));
    assert!(events.contains("\"reason\":\"max_output_tokens\""));
    assert!(!events.contains("event: response.completed"));
}

#[test]
fn responses_reasoning_does_not_forge_anthropic_signature() {
    let (converted, _) = responses_response_to_anthropic(
        &json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "thought" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "answer" }]
                }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        }),
        "coding",
    );
    assert_eq!(converted["content"].as_array().unwrap().len(), 1);
    assert_eq!(converted["content"][0]["type"], "text");
    assert_eq!(converted["content"][0]["text"], "answer");
}

#[test]
fn chat_reasoning_does_not_forge_anthropic_signature() {
    let (converted, _) = openai_response_to_anthropic(
        &json!({
            "choices": [{
                "message": {"role": "assistant", "reasoning_content": "private plan", "content": "answer"},
                "finish_reason": "stop"
            }]
        }),
        "coding",
    );
    assert_eq!(
        converted["content"],
        json!([{"type": "text", "text": "answer"}])
    );
}

#[test]
fn chat_reasoning_content_becomes_responses_reasoning() {
    let (converted, _) = chat_response_to_responses(
        &json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "thought"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        }),
        "coding",
    );
    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(converted["output"][0]["summary"][0]["text"], "thought");
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][1]["content"][0]["text"], "answer");
}

#[test]
fn anthropic_thinking_sets_chat_reasoning_content() {
    let (converted, _) = convert_anthropic_response(&json!({
        "id": "msg_1",
        "stop_reason": "end_turn",
        "content": [
            { "type": "thinking", "thinking": "plan", "signature": "sig" },
            { "type": "text", "text": "ok" }
        ],
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    }));
    assert_eq!(
        converted["choices"][0]["message"]["reasoning_content"],
        "plan"
    );
    assert_eq!(converted["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn anthropic_thinking_stream_emits_chat_reasoning_content() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    let mut event_name = String::new();
    let mut usage = Usage::default();
    let mut output_chars = 0usize;
    let mut text = String::new();
    let mut sent_role = false;
    let mut saw_tool_use = false;
    let mut next_tool_index = 0usize;
    let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
    let mut first_token_ms: Option<i64> = None;
    for line in [
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(joined.contains("\"reasoning_content\":\"plan\""));
}

#[tokio::test]
async fn responses_reasoning_stream_does_not_emit_unsigned_anthropic_thinking() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    let context = StreamContext {
        message_id: "msg_test".to_string(),
        model: "coding".to_string(),
        input_tokens: 1,
        started: Instant::now(),
    };
    let mut state = AnthropicStreamState::default();
    let mut tool_indices = std::collections::HashMap::new();
    let mut event_name = String::new();
    for line in [
            b"event: response.output_item.added\n".as_slice(),
            b"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n".as_slice(),
            b"event: response.reasoning_summary_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"hmm\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n".as_slice(),
        ] {
            process_responses_line_for_anthropic(
                line,
                &mut event_name,
                &mut state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
    finish_anthropic_stream(&mut state, &context, &tx).await;
    drop(tx);
    let mut joined = String::new();
    while let Some(chunk) = rx.recv().await {
        joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(!joined.contains("\"thinking_delta\""));
    assert!(!joined.contains("\"signature\":\"\""));
    assert!(joined.contains("\"text\":\"done\""));
    assert!(joined.contains("event: message_stop"));
}

#[test]
fn upstream_rate_limit_errors_keep_status_and_retry_after() {
    // Sub-second windows round up to 1 so a client never reads "0".
    let openai = AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: "mock returned 429".to_string(),
        retry_after: Some(Duration::from_millis(500)),
    }
    .into_response();
    assert_eq!(openai.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        openai.headers().get(reqwest::header::RETRY_AFTER).unwrap(),
        "1"
    );

    let anthropic = anthropic_error_response(AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: "mock returned 429".to_string(),
        retry_after: Some(Duration::from_secs(30)),
    });
    assert_eq!(anthropic.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        anthropic
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "30"
    );

    // A non-429 status must not grow a Retry-After header from nothing.
    let plain = AppError::Upstream("upstream exploded".to_string()).into_response();
    assert_eq!(plain.status(), StatusCode::BAD_GATEWAY);
    assert!(plain.headers().get(reqwest::header::RETRY_AFTER).is_none());
}

#[test]
fn gateway_tuning_env_parsers_fall_back_safely() {
    use crate::state::{
        DEFAULT_MAX_BODY_MIB, DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS, parse_max_body_mib,
        parse_positive_secs,
    };

    // Missing, empty, unparsable, and non-positive values keep the default so a
    // typo can never shrink the body cap or disable the idle timeout.
    assert_eq!(parse_max_body_mib(None), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("   ")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("0")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("-8")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("lots")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some(" 64 ")), 64);

    assert_eq!(
        parse_positive_secs(None, DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS),
        DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS
    );
    assert_eq!(parse_positive_secs(Some("0"), 300), 300);
    assert_eq!(parse_positive_secs(Some("bad"), 300), 300);
    assert_eq!(parse_positive_secs(Some(" 900 "), 300), 900);
}

#[tokio::test]
async fn all_rate_limited_targets_surface_429_to_the_client() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            let mut response = axum::response::Response::new(axum::body::Body::from(
                "{\"error\":{\"message\":\"rate limited\"}}",
            ));
            *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            response.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
                .headers_mut()
                .insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("30"));
            response
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled)
             VALUES (1, 'rate-limited', 'openai', ?, '', 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'upstream', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let uri: Uri = OPENAI_CHAT_COMPLETIONS.parse().unwrap();
    let body = Bytes::from(
        json!({
            "model": "upstream",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai(State(state), HeaderMap::new(), uri, body).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "30"
    );
    assert!(response.headers().get("x-openllm-request-id").is_some());

    server.abort();
}
