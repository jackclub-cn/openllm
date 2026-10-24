use super::*;

/// Rewrites an OpenAI SSE stream into the Anthropic event protocol.
#[allow(clippy::too_many_arguments)]
pub(crate) fn openai_stream_to_anthropic(
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
        let context = StreamContext {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: requested_model.clone(),
            input_tokens: request_tokens,
            started,
        };
        let mut stream_state = AnthropicStreamState::default();
        let mut buffer = SseLineBuffer::default();
        let mut stream_error = None;
        let mut done = false;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while !done
            && let Some(chunk) = next_upstream_chunk(&mut upstream, &tx, state.sse_keepalive).await
        {
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
            for line in buffer.push(&chunk) {
                // `false` means `[DONE]`: the upstream body is complete.
                if !process_openai_line_for_anthropic(&line, &mut stream_state, &context, &tx).await
                {
                    // `[DONE]` received: stop reading the upstream body.
                    done = true;
                    break;
                }
            }
            heartbeat.observe_first_token(stream_state.first_token_ms);
        }
        if !done && let Some(line) = buffer.finish() {
            process_openai_line_for_anthropic(&line, &mut stream_state, &context, &tx).await;
        }
        heartbeat.observe_first_token(stream_state.first_token_ms);

        finish_anthropic_stream(&mut stream_state, &context, &tx).await;
        drop(tx);

        let usage = Usage {
            prompt_tokens: if stream_state.input_tokens > 0 {
                stream_state.input_tokens
            } else {
                request_tokens
            },
            completion_tokens: stream_state.output_tokens,
            total_tokens: stream_state.input_tokens + stream_state.output_tokens,
            cache_read_tokens: stream_state.cache_read_tokens,
            cache_write_tokens: stream_state.cache_write_tokens,
        }
        .normalized();
        let preview = response_preview(stream_state.text.as_bytes());
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream_state
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
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

/// Rewrites a Responses SSE stream into the Anthropic event protocol so that
/// Anthropic clients can be served by a Responses-only OpenAI upstream.
#[allow(clippy::too_many_arguments)]
pub(crate) fn responses_stream_to_anthropic(
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
        let context = StreamContext {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: requested_model.clone(),
            input_tokens: request_tokens,
            started,
        };
        let mut stream_state = AnthropicStreamState::default();
        let mut tool_indices = std::collections::HashMap::<String, usize>::new();
        let mut buffer = SseLineBuffer::default();
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
            for line in buffer.push(&chunk) {
                process_responses_line_for_anthropic(
                    &line,
                    &mut event_name,
                    &mut stream_state,
                    &mut tool_indices,
                    &context,
                    &tx,
                )
                .await;
            }
            heartbeat.observe_first_token(stream_state.first_token_ms);
        }
        if let Some(line) = buffer.finish() {
            process_responses_line_for_anthropic(
                &line,
                &mut event_name,
                &mut stream_state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
        heartbeat.observe_first_token(stream_state.first_token_ms);
        let stream_error = stream_error
            .or_else(|| stream_state.response_error.clone())
            .or_else(|| {
                (!stream_state.response_terminated)
                    .then(|| "upstream Responses stream ended without a terminal event".to_string())
            });
        if let Some(error) = stream_error.as_deref() {
            send_anthropic_event(&tx, "error", anthropic_error_body("api_error", error)).await;
        } else {
            finish_anthropic_stream(&mut stream_state, &context, &tx).await;
        }
        drop(tx);

        let prompt_tokens = if stream_state.input_tokens > 0 {
            stream_state.input_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if stream_state.output_tokens > 0 {
            stream_state.output_tokens
        } else {
            (stream_state.output_chars / 4) as i64
        };
        let usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: stream_state.cache_read_tokens,
            cache_write_tokens: stream_state.cache_write_tokens,
        }
        .normalized();
        let preview = response_preview(stream_state.text.as_bytes());
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream_state
            .first_token_ms
            .or_else(|| (completion_tokens > 0).then_some(latency_ms));
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
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

pub(crate) fn responses_stream_error(value: &Value) -> String {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .unwrap_or("upstream response failed")
        .to_string()
}

/// Handles one Responses SSE line, emitting the matching Anthropic events.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_responses_line_for_anthropic(
    line: &[u8],
    event_name: &mut String,
    state: &mut AnthropicStreamState,
    tool_indices: &mut std::collections::HashMap<String, usize>,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if let Some(event) = line.strip_prefix("event:") {
        *event_name = event.trim().to_string();
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(event_name.as_str());

    match event {
        "response.output_item.added" => {
            let item = value.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                state.close_text_block(tx).await;
                let index = state.next_index;
                state.next_index += 1;
                state.open_tools.push(index);
                state.has_tool_use = true;
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    tool_indices.insert(item_id.to_string(), index);
                }
                send_anthropic_event(
                    tx,
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned()
                                .unwrap_or_else(|| json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))),
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "input": {}
                        }
                    }),
                )
                .await;
            }
        }
        "response.output_text.delta" => {
            if let Some(text) = value.get("delta").and_then(Value::as_str)
                && !text.is_empty()
            {
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                let index = match state.text_index {
                    Some(index) => index,
                    None => {
                        let index = state.next_index;
                        state.next_index += 1;
                        state.text_index = Some(index);
                        send_anthropic_event(
                            tx,
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": {"type": "text", "text": ""}
                            }),
                        )
                        .await;
                        index
                    }
                };
                state.output_chars = state.output_chars.saturating_add(text.chars().count());
                push_preview_text(&mut state.text, text);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "text_delta", "text": text}
                    }),
                )
                .await;
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(partial) = value.get("delta").and_then(Value::as_str)
                && !partial.is_empty()
                && let Some(index) = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .and_then(|item_id| tool_indices.get(item_id).copied())
            {
                state.mark_first_token(ctx.started);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": partial}
                    }),
                )
                .await;
            }
        }
        "response.completed" | "response.incomplete" | "response.failed" => {
            let response = value.get("response").unwrap_or(&Value::Null);
            state.response_terminated = true;
            if event == "response.failed" {
                state.response_error = Some(responses_stream_error(&value));
            }
            if let Some(usage) = usage_from_value(response) {
                if usage.prompt_tokens > 0 {
                    state.input_tokens = usage.prompt_tokens;
                }
                if usage.completion_tokens > 0 {
                    state.output_tokens = usage.completion_tokens;
                }
                state.cache_read_tokens = usage.cache_read_tokens;
                state.cache_write_tokens = usage.cache_write_tokens;
            }
            if event == "response.incomplete"
                || response.get("status").and_then(Value::as_str) == Some("incomplete")
            {
                state.finish_reason = Some("length".to_string());
            }
        }
        _ => {}
    }
}
