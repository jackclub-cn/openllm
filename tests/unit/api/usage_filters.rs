use super::*;

#[tokio::test]
async fn usage_filter_can_select_in_flight_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens, total_tokens,
                latency_ms, status_code, in_flight, success, created_at
             ) VALUES
                ('pending', 'm', '/v1/chat/completions', 10, 10, 0, 0, 1, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('finished', 'm', '/v1/chat/completions', 10, 20, 100, 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: Some(true),
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "pending");
    assert!(page.items[0].in_flight);
}

#[tokio::test]
async fn usage_filter_can_select_endpoint() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('chat', 'm', '/v1/chat/completions', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('responses', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: Some("/v1/responses".to_string()),
            success: None,
            in_flight: None,
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "responses");
}

#[tokio::test]
async fn usage_filter_can_select_gateway_adjusted_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success, warning_message, created_at
             ) VALUES
                ('adjusted', 'm', '/v1/chat/completions', 200, 0, 1,
                 'repaired request', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('original', 'm', '/v1/chat/completions', 200, 0, 1,
                 NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(adjusted) = list_usage(
        State(state.clone()),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: Some(true),
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(adjusted.total, 1);
    assert_eq!(adjusted.items[0].request_id, "adjusted");

    let Json(original) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: None,
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: Some(false),
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(original.total, 1);
    assert_eq!(original.items[0].request_id, "original");
}

#[tokio::test]
async fn usage_filter_can_select_session() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, session_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('session-a', 'codex-session-a', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('session-b', 'codex-session-b', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool, None);
    let Json(page) = list_usage(
        State(state),
        Query(UsageQuery {
            page: 1,
            page_size: 20,
            provider_id: None,
            provider_api_key_id: None,
            api_key_id: None,
            route_id: None,
            model: None,
            request_id: None,
            session_id: Some("session-a".to_string()),
            endpoint: None,
            success: None,
            in_flight: None,
            gateway_adjusted: None,
            from: None,
            to: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, "session-a");
    assert_eq!(page.items[0].session_id.as_deref(), Some("codex-session-a"));
}
