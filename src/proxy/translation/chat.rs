use super::*;

pub(crate) async fn process_chat_chunk_line(
    stream: &mut ResponsesStreamState,
    line: &[u8],
    tool_index: &mut Option<i64>,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
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
    stream.ensure_created(tx).await;

    if value.get("usage").is_some_and(|usage| !usage.is_null())
        && let Some(usage) = usage_from_value(&value)
    {
        stream.usage = usage;
    }

    let Some(choice) = value.pointer("/choices/0") else {
        return;
    };
    let delta = choice.get("delta").unwrap_or(&Value::Null);

    if let Some(reasoning) = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        if !matches!(stream.current, Some(ResponsesStreamBlock::Reasoning { .. })) {
            stream.finish_current(tx).await;
            let item_id = format!("rs_{}", uuid::Uuid::new_v4().simple());
            let output_index = stream.output.len();
            stream.current = Some(ResponsesStreamBlock::Reasoning {
                item_id: item_id.clone(),
                output_index,
                text: String::new(),
            });
            stream
                .send_event(
                    tx,
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "reasoning",
                            "status": "in_progress",
                            "summary": []
                        }
                    }),
                )
                .await;
            stream
                .send_event(
                    tx,
                    "response.reasoning_summary_part.added",
                    json!({
                        "type": "response.reasoning_summary_part.added",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "part": {"type": "summary_text", "text": ""}
                    }),
                )
                .await;
        }
        let (item_id, output_index) = match stream.current.as_ref() {
            Some(ResponsesStreamBlock::Reasoning {
                item_id,
                output_index,
                ..
            }) => (item_id.clone(), *output_index),
            _ => return,
        };
        if let Some(ResponsesStreamBlock::Reasoning { text, .. }) = stream.current.as_mut() {
            text.push_str(reasoning);
        }
        stream.output_chars += reasoning.chars().count();
        stream.mark_first_token();
        stream
            .send_event(
                tx,
                "response.reasoning_summary_text.delta",
                json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "delta": reasoning
                }),
            )
            .await;
    }

    if let Some(content) = delta.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        if !matches!(stream.current, Some(ResponsesStreamBlock::Text { .. })) {
            stream.finish_current(tx).await;
            let item_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
            let output_index = stream.output.len();
            stream.current = Some(ResponsesStreamBlock::Text {
                item_id: item_id.clone(),
                output_index,
                text: String::new(),
            });
            stream
                .send_event(
                    tx,
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        }
                    }),
                )
                .await;
            stream
                .send_event(
                    tx,
                    "response.content_part.added",
                    json!({
                        "type": "response.content_part.added",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": {
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                            "logprobs": []
                        }
                    }),
                )
                .await;
        }
        let (item_id, output_index) = match stream.current.as_ref() {
            Some(ResponsesStreamBlock::Text {
                item_id,
                output_index,
                ..
            }) => (item_id.clone(), *output_index),
            _ => return,
        };
        if let Some(ResponsesStreamBlock::Text { text, .. }) = stream.current.as_mut() {
            text.push_str(content);
        }
        stream.output_chars += content.chars().count();
        stream.mark_first_token();
        stream
            .send_event(
                tx,
                "response.output_text.delta",
                json!({
                    "type": "response.output_text.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "delta": content
                }),
            )
            .await;
    }

    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
            let start_new = !matches!(
                (&stream.current, *tool_index),
                (Some(ResponsesStreamBlock::Tool { .. }), Some(current)) if current == index
            );
            if start_new {
                stream.finish_current(tx).await;
                let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
                let output_index = stream.output.len();
                let call_id = call
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple())));
                let name = call
                    .pointer("/function/name")
                    .cloned()
                    .unwrap_or(Value::Null);
                stream.current = Some(ResponsesStreamBlock::Tool {
                    item_id: item_id.clone(),
                    output_index,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                *tool_index = Some(index);
                stream
                    .send_event(
                        tx,
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {
                                "id": item_id,
                                "type": "function_call",
                                "status": "in_progress",
                                "call_id": call_id,
                                "name": name,
                                "arguments": ""
                            }
                        }),
                    )
                    .await;
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                let (item_id, output_index) = match stream.current.as_ref() {
                    Some(ResponsesStreamBlock::Tool {
                        item_id,
                        output_index,
                        ..
                    }) => (item_id.clone(), *output_index),
                    _ => continue,
                };
                if let Some(ResponsesStreamBlock::Tool { arguments: acc, .. }) =
                    stream.current.as_mut()
                {
                    acc.push_str(arguments);
                }
                stream.output_chars += arguments.chars().count();
                stream.mark_first_token();
                stream
                    .send_event(
                        tx,
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": arguments
                        }),
                    )
                    .await;
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        stream.stop_reason = Some(if reason == "length" {
            "max_tokens".to_string()
        } else {
            reason.to_string()
        });
    }
}

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
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut chat_state = ChatStreamState::default();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx).await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
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
        }
        if !buffer.is_empty() {
            process_responses_line_for_chat(
                &buffer,
                &mut event_name,
                &mut chat_state,
                &message_id,
                &requested_model,
                started,
                &tx,
            )
            .await;
        }

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
            (chat_state.text.chars().count() / 4) as i64
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
    pub(crate) text: String,
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
        "response.reasoning_summary_text.delta" => {
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
                state.text.push_str(delta);
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

/// Streams a legacy `/v1/completions` response from a chat-completions upstream
/// chunk stream, used when the upstream has no native completions endpoint.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chat_stream_to_completions(
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
        let completion_id = format!("cmpl-{}", uuid::Uuid::new_v4().simple());
        let mut buffer = Vec::<u8>::new();
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut finish_reason: Option<String> = None;
        let mut first_token_ms = None;
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx).await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_chat_line_for_completions(
                    &line,
                    &mut text,
                    &mut usage,
                    &mut finish_reason,
                    &mut first_token_ms,
                    started,
                    &completion_id,
                    &requested_model,
                    &tx,
                )
                .await;
            }
        }
        if !buffer.is_empty() {
            process_chat_line_for_completions(
                &buffer,
                &mut text,
                &mut usage,
                &mut finish_reason,
                &mut first_token_ms,
                started,
                &completion_id,
                &requested_model,
                &tx,
            )
            .await;
        }

        if stream_error.is_none() {
            let final_chunk = completions_stream_chunk(
                &completion_id,
                &requested_model,
                "",
                Some(finish_reason.as_deref().unwrap_or("stop")),
            );
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&final_chunk).unwrap_or_default()
                ))))
                .await;
        }
        drop(tx);

        if usage.prompt_tokens == 0 {
            usage.prompt_tokens = request_tokens;
        }
        if usage.completion_tokens == 0 {
            usage.completion_tokens = (text.chars().count() / 4) as i64;
        }
        let usage = usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms =
            first_token_ms.or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_COMPLETIONS,
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
pub(crate) async fn process_chat_line_for_completions(
    line: &[u8],
    text: &mut String,
    usage: &mut Usage,
    finish_reason: &mut Option<String>,
    first_token_ms: &mut Option<i64>,
    started: Instant,
    completion_id: &str,
    model: &str,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
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
    if let Some(parsed) = usage_from_value(&value) {
        *usage = parsed;
    }
    let Some(choice) = value.pointer("/choices/0") else {
        return;
    };
    if let Some(delta) = choice.pointer("/delta/content").and_then(Value::as_str)
        && !delta.is_empty()
    {
        text.push_str(delta);
        if first_token_ms.is_none() {
            *first_token_ms = Some(started.elapsed().as_millis() as i64);
        }
        let chunk = completions_stream_chunk(completion_id, model, delta, None);
        let _ = tx
            .send(Ok(Bytes::from(format!(
                "data: {}\n\n",
                serde_json::to_string(&chunk).unwrap_or_default()
            ))))
            .await;
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        *finish_reason = Some(reason.to_string());
    }
}

