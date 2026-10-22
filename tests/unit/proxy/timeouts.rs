use super::*;

#[tokio::test]
async fn provider_timeout_aborts_slow_upstreams_and_opens_cooldowns() {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            StatusCode::OK
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
    // Disable same-target retries so this test measures the timeout path only.
    sqlx::query("INSERT INTO settings (key, value) VALUES ('resilience', ?)")
        .bind(
            json!({
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
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.base_url = format!("http://{address}");
    target.timeout_seconds = Some(1);

    let url = format!("http://{address}/v1/chat/completions");
    assert!(
        send_provider_request(&state, &target, false, || Ok(state.client.post(&url)))
            .await
            .is_err()
    );
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(7, target.upstream_model.clone()))
    );
    assert!(state.provider_cooldown.lock().await.contains_key(&7));
    assert_eq!(
        state
            .failure_classifications
            .lock()
            .await
            .get(&(
                "transport".to_string(),
                "next_target".to_string(),
                "upstream_transport_error".to_string(),
            ))
            .copied(),
        Some(1)
    );

    server.abort();
}

/// A provider request timeout must not become a total timeout for a stream:
/// it bounds only the wait for response headers, so a generation that runs
/// longer than the budget is still delivered in full.
#[tokio::test]
async fn provider_timeout_does_not_truncate_a_stream() {
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            let stream = async_stream::stream! {
                yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"early\"}}]}\n\n",
                ));
                tokio::time::sleep(Duration::from_millis(1200)).await;
                yield Ok(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\ndata: [DONE]\n\n",
                ));
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
        "INSERT INTO providers \
             (id, name, provider_type, base_url, model_prefix, enabled, timeout_seconds) \
             VALUES (1, 'slow-stream', 'openai', ?, '', 1, 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) \
             VALUES (1, 'slow-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let uri: Uri = OPENAI_CHAT_COMPLETIONS.parse().unwrap();
    let body = Bytes::from(
        json!({
            "model": "slow-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        })
        .to_string(),
    );
    let response = proxy_openai(
        State(state),
        Extension(GatewayAuth(None)),
        HeaderMap::new(),
        uri,
        body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("early"), "{text}");
    assert!(
        text.contains("late"),
        "the provider total timeout must not cut a stream: {text}"
    );

    server.abort();
}
