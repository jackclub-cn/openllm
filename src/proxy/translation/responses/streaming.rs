use super::*;

/// Streams a Responses API event stream from an OpenAI chat-completions chunk
/// stream, so Responses clients can use chat-only upstreams.
#[allow(clippy::too_many_arguments)]
pub(crate) fn openai_chat_stream_to_responses(
    state: AppState,
    mut upstream: UpstreamByteStream,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let mut stream = ResponsesStreamState::new(requested_model.clone(), started);
        let mut buffer = Vec::<u8>::new();
        let mut tool_index: Option<i64> = None;
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx, state.sse_keepalive).await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    let message = error.to_string();
                    mark_stream_failure(&state, &target, &message).await;
                    stream_error = Some(message);
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_chat_chunk_line(&mut stream, &line, &mut tool_index, &tx).await;
            }
            heartbeat.observe_first_token(stream.first_token_ms);
        }
        if !buffer.is_empty() {
            process_chat_chunk_line(&mut stream, &buffer, &mut tool_index, &tx).await;
        }
        heartbeat.observe_first_token(stream.first_token_ms);

        if let Some(error) = stream_error.as_deref() {
            stream.fail(&tx, error).await;
        } else {
            stream.ensure_created(&tx).await;
            stream.complete(&tx).await;
        }
        drop(tx);

        if stream.usage.prompt_tokens == 0 {
            stream.usage.prompt_tokens = request_tokens;
        }
        if stream.usage.completion_tokens == 0 {
            stream.usage.completion_tokens = (stream.output_chars / 4) as i64;
        }
        let usage = stream.usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(
            stream
                .output
                .iter()
                .filter_map(|item| {
                    item.pointer("/content/0/text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            item.get("arguments")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        })
                })
                .collect::<Vec<_>>()
                .join("")
                .as_bytes(),
        );
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_RESPONSES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn anthropic_stream_to_responses(
    state: AppState,
    mut upstream: UpstreamByteStream,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let mut stream = ResponsesStreamState::new(requested_model.clone(), started);
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx, state.sse_keepalive).await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    let message = error.to_string();
                    mark_stream_failure(&state, &target, &message).await;
                    stream_error = Some(message);
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                stream.handle_line(&line, &mut event_name, &tx).await;
            }
            heartbeat.observe_first_token(stream.first_token_ms);
        }
        if !buffer.is_empty() {
            stream.handle_line(&buffer, &mut event_name, &tx).await;
        }
        heartbeat.observe_first_token(stream.first_token_ms);

        if let Some(error) = stream_error.as_deref() {
            stream.fail(&tx, error).await;
        } else {
            stream.complete(&tx).await;
        }
        drop(tx);

        if stream.usage.prompt_tokens == 0 {
            stream.usage.prompt_tokens = request_tokens;
        }
        if stream.usage.completion_tokens == 0 {
            stream.usage.completion_tokens = (stream.output_chars / 4) as i64;
        }
        let usage = stream.usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(
            stream
                .output
                .iter()
                .filter_map(|item| {
                    item.pointer("/content/0/text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect::<Vec<_>>()
                .join("")
                .as_bytes(),
        );
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_RESPONSES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}
