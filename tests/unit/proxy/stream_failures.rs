use super::*;

/// A stream that dies mid-response must cool the provider so the next request
/// does not pick the same unhealthy target and stall again.
#[tokio::test]
async fn a_mid_stream_upstream_failure_cools_the_provider() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                ));
                // Give hyper a beat to flush the head and first frame before
                // the body errors, so the client sees a committed 200.
                tokio::time::sleep(Duration::from_millis(50)).await;
                yield Err(std::io::Error::other("upstream connection reset mid-stream"));
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(stream))
                .unwrap()
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
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled) \
             VALUES (1, 'flaky-stream', 'openai', ?, '', 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) \
             VALUES (1, 'flaky-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let uri: Uri = OPENAI_CHAT_COMPLETIONS.parse().unwrap();
    let body = Bytes::from(
        json!({
            "model": "flaky-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        })
        .to_string(),
    );
    let response = proxy_openai(State(state.clone()), HeaderMap::new(), uri, body).await;
    let status = response.status();
    if status != StatusCode::OK {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        panic!(
            "expected a committed 200, got {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    // Drain the body so the translator observes the upstream stream error.
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(1, "flaky-model".to_string())),
        "the model target must cool after a mid-stream failure"
    );
    assert!(
        state.provider_cooldown.lock().await.contains_key(&1),
        "the provider must cool after a mid-stream transport failure"
    );

    server.abort();
}

#[tokio::test]
async fn mark_stream_failure_cools_target_and_provider() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 11;
    target.upstream_model = "m".to_string();

    mark_stream_failure(&state, &target, "boom").await;

    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(11, "m".to_string()))
    );
    assert!(state.provider_cooldown.lock().await.contains_key(&11));
    assert_eq!(
        state
            .failure_classifications
            .lock()
            .await
            .get(&(
                "transport".to_string(),
                "next_target".to_string(),
                "upstream_stream_failed".to_string(),
            ))
            .copied(),
        Some(1)
    );
}
