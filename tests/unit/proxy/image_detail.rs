use super::*;

#[tokio::test]
async fn translated_requests_remember_image_detail_under_the_upstream_endpoint() {
    for (endpoint, upstream_endpoint) in [
        (OPENAI_RESPONSES, OPENAI_CHAT_COMPLETIONS),
        (OPENAI_CHAT_COMPLETIONS, OPENAI_RESPONSES),
        (ANTHROPIC_MESSAGES, OPENAI_CHAT_COMPLETIONS),
        (ANTHROPIC_MESSAGES, OPENAI_RESPONSES),
    ] {
        let requests = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let detail_path = if upstream_endpoint == OPENAI_CHAT_COMPLETIONS {
            "/messages/0/content/0/image_url/detail"
        } else {
            "/input/0/content/0/detail"
        };
        let app = axum::Router::new().route(
            upstream_endpoint,
            axum::routing::post({
                let requests = requests.clone();
                move |Json(body): Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        let rejected = body.pointer(detail_path) == Some(&json!("original"));
                        requests.lock().await.push(body);
                        if rejected {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "error": {
                                        "message": "content part: unknown variant `original`, expected one of `auto`, `low`, `high`"
                                    }
                                })),
                            );
                        }
                        let payload = if upstream_endpoint == OPENAI_CHAT_COMPLETIONS {
                            json!({
                                "id": "chatcmpl-image",
                                "object": "chat.completion",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }]
                            })
                        } else {
                            json!({
                                "id": "resp-image",
                                "object": "response",
                                "status": "completed",
                                "output": [{
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": "ok"}]
                                }]
                            })
                        };
                        (StatusCode::OK, Json(payload))
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
        let supported = json!([upstream_endpoint]).to_string();
        let mut target = endpoint_test_target("openai", Some(&supported));
        target.base_url = format!("http://{address}");
        let body = if endpoint == OPENAI_RESPONSES {
            json!({
                "reasoning": {"effort": "xhigh"},
                "input": [{"role": "user", "content": [{
                    "type": "input_image",
                    "image_url": "data:image/png;base64,AAAA",
                    "detail": "original"
                }]}]
            })
        } else {
            json!({
                "reasoning_effort": "xhigh",
                "messages": [{"role": "user", "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,AAAA", "detail": "original"}
                }]}]
            })
        };
        for attempt in 0..2 {
            let request_id = format!("translated-image-{attempt}");
            let response = if endpoint == ANTHROPIC_MESSAGES {
                forward_openai_as_anthropic(
                    &state,
                    &request_id,
                    "model",
                    &body,
                    target.clone(),
                    false,
                    10,
                    None,
                    Instant::now(),
                    None,
                    None,
                )
                .await
            } else {
                forward_to_target(
                    &state,
                    &request_id,
                    endpoint,
                    "model",
                    &body,
                    &Bytes::new(),
                    None,
                    target.clone(),
                    false,
                    10,
                    None,
                    Instant::now(),
                    None,
                )
                .await
            }
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
        }
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 3, "{endpoint} -> {upstream_endpoint}");
        for (body, detail) in requests.iter().zip(["original", "high", "high"]) {
            assert_eq!(body.pointer(detail_path), Some(&json!(detail)));
            assert_eq!(
                body.get("reasoning_effort")
                    .or_else(|| body.pointer("/reasoning/effort")),
                Some(&json!("xhigh"))
            );
        }
        assert_eq!(
            crate::capabilities::capability_support(
                &state,
                target.provider_id,
                "model",
                upstream_endpoint,
                crate::capabilities::CAPABILITY_IMAGE_DETAIL_ORIGINAL,
            )
            .await,
            Some(false)
        );
        server.abort();
    }
}
