use super::*;

#[tokio::test]
async fn stream_recovery_commits_at_the_first_useful_chunk() {
    let chunk = Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
    let expected = chunk.clone();
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(chunk) })
        .chain(futures_util::stream::pending());
    let response = UpstreamResponse::from_stream(StatusCode::OK, Box::pin(stream));

    let started = Instant::now();
    let recovered = response
        .recover_prefix()
        .await
        .expect("a stream that carries content is not a truncation");
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "the holdback must release as soon as content arrives, took {:?}",
        started.elapsed()
    );

    let mut stream = recovered.into_stream();
    let first = stream
        .next()
        .await
        .expect("the buffered prefix is replayed")
        .unwrap();
    assert_eq!(first, expected);
}

#[test]
fn usable_content_detection_covers_every_protocol() {
    for positive in [
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0}]}}]}\n\n",
        "data: {\"choices\":[{\"text\":\"hi\"}]}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\"}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\"}}\n\n",
        "data: [DONE]\n\n",
        "{\"id\":\"chatcmpl-1\",\"choices\":[]}",
    ] {
        assert!(
            stream_has_usable_content(positive.as_bytes()),
            "expected usable content in {positive:?}"
        );
    }

    for negative in [
        "",
        ": keep-alive\n\n",
        "data: \n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}]}\n\n",
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"text\"}}\n\n",
        "data: {\"type\":\"response.created\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"\"}\n\n",
    ] {
        assert!(
            !stream_has_usable_content(negative.as_bytes()),
            "expected no usable content in {negative:?}"
        );
    }
}

#[tokio::test]
async fn stream_recovery_keeps_a_turn_that_closes_without_a_done_marker() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Several OpenAI-compatible servers end the body after the last content
    // chunk without sending `[DONE]`. Replaying that turn would burn a whole
    // extra generation and then fail outright on the next attempt.
    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"no done marker\"}}]}\n\n",
                        ))
                        .unwrap()
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
         VALUES (1, 'terse stream', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'terse route', 'terse-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'terse-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('resilience', ?)")
        .bind(
            json!({
                "stream_recovery_enabled": true,
                "max_retries": 2,
                "retry_backoff_ms": 1,
                "retry_max_backoff_ms": 5
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "terse-model",
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
        "terse-stream-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a turn that delivered content must not be replayed"
    );
    assert!(
        !state.provider_cooldown.lock().await.contains_key(&1),
        "a delivered turn must not open a provider cooldown"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("no done marker"));

    server.abort();
}

#[tokio::test]
async fn stream_recovery_retries_an_empty_200_before_committing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
                            .body(Body::empty())
                            .unwrap()
                    } else {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
                            .body(Body::from(
                                "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n\
                                 data: [DONE]\n\n",
                            ))
                            .unwrap()
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
         VALUES (1, 'flaky stream', 'openai', ?)",
    )
    .bind(format!("http://{address}/v1"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
         VALUES (1, 'stream retry route', 'stream-retry-model', 'priority', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, priority)
         VALUES (1, 1, 'stream-retry-model', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('resilience', ?)")
        .bind(
            json!({
                "stream_recovery_enabled": true,
                "stream_recovery_max_retries": 1,
                // Same-target retries stay off: the recovery budget must stand
                // on its own, otherwise the toggle silently does nothing.
                "max_retries": 0,
                "retry_backoff_ms": 1,
                "retry_max_backoff_ms": 5
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();

    let state = AppState::new(pool, None);
    let body = Bytes::from(
        json!({
            "model": "stream-retry-model",
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
        "stream-recovery-request",
        None,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(response.headers()["x-openllm-fallback-count"], "0");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("recovered"));
    assert!(body.contains("[DONE]"));

    server.abort();
}
