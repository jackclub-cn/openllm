use super::*;

/// Collects Anthropic text from either a bare string or an array of blocks.
pub(crate) fn anthropic_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// Converts one Anthropic image block into an OpenAI `image_url` part.
pub(crate) fn anthropic_image_to_openai(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media_type};base64,{data}")
        }
        Some("url") => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

/// Converts one non-`tool_result` Anthropic content block into an OpenAI part.
pub(crate) fn anthropic_block_to_openai_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("image") => anthropic_image_to_openai(block),
        _ => None,
    }
}

pub(crate) fn anthropic_tool_choice_to_openai(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("tool") => Some(json!({
            "type": "function",
            "function": {"name": choice.get("name").cloned().unwrap_or(Value::Null)}
        })),
        _ => None,
    }
}

/// Converts an inbound Anthropic Messages body into the OpenAI chat shape that
/// the rest of the gateway (routing, barrel clamping, forwarding) understands.
pub(crate) fn anthropic_request_to_openai(input: &Value, model: &str) -> Value {
    let mut messages = Vec::new();
    // Anthropic carries the system prompt outside `messages`.
    if let Some(system) = input.get("system")
        && let Some(text) = anthropic_text(system)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            anthropic_message_to_openai(role, &content, &mut messages);
        }
    }

    let mut output = json!({"model": model, "messages": messages});
    for (source, target) in [
        ("max_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(stop) = input.get("stop_sequences") {
        output["stop"] = stop.clone();
    }
    if input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        output["stream"] = json!(true);
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                    }
                }))
            })
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            output["tools"] = json!(converted);
        }
    }
    if let Some(choice) = input.get("tool_choice")
        && let Some(mapped) = anthropic_tool_choice_to_openai(choice)
    {
        output["tool_choice"] = mapped;
    }
    output
}

/// Appends the OpenAI messages equivalent to one Anthropic message.
///
/// A single Anthropic user turn may mix text and `tool_result` blocks. Only the
/// latter become `role: tool` messages, and they are emitted in place so they
/// still follow the assistant turn whose `tool_calls` they answer.
pub(crate) fn anthropic_message_to_openai(role: &str, content: &Value, messages: &mut Vec<Value>) {
    if let Some(text) = content.as_str() {
        messages.push(json!({"role": role, "content": text}));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };

    if role == "assistant" {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(
                            block.get("input").unwrap_or(&json!({}))
                        ).unwrap_or_else(|_| "{}".to_string())
                    }
                })),
                _ => {}
            }
        }
        let mut message = json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { json!(text) }
        });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        messages.push(message);
        return;
    }

    // User (and any other) turn: split tool results out into their own messages.
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                "content": anthropic_text(block.get("content").unwrap_or(&Value::Null))
                    .unwrap_or_default()
            }));
        } else if let Some(part) = anthropic_block_to_openai_part(block) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return;
    }
    // Collapse text-only content to a plain string: most providers (and some
    // strict validators) handle that more reliably than a one-element array.
    if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        messages.push(json!({"role": role, "content": text}));
    } else {
        messages.push(json!({"role": role, "content": parts}));
    }
}

/// Converts a non-streamed OpenAI completion into an Anthropic message.
pub(crate) fn openai_response_to_anthropic(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut content = Vec::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").cloned().unwrap_or_else(
                    || json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))
                ),
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "input": input
            }));
        }
    }
    let has_tool_use = content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    let finish = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("msg_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple()));
    (
        json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": requested_model,
            "content": content,
            "stop_reason": anthropic_stop_reason(finish, has_tool_use),
            "stop_sequence": Value::Null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        }),
        usage,
    )
}

/// Converts a Responses object into an Anthropic message.
///
/// Responses-first OpenAI upstreams serve Anthropic clients by normalising the
/// payload through the chat shape the Anthropic converter already understands.
pub(crate) fn responses_response_to_anthropic(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = responses_response_to_chat(value, requested_model);
    let (anthropic, _) = openai_response_to_anthropic(&chat, requested_model);
    (anthropic, usage)
}

