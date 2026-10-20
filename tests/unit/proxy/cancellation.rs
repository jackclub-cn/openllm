use super::*;

#[tokio::test]
async fn cancelled_routing_is_logged_before_the_stale_request_sweep() {
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let received = received.clone();
            move || {
                let received = received.clone();
                async move {
                    received.notify_one();
                    std::future::pending::<Json<Value>>().await
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = circuit_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'waiting', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) \
         VALUES (1, 'model', '[\"/chat/completions\"]')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let request = tokio::spawn({
        let state = state.clone();
        async move {
            proxy_openai_inner(
                &state,
                &HeaderMap::new(),
                &Uri::from_static(OPENAI_RESPONSES),
                &Bytes::from_static(br#"{"model":"model","input":"hello","stream":true}"#),
                "cancelled-routing",
                None,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), received.notified())
        .await
        .expect("the upstream must receive the request");
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let row = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row: (i64, i64, i64, String) = sqlx::query_as(
                "SELECT in_flight, status_code, latency_ms, COALESCE(error_message, '') \
                 FROM usage_logs WHERE request_id = 'cancelled-routing'",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
            if row.0 == 0 {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancellation must be logged without waiting fifteen minutes");
    server.abort();
    assert_eq!(row.1, 499);
    assert!(
        row.2 < 10_000,
        "must record request duration, not cleanup age"
    );
    assert!(row.3.contains("cancelled while waiting for upstream"));
    assert!(state.provider_cooldown.lock().await.is_empty());
    assert!(state.target_cooldown.lock().await.is_empty());
}

#[tokio::test]
async fn routing_usage_heartbeat_protects_live_waits_and_preserves_completed_results() {
    let state = circuit_state().await;
    sqlx::query(
        "INSERT INTO usage_logs (request_id, requested_model, endpoint, status_code, \
                                 in_flight, success, created_at, last_activity_at) \
         VALUES ('long-wait', 'm', '/v1/responses', 0, 1, 0, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 hour'), \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 hour'))",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let (finish, wait) = tokio::sync::oneshot::channel::<()>();
    let request = tokio::spawn({
        let state = state.clone();
        async move {
            track_usage_request(&state, "long-wait", Instant::now(), async {
                wait.await.unwrap();
                // A normal result is finalized by the forwarding path.
                sqlx::query(
                    "UPDATE usage_logs SET in_flight = 0, status_code = 200, success = 1 \
                     WHERE request_id = 'long-wait'",
                )
                .execute(&state.pool)
                .await
                .unwrap();
            })
            .await;
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let fresh: bool = sqlx::query_scalar(
                "SELECT julianday(last_activity_at) > julianday('now', '-1 minute') \
                 FROM usage_logs WHERE request_id = 'long-wait'",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
            if fresh {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("live routing must renew its activity timestamp");
    crate::api::reconcile_stale_usage_requests(state.clone()).await;
    let running: i64 =
        sqlx::query_scalar("SELECT in_flight FROM usage_logs WHERE request_id = 'long-wait'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(running, 1);
    finish.send(()).unwrap();
    request.await.unwrap();
    let row: (i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT in_flight, status_code, success, error_message FROM usage_logs \
         WHERE request_id = 'long-wait'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(row, (0, 200, 1, None));
}
