use super::*;

#[tokio::test]
async fn streaming_busy_wait_commits_an_sse_response_before_retrying() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "0")],
                            Json(json!({
                                "error": {
                                    "message": "temporarily busy",
                                    "type": "rate_limit_error"
                                }
                            })),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        [("content-type", "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'busy', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests,
             keepalive_during_wait
         ) VALUES (1, 'streaming', 'streaming-model', 'priority', 1, 2, 0, 2, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'streaming-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "streaming-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_openai_inner(
        &state,
        &HeaderMap::new(),
        &"/v1/chat/completions".parse().unwrap(),
        &body,
        "streaming-busy-wait-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(&first[..], b": keep-alive\n\n");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "the SSE response must be committed before the retry wait finishes"
    );
    let mut body = String::from_utf8_lossy(&first).into_owned();
    while let Some(chunk) = stream.next().await {
        body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        if body.contains("\"content\":\"ok\"") {
            break;
        }
    }
    assert!(
        body.contains("\"content\":\"ok\""),
        "unexpected SSE body: {body}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    server.abort();
}

#[tokio::test]
async fn anthropic_streaming_busy_wait_commits_keepalive_before_retrying() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        ANTHROPIC_MESSAGES,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "0")],
                            Json(json!({
                                "type": "error",
                                "error": {
                                    "type": "rate_limit_error",
                                    "message": "temporarily busy"
                                }
                            })),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        [("content-type", "text/event-stream")],
                        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'busy-anthropic', 'anthropic', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
             id, name, model_pattern, strategy, enabled,
             all_targets_busy_wait_seconds, retry_jitter_ms, max_waiting_requests,
             keepalive_during_wait
         ) VALUES (1, 'anthropic-streaming', 'claude-test', 'priority', 1, 2, 0, 2, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'claude-test', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "claude-test",
            "max_tokens": 16,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let response = proxy_anthropic_inner(
        &state,
        &HeaderMap::new(),
        &ANTHROPIC_MESSAGES.parse().unwrap(),
        &body,
        "anthropic-streaming-busy-wait-request",
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(&first[..], b": keep-alive\n\n");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    let mut body = String::from_utf8_lossy(&first).into_owned();
    while let Some(chunk) = stream.next().await {
        body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        if body.contains("\"text\":\"ok\"") {
            break;
        }
    }
    assert!(
        body.contains("\"text\":\"ok\""),
        "unexpected SSE body: {body}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    server.abort();
}
