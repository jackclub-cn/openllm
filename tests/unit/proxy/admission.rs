use super::*;

#[tokio::test]
async fn invalid_gateway_key_is_rejected_before_the_body_is_polled() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO api_keys (name, key_prefix, key_suffix, key_hash, enabled) \
         VALUES ('test', 'sk-test', 'test', ?, 1)",
    )
    .bind(hash_secret("sk-valid"))
    .execute(&pool)
    .await
    .unwrap();
    let router = crate::build_router(AppState::new(pool, None));
    let polled = Arc::new(AtomicBool::new(false));
    let body_polled = polled.clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        body_polled.store(true, Ordering::SeqCst);
        Ok::<_, std::convert::Infallible>(Bytes::from_static(b"{}"))
    }));
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .header("authorization", "Bearer sk-invalid")
        .body(body)
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !polled.load(Ordering::SeqCst),
        "invalid credentials must not trigger a request-body read"
    );
}

#[test]
fn overloaded_response_is_a_retryable_429_with_limit_headers() {
    let response = overloaded_response(4, false);
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "5"
    );
    assert_eq!(response.headers().get("x-ratelimit-limit").unwrap(), "4");
    assert_eq!(
        response.headers().get("x-ratelimit-remaining").unwrap(),
        "0"
    );
}

#[tokio::test]
async fn overloaded_response_uses_the_anthropic_envelope_when_asked() {
    let response = overloaded_response(2, true);
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["type"], "error");
    assert_eq!(value["error"]["type"], "overloaded_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("concurrency limit")
    );
}

#[tokio::test]
async fn request_capacity_slot_is_held_until_the_body_completes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut state = AppState::new(pool, None);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    state.request_capacity = Some(semaphore.clone());
    state.request_capacity_limit = 1;

    let RequestCapacity::Acquired(permit) = acquire_request_capacity(&state, Duration::ZERO).await
    else {
        panic!("the first request must take the only slot");
    };
    assert_eq!(semaphore.available_permits(), 0);
    assert!(matches!(
        acquire_request_capacity(&state, Duration::ZERO).await,
        RequestCapacity::Overloaded
    ));
    assert_eq!(
        state
            .requests_shed
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a refused request is counted for metrics"
    );

    // Attaching moves the permit into the response body so a stream keeps the
    // slot for its whole lifetime, not just until the headers are returned.
    let stream = futures_util::stream::pending::<Result<Bytes, std::convert::Infallible>>();
    let response = Response::new(Body::from_stream(stream));
    let response = attach_request_capacity(response, Some(permit));
    assert_eq!(
        semaphore.available_permits(),
        0,
        "a live body must keep holding the slot"
    );

    // Completing (here, dropping) the body releases the slot for the next call.
    drop(response);
    assert_eq!(semaphore.available_permits(), 1);
    assert!(matches!(
        acquire_request_capacity(&state, Duration::ZERO).await,
        RequestCapacity::Acquired(_)
    ));
}

#[tokio::test]
async fn global_request_cap_sheds_excess_streams_with_429() {
    // Upstream that opens an SSE stream and then stays silent, so the caller
    // holds the slot for as long as it keeps the response body.
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post(|| async {
            let stream = futures_util::stream::once(async {
                Ok::<_, std::convert::Infallible>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                ))
            })
            .chain(futures_util::stream::pending());
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
        "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled)
             VALUES (1, 'streamer', 'openai', ?, '', 1)",
    )
    .bind(format!("http://{address}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'stream-model', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut state = AppState::new(pool, None);
    state.request_capacity = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
    state.request_capacity_limit = 1;

    // The cap is enforced by the router's admission middleware, so drive real
    // requests through it rather than calling the handler directly.
    let router = crate::build_router(state);
    let body = Bytes::from(
        json!({
            "model": "stream-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    let request = || {
        axum::http::Request::builder()
            .method("POST")
            .uri(OPENAI_CHAT_COMPLETIONS)
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    let first = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the first request is admitted"
    );

    let second = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a second stream while the first holds the slot must be shed"
    );
    assert_eq!(
        second.headers().get(reqwest::header::RETRY_AFTER).unwrap(),
        "5"
    );

    // Releasing the first body frees the slot for the next request.
    drop(first);
    let third = router.oneshot(request()).await.unwrap();
    assert_eq!(third.status(), StatusCode::OK);

    server.abort();
}

/// Admission must run before the handler buffers the request body: an
/// over-capacity request with a body that never arrives has to be refused
/// immediately rather than held until the body is read.
#[tokio::test]
async fn admission_rejects_before_reading_the_body() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    // Zero permits: every request is over the cap.
    state.request_capacity = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
    state.request_capacity_limit = 1;
    let router = crate::build_router(state);

    // A body that never produces data. If the handler tried to buffer it, this
    // request would hang instead of being shed.
    let hanging = Body::from_stream(futures_util::stream::pending::<
        Result<Bytes, std::convert::Infallible>,
    >());
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .body(hanging)
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(2), router.oneshot(request))
        .await
        .expect("admission must shed without waiting for the body")
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn admission_uses_the_anthropic_envelope_for_message_routes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    state.request_capacity = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
    state.request_capacity_limit = 2;
    let router = crate::build_router(state);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["type"], "error");
    assert_eq!(value["error"]["type"], "overloaded_error");
}