/// Accumulates state while rewriting an OpenAI SSE stream into Anthropic events.
#[derive(Default)]
pub(crate) struct AnthropicStreamState {
    pub(crate) started: bool,
    pub(crate) text_index: Option<usize>,
    pub(crate) open_tools: Vec<usize>,
    pub(crate) tool_indices: std::collections::HashMap<i64, usize>,
    pub(crate) next_index: usize,
    pub(crate) has_tool_use: bool,
    pub(crate) finish_reason: Option<String>,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) text: String,
    pub(crate) first_token_ms: Option<i64>,
    pub(crate) response_terminated: bool,
    pub(crate) response_error: Option<String>,
}

pub(crate) async fn send_anthropic_event(
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
    event: &str,
    data: Value,
) {
    let _ = tx.send(Ok(Bytes::from(sse_line(event, data)))).await;
}

impl AnthropicStreamState {
    pub(crate) fn mark_first_token(&mut self, started: Instant) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(started.elapsed().as_millis() as i64);
        }
    }

    /// Emits `message_start` once, before any content block.
    pub(crate) async fn ensure_started(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
        ctx: &StreamContext,
    ) {
        if self.started {
            return;
        }
        self.started = true;
        send_anthropic_event(
            tx,
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": ctx.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": ctx.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {"input_tokens": ctx.input_tokens, "output_tokens": 0}
                }
            }),
        )
        .await;
    }

    /// Closes the open text block so a tool block can start.
    pub(crate) async fn close_text_block(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        if let Some(index) = self.text_index.take() {
            send_anthropic_event(
                tx,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            )
            .await;
        }
    }

    /// Closes every still-open block at end of stream.
    pub(crate) async fn close_all_blocks(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        self.close_text_block(tx).await;
        for index in self.open_tools.drain(..) {
            send_anthropic_event(
                tx,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            )
            .await;
        }
    }
}

/// Immutable per-stream values shared with the state machine.
pub(crate) struct StreamContext {
    pub(crate) message_id: String,
    pub(crate) model: String,
    pub(crate) input_tokens: i64,
    pub(crate) started: Instant,
}

/// Handles one OpenAI SSE line, emitting the matching Anthropic events.
///
/// Returns `false` once `[DONE]` is seen so the caller can stop reading.
pub(crate) async fn process_openai_line_for_anthropic(
    line: &[u8],
    state: &mut AnthropicStreamState,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) -> bool {
    let line = String::from_utf8_lossy(line);
    let Some(data) = line.trim().strip_prefix("data:") else {
        return true;
    };
    let data = data.trim();
    if data == "[DONE]" {
        return false;
    }
    if data.is_empty() {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return true;
    };

    if let Some(usage) = value.get("usage").filter(|value| !value.is_null()) {
        if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_i64) {
            state.input_tokens = prompt;
        }
        if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_i64) {
            state.output_tokens = completion;
        }
        let cache_read = cache_read_of(usage);
        if cache_read > 0 {
            state.cache_read_tokens = cache_read;
        }
        let cache_write = cache_write_of(usage);
        if cache_write > 0 {
            state.cache_write_tokens = cache_write;
        }
    }
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        state.finish_reason = Some(reason.to_string());
    }

    let delta = value.pointer("/choices/0/delta");
    if let Some(text) = delta
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
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
        state.text.push_str(text);
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

    if let Some(calls) = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let openai_index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
            let function = call.get("function").unwrap_or(call);
            let index = if let Some(index) = state.tool_indices.get(&openai_index) {
                *index
            } else {
                // A new tool call: text must not stay open alongside it.
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                state.close_text_block(tx).await;
                let index = state.next_index;
                state.next_index += 1;
                state.tool_indices.insert(openai_index, index);
                state.open_tools.push(index);
                state.has_tool_use = true;
                send_anthropic_event(
                    tx,
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": call.get("id").cloned().unwrap_or_else(
                                || json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))
                            ),
                            "name": function.get("name").cloned().unwrap_or(Value::Null),
                            "input": {}
                        }
                    }),
                )
                .await;
                index
            };
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                state.mark_first_token(ctx.started);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": arguments}
                    }),
                )
                .await;
            }
        }
    }
    true
}

