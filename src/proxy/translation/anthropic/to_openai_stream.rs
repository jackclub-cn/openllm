use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn anthropic_stream_response(
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
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut sent_role = false;
        let mut saw_tool_use = false;
        // Anthropic numbers every content block (text and tools alike), while
        // OpenAI numbers only tool calls. Track the mapping so a text block
        // before a tool call does not shift the tool index.
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut stream_error = None;
        let mut text = String::new();
        let mut first_token_ms = None;
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
                process_anthropic_line(
                    &line,
                    &mut event_name,
                    &mut usage,
                    &mut output_chars,
                    &mut text,
                    &mut sent_role,
                    &mut saw_tool_use,
                    &mut next_tool_index,
                    &mut tool_indices,
                    &mut first_token_ms,
                    started,
                    &message_id,
                    &requested_model,
                    &tx,
                )
                .await;
            }
        }

        if !buffer.is_empty() {
            process_anthropic_line(
                &buffer,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                started,
                &message_id,
                &requested_model,
                &tx,
            )
            .await;
        }

        // Prefer the real token counts Anthropic reported (message_start /
        // message_delta) and only fall back to our estimate when absent.
        let prompt_tokens = if usage.prompt_tokens > 0 {
            usage.prompt_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if usage.completion_tokens > 0 {
            usage.completion_tokens
        } else {
            (output_chars / 4) as i64
        };
        usage.prompt_tokens = prompt_tokens;
        usage.completion_tokens = completion_tokens;
        usage.total_tokens = prompt_tokens + completion_tokens;

        // Emit the terminal choice first so clients observe `finish_reason`
        // (`tool_calls` triggers tool execution in the OpenAI SDKs), then send
        // usage in a separate chunk — a usage-bearing chunk carries no choices
        // and would otherwise swallow the finish reason entirely.
        let final_chunk = openai_stream_chunk(
            &message_id,
            &requested_model,
            json!({}),
            Some(if saw_tool_use { "tool_calls" } else { "stop" }),
            None,
        );
        let usage_chunk = openai_stream_chunk(
            &message_id,
            &requested_model,
            json!({}),
            None,
            Some(json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": usage.total_tokens
            })),
        );
        let _ = tx
            .send(Ok(Bytes::from(format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                serde_json::to_string(&final_chunk).unwrap_or_default(),
                serde_json::to_string(&usage_chunk).unwrap_or_default()
            ))))
            .await;
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
                endpoint: OPENAI_CHAT_COMPLETIONS,
                usage: usage.normalized(),
                latency_ms,
                first_token_ms,
                status_code: 200,
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
pub(crate) async fn process_anthropic_line(
    line: &[u8],
    event_name: &mut String,
    usage: &mut Usage,
    output_chars: &mut usize,
    text_acc: &mut String,
    sent_role: &mut bool,
    saw_tool_use: &mut bool,
    next_tool_index: &mut usize,
    tool_indices: &mut std::collections::HashMap<i64, usize>,
    first_token_ms: &mut Option<i64>,
    started: Instant,
    message_id: &str,
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
                // Anthropic reports cache traffic on the initial message too.
                usage.cache_read_tokens = cache_read_of(message_usage);
                usage.cache_write_tokens = cache_write_of(message_usage);
            }
            if !*sent_role {
                *sent_role = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"role": "assistant", "content": ""}),
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
        "content_block_delta" => {
            if let Some(delta_text) = value.pointer("/delta/text").and_then(Value::as_str) {
                if first_token_ms.is_none() && !delta_text.is_empty() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                *output_chars += delta_text.chars().count();
                push_preview_text(text_acc, delta_text);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"content": delta_text}),
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
            if let Some(thinking) = value.pointer("/delta/thinking").and_then(Value::as_str)
                && !thinking.is_empty()
            {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                *output_chars += thinking.chars().count();
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"reasoning_content": thinking}),
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
            // Tool call arguments stream as JSON fragments; OpenAI expects
            // them accumulated under `tool_calls[].function.arguments`.
            if value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta")
                && let Some(partial) = value.pointer("/delta/partial_json").and_then(Value::as_str)
            {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let block_index = value.get("index").and_then(Value::as_i64).unwrap_or(0);
                let index = *tool_indices.get(&block_index).unwrap_or(&0);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "function": { "arguments": partial }
                        }]
                    }),
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
        "content_block_start" => {
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let id = value
                    .pointer("/content_block/id")
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = value
                    .pointer("/content_block/name")
                    .cloned()
                    .unwrap_or(Value::Null);
                // Parallel tool calls arrive as separate content blocks; the
                // OpenAI `index` must distinguish them or clients merge them.
                let block_index = value.get("index").and_then(Value::as_i64).unwrap_or(0);
                let index = *next_tool_index;
                *next_tool_index += 1;
                tool_indices.insert(block_index, index);
                *saw_tool_use = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" }
                        }]
                    }),
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
        "message_delta" => {
            if let Some(output_tokens) = value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_i64)
            {
                usage.completion_tokens = output_tokens;
            }
            // `message_delta` may also repeat cache counters; keep them in sync.
            if let Some(delta_usage) = value.get("usage") {
                let cache_read = cache_read_of(delta_usage);
                if cache_read > 0 {
                    usage.cache_read_tokens = cache_read;
                }
                let cache_write = cache_write_of(delta_usage);
                if cache_write > 0 {
                    usage.cache_write_tokens = cache_write;
                }
            }
        }
        _ => {}
    }
}
