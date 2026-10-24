use super::*;

/// Streams a chat-completions response from a Responses-only upstream by
/// translating the Responses SSE events back into chat chunks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn responses_stream_to_chat(
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
        let message_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let mut buffer = SseLineBuffer::default();
        let mut event_name = String::new();
        let mut chat_state = ChatStreamState::default();
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
                process_responses_line_for_chat(
                    &line,
                    &mut event_name,
                    &mut chat_state,
                    &message_id,
                    &requested_model,
                    started,
                    &tx,
                )
                .await;
            }
            heartbeat.observe_first_token(chat_state.first_token_ms);
        }
        if let Some(line) = buffer.finish() {
            process_responses_line_for_chat(
                &line,
                &mut event_name,
                &mut chat_state,
                &message_id,
                &requested_model,
                started,
                &tx,
            )
            .await;
        }
        heartbeat.observe_first_token(chat_state.first_token_ms);

        let stream_error = stream_error
            .or_else(|| chat_state.response_error.clone())
            .or_else(|| {
                (!chat_state.response_terminated)
                    .then(|| "upstream Responses stream ended without a terminal event".to_string())
            });
        if let Some(error) = stream_error.as_deref() {
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"error": {"type": "api_error", "message": error}})
                ))))
                .await;
        } else {
            let reason = chat_state.finish_reason();
            let final_chunk =
                openai_stream_chunk(&message_id, &requested_model, json!({}), Some(reason), None);
            let usage_chunk = openai_stream_chunk(
                &message_id,
                &requested_model,
                json!({}),
                None,
                Some(json!({
                    "prompt_tokens": chat_state.prompt_tokens,
                    "completion_tokens": chat_state.completion_tokens,
                    "total_tokens": chat_state.prompt_tokens + chat_state.completion_tokens
                })),
            );
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&final_chunk).unwrap_or_default(),
                    serde_json::to_string(&usage_chunk).unwrap_or_default()
                ))))
                .await;
        }
        drop(tx);

        let prompt_tokens = if chat_state.prompt_tokens > 0 {
            chat_state.prompt_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if chat_state.completion_tokens > 0 {
            chat_state.completion_tokens
        } else {
            (chat_state.output_chars / 4) as i64
        };
        let usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: chat_state.cache_read_tokens,
            cache_write_tokens: chat_state.cache_write_tokens,
        }
        .normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let preview = response_preview(chat_state.text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_CHAT_COMPLETIONS,
                usage,
                latency_ms,
                first_token_ms: chat_state.first_token_ms,
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

#[derive(Default)]
pub(crate) struct ChatStreamState {
    /// Bounded output preview; never the complete generated response.
    pub(crate) text: String,
    pub(crate) output_chars: usize,
    pub(crate) prompt_tokens: i64,
    pub(crate) completion_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) first_token_ms: Option<i64>,
    pub(crate) sent_role: bool,
    pub(crate) saw_tool_call: bool,
    pub(crate) stop_reason: Option<String>,
    pub(crate) response_terminated: bool,
    pub(crate) response_error: Option<String>,
    /// Maps a Responses function-call item id to its chat tool-call index.
    pub(crate) tool_indices: std::collections::HashMap<String, usize>,
}

impl ChatStreamState {
    pub(crate) fn finish_reason(&self) -> &'static str {
        if self.saw_tool_call {
            "tool_calls"
        } else if self.stop_reason.as_deref() == Some("max_tokens")
            || self.stop_reason.as_deref() == Some("length")
        {
            "length"
        } else {
            "stop"
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_responses_line_for_chat(
    line: &[u8],
    event_name: &mut String,
    state: &mut ChatStreamState,
    message_id: &str,
    model: &str,
    started: Instant,
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
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"reasoning_content": delta}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.output_item.added" => {
            let item = value.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = item.get("name").cloned().unwrap_or(Value::Null);
                let index = state.tool_indices.len();
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    state.tool_indices.insert(item_id.to_string(), index);
                }
                state.saw_tool_call = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"tool_calls": [{
                        "index": index,
                        "id": call_id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }]}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                state.output_chars = state.output_chars.saturating_add(delta.chars().count());
                push_preview_text(&mut state.text, delta);
                if state.first_token_ms.is_none() {
                    state.first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let delta_json = if state.sent_role {
                    json!({"content": delta})
                } else {
                    state.sent_role = true;
                    json!({"role": "assistant", "content": delta})
                };
                let chunk = openai_stream_chunk(message_id, model, delta_json, None, None);
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                let index = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .and_then(|item_id| state.tool_indices.get(item_id).copied())
                    .unwrap_or(0);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"tool_calls": [{
                        "index": index,
                        "function": {"arguments": delta}
                    }]}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
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
                state.prompt_tokens = usage.prompt_tokens;
                state.completion_tokens = usage.completion_tokens;
                state.cache_read_tokens = usage.cache_read_tokens;
                state.cache_write_tokens = usage.cache_write_tokens;
            }
            if event == "response.incomplete" {
                state.stop_reason = Some("length".to_string());
            } else if let Some(details) = response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
            {
                state.stop_reason = Some(details.to_string());
            }
        }
        _ => {}
    }
}
