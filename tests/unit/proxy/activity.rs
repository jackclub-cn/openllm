use super::*;
use std::sync::Arc;

async fn activity_upstream(
    endpoint: &'static str,
) -> (
    std::net::SocketAddr,
    mpsc::Sender<Result<Bytes, io::Error>>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = mpsc::channel(16);
    let receiver = Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    let app = axum::Router::new().route(
        endpoint,
        axum::routing::post(move || {
            let receiver = receiver.clone();
            async move {
                let receiver = receiver.lock().await.take().unwrap();
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(ReceiverStream::new(receiver)))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, sender, server)
}

async fn activity_state(address: std::net::SocketAddr, protocol: &str) -> AppState {
    let state = circuit_state().await;
    sqlx::query("INSERT INTO providers (id, name, provider_type, base_url, model_prefix) VALUES (1, 'live', ?, ?, 'live/')")
        .bind(protocol).bind(format!("http://{address}")).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO provider_models (provider_id, model_name, context_limit, input_limit) VALUES (1, 'model', 10000, 8000)")
        .execute(&state.pool).await.unwrap();
    state
}

async fn wait_activity(
    state: &AppState,
    id: &str,
    predicate: impl Fn(&crate::state::RequestActivitySnapshot) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.activity.snapshot(id).as_ref().is_some_and(&predicate) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn activity_anthropic_stream_exposes_cache_before_first_output_and_persists_it() {
    let (address, sender, server) = activity_upstream(ANTHROPIC_MESSAGES).await;
    let state = activity_state(address, "anthropic").await;
    let body = Bytes::from(json!({
        "model": "live/model", "max_tokens": 100, "stream": true,
        "system": "follow instructions", "tools": [{"name":"lookup","description":"lookup data","input_schema":{}}],
        "messages": [{"role":"user","content":[{"type":"text","text":"inspect"},{"type":"tool_result","tool_use_id":"a","content":"result"}]}]
    }).to_string());
    sender.send(Ok(Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"model\",\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":60,\"cache_creation_input_tokens\":20}}}\n\n"))).await.unwrap();
    let response = proxy_anthropic_inner(
        &state,
        &HeaderMap::new(),
        &ANTHROPIC_MESSAGES.parse().unwrap(),
        &body,
        "native-live",
    )
    .await
    .unwrap();
    wait_activity(&state, "native-live", |item| item.cache.state == "reported").await;
    let snapshot = state.activity.snapshot("native-live").unwrap();
    assert_eq!(snapshot.outcome, "running");
    assert_eq!(snapshot.prompt_tokens, Some(100));
    assert_eq!(snapshot.cache.hit_rate, Some(0.6));
    assert_eq!(snapshot.timings.time_to_first_token_ms, None);
    assert_eq!(
        snapshot.context.as_ref().unwrap().window_tokens,
        Some(10000)
    );
    assert_eq!(snapshot.context.as_ref().unwrap().input_limit, Some(8000));
    sender.send(Ok(Bytes::from_static(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n"))).await.unwrap();
    wait_activity(&state, "native-live", |item| item.phase == "streaming").await;
    sender.send(Ok(Bytes::from_static(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))).await.unwrap();
    drop(sender);
    axum::body::to_bytes(response.into_body(), 100000)
        .await
        .unwrap();
    wait_activity(&state, "native-live", |item| item.outcome == "success").await;
    let detail = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = sqlx::query_as::<_, (i64, Option<i64>)>("SELECT in_flight, cache_usage_reported FROM usage_logs WHERE request_id = 'native-live'")
                .fetch_optional(&state.pool).await.unwrap();
            if let Some((0, reported)) = row { break reported; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(detail, Some(1));
    let snapshot = state.activity.snapshot("native-live").unwrap();
    assert_eq!(snapshot.cache.hit_rate, Some(0.6));
    assert!(snapshot.timings.time_to_first_token_ms.is_some());
    assert_eq!(snapshot.attempt_count, 1);
    server.abort();
}

#[tokio::test]
async fn activity_response_drop_cancels_the_stream_and_usage_row() {
    let (address, sender, server) = activity_upstream(OPENAI_CHAT_COMPLETIONS).await;
    let state = activity_state(address, "openai").await;
    sender
        .send(Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        )))
        .await
        .unwrap();
    let body = Bytes::from(
        json!({"model":"live/model","stream":true,"messages":[{"role":"user","content":"hi"}]})
            .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "cancel-live",
        None,
    )
    .await
    .unwrap();
    drop(response);
    wait_activity(&state, "cancel-live", |item| item.outcome == "cancelled").await;
    sender
        .send(Ok(Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
        )))
        .await
        .ok();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = sqlx::query_scalar::<_, i64>("SELECT status_code FROM usage_logs WHERE request_id = 'cancel-live' AND in_flight = 0")
                .fetch_optional(&state.pool).await.unwrap();
            if let Some(status) = status { assert_eq!(status, 499); break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(state.activity.recent(50).active_count, 0);
    server.abort();
}

#[tokio::test]
async fn activity_nonstreamed_fallback_aligns_with_persisted_trace() {
    let (address, _, server) = spawn_chat_upstream().await;
    let state = activity_state(address, "openai").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let failed_address = listener.local_addr().unwrap();
    let failed_app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":{"message":"temporarily unavailable"}})),
            )
        }),
    );
    let failed_server =
        tokio::spawn(async move { axum::serve(listener, failed_app).await.unwrap() });
    sqlx::query("INSERT INTO providers (id, name, provider_type, base_url) VALUES (2, 'failed', 'openai', ?)")
        .bind(format!("http://{failed_address}")).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO routes (id, name, model_pattern, strategy) VALUES (1, 'fallback', 'routed', 'priority')")
        .execute(&state.pool).await.unwrap();
    for (provider, priority) in [(2, 0), (1, 1)] {
        sqlx::query("INSERT INTO route_targets (route_id, provider_id, upstream_model, priority) VALUES (1, ?, 'model', ?)")
            .bind(provider).bind(priority).execute(&state.pool).await.unwrap();
    }
    let body = Bytes::from(
        json!({"model":"routed","messages":[{"role":"user","content":"hi"}]}).to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "fallback-live",
        None,
    )
    .await
    .unwrap();
    axum::body::to_bytes(response.into_body(), 100000)
        .await
        .unwrap();
    let snapshot = state.activity.snapshot("fallback-live").unwrap();
    assert_eq!(snapshot.attempt_count, 2);
    assert_eq!(snapshot.fallback_count, 1);
    assert!(!snapshot.attempts[0].success);
    assert!(snapshot.attempts[1].success);
    assert_eq!(snapshot.timings.time_to_first_token_ms, None);
    assert_eq!(snapshot.target.unwrap().provider_id, 1);
    assert_eq!(snapshot.context.unwrap().window_tokens, Some(10000));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let trace = sqlx::query_as::<_, (i64, i64)>(
                "SELECT attempt_count, fallback_count FROM routing_traces WHERE request_id = 'fallback-live'",
            ).fetch_optional(&state.pool).await.unwrap();
            if let Some(counts) = trace {
                assert_eq!(counts, (2, 1));
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    server.abort();
    failed_server.abort();
}

#[tokio::test]
async fn activity_upstream_stream_error_is_failed_not_client_cancelled() {
    let (address, sender, server) = activity_upstream(OPENAI_CHAT_COMPLETIONS).await;
    let state = activity_state(address, "openai").await;
    sender
        .send(Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        )))
        .await
        .unwrap();
    let body = Bytes::from(
        json!({"model":"live/model","stream":true,"messages":[{"role":"user","content":"hi"}]})
            .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "failed-stream",
        None,
    )
    .await
    .unwrap();
    sender
        .send(Err(io::Error::other("upstream test connection lost")))
        .await
        .unwrap();
    drop(sender);
    let _ = axum::body::to_bytes(response.into_body(), 100000).await;
    wait_activity(&state, "failed-stream", |item| item.outcome == "failed").await;
    assert_eq!(
        state
            .activity
            .snapshot("failed-stream")
            .unwrap()
            .status_code,
        Some(502)
    );
    server.abort();
}

#[tokio::test]
async fn activity_and_response_complete_even_when_usage_persistence_fails() {
    let (address, sender, server) = activity_upstream(OPENAI_CHAT_COMPLETIONS).await;
    let state = activity_state(address, "openai").await;
    sender
        .send(Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        )))
        .await
        .unwrap();
    let body = Bytes::from(
        json!({"model":"live/model","stream":true,"messages":[{"role":"user","content":"hi"}]})
            .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &OPENAI_CHAT_COMPLETIONS.parse().unwrap(),
        &body,
        "persistence-failed",
        None,
    )
    .await
    .unwrap();
    sqlx::query("DROP TABLE usage_logs")
        .execute(&state.pool)
        .await
        .unwrap();
    sender
        .send(Ok(Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n",
        )))
        .await
        .unwrap();
    drop(sender);
    let bytes = axum::body::to_bytes(response.into_body(), 100000)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("hello"));
    wait_activity(&state, "persistence-failed", |item| {
        item.outcome == "success"
    })
    .await;
    assert_eq!(
        state
            .activity
            .snapshot("persistence-failed")
            .unwrap()
            .prompt_tokens,
        Some(10)
    );
    server.abort();
}
