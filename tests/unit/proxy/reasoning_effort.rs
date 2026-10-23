use super::*;

#[tokio::test]
async fn model_metadata_maps_xhigh_before_the_first_attempt() {
    let state = circuit_state().await;
    sqlx::query(
        "INSERT INTO usage_logs (
             request_id, requested_model, endpoint, status_code, success
         ) VALUES (?, 'model', ?, 200, 1)",
    )
    .bind("reasoning-effort-metadata")
    .bind(OPENAI_CHAT_COMPLETIONS)
    .execute(&state.pool)
    .await
    .unwrap();
    let mut target = endpoint_test_target("openai", None);
    target.reasoning_efforts = Some(json!(["low", "high", "max"]).to_string());
    let mut body = json!({
        "model": "model",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "xhigh"
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

    assert_eq!(body["reasoning_effort"], "max");
    let warning: Option<String> =
        sqlx::query_scalar("SELECT warning_message FROM usage_logs WHERE request_id = ?")
            .bind("reasoning-effort-metadata")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(
        warning.is_none(),
        "routine effort mapping must not become a per-request gateway warning"
    );
}

#[tokio::test]
async fn reasoning_enum_rejection_retries_and_persists_efforts_for_both_protocols() {
    for endpoint in [OPENAI_CHAT_COMPLETIONS, OPENAI_RESPONSES] {
        let requests = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let app = axum::Router::new().route(
            endpoint,
            axum::routing::post({
                let requests = requests.clone();
                move |Json(body): Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        let mut requests = requests.lock().await;
                        requests.push(body);
                        if requests.len() == 1 {
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({
                                    "error": {
                                        "message": "reasoning effort: unknown variant `xhigh`, expected one of `low`, `high`, `max`"
                                    }
                                })),
                            );
                        }
                        (StatusCode::OK, Json(json!({"ok": true})))
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
        let (shape, effort_path, mut body) = if endpoint == OPENAI_CHAT_COMPLETIONS {
            (
                UpstreamShape::Chat,
                "/reasoning_effort",
                json!({"messages": [{"role": "user", "content": "hi"}], "reasoning_effort": "xhigh"}),
            )
        } else {
            (
                UpstreamShape::Responses,
                "/reasoning/effort",
                json!({"input": "hi", "reasoning": {"effort": "xhigh"}}),
            )
        };
        let result = send_provider_request_with_compat_retry(
            &state,
            &target,
            None,
            "reasoning-effort-retry",
            endpoint,
            shape,
            false,
            &mut body,
            |body| {
                Ok(state
                    .client
                    .post(format!("http://{address}{endpoint}"))
                    .json(body))
            },
        )
        .await
        .unwrap();
        assert!(matches!(result, UpstreamAttempt::Ok(_)));
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].pointer(effort_path).unwrap(), "xhigh");
        assert_eq!(requests[1].pointer(effort_path).unwrap(), "max");
        let stored: String = sqlx::query_scalar(
            "SELECT reasoning_efforts FROM provider_models \
             WHERE provider_id = 1 AND model_name = 'model'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&stored).unwrap(),
            ["low", "high", "max"]
        );
        server.abort();
    }
}

#[tokio::test]
async fn image_detail_rejection_retries_without_changing_reasoning_effort() {
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
        "messages": [{
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": {
                    "url": "data:image/png;base64,AAAA",
                    "detail": "original"
                }
            }]
        }],
        "reasoning_effort": "xhigh"
    });
    let result = send_provider_request_with_compat_retry(
        &state,
        &target,
        None,
        "image-detail-retry",
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
    assert_eq!(requests[0]["reasoning_effort"], "xhigh");
    assert_eq!(
        requests[0]["messages"][0]["content"][0]["image_url"]["detail"],
        "original"
    );
    assert_eq!(requests[1]["reasoning_effort"], "xhigh");
    assert_eq!(
        requests[1]["messages"][0]["content"][0]["image_url"]["detail"],
        "high"
    );
    drop(requests);

    let stored: Option<i64> = sqlx::query_scalar(
        "SELECT supported FROM provider_capability_profiles \
         WHERE provider_id = 1 AND model = 'model' \
           AND endpoint = ? AND capability = ?",
    )
    .bind(OPENAI_CHAT_COMPLETIONS)
    .bind(crate::capabilities::CAPABILITY_IMAGE_DETAIL_ORIGINAL)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(stored, Some(0));
    server.abort();
}

#[tokio::test]
async fn command_code_downlevels_original_image_detail_before_first_attempt() {
    let state = circuit_state().await;
    let mut target = endpoint_test_target("openai", None);
    target.base_url = "https://api.commandcode.ai/provider/v1".to_string();
    let mut body = json!({
        "model": "model",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA",
                "detail": "original"
            }]
        }],
        "reasoning": {"effort": "xhigh"}
    });

    apply_upstream_compat(
        &state,
        "command-code-image-detail",
        &target,
        OPENAI_RESPONSES,
        UpstreamShape::Responses,
        &mut body,
    )
    .await;

    assert_eq!(body["input"][0]["content"][0]["detail"], "high");
    assert_eq!(body["reasoning"]["effort"], "xhigh");
}

#[tokio::test]
async fn learned_image_detail_capability_is_normalized_before_the_next_attempt() {
    let state = circuit_state().await;
    let target = endpoint_test_target("openai", None);
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (?, 'test', 'openai', 'http://upstream')",
    )
    .bind(target.provider_id)
    .execute(&state.pool)
    .await
    .unwrap();
    crate::capabilities::record_capability(
        &state,
        crate::capabilities::CapabilityKey {
            provider_id: target.provider_id,
            model: target.upstream_model.clone(),
            endpoint: OPENAI_RESPONSES.to_string(),
            capability: crate::capabilities::CAPABILITY_IMAGE_DETAIL_ORIGINAL.to_string(),
        },
        false,
        crate::capabilities::SOURCE_RUNTIME,
        "exact",
    )
    .await;
    let mut body = json!({
        "model": "model",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA",
                "detail": "original"
            }]
        }],
        "reasoning": {"effort": "xhigh"}
    });

    apply_upstream_compat(
        &state,
        "learned-image-detail",
        &target,
        OPENAI_RESPONSES,
        UpstreamShape::Responses,
        &mut body,
    )
    .await;

    assert_eq!(body["input"][0]["content"][0]["detail"], "high");
    assert_eq!(body["reasoning"]["effort"], "xhigh");
}
