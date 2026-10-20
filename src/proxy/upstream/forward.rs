use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_to_target(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    request_json: &Value,
    _raw_body: &Bytes,
    session_id: Option<&str>,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let provider_type =
        ProviderType::from_str(&target.provider_type).map_err(AppError::BadRequest)?;
    let upstream_endpoint = target_upstream_endpoint(&target, endpoint).unwrap_or(endpoint);
    // Chat completions upstreams can serve a Responses client through
    // translation; everything else is a straight passthrough.
    let translate_responses_to_chat =
        endpoint == OPENAI_RESPONSES && upstream_endpoint == OPENAI_CHAT_COMPLETIONS;
    let translate_chat_to_responses =
        endpoint == OPENAI_CHAT_COMPLETIONS && upstream_endpoint == OPENAI_RESPONSES;
    // Legacy completions clients are served through chat or Responses when the
    // upstream does not expose `/v1/completions` itself.
    let translate_completions_to_chat =
        endpoint == OPENAI_COMPLETIONS && upstream_endpoint == OPENAI_CHAT_COMPLETIONS;
    let translate_completions_to_responses =
        endpoint == OPENAI_COMPLETIONS && upstream_endpoint == OPENAI_RESPONSES;

    let (url, mut request_body) = match provider_type {
        ProviderType::Anthropic => {
            let body = match endpoint {
                OPENAI_RESPONSES => {
                    responses_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                OPENAI_COMPLETIONS => {
                    completions_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                _ => convert_request_to_anthropic(request_json, &target.upstream_model, streamed),
            };
            (join_upstream_url(&target.base_url, "/v1/messages"), body)
        }
        ProviderType::Openai | ProviderType::Ollama | ProviderType::Custom => {
            let body = if translate_completions_to_responses {
                let chat =
                    completions_request_to_chat(request_json, &target.upstream_model, streamed);
                chat_request_to_responses(&chat, &target.upstream_model, streamed)
            } else if translate_completions_to_chat {
                completions_request_to_chat(request_json, &target.upstream_model, streamed)
            } else if translate_responses_to_chat {
                responses_request_to_chat(request_json, &target.upstream_model, streamed)
            } else if translate_chat_to_responses {
                chat_request_to_responses(request_json, &target.upstream_model, streamed)
            } else {
                let mut body = request_json.clone();
                body["model"] = json!(target.upstream_model);
                body
            };
            (join_upstream_url(&target.base_url, upstream_endpoint), body)
        }
    };

    let shape = classify_upstream_shape(provider_type, &request_body);
    apply_upstream_compat(
        state,
        request_id,
        &target,
        upstream_endpoint,
        shape,
        &mut request_body,
    )
    .await;

    let response = match send_provider_request_with_compat_retry(
        state,
        &target,
        api_key.map(|key| key.id),
        request_id,
        endpoint,
        shape,
        streamed,
        &mut request_body,
        |body| build_upstream_request(state, &url, provider_type, &target, body, session_id),
    )
    .await?
    {
        UpstreamAttempt::Ok(response) => response,
        UpstreamAttempt::Error {
            status,
            headers: response_headers,
            body: response_body,
        } => {
            return upstream_error_response(
                state,
                request_id,
                endpoint,
                requested_model,
                &target,
                streamed,
                request_tokens,
                api_key,
                started,
                status,
                response_headers,
                response_body,
            )
            .await;
        }
    };
    let status = response.status();
    let response_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if provider_type == ProviderType::Anthropic {
        if streamed {
            if endpoint == OPENAI_RESPONSES {
                return Ok(anthropic_stream_to_responses(
                    state.clone(),
                    response.into_stream(),
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            if endpoint == OPENAI_COMPLETIONS {
                return Ok(anthropic_stream_to_completions(
                    state.clone(),
                    response.into_stream(),
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            return Ok(anthropic_stream_response(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt,
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Upstream(format!("invalid JSON from Anthropic: {error}")))?;
        let (converted, usage) = match endpoint {
            OPENAI_RESPONSES => anthropic_response_to_responses(&upstream_json, requested_model),
            OPENAI_COMPLETIONS => {
                anthropic_response_to_completions(&upstream_json, requested_model)
            }
            _ => convert_anthropic_response(&upstream_json),
        };
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if translate_responses_to_chat {
        if streamed {
            return Ok(openai_chat_stream_to_responses(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt,
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from chat completions: {error}"))
        })?;
        let (converted, usage) = chat_response_to_responses(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if translate_chat_to_responses {
        if streamed {
            return Ok(responses_stream_to_chat(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt.clone(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from responses upstream: {error}"))
        })?;
        let (converted, usage) = responses_response_to_chat(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if translate_completions_to_chat || translate_completions_to_responses {
        if streamed {
            return Ok(chat_stream_to_completions(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt.clone(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Upstream(format!("invalid JSON from upstream: {error}")))?;
        let chat = if translate_completions_to_responses {
            responses_response_to_chat(&upstream_json, requested_model).0
        } else {
            upstream_json
        };
        let (converted, usage) = chat_response_to_completions(&chat, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if streamed {
        return Ok(passthrough_stream_response(
            state.clone(),
            response.into_stream(),
            response_content_type,
            endpoint.to_string(),
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
            receipt,
        ));
    }

    let response_bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    // Every endpoint that reaches this branch answers with JSON: chat
    // completions, completions, responses and embeddings. A success status
    // whose body is not JSON is a broken upstream — an HTML error page from a
    // proxy, an empty body, a truncated payload — and forwarding it does two
    // kinds of damage: the client receives garbage under a 2xx, and the caller
    // marks the provider healthy on the strength of it, clearing the very
    // circuit that should have routed around it. Fail it as a bad gateway so
    // the fallback loop tries the next target, which is already what the
    // translating branches above do when their upstream body fails to parse.
    if serde_json::from_slice::<Value>(&response_bytes).is_err() {
        let snippet = String::from_utf8_lossy(&response_bytes)
            .chars()
            .take(200)
            .collect::<String>();
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            request_id,
            bytes = response_bytes.len(),
            "upstream returned a success status with a non-JSON body; trying the next target"
        );
        record_upstream_failure(
            state,
            &target,
            StatusCode::BAD_GATEWAY,
            // A `Retry-After` on a 2xx carrying a broken body is not a signal
            // worth honouring, and the response headers are already consumed
            // with the body; fall back to the built-in bad-gateway cooldown.
            &HeaderMap::new(),
            &snippet,
        )
        .await;
        return Err(upstream_failure_error(
            StatusCode::BAD_GATEWAY,
            format!(
                "{} returned a {} response that is not JSON: {snippet}",
                target.provider_name,
                status.as_u16()
            ),
            None,
        ));
    }
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let response_text = String::from_utf8_lossy(&response_bytes)
        .chars()
        .take(600)
        .collect::<String>();
    let preview = response_preview(&response_bytes);
    let latency_ms = started.elapsed().as_millis() as i64;
    // Detach: the caller should not wait on a SQLite write to receive the
    // upstream response they already paid for.
    log_usage_detached(
        state.clone(),
        OwnedUsageLogEntry {
            request_id: request_id.to_string(),
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model: requested_model.to_string(),
            upstream_model: Some(target.upstream_model.clone()),
            endpoint: endpoint.to_string(),
            usage,
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview,
        },
    );

    // Inject the receipt only when the body is a JSON object, and only after
    // usage logging has read the original bytes.
    let body_bytes = inject_capability_receipt(&response_bytes, &receipt)
        .map(Bytes::from)
        .unwrap_or(response_bytes);
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, response_content_type)
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| {
            AppError::Upstream(format!("could not forward response: {response_text}"))
                .into_response()
        });
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}
