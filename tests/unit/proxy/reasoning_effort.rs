use super::*;

#[tokio::test]
async fn model_metadata_maps_original_before_the_first_attempt() {
    let state = circuit_state().await;
    let mut target = endpoint_test_target("openai", None);
    target.reasoning_efforts = Some(json!(["auto", "low", "high"]).to_string());
    let mut body = json!({
        "model": "model",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "original"
    });

    apply_upstream_compat(
        &state,
        "reasoning-effort-metadata",
        &target,
        OPENAI_CHAT_COMPLETIONS,
        UpstreamShape::Chat,
        &mut body,
    )
    .await;

    assert_eq!(body["reasoning_effort"], "auto");
}

#[tokio::test]
async fn enum_rejection_retries_with_the_nearest_value_and_remembers_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let attempts = attempts.clone();
            let requests = requests.clone();
            move |Json(body): Json<Value>| {
                let attempts = attempts.clone();
                let requests = requests.clone();
                async move {
                    requests.lock().await.push(body);
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": {
                                    "message": "Upstream request failed: [invalid_request_error] content part: unknown variant `original`, expected one of `auto`, `low`, `high` (request_id: gw-test)",
                                    "type": "invalid_request_error"
                                }
                            })),
                        )
                            .into_response();
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-effort",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 1,
                                "completion_tokens": 1,
                                "total_tokens": 2
                            }
                        })),
                    )
                        .into_response()
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
         VALUES (1, 'mock', 'openai', ?)",
    )
    .bind(format!("http://{address}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) \
         VALUES (1, 'model', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let target = endpoint_test_target("openai", None);
    let mut body = json!({
        "model": "model",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "original"
    });
    let result = send_provider_request_with_compat_retry(
        &state,
        &target,
        None,
        "reasoning-effort-retry",
        OPENAI_CHAT_COMPLETIONS,
        UpstreamShape::Chat,
        false,
        &mut body,
        |body| {
            Ok(state
                .client
                .post(format!("http://{address}{OPENAI_CHAT_COMPLETIONS}"))
                .json(body))
        },
    )
    .await
    .unwrap();
    assert!(matches!(result, UpstreamAttempt::Ok(_)));

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["reasoning_effort"], "original");
    assert_eq!(requests[1]["reasoning_effort"], "auto");
    drop(requests);

    let stored: Option<String> = sqlx::query_scalar(
        "SELECT reasoning_efforts FROM provider_models \
         WHERE provider_id = 1 AND model_name = 'model'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&stored.unwrap()).unwrap(),
        ["auto", "low", "high"]
    );
    server.abort();
}