pub(crate) fn completions_stream_chunk(
    id: &str,
    model: &str,
    text: &str,
    finish_reason: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "object": "text_completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "text": text,
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": finish_reason
        }]
    })
}

/// Streams a legacy `/v1/completions` response from an Anthropic Messages SSE
/// stream. Only text deltas map across; tool use has no legacy equivalent and
/// is dropped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn anthropic_stream_to_completions(
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
        let completion_id = format!("cmpl-{}", uuid::Uuid::new_v4().simple());
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut text = String::new();
        let mut stop_reason: Option<String> = None;
        let mut first_token_ms = None;
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx).await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_completions_anthropic_line(
                    &line,
                    &mut event_name,
                    &mut usage,
                    &mut text,
                    &mut stop_reason,
                    &mut first_token_ms,
                    started,
                    &completion_id,
                    &requested_model,
                    &tx,
                )
                .await;
            }
        }
        if !buffer.is_empty() {
            process_completions_anthropic_line(
                &buffer,
                &mut event_name,
                &mut usage,
                &mut text,
                &mut stop_reason,
                &mut first_token_ms,
                started,
                &completion_id,
                &requested_model,
                &tx,
            )
            .await;
        }

        let finish_reason = if stop_reason.as_deref() == Some("max_tokens") {
            "length"
        } else {
            "stop"
        };
        if stream_error.is_none() {
            let final_chunk =
                completions_stream_chunk(&completion_id, &requested_model, "", Some(finish_reason));
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&final_chunk).unwrap_or_default()
                ))))
                .await;
        }
        drop(tx);

        if usage.prompt_tokens == 0 {
            usage.prompt_tokens = request_tokens;
        }
        if usage.completion_tokens == 0 {
            usage.completion_tokens = (text.chars().count() / 4) as i64;
        }
        let usage = usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms =
            first_token_ms.or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_COMPLETIONS,
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
pub(crate) async fn process_completions_anthropic_line(
    line: &[u8],
    event_name: &mut String,
    usage: &mut Usage,
    text: &mut String,
    stop_reason: &mut Option<String>,
    first_token_ms: &mut Option<i64>,
    started: Instant,
    completion_id: &str,
    model: &str,
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

    match event_name.as_str() {
        "message_start" => {
            if let Some(message_usage) = value.pointer("/message/usage") {
                if let Some(input_tokens) =
                    message_usage.get("input_tokens").and_then(Value::as_i64)
                {
                    usage.prompt_tokens = input_tokens;
                }
                usage.cache_read_tokens = cache_read_of(message_usage);
                usage.cache_write_tokens = cache_write_of(message_usage);
            }
        }
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            if delta.get("type").and_then(Value::as_str) == Some("text_delta")
                && let Some(part) = delta.get("text").and_then(Value::as_str)
            {
                text.push_str(part);
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let chunk = completions_stream_chunk(completion_id, model, part, None);
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "message_delta" => {
            if let Some(output_tokens) = value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_i64)
            {
                usage.completion_tokens = output_tokens;
            }
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                *stop_reason = Some(reason.to_string());
            }
        }
        _ => {}
    }
}