/// Finalises an Anthropic stream: close open blocks, then emit the terminal
/// `message_delta` and `message_stop` events.
///
/// Shared by the live handler and tests so the closing sequence is defined once.
pub(crate) async fn finish_anthropic_stream(
    state: &mut AnthropicStreamState,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    // A stream that produced nothing still owes the client a well-formed
    // message, so open the message before closing out.
    if !state.started {
        state.ensure_started(tx, ctx).await;
    }
    state.close_all_blocks(tx).await;
    let stop_reason = anthropic_stop_reason(state.finish_reason.as_deref(), state.has_tool_use);
    send_anthropic_event(
        tx,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
            "usage": {"output_tokens": state.output_tokens}
        }),
    )
    .await;
    send_anthropic_event(tx, "message_stop", json!({"type": "message_stop"})).await;
}

/// Rewrites an OpenAI SSE stream into the Anthropic event protocol.
#[allow(clippy::too_many_arguments)]
pub(crate) fn openai_stream_to_anthropic(
    state: AppState,
    response: reqwest::Response,
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
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut stream_error = None;
        let mut done = false;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while !done && let Some(chunk) = upstream.next().await {
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
                // `false` means `[DONE]`: the upstream body is complete.
                if !process_openai_line_for_anthropic(&line, &mut stream_state, &context, &tx).await
                {
                    // `[DONE]` received: stop reading the upstream body.
                    done = true;
                    break;
                }
            }
        }
        if !done && !buffer.is_empty() {
            process_openai_line_for_anthropic(&buffer, &mut stream_state, &context, &tx).await;
        }

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
    response: reqwest::Response,
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
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
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
        }
        if !buffer.is_empty() {
            process_responses_line_for_anthropic(
                &buffer,
                &mut event_name,
                &mut stream_state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
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
            (stream_state.text.chars().count() / 4) as i64
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
                state.text.push_str(text);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn passthrough_stream_response(
    state: AppState,
    response: reqwest::Response,
    content_type: String,
    endpoint: String,
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
        let mut upstream = response.bytes_stream();
        let mut parser = UsageParser::new(started);
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    heartbeat.touch().await;
                    parser.push(&bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    let _ = tx.send(Err(io::Error::other(error))).await;
                    break;
                }
            }
        }
        let usage = parser
            .finish()
            .map(|mut usage| {
                if usage.prompt_tokens == 0 {
                    usage.prompt_tokens = request_tokens;
                }
                usage.normalized()
            })
            .unwrap_or_else(|| Usage {
                prompt_tokens: request_tokens,
                completion_tokens: 1,
                total_tokens: request_tokens + 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = parser
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = parser.preview();
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: &endpoint,
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
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn anthropic_stream_response(
    state: AppState,
    response: reqwest::Response,
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
        let mut upstream = response.bytes_stream();
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

        while let Some(chunk) = upstream.next().await {
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

pub(crate) fn convert_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let mut output = json!({
        "model": model,
        "max_tokens": requested_output_tokens_of(input).unwrap_or(4096),
        "stream": streamed
    });

    let mut system = Vec::new();
    let mut messages = Vec::new();
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content) {
                    system.push(text);
                }
                continue;
            }

            // A tool result carries its association through `tool_call_id`.
            // Anthropic expects a user message holding a `tool_result` block.
            if role == "tool" {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_text(&content).unwrap_or_default()
                    }]
                }));
                continue;
            }

            let anthropic_role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            let mut blocks = convert_anthropic_content(&content);
            // An assistant turn may request tools; Anthropic represents those
            // as `tool_use` blocks alongside any text.
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
                && let Some(array) = blocks.as_array_mut()
            {
                // An assistant turn that only calls tools has no text content;
                // drop the placeholder empty block so Anthropic does not reject it.
                array.retain(|block| {
                    block.get("type").and_then(Value::as_str) != Some("text")
                        || block
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty())
                });
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    let Ok(arguments) = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .parse::<Value>()
                    else {
                        continue;
                    };
                    array.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "input": arguments
                    }));
                }
            }
            messages.push(json!({
                "role": anthropic_role,
                "content": blocks
            }));
        }
    }

    if !system.is_empty() {
        output["system"] = json!(system.join("\n\n"));
    }
    output["messages"] = json!(messages);

    if let Some(temperature) = input.get("temperature") {
        output["temperature"] = temperature.clone();
    }
    if let Some(top_p) = input.get("top_p") {
        output["top_p"] = top_p.clone();
    }
    if let Some(stop) = input.get("stop") {
        output["stop_sequences"] = match stop {
            Value::String(value) => json!([value]),
            Value::Array(_) => stop.clone(),
            _ => Value::Null,
        };
    }

    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let anthropic_tools = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function").unwrap_or(tool);
                let name = function.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": function.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": function.get("parameters").cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                }))
            })
            .collect::<Vec<_>>();
        if !anthropic_tools.is_empty() {
            output["tools"] = json!(anthropic_tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::String(value) if value == "required" => json!({"type": "any"}),
            Value::String(value) if value == "auto" || value == "none" => {
                json!({"type": value})
            }
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "tool", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

/// Converts an OpenAI Responses request into the Anthropic Messages shape.
///
/// Responses is item-oriented while Messages is message-oriented, so the
/// conversion first normalizes the input into the chat shape that
/// `convert_request_to_anthropic` already knows how to handle.
pub(crate) fn responses_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let chat = responses_request_to_chat(input, model, streamed);
    convert_request_to_anthropic(&chat, model, streamed)
}

pub(crate) fn completions_request_to_anthropic(
    input: &Value,
    model: &str,
    streamed: bool,
) -> Value {
    let chat = completions_request_to_chat(input, model, streamed);
    convert_request_to_anthropic(&chat, model, streamed)
}

pub(crate) fn convert_anthropic_content(content: &Value) -> Value {
    match content {
        Value::String(text) => json!([{"type": "text", "text": text}]),
        Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = item.pointer("/image_url/url").and_then(Value::as_str) {
                            if let Some((meta, data)) = url.split_once(',') {
                                let media_type = meta
                                    .strip_prefix("data:")
                                    .and_then(|value| value.split(';').next())
                                    .unwrap_or("image/png");
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data
                                    }
                                }));
                            } else {
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {"type": "url", "url": url}
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if blocks.is_empty() {
                json!([{"type": "text", "text": ""}])
            } else {
                json!(blocks)
            }
        }
        _ => json!([{"type": "text", "text": ""}]),
    }
}

pub(crate) fn convert_anthropic_response(value: &Value) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = String::new();
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    if let Some(part) = block.get("thinking").and_then(Value::as_str) {
                        reasoning.push_str(part);
                    }
                }
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "id": block.get("id").cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": serde_json::to_string(
                                block.get("input").unwrap_or(&Value::Null)
                            ).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let finish_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
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

    // Reuse the shared reader so cache counters (`cache_read_input_tokens`,
    // `cache_creation_input_tokens`) are captured for native Anthropic too.
    let usage = value
        .get("usage")
        .map(|usage| {
            Usage {
                prompt_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                completion_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                total_tokens: 0,
                cache_read_tokens: cache_read_of(usage),
                cache_write_tokens: cache_write_of(usage),
            }
            .normalized()
        })
        .unwrap_or_default();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let model = value.get("model").cloned().unwrap_or(Value::Null);

    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
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

pub(crate) fn anthropic_response_to_responses(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let (response, _) = chat_response_to_responses(&chat, requested_model);
    (response, usage)
}

pub(crate) fn anthropic_response_to_completions(
    value: &Value,
    requested_model: &str,
) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let (completions, _) = chat_response_to_completions(&chat, requested_model);
    (completions, usage)
}