#[test]
fn request_byte_budget_tracks_and_releases_reservations() {
    let budget = std::sync::Arc::new(RequestByteBudget::new(100));
    assert!(
        budget.reserve(0).is_some(),
        "empty bodies never trip the guard"
    );

    let first = budget.reserve(60).expect("60 fits in the 100-byte budget");
    assert_eq!(budget.used(), 60);
    assert_eq!(budget.remaining(), 40);
    assert!(
        budget.reserve(60).is_none(),
        "a reservation past the limit is refused"
    );
    assert_eq!(budget.used(), 60, "a refused reservation must not charge");

    let mut second = budget.reserve(40).expect("the remaining 40 fits");
    assert_eq!(budget.used(), 100);
    second.shrink_to(10);
    assert_eq!(budget.used(), 70, "shrinking releases the slack");

    drop(first);
    assert_eq!(budget.used(), 10);
    drop(second);
    assert_eq!(
        budget.used(),
        0,
        "dropping the guard frees every reserved byte"
    );
}

#[tokio::test]
async fn inflight_byte_guard_is_held_until_the_response_body_completes() {
    let budget = std::sync::Arc::new(RequestByteBudget::new(64));
    let guard = budget.reserve(64).expect("the whole budget is available");
    let stream = futures_util::stream::pending::<Result<Bytes, std::convert::Infallible>>();
    let response =
        attach_request_guards(Response::new(Body::from_stream(stream)), None, Some(guard));
    assert_eq!(budget.used(), 64, "a live body must keep holding its bytes");
    drop(response);
    assert_eq!(budget.used(), 0, "completing the body releases the bytes");
}

/// The byte budget must shed a declared-oversize body before reading any of it,
/// the same way the concurrency cap refuses before buffering.
#[tokio::test]
async fn admission_sheds_bodies_over_the_inflight_byte_budget() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    let budget = std::sync::Arc::new(RequestByteBudget::new(16));
    state.inflight_request_bytes = Some(budget.clone());
    state.request_bytes_limit = 16;
    let shed = state.request_bytes_shed.clone();
    let router = crate::build_router(state);

    // A body that never produces data. If admission tried to read it, this
    // request would hang instead of being shed.
    let hanging = Body::from_stream(futures_util::stream::pending::<
        Result<Bytes, std::convert::Infallible>,
    >());
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .header("content-length", "1024")
        .body(hanging)
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(2), router.oneshot(request))
        .await
        .expect("the byte budget must shed without waiting for the body")
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "5"
    );
    assert_eq!(
        shed.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a byte-budget refusal is counted for metrics"
    );
    assert_eq!(
        budget.used(),
        0,
        "a shed request must not leak a byte reservation"
    );
}

#[tokio::test]
async fn admission_wait_takes_a_slot_as_soon_as_one_frees() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut state = AppState::new(pool, None);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    state.request_capacity = Some(semaphore.clone());
    state.request_capacity_limit = 1;

    let holder = semaphore.clone().acquire_owned().await.unwrap();
    let waiter =
        tokio::spawn(async move { acquire_request_capacity(&state, Duration::from_secs(2)).await });
    // Free the slot shortly after the waiter parks on it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(holder);

    match waiter.await.unwrap() {
        RequestCapacity::Acquired(_) => {}
        _ => panic!("a waiting request must take the slot once it frees"),
    }
}

#[tokio::test]
async fn admission_wait_sheds_after_the_deadline() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut state = AppState::new(pool, None);
    // Zero permits: nothing can free, so the wait must expire into a shed.
    state.request_capacity = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
    state.request_capacity_limit = 1;

    let wait = Duration::from_millis(120);
    let started = Instant::now();
    let outcome = acquire_request_capacity(&state, wait).await;
    assert!(
        matches!(outcome, RequestCapacity::Overloaded),
        "a wait with no release must still shed"
    );
    assert!(
        started.elapsed() >= wait,
        "shedding waits the configured budget before giving up"
    );
    assert_eq!(
        state
            .requests_shed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[tokio::test]
async fn byte_budget_wait_reserves_once_room_frees() {
    let budget = std::sync::Arc::new(RequestByteBudget::new(64));
    let held = budget.reserve(64).expect("the whole budget is free");
    let waiter = {
        let budget = budget.clone();
        tokio::spawn(async move { budget.reserve_within(64, Duration::from_secs(2)).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(held);
    let guard = waiter
        .await
        .unwrap()
        .expect("the waiter takes the bytes once they free");
    assert_eq!(budget.used(), 64);
    drop(guard);
    assert_eq!(budget.used(), 0);
}

/// End-to-end: with an admission wait configured, a request that arrives while
/// the only slot is busy is admitted once the slot frees instead of being shed.
#[tokio::test]
async fn admission_wait_lets_a_request_through_once_a_slot_frees() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut state = AppState::new(pool, None);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    state.request_capacity = Some(semaphore.clone());
    state.request_capacity_limit = 1;
    state.admission_wait = Duration::from_secs(2);
    let router = crate::build_router(state);

    // Occupy the only slot so the request below must wait for it.
    let holder = semaphore.clone().acquire_owned().await.unwrap();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .body(Body::from("{\"model\":\"m\",\"messages\":[]}"))
        .unwrap();
    let pending = tokio::spawn(async move { router.oneshot(request).await.unwrap() });

    tokio::time::sleep(Duration::from_millis(80)).await;
    drop(holder);

    let response = pending.await.unwrap();
    assert_ne!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a waiting request must be admitted once a slot frees"
    );
}