pub(crate) fn responses_request_to_chat(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(instructions) = input.get("instructions")
        && let Some(text) = content_text(instructions)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    append_responses_input(&mut messages, input.get("input"));

    let mut output = json!({"model": model, "messages": messages});
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("parallel_tool_calls", "parallel_tool_calls"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(effort) = input.pointer("/reasoning/effort") {
        output["reasoning_effort"] = effort.clone();
    }
    if let Some(format) = input.pointer("/text/format")
        && let Some(response_format) = responses_format_to_chat(format)
    {
        output["response_format"] = response_format;
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(responses_tool_to_chat)
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            output["tools"] = json!(tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "function", "function": {"name": name}})
            }
            _ => choice.clone(),
        };
    }
    output
}

/// Maps a Responses `text.format` onto the chat-completions `response_format`.
pub(crate) fn responses_format_to_chat(format: &Value) -> Option<Value> {
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => Some(json!({"type": "json_object"})),
        Some("json_schema") => {
            let mut inner = format.clone();
            if let Some(object) = inner.as_object_mut() {
                object.remove("type");
            }
            Some(json!({"type": "json_schema", "json_schema": inner}))
        }
        _ => None,
    }
}

pub(crate) fn append_responses_input(messages: &mut Vec<Value>, input: Option<&Value>) {
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let call = json!({
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned()
                                .unwrap_or(Value::Null),
                            "type": "function",
                            "function": {
                                "name": item.get("name").cloned().unwrap_or(Value::Null),
                                "arguments": item.get("arguments").cloned()
                                    .unwrap_or_else(|| json!("{}"))
                            }
                        });
                        if let Some(last) = messages.last_mut()
                            && last.get("role").and_then(Value::as_str) == Some("assistant")
                        {
                            let calls = last
                                .as_object_mut()
                                .and_then(|object| object.get_mut("tool_calls"))
                                .and_then(Value::as_array_mut);
                            if let Some(calls) = calls {
                                calls.push(call);
                                continue;
                            }
                        }
                        messages.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [call]
                        }));
                    }
                    Some("function_call_output") => {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                            "content": item.get("output").cloned().unwrap_or(Value::Null)
                        }));
                    }
                    _ => {
                        let Some(role) = item.get("role").and_then(Value::as_str) else {
                            continue;
                        };
                        let content =
                            responses_content_to_chat(item.get("content").unwrap_or(&Value::Null));
                        messages.push(json!({"role": role, "content": content}));
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn responses_content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(_) | Value::Null => content.clone(),
        Value::Array(items) => json!(
            items
                .iter()
                .filter_map(responses_content_part_to_chat)
                .collect::<Vec<_>>()
        ),
        _ => content.clone(),
    }
}

