use super::*;

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
            heartbeat.observe_first_token(first_token_ms);
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
        heartbeat.observe_first_token(first_token_ms);

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
            heartbeat.observe_first_token(first_token_ms);
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
        heartbeat.observe_first_token(first_token_ms);

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