pub(crate) fn responses_content_part_to_chat(item: &Value) -> Option<Value> {
    match item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text" | "text") => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("input_image") => {
            let url = item
                .get("image_url")
                .or_else(|| item.get("url"))
                .and_then(Value::as_str)?;
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        }
        Some("image_url") => Some(item.clone()),
        _ => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
    }
}

pub(crate) fn responses_tool_to_chat(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    Some(json!({
        "type": "function",
        "function": {
            "name": tool.get("name")?.clone(),
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": tool.get("parameters").cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
        }
    }))
}

/// Converts a Responses object into a chat-completions response.
pub(crate) fn responses_response_to_chat(value: &Value, requested_model: &str) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = String::new();
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(part) = part.get("text").and_then(Value::as_str) {
                                reasoning.push_str(part);
                            }
                        }
                    }
                }
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(part) = part.get("text").and_then(Value::as_str) {
                                text.push_str(part);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(json!({
                        "id": item.get("call_id").or_else(|| item.get("id"))
                            .cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": item.get("arguments").cloned()
                                .unwrap_or_else(|| json!("{}"))
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let incomplete = value.get("status").and_then(Value::as_str) == Some("incomplete");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if incomplete {
        "length"
    } else {
        "stop"
    };
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { json!(text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }

    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("chatcmpl-{}", id.trim_start_matches("resp_")))
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let created = value
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": created,
            "model": requested_model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}

/// Converts a legacy `/v1/completions` request into the chat-completions shape
/// so OpenAI-family upstreams can serve it through their chat endpoint.
pub(crate) fn completions_request_to_chat(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(prompt) = completions_prompt_text(input.get("prompt")) {
        messages.push(json!({"role": "user", "content": prompt}));
    }

    let mut chat = json!({"model": model, "messages": messages});
    if streamed {
        chat["stream"] = json!(true);
    }
    for key in [
        "max_tokens",
        "temperature",
        "top_p",
        "stop",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "response_format",
    ] {
        if let Some(value) = input.get(key) {
            chat[key] = value.clone();
        }
    }
    chat
}

/// Flattens the legacy `prompt` field into a single string. OpenAI allows a
/// plain string, an array of strings, or token arrays; token arrays cannot be
/// decoded without the provider's tokenizer, so they are skipped.
pub(crate) fn completions_prompt_text(prompt: Option<&Value>) -> Option<String> {
    match prompt {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let mut text = String::new();
            for item in items {
                match item {
                    Value::String(part) => text.push_str(part),
                    Value::Array(parts) => {
                        for part in parts {
                            if let Some(part) = part.as_str() {
                                text.push_str(part);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(text)
        }
        _ => None,
    }
}

/// Converts a chat completion into the legacy `/v1/completions` shape.
pub(crate) fn chat_response_to_completions(value: &Value, requested_model: &str) -> (Value, Usage) {
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| {
            format!(
                "cmpl-{}",
                id.trim_start_matches("msg_")
                    .trim_start_matches("chatcmpl-")
            )
        })
        .unwrap_or_else(|| format!("cmpl-{}", uuid::Uuid::new_v4().simple()));
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    (
        json!({
            "id": id,
            "object": "text_completion",
            "created": chrono::Utc::now().timestamp(),
            "model": requested_model,
            "choices": [{
                "text": text,
                "index": 0,
                "logprobs": Value::Null,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}
