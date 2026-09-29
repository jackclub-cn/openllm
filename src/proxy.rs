use std::io;
use std::str::FromStr;
use std::time::Instant;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use globset::Glob;
use rand::distributions::{Distribution, WeightedIndex};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{AppError, AppResult};
use crate::models::{
    ApiKeyRecord, ModelList, ProviderType, PublicModel, Route, RouteStrategy, RouteTarget, Usage,
};
use crate::registry::BarrelEnvelope;
use crate::state::AppState;

const OPENAI_CHAT_COMPLETIONS: &str = "/v1/chat/completions";

pub async fn public_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ModelList>> {
    authenticate_gateway(&state, &headers).await?;
    let routes = crate::registry::route_models(&state.pool).await?;
    let synced = crate::registry::synced_models(&state.pool).await?;
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    // A route pattern can coincide with a synced model id (for example an
    // exact-match route for a prefixed model). Explicit routes win because they
    // carry the barrel intersection across their targets, while deduping keeps
    // clients from seeing the same id twice.
    let mut by_id = std::collections::BTreeMap::new();
    for model in routes {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created,
                owned_by: "openllm",
                provider: None,
                upstream_model: None,
                capabilities: model.capabilities,
                target_count: Some(model.target_count),
                limits_verified: Some(!model.incomplete),
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
            }
            .with_flat_limits(),
        );
    }
    for model in synced {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created,
                owned_by: "openllm",
                provider: Some(model.provider_name),
                upstream_model: Some(model.upstream_model),
                capabilities: model.capabilities,
                target_count: None,
                limits_verified: None,
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
            }
            .with_flat_limits(),
        );
    }
    let data = by_id.into_values().collect();
    Ok(Json(ModelList {
        object: "list",
        data,
    }))
}

// ===== Inbound Anthropic Messages API compatibility =====
//
// Anthropic-protocol clients (Claude Code, the Anthropic SDKs) POST to
// `/v1/messages`. We accept that shape, route it through the same
// resolve/barrel machinery as OpenAI traffic, and translate at the boundary:
// a OpenAI-compatible target gets a converted request and its answer is
// converted back, while a native Anthropic target is used verbatim.

const ANTHROPIC_MESSAGES: &str = "/v1/messages";

/// Extracts the required `model` field from a request body.
fn requested_model_of(body: &Value) -> AppResult<String> {
    body.get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::BadRequest("request body must include a model".to_string()))
}

/// Formats one server-sent event the way Anthropic clients expect it.
fn sse_line(event: &str, data: Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    )
}

/// Collects Anthropic text from either a bare string or an array of blocks.
fn anthropic_text(value: &Value) -> Option<String> {
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
fn anthropic_image_to_openai(block: &Value) -> Option<Value> {
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
fn anthropic_block_to_openai_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("image") => anthropic_image_to_openai(block),
        _ => None,
    }
}

fn anthropic_tool_choice_to_openai(choice: &Value) -> Option<Value> {
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
fn anthropic_request_to_openai(input: &Value, model: &str) -> Value {
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
fn anthropic_message_to_openai(role: &str, content: &Value, messages: &mut Vec<Value>) {
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

/// Maps an OpenAI finish reason onto the Anthropic `stop_reason` vocabulary.
fn anthropic_stop_reason(finish: Option<&str>, has_tool_use: bool) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ if has_tool_use => "tool_use",
        _ => "end_turn",
    }
}

/// Converts a non-streamed OpenAI completion into an Anthropic message.
fn openai_response_to_anthropic(value: &Value, requested_model: &str) -> (Value, Usage) {
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

/// Wraps a message in Anthropic's error envelope so Anthropic clients can parse
/// gateway-side failures the same way they parse upstream errors.
fn anthropic_error_body(error_type: &str, message: &str) -> Value {
    json!({"type": "error", "error": {"type": error_type, "message": message}})
}

/// Accumulates state while rewriting an OpenAI SSE stream into Anthropic events.
#[derive(Default)]
struct AnthropicStreamState {
    started: bool,
    text_index: Option<usize>,
    open_tools: Vec<usize>,
    tool_indices: std::collections::HashMap<i64, usize>,
    next_index: usize,
    has_tool_use: bool,
    finish_reason: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    text: String,
}

async fn send_anthropic_event(
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
    event: &str,
    data: Value,
) {
    let _ = tx.send(Ok(Bytes::from(sse_line(event, data)))).await;
}

impl AnthropicStreamState {
    /// Emits `message_start` once, before any content block.
    async fn ensure_started(
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
    async fn close_text_block(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
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
    async fn close_all_blocks(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
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
struct StreamContext {
    message_id: String,
    model: String,
    input_tokens: i64,
}

/// Handles one OpenAI SSE line, emitting the matching Anthropic events.
///
/// Returns `false` once `[DONE]` is seen so the caller can stop reading.
async fn process_openai_line_for_anthropic(
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
async fn finish_anthropic_stream(
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
fn openai_stream_to_anthropic(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let context = StreamContext {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: requested_model.clone(),
            input_tokens: request_tokens,
        };
        let mut stream_state = AnthropicStreamState::default();
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut stream_error = None;
        let mut done = false;

        while !done && let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
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
        }
        .normalized();
        let preview = response_preview(stream_state.text.as_bytes());
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
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Inbound entry point for the Anthropic Messages API (`POST /v1/messages`).
///
/// Anthropic-protocol clients (Claude Code, the Anthropic SDKs) can point their
/// base URL at this gateway. The request is converted into the gateway's
/// internal OpenAI shape so it reuses routing, barrel clamping and capability
/// headers, then the answer is converted back into Anthropic's message and
/// event protocol. Native Anthropic targets skip the conversion entirely.
pub async fn proxy_anthropic(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    match proxy_anthropic_inner(&state, &headers, &uri, &body).await {
        Ok(response) => response,
        Err(error) => anthropic_error_response(error),
    }
}

/// Renders a gateway error in Anthropic's envelope and status vocabulary so an
/// Anthropic client can parse it with its usual error handling.
fn anthropic_error_response(error: AppError) -> Response {
    let (status, error_type) = match &error {
        AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
        AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
        AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
        AppError::Conflict(_) => (StatusCode::CONFLICT, "invalid_request_error"),
        AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "api_error"),
        AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "api_error")
        }
    };
    if status.is_server_error() {
        tracing::error!(error = %error, "anthropic request failed");
    }
    (
        status,
        Json(anthropic_error_body(error_type, &error.to_string())),
    )
        .into_response()
}

async fn proxy_anthropic_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let inbound: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&inbound)?;
    let streamed = inbound
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Convert once up front: routing, barrel clamping and token estimation all
    // operate on the OpenAI shape.
    let mut request_json = anthropic_request_to_openai(&inbound, &requested_model);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = authenticate_gateway(state, headers).await?;
    let resolved = resolve_route(state, &requested_model).await?;
    let route_id = resolved.route_id;
    let requested_output_tokens = request_json.get("max_tokens").and_then(Value::as_i64);
    let clamped_output_tokens = clamp_output_request(&mut request_json, resolved.barrel.as_ref());
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    let ordering_key = route_id.unwrap_or(-1);
    let ordered_targets =
        order_targets(state, ordering_key, &resolved.strategy, resolved.targets).await?;

    let mut last_error = None;
    for target in ordered_targets {
        let result = if target.provider_type == "anthropic" {
            // Native target: forward the caller's Anthropic payload unchanged,
            // only swapping in the resolved upstream model.
            let mut native = inbound.clone();
            native["model"] = json!(target.upstream_model);
            forward_anthropic_native(
                state,
                &request_id,
                &requested_model,
                native,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
            )
            .await
        } else {
            forward_openai_as_anthropic(
                state,
                &request_id,
                &requested_model,
                &request_json,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
            )
            .await
        };
        match result {
            Ok(response) => return Ok(response),
            Err(error) => last_error = Some(error),
        }
    }

    let error = last_error
        .unwrap_or_else(|| AppError::Upstream("all configured route targets failed".to_string()));
    log_usage(
        state,
        UsageLogEntry {
            request_id: &request_id,
            api_key_id: api_key.as_ref().map(|key| key.id),
            route_id,
            provider_id: None,
            requested_model: &requested_model,
            upstream_model: None,
            endpoint: &endpoint,
            usage: Usage {
                prompt_tokens: request_tokens,
                completion_tokens: 0,
                total_tokens: request_tokens,
            },
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: 502,
            success: false,
            streamed,
            error_message: Some(&error.to_string()),
            response_preview: None,
        },
    )
    .await;
    Err(error)
}

/// Forwards an Anthropic-shaped request to a native Anthropic provider.
///
/// The upstream already speaks the caller's protocol, so the response can be
/// streamed straight back with no translation.
#[allow(clippy::too_many_arguments)]
async fn forward_anthropic_native(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    body: Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let url = join_upstream_url(&target.base_url, ANTHROPIC_MESSAGES);
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body);
    if let Some(key) = &target.api_key {
        request = request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");
    }
    request = apply_custom_headers(request, &target.provider_headers)?;

    let response = request.send().await.map_err(|error| {
        AppError::Upstream(format!("{} request failed: {error}", target.provider_name))
    })?;
    let status = response.status();
    if !status.is_success() {
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let message = String::from_utf8_lossy(&response_body)
            .chars()
            .take(600)
            .collect::<String>();
        if retryable_status(status) {
            return Err(AppError::Upstream(format!(
                "{} returned {}: {}",
                target.provider_name, status, message
            )));
        }
        let preview = response_preview(&response_body);
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage: Usage {
                    prompt_tokens: request_tokens,
                    completion_tokens: 0,
                    total_tokens: request_tokens,
                },
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: status.as_u16() as i64,
                success: false,
                streamed,
                error_message: Some(&message),
                response_preview: preview.as_deref(),
            },
        )
        .await;
        // Preserve the upstream status and Anthropic-shaped error body.
        return Ok(Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_body))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
    }

    if streamed {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        return Ok(passthrough_stream_response(
            state.clone(),
            response,
            content_type,
            ANTHROPIC_MESSAGES.to_string(),
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
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let preview = response_preview(&response_bytes);
    log_usage_detached(
        state.clone(),
        OwnedUsageLogEntry {
            request_id: request_id.to_string(),
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model: requested_model.to_string(),
            upstream_model: Some(target.upstream_model.clone()),
            endpoint: ANTHROPIC_MESSAGES.to_string(),
            usage,
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview,
        },
    );
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from(response_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}

/// Forwards an OpenAI-shaped request to an OpenAI-compatible provider and
/// converts the completion back into an Anthropic message.
#[allow(clippy::too_many_arguments)]
async fn forward_openai_as_anthropic(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    request_json: &Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let mut body = request_json.clone();
    body["model"] = json!(target.upstream_model);
    let url = join_upstream_url(&target.base_url, OPENAI_CHAT_COMPLETIONS);
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body);
    if let Some(key) = &target.api_key {
        request = request.bearer_auth(key);
    }
    request = apply_custom_headers(request, &target.provider_headers)?;

    let response = request.send().await.map_err(|error| {
        AppError::Upstream(format!("{} request failed: {error}", target.provider_name))
    })?;
    let status = response.status();
    if !status.is_success() {
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let message = String::from_utf8_lossy(&response_body)
            .chars()
            .take(600)
            .collect::<String>();
        if retryable_status(status) {
            return Err(AppError::Upstream(format!(
                "{} returned {}: {}",
                target.provider_name, status, message
            )));
        }
        // Non-retryable: surface the upstream failure verbatim so the operator
        // sees the provider's own message rather than a generic gateway error.
        let preview = response_preview(&response_body);
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage: Usage {
                    prompt_tokens: request_tokens,
                    completion_tokens: 0,
                    total_tokens: request_tokens,
                },
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: status.as_u16() as i64,
                success: false,
                streamed,
                error_message: Some(&message),
                response_preview: preview.as_deref(),
            },
        )
        .await;
        return Ok(Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&anthropic_error_body("api_error", &message))
                    .unwrap_or_default(),
            ))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
    }

    if streamed {
        return Ok(openai_stream_to_anthropic(
            state.clone(),
            response,
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let upstream: Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Upstream(format!("invalid JSON from upstream: {error}")))?;
    let (converted, usage) = openai_response_to_anthropic(&upstream, requested_model);
    let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
    let preview = response_preview(converted_bytes.as_slice());
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model,
            upstream_model: Some(&target.upstream_model),
            endpoint: ANTHROPIC_MESSAGES,
            usage: fill_usage(usage, request_tokens, &converted),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
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
    Ok(response)
}

pub async fn proxy_openai(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut request_json: Value = serde_json::from_slice(&body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&request_json)?;
    let streamed = request_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = authenticate_gateway(&state, &headers).await?;
    let resolved = resolve_route(&state, &requested_model).await?;
    let route_id = resolved.route_id;
    // Barrel mode: clamp the requested output length to the strictest common
    // ceiling across every target, so no target is picked that would reject the
    // request as too large.
    let requested_output_tokens = request_json
        .get("max_tokens")
        .or_else(|| request_json.get("max_completion_tokens"))
        .and_then(Value::as_i64);
    let clamped_output_tokens = clamp_output_request(&mut request_json, resolved.barrel.as_ref());
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    let ordering_key = route_id.unwrap_or(-1);
    let ordered_targets =
        order_targets(&state, ordering_key, &resolved.strategy, resolved.targets).await?;

    let mut last_error = None;
    for target in ordered_targets {
        if target.provider_type == "anthropic" && endpoint != OPENAI_CHAT_COMPLETIONS {
            last_error = Some(format!(
                "{} does not support the {} endpoint",
                target.provider_name, endpoint
            ));
            continue;
        }

        match forward_to_target(
            &state,
            &request_id,
            &endpoint,
            &requested_model,
            &request_json,
            &body,
            target,
            streamed,
            request_tokens,
            api_key.as_ref(),
            started,
            receipt.clone(),
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(AppError::BadRequest(message)) => {
                last_error = Some(message);
            }
            Err(AppError::Upstream(message)) => {
                last_error = Some(message);
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
    }

    let message = last_error.unwrap_or_else(|| "all configured route targets failed".to_string());
    log_usage(
        &state,
        UsageLogEntry {
            request_id: &request_id,
            api_key_id: api_key.as_ref().map(|key| key.id),
            route_id,
            provider_id: None,
            requested_model: &requested_model,
            upstream_model: None,
            endpoint: &endpoint,
            usage: Usage {
                prompt_tokens: request_tokens,
                completion_tokens: 0,
                total_tokens: request_tokens,
            },
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: 502,
            success: false,
            streamed,
            error_message: Some(&message),
            response_preview: None,
        },
    )
    .await;
    Err(AppError::Upstream(message))
}

#[allow(clippy::too_many_arguments)]
async fn forward_to_target(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    request_json: &Value,
    _raw_body: &Bytes,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let provider_type =
        ProviderType::from_str(&target.provider_type).map_err(AppError::BadRequest)?;

    let (url, request_body) = match provider_type {
        ProviderType::Anthropic => (
            join_upstream_url(&target.base_url, "/v1/messages"),
            convert_request_to_anthropic(request_json, &target.upstream_model, streamed),
        ),
        ProviderType::Openai | ProviderType::Ollama | ProviderType::Custom => {
            let mut body = request_json.clone();
            body["model"] = json!(target.upstream_model);
            (join_upstream_url(&target.base_url, endpoint), body)
        }
    };

    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&request_body);

    request = match provider_type {
        ProviderType::Anthropic => {
            if let Some(key) = &target.api_key {
                request = request
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
            request
        }
        _ => {
            if let Some(key) = &target.api_key {
                request = request.bearer_auth(key);
            }
            request
        }
    };
    request = apply_custom_headers(request, &target.provider_headers)?;

    let response = request.send().await.map_err(|error| {
        AppError::Upstream(format!("{} request failed: {error}", target.provider_name))
    })?;
    let status = response.status();

    if !status.is_success() {
        let response_headers = response.headers().clone();
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let message = String::from_utf8_lossy(&response_body)
            .chars()
            .take(600)
            .collect::<String>();

        if retryable_status(status) {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                %status,
                "upstream failed, trying next route target"
            );
            return Err(AppError::Upstream(format!(
                "{} returned {}: {}",
                target.provider_name, status, message
            )));
        }

        let preview = response_preview(&response_body);
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
                usage: Usage {
                    prompt_tokens: request_tokens,
                    completion_tokens: 0,
                    total_tokens: request_tokens,
                },
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: status.as_u16() as i64,
                success: false,
                streamed,
                error_message: Some(&message),
                response_preview: preview.as_deref(),
            },
        )
        .await;

        let mut builder = Response::builder().status(status);
        if let Some(value) = response_headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
        {
            builder = builder.header(reqwest::header::CONTENT_TYPE, value);
        }
        return Ok(builder
            .body(Body::from(response_body))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
    }

    let response_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if provider_type == ProviderType::Anthropic {
        if streamed {
            return Ok(anthropic_stream_response(
                state.clone(),
                response,
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
        let (converted, usage) = convert_anthropic_response(&upstream_json);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
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
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
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
            response,
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
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let response_text = String::from_utf8_lossy(&response_bytes)
        .chars()
        .take(600)
        .collect::<String>();
    let preview = response_preview(&response_bytes);
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
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
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

#[allow(clippy::too_many_arguments)]
fn passthrough_stream_response(
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
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
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
            });
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
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: parser.first_token_ms,
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
fn anthropic_stream_response(
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

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
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
                latency_ms: started.elapsed().as_millis() as i64,
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
async fn process_anthropic_line(
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
            if let Some(input_tokens) = value
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_i64)
            {
                usage.prompt_tokens = input_tokens;
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
        }
        _ => {}
    }
}

fn convert_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let mut output = json!({
        "model": model,
        "max_tokens": input.get("max_tokens")
            .or_else(|| input.get("max_completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(4096),
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
    output
}

fn convert_anthropic_content(content: &Value) -> Value {
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

fn convert_anthropic_response(value: &Value) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
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

    let prompt_tokens = value
        .pointer("/usage/input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let completion_tokens = value
        .pointer("/usage/output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let usage = Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    };
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

fn openai_stream_chunk(
    id: &str,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<Value>,
) -> Value {
    let mut value = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason
        }]
    });
    if let Some(usage) = usage {
        value["usage"] = usage;
        value["choices"] = json!([]);
    }
    value
}

async fn authenticate_gateway(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ApiKeyRecord>> {
    // Presence of any configured key enables gateway auth. Counting only
    // enabled keys would silently fall back to anonymous access when every
    // key is disabled, which is the opposite of the operator's intent.
    // Copy the cached value out in its own statement: keeping the read guard
    // alive across the match would deadlock when a write lock is requested
    // below (tokio's RwLock is write-preferring).
    let cached = *state.auth_required.read().await;
    let auth_required = match cached {
        Some(cached) => cached,
        None => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
                .fetch_one(&state.pool)
                .await?;
            let required = count > 0;
            *state.auth_required.write().await = Some(required);
            required
        }
    };
    if !auth_required {
        return Ok(None);
    }

    let supplied = bearer_token(headers)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .ok_or_else(|| {
            AppError::Unauthorized("missing gateway API key in Authorization header".to_string())
        })?;
    let hash = hash_secret(supplied);
    let record = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys WHERE key_hash = ? AND enabled = 1",
    )
    .bind(hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("invalid gateway API key".to_string()))?;

    // Writing on every request would take a SQLite write lock each time.
    // Once a minute per key is plenty for a "last used" display.
    let should_touch = {
        let mut touched = state.key_touched.lock().await;
        match touched.get(&record.id) {
            Some(last) if last.elapsed() < std::time::Duration::from_secs(60) => false,
            _ => {
                touched.insert(record.id, std::time::Instant::now());
                true
            }
        }
    };
    if should_touch
        && let Err(error) = sqlx::query(
            "UPDATE api_keys SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
        )
        .bind(record.id)
        .execute(&state.pool)
        .await
    {
        tracing::warn!(%error, key_id = record.id, "failed to update API key last_used_at");
    }
    Ok(Some(record))
}

struct ResolvedRoute {
    route_id: Option<i64>,
    strategy: String,
    targets: Vec<RouteTarget>,
    /// Strictest common capability envelope across the route's targets. Only
    /// meaningful for explicit routes; a directly matched model reports its own
    /// capabilities.
    barrel: Option<BarrelEnvelope>,
}

async fn resolve_route(state: &AppState, model: &str) -> AppResult<ResolvedRoute> {
    if let Some(route) = find_explicit_route(state, model).await? {
        let targets = load_targets(state, route.id).await?;
        if targets.is_empty() {
            return Err(AppError::Upstream(
                "the matched route has no enabled provider targets".to_string(),
            ));
        }
        let pairs = targets
            .iter()
            .map(|target| (target.provider_id, target.upstream_model.clone()))
            .collect::<Vec<_>>();
        let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
            strategy: route.strategy,
            targets,
            barrel: Some(barrel),
        });
    }

    let targets = find_prefixed_targets(state, model).await?;
    let pairs = targets
        .iter()
        .map(|target| (target.provider_id, target.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
    Ok(ResolvedRoute {
        route_id: None,
        strategy: "priority".to_string(),
        targets,
        barrel: Some(barrel),
    })
}

async fn find_explicit_route(state: &AppState, model: &str) -> AppResult<Option<Route>> {
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT * FROM routes
        WHERE enabled = 1
        ORDER BY
            CASE WHEN instr(model_pattern, '*') = 0 AND instr(model_pattern, '?') = 0 THEN 0 ELSE 1 END,
            length(model_pattern) DESC,
            id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for route in routes {
        let Ok(glob) = Glob::new(&route.model_pattern) else {
            tracing::warn!(pattern = %route.model_pattern, "ignoring invalid route pattern");
            continue;
        };
        if glob.compile_matcher().is_match(model) {
            return Ok(Some(route));
        }
    }

    Ok(None)
}

async fn find_prefixed_targets(state: &AppState, model: &str) -> AppResult<Vec<RouteTarget>> {
    let prefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix <> ''
          AND substr(?, 1, length(p.model_prefix)) = p.model_prefix
          AND substr(?, length(p.model_prefix) + 1) = pm.model_name
        ORDER BY p.id, pm.model_name
        "#,
    )
    .bind(model)
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    if !prefixed.is_empty() {
        return Ok(prefixed);
    }

    let unprefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix = ''
          AND pm.model_name = ?
        ORDER BY p.id
        LIMIT 2
        "#,
    )
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    if unprefixed.len() > 1 {
        return Err(AppError::Conflict(format!(
            "model '{model}' exists on multiple providers; configure a model prefix or an explicit route"
        )));
    }
    if unprefixed.is_empty() {
        return Err(AppError::NotFound(format!(
            "no enabled route or provider model matches '{model}'"
        )));
    }
    Ok(unprefixed)
}

async fn load_targets(state: &AppState, route_id: i64) -> AppResult<Vec<RouteTarget>> {
    Ok(sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT rt.*, p.name AS provider_name, p.provider_type,
               p.base_url, p.model_prefix, p.api_key, p.headers AS provider_headers,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        WHERE rt.route_id = ? AND rt.enabled = 1 AND p.enabled = 1
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route_id)
    .fetch_all(&state.pool)
    .await?)
}

async fn order_targets(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    mut targets: Vec<RouteTarget>,
) -> AppResult<Vec<RouteTarget>> {
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    match strategy {
        RouteStrategy::Priority => {
            targets.sort_by_key(|target| (target.priority, target.id));
        }
        RouteStrategy::Weighted => {
            let mut rng = rand::thread_rng();
            let mut selected = Vec::with_capacity(targets.len());
            while !targets.is_empty() {
                let weights = targets
                    .iter()
                    .map(|target| target.weight.max(1) as u32)
                    .collect::<Vec<_>>();
                let index = WeightedIndex::new(&weights)
                    .map(|distribution| distribution.sample(&mut rng))
                    .unwrap_or(0);
                selected.push(targets.remove(index));
            }
            targets = selected;
        }
        RouteStrategy::RoundRobin => {
            let mut cursors = state.round_robin.lock().await;
            let cursor = cursors.entry(route_id).or_default();
            let offset = *cursor % targets.len();
            targets.rotate_left(offset);
            *cursor = cursor.wrapping_add(1);
        }
    }
    Ok(targets)
}

struct UsageParser {
    buffer: Vec<u8>,
    usage: Option<Usage>,
    output_chars: usize,
    text: String,
    started_at: Instant,
    first_token_ms: Option<i64>,
}

impl UsageParser {
    fn new(started_at: Instant) -> Self {
        Self {
            buffer: Vec::new(),
            usage: None,
            output_chars: 0,
            text: String::new(),
            started_at,
            first_token_ms: None,
        }
    }

    /// Records time-to-first-token the first time any real content arrives.
    fn mark_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.started_at.elapsed().as_millis() as i64);
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=position).collect::<Vec<_>>();
            self.parse_line(&line);
        }
    }

    fn finish(&mut self) -> Option<Usage> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.parse_line(&line);
        }
        let estimated =
            (self.output_chars / 4).max(if self.output_chars > 0 { 1 } else { 0 }) as i64;
        match self.usage.take() {
            Some(mut usage) => {
                if usage.completion_tokens == 0 {
                    usage.completion_tokens = estimated.max(1);
                }
                Some(usage.normalized())
            }
            // No usage frame was sent: fall back to the characters we actually
            // saw streamed, not a fixed constant.
            None if self.output_chars > 0 => Some(Usage {
                prompt_tokens: 0,
                completion_tokens: estimated,
                total_tokens: 0,
            }),
            None => None,
        }
    }

    fn preview(&self) -> Option<String> {
        response_preview(self.text.as_bytes())
    }

    fn parse_line(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let Some(data) = line.trim().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(usage) = usage_from_value(&value) {
            // Merge instead of replace: Anthropic reports input tokens in
            // `message_start` and output tokens later in `message_delta`, so
            // overwriting would drop whichever side arrived first.
            self.usage = Some(match self.usage.take() {
                Some(previous) => Usage {
                    prompt_tokens: if usage.prompt_tokens > 0 {
                        usage.prompt_tokens
                    } else {
                        previous.prompt_tokens
                    },
                    completion_tokens: if usage.completion_tokens > 0 {
                        usage.completion_tokens
                    } else {
                        previous.completion_tokens
                    },
                    total_tokens: 0,
                }
                .normalized(),
                None => usage,
            });
        }
        if let Some(content) = value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += content.chars().count();
            push_preview_text(&mut self.text, content);
        }
        // Legacy `/v1/completions` streams put the text in `choices[0].text`
        // instead of a delta object.
        if let Some(text) = value.pointer("/choices/0/text").and_then(Value::as_str) {
            if !text.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += text.chars().count();
            push_preview_text(&mut self.text, text);
        }
        if let Some(text) = value.get("output_text").and_then(Value::as_str) {
            if !text.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += text.chars().count();
            push_preview_text(&mut self.text, text);
        }
        // Responses API streams `response.output_text.delta` events whose text
        // lives in a top-level `delta` string rather than a choices array.
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "response.output_text.delta")
            && let Some(delta) = value.get("delta").and_then(Value::as_str)
        {
            if !delta.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += delta.chars().count();
            push_preview_text(&mut self.text, delta);
        }
    }
}

fn extract_usage_from_json(bytes: &[u8]) -> Option<Usage> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    usage_from_value(&value)
}

fn usage_from_value(value: &Value) -> Option<Usage> {
    let usage = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        // Native Anthropic streams report usage in two different places:
        // `message_start` nests it under `message.usage`, while `message_delta`
        // carries a top-level `usage` (handled by the first branch above).
        .or_else(|| value.pointer("/message/usage"))?;
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(prompt_tokens + completion_tokens);
    Some(
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        }
        .normalized(),
    )
}

fn estimated_completion_usage(request_tokens: i64, bytes: &[u8]) -> Usage {
    let completion_tokens = (bytes.len() / 4).max(1) as i64;
    Usage {
        prompt_tokens: request_tokens,
        completion_tokens,
        total_tokens: request_tokens + completion_tokens,
    }
}

fn estimate_request_tokens(value: &Value) -> i64 {
    let text = value
        .get("messages")
        .or_else(|| value.get("input"))
        .or_else(|| value.get("prompt"))
        .map(Value::to_string)
        .unwrap_or_default();
    (text.chars().count() / 4).max(1) as i64
}

fn fill_usage(usage: Usage, request_tokens: i64, response: &Value) -> Usage {
    let mut usage = usage;
    if usage.prompt_tokens == 0 {
        usage.prompt_tokens = request_tokens;
    }
    if usage.completion_tokens == 0 {
        usage.completion_tokens = (response.to_string().chars().count() / 4).max(1) as i64;
    }
    usage.normalized()
}

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

fn join_upstream_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = format!("/{}", path.trim_start_matches('/'));
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{base}{}", &path[3..])
    } else {
        format!("{base}{path}")
    }
}

pub(crate) fn apply_custom_headers(
    mut request: RequestBuilder,
    headers: &str,
) -> AppResult<RequestBuilder> {
    let Ok(headers) = serde_json::from_str::<Value>(headers) else {
        return Ok(request);
    };
    let Some(headers) = headers.as_object() else {
        return Ok(request);
    };
    for (name, value) in headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            AppError::BadRequest(format!("invalid header name '{name}': {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            AppError::BadRequest(format!("invalid value for header '{name}': {error}"))
        })?;
        request = request.header(name, value);
    }
    Ok(request)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn hash_secret(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// Downward compatibility for multi-target routes.
///
/// A route may fan out to models with different ceilings. Clamping the
/// requested output length to the strictest common `output_limit` keeps the
/// request valid for *every* target, so whichever one the strategy picks can
/// serve it instead of failing with a "max_tokens too large" error.
///
/// Returns the clamped value when a reduction happened, so the caller can
/// surface it in the response receipt.
fn clamp_output_request(body: &mut Value, barrel: Option<&BarrelEnvelope>) -> Option<i64> {
    let limit = barrel
        .and_then(|barrel| barrel.capabilities.as_ref())
        .and_then(|capabilities| capabilities.output_limit)?;
    let mut clamped = None;
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(requested) = body.get(key).and_then(Value::as_i64)
            && requested > limit
        {
            body[key] = json!(limit);
            clamped = Some(limit);
        }
    }
    clamped
}

/// Builds the capability receipt returned alongside a completion.
///
/// `requested_output_tokens` and `clamped_output_tokens` make the barrel
/// behaviour visible: a client that asked for more than the route supports can
/// see that the gateway reduced the request rather than silently ignoring it.
fn capability_receipt(
    barrel: Option<&BarrelEnvelope>,
    requested_output_tokens: Option<i64>,
    clamped_output_tokens: Option<i64>,
) -> Option<Value> {
    let capabilities = barrel.and_then(|barrel| barrel.capabilities.as_ref())?;
    let mut receipt = serde_json::to_value(capabilities).ok()?;
    let object = receipt.as_object_mut()?;
    let incomplete = barrel.is_some_and(|barrel| barrel.incomplete);
    object.insert("limits_verified".to_string(), json!(!incomplete));
    if let Some(barrel) = barrel {
        object.insert("target_count".to_string(), json!(barrel.target_count));
    }
    if let Some(requested) = requested_output_tokens {
        object.insert("requested_output_tokens".to_string(), json!(requested));
    }
    if let Some(clamped) = clamped_output_tokens {
        object.insert("clamped_output_tokens".to_string(), json!(clamped));
    }
    Some(receipt)
}

/// Injects the receipt into a JSON response body. Bodies that are not JSON
/// objects are returned unchanged so upstream payloads are never corrupted.
fn inject_capability_receipt(bytes: &[u8], receipt: &Option<Value>) -> Option<Vec<u8>> {
    let receipt = receipt.as_ref()?;
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    value
        .as_object_mut()?
        .insert("capabilities".to_string(), receipt.clone());
    serde_json::to_vec(&value).ok()
}

/// Limits mirrored onto response headers so streaming clients, which never
/// receive a single JSON body, can still read the effective ceiling.
fn apply_capability_headers(response: &mut Response, receipt: &Option<Value>) {
    let Some(receipt) = receipt.as_ref() else {
        return;
    };
    let Some(output) = receipt.get("output_limit").and_then(Value::as_i64) else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(&output.to_string()) {
        response
            .headers_mut()
            .insert("x-openllm-max-output-tokens", value);
    }
    if let Some(context) = receipt.get("context_limit").and_then(Value::as_i64)
        && let Ok(value) = HeaderValue::from_str(&context.to_string())
    {
        response
            .headers_mut()
            .insert("x-openllm-max-context-tokens", value);
    }
}

/// Keep a short, human-readable slice of the response for later debugging
/// without storing unbounded payloads.
fn response_preview(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = trimmed.chars().take(2000).collect::<String>();
    Some(preview)
}

/// Upper bound on how much streamed text is retained for the preview. Anything
/// beyond this is discarded as it arrives, so a very long generation does not
/// buffer its entire output in memory just to store 2000 characters.
const PREVIEW_CHAR_LIMIT: usize = 2000;

/// Appends `chunk` to `target` only while the preview budget allows it.
fn push_preview_text(target: &mut String, chunk: &str) {
    let remaining = PREVIEW_CHAR_LIMIT.saturating_sub(target.chars().count());
    if remaining == 0 {
        return;
    }
    target.extend(chunk.chars().take(remaining));
}

struct UsageLogEntry<'a> {
    request_id: &'a str,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    provider_id: Option<i64>,
    requested_model: &'a str,
    upstream_model: Option<&'a str>,
    endpoint: &'a str,
    usage: Usage,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    status_code: i64,
    success: bool,
    streamed: bool,
    error_message: Option<&'a str>,
    response_preview: Option<&'a str>,
}

/// Owned mirror of [`UsageLogEntry`]; needed when the log is written from a
/// detached task that outlives the request's borrowed values.
struct OwnedUsageLogEntry {
    request_id: String,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    provider_id: Option<i64>,
    requested_model: String,
    upstream_model: Option<String>,
    endpoint: String,
    usage: Usage,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    status_code: i64,
    success: bool,
    streamed: bool,
    error_message: Option<String>,
    response_preview: Option<String>,
}

impl OwnedUsageLogEntry {
    fn as_borrowed(&self) -> UsageLogEntry<'_> {
        UsageLogEntry {
            request_id: &self.request_id,
            api_key_id: self.api_key_id,
            route_id: self.route_id,
            provider_id: self.provider_id,
            requested_model: &self.requested_model,
            upstream_model: self.upstream_model.as_deref(),
            endpoint: &self.endpoint,
            usage: self.usage,
            latency_ms: self.latency_ms,
            first_token_ms: self.first_token_ms,
            status_code: self.status_code,
            success: self.success,
            streamed: self.streamed,
            error_message: self.error_message.as_deref(),
            response_preview: self.response_preview.as_deref(),
        }
    }
}

/// Writes the usage row off the response path so a slow SQLite write lock does
/// not inflate the latency the caller observes.
fn log_usage_detached(state: AppState, entry: OwnedUsageLogEntry) {
    tokio::spawn(async move {
        log_usage(&state, entry.as_borrowed()).await;
    });
}

async fn log_usage(state: &AppState, entry: UsageLogEntry<'_>) {
    let usage = entry.usage.normalized();
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, latency_ms, first_token_ms, status_code, success, streamed,
            error_message, response_preview
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(entry.request_id)
    .bind(entry.api_key_id)
    .bind(entry.route_id)
    .bind(entry.provider_id)
    .bind(entry.requested_model)
    .bind(entry.upstream_model)
    .bind(entry.endpoint)
    .bind(usage.prompt_tokens)
    .bind(usage.completion_tokens)
    .bind(usage.total_tokens)
    .bind(entry.latency_ms)
    .bind(entry.first_token_ms)
    .bind(entry.status_code)
    .bind(entry.success as i64)
    .bind(entry.streamed as i64)
    .bind(entry.error_message)
    .bind(entry.response_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(result) => {
            let _ = state.events.send(crate::state::UsageEvent {
                id: result.last_insert_rowid(),
                request_id: entry.request_id.to_string(),
                success: entry.success,
                streamed: entry.streamed,
            });
        }
        Err(error) => {
            tracing::error!(%error, request_id = %entry.request_id, "failed to write usage log");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_base_and_path_without_double_v1() {
        assert_eq!(
            join_upstream_url("https://api.openai.com/v1", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream_url("https://api.openai.com", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream_url("http://localhost:11434/v1", "/models"),
            "http://localhost:11434/v1/models"
        );
    }

    #[tokio::test]
    async fn prefix_matching_is_literal_and_case_sensitive() {
        // A pooled `sqlite::memory:` database gives each connection its own
        // empty database, so pin the pool to a single connection.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'underscore', 'openai', 'http://upstream', 'a_b/')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO provider_models (provider_id, model_name) VALUES (1, 'x')")
            .execute(&pool)
            .await
            .unwrap();

        let state = AppState::new(pool, None);

        // `_` must be treated literally, not as a single-character wildcard.
        let literal = find_prefixed_targets(&state, "a_b/x").await.unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].upstream_model, "x");
        assert!(find_prefixed_targets(&state, "aXb/x").await.is_err());

        // Prefix comparison must stay case-sensitive so `A_B/x` cannot reach a
        // provider registered as `a_b`.
        assert!(find_prefixed_targets(&state, "A_B/x").await.is_err());
    }

    #[test]
    fn retries_only_transient_statuses() {
        for status in [408, 409, 425, 429, 500, 502, 503] {
            assert!(
                retryable_status(StatusCode::from_u16(status).unwrap()),
                "{status} should be retryable"
            );
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !retryable_status(StatusCode::from_u16(status).unwrap()),
                "{status} should not be retryable"
            );
        }
    }

    fn barrel_with_output_limit(output_limit: Option<i64>) -> BarrelEnvelope {
        BarrelEnvelope {
            capabilities: Some(crate::models::ModelCapabilities {
                output_limit,
                ..Default::default()
            }),
            incomplete: false,
            target_count: 2,
        }
    }

    #[test]
    fn clamps_requested_output_to_barrel_limit() {
        let barrel = barrel_with_output_limit(Some(8000));
        let mut body = json!({"model": "m", "max_tokens": 32000});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(8000));
        assert_eq!(body["max_tokens"], 8000);
    }

    #[test]
    fn leaves_requests_within_the_barrel_untouched() {
        let barrel = barrel_with_output_limit(Some(8000));
        let mut body = json!({"model": "m", "max_tokens": 4096});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), None);
        assert_eq!(body["max_tokens"], 4096);

        // No known limit means there is nothing to clamp against.
        let unknown = barrel_with_output_limit(None);
        let mut body = json!({"model": "m", "max_tokens": 999999});
        assert_eq!(clamp_output_request(&mut body, Some(&unknown)), None);
        assert_eq!(body["max_tokens"], 999999);
    }

    #[test]
    fn clamps_max_completion_tokens_too() {
        let barrel = barrel_with_output_limit(Some(1000));
        let mut body = json!({"model": "m", "max_completion_tokens": 5000});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
        assert_eq!(body["max_completion_tokens"], 1000);
    }

    #[test]
    fn receipt_reports_clamping_and_target_count() {
        let barrel = barrel_with_output_limit(Some(8000));
        let receipt = capability_receipt(Some(&barrel), Some(32000), Some(8000)).unwrap();
        assert_eq!(receipt["output_limit"], 8000);
        assert_eq!(receipt["requested_output_tokens"], 32000);
        assert_eq!(receipt["clamped_output_tokens"], 8000);
        assert_eq!(receipt["target_count"], 2);
        assert_eq!(receipt["limits_verified"], true);
    }

    #[test]
    fn receipt_injected_only_into_json_objects() {
        let receipt = Some(json!({"output_limit": 8000}));
        let injected = inject_capability_receipt(br#"{"id":"x"}"#, &receipt).unwrap();
        let value: Value = serde_json::from_slice(&injected).unwrap();
        assert_eq!(value["capabilities"]["output_limit"], 8000);
        // Non-JSON bodies must be left alone rather than replaced.
        assert!(inject_capability_receipt(b"not json", &receipt).is_none());
        assert!(inject_capability_receipt(b"{}", &None).is_none());
    }

    #[test]
    fn converts_anthropic_request_into_openai_shape() {
        let inbound = json!({
            "model": "claude-x",
            "system": "be brief",
            "max_tokens": 256,
            "temperature": 0.3,
            "stop_sequences": ["STOP"],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]}
            ],
            "tools": [{
                "name": "get_weather",
                "description": "weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }],
            "tool_choice": {"type": "tool", "name": "get_weather"}
        });
        let openai = anthropic_request_to_openai(&inbound, "claude-x");

        assert_eq!(openai["max_tokens"], 256);
        assert_eq!(openai["temperature"], 0.3);
        assert_eq!(openai["stop"], json!(["STOP"]));
        // The system prompt becomes a leading system message.
        assert_eq!(openai["messages"][0]["role"], "system");
        assert_eq!(openai["messages"][0]["content"], "be brief");
        // A text-only user turn collapses to a plain string.
        assert_eq!(openai["messages"][1]["role"], "user");
        assert_eq!(openai["messages"][1]["content"], "hi");
        // Anthropic tool schema maps onto the OpenAI function shape.
        assert_eq!(openai["tools"][0]["type"], "function");
        assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(
            openai["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        assert_eq!(openai["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn converts_anthropic_tool_use_and_results_round_trip() {
        // Assistant asks for a tool, then the user returns its result.
        let inbound = json!({
            "model": "claude-x",
            "max_tokens": 128,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C"}
                ]}
            ]
        });
        let openai = anthropic_request_to_openai(&inbound, "claude-x");
        let assistant = &openai["messages"][0];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "checking");
        assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        // The tool result becomes a dedicated role: tool message.
        let tool = &openai["messages"][1];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "toolu_1");
        assert_eq!(tool["content"], "18C");
    }

    #[test]
    fn converts_openai_completion_into_anthropic_message() {
        let upstream = json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
        });
        let (message, usage) = openai_response_to_anthropic(&upstream, "claude-x");
        assert_eq!(message["type"], "message");
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["model"], "claude-x");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "hello");
        assert_eq!(message["stop_reason"], "end_turn");
        assert_eq!(message["usage"]["input_tokens"], 4);
        assert_eq!(message["usage"]["output_tokens"], 2);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
    }

    #[test]
    fn converts_openai_tool_call_into_anthropic_tool_use() {
        let upstream = json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8}
        });
        let (message, _) = openai_response_to_anthropic(&upstream, "claude-x");
        // No text block: a tool-only answer must not emit an empty text block.
        assert_eq!(message["content"].as_array().unwrap().len(), 1);
        assert_eq!(message["content"][0]["type"], "tool_use");
        assert_eq!(message["content"][0]["id"], "call_1");
        assert_eq!(message["content"][0]["name"], "get_weather");
        assert_eq!(message["content"][0]["input"]["city"], "Paris");
        assert_eq!(message["stop_reason"], "tool_use");
    }

    #[test]
    fn maps_openai_finish_reasons_to_anthropic_stop_reasons() {
        assert_eq!(anthropic_stop_reason(Some("length"), false), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("tool_calls"), false), "tool_use");
        assert_eq!(anthropic_stop_reason(Some("stop"), false), "end_turn");
        // A tool_use block is authoritative even without a matching reason.
        assert_eq!(anthropic_stop_reason(None, true), "tool_use");
    }

    #[tokio::test]
    async fn rewrites_openai_stream_into_anthropic_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let context = StreamContext {
            message_id: "msg_test".to_string(),
            model: "claude-x".to_string(),
            input_tokens: 7,
        };
        let mut state = AnthropicStreamState::default();

        let lines: [&[u8]; 5] = [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n",
            b"data: [DONE]\n",
        ];
        let mut saw_done = false;
        for line in lines {
            if !process_openai_line_for_anthropic(line, &mut state, &context, &tx).await {
                saw_done = true;
            }
        }
        assert!(saw_done, "[DONE] must terminate the stream");
        // Exercise the same closing sequence the live handler uses.
        finish_anthropic_stream(&mut state, &context, &tx).await;
        drop(tx);

        let mut frames = Vec::new();
        while let Some(item) = rx.recv().await {
            frames.push(String::from_utf8(item.unwrap().to_vec()).unwrap());
        }
        let all = frames.join("");

        assert!(all.contains("event: message_start"), "{all}");
        assert!(all.contains("event: content_block_start"), "{all}");
        assert!(all.contains("\"type\":\"text_delta\""), "{all}");
        // Text deltas must not be re-joined; each chunk passes through.
        assert!(all.contains("\"text\":\"Hel\""), "{all}");
        assert!(all.contains("\"text\":\"lo\""), "{all}");
        assert!(all.contains("event: content_block_stop"), "{all}");
        assert!(all.contains("event: message_delta"), "{all}");
        assert!(all.contains("\"stop_reason\":\"end_turn\""), "{all}");
        assert!(all.contains("event: message_stop"), "{all}");
        assert_eq!(state.output_tokens, 2);
        assert_eq!(state.text, "Hello");
    }

    #[tokio::test]
    async fn anthropic_stream_maps_tool_calls_to_tool_use_blocks() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let context = StreamContext {
            message_id: "msg_test".to_string(),
            model: "claude-x".to_string(),
            input_tokens: 5,
        };
        let mut state = AnthropicStreamState::default();
        // Build the frames with serde so nested JSON escaping stays correct.
        let frames = [
            json!({"choices":[{"index":0,"delta":{"content":"thinking"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}),
        ];
        for frame in frames {
            let line = format!("data: {frame}\n");
            process_openai_line_for_anthropic(line.as_bytes(), &mut state, &context, &tx).await;
        }
        process_openai_line_for_anthropic(b"data: [DONE]\n", &mut state, &context, &tx).await;
        drop(tx);

        let mut all = String::new();
        while let Some(item) = rx.recv().await {
            all.push_str(std::str::from_utf8(&item.unwrap()).unwrap());
        }
        assert!(all.contains("\"type\":\"tool_use\""), "{all}");
        assert!(all.contains("\"name\":\"get_weather\""), "{all}");
        assert!(all.contains("\"type\":\"input_json_delta\""), "{all}");
        assert!(all.contains("partial_json"), "{all}");
        assert!(state.has_tool_use);
        assert_eq!(state.finish_reason, None);
    }

    #[test]
    fn anthropic_request_requires_a_model() {
        let missing = json!({"max_tokens": 10, "messages": []});
        assert!(requested_model_of(&missing).is_err());
        let present = json!({"model": "claude-x"});
        assert_eq!(requested_model_of(&present).unwrap(), "claude-x");
    }

    #[test]
    fn stream_usage_merges_anthropic_start_and_delta() {
        // Anthropic sends input tokens in `message_start`...
        let mut parser = UsageParser::new(Instant::now());
        parser.push(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":0}}}\n\n",
        );
        // ...and output tokens later in `message_delta`.
        parser.push(
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        );
        let usage = parser.finish().expect("usage should be parsed");
        // Both sides must survive; replacing would leave one of them at zero.
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.total_tokens, 14);
    }

    #[test]
    fn extracts_openai_usage() {
        let usage = usage_from_value(&json!({
            "usage": { "prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19 }
        }))
        .expect("usage should parse");
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (12, 7, 19)
        );
    }

    #[test]
    fn extracts_nested_responses_usage() {
        let usage = usage_from_value(&json!({
            "type": "response.completed",
            "response": { "usage": { "input_tokens": 88, "output_tokens": 42, "total_tokens": 130 } }
        }))
        .expect("nested responses usage should parse");
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (88, 42, 130)
        );
    }

    #[test]
    fn derives_total_when_upstream_omits_it() {
        let usage = usage_from_value(&json!({
            "usage": { "prompt_tokens": 5, "completion_tokens": 3 }
        }))
        .expect("usage should parse");
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn stream_parser_counts_chat_completion_text() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
        parser.push(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"total_tokens\":4}}\n\n",
        );
        let usage = parser.finish().expect("usage should be parsed");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (2, 2));
    }

    #[test]
    fn stream_parser_records_first_token_once_on_real_content() {
        let mut parser = UsageParser::new(Instant::now());
        // A role-only opening frame and an empty delta must not start the
        // clock; otherwise TTFT would be reported as roughly zero even though
        // the model has not produced anything yet.
        parser.push(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n");
        assert_eq!(parser.first_token_ms, None);
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
        let first = parser.first_token_ms.expect("first token should be timed");
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n");
        assert_eq!(
            parser.first_token_ms,
            Some(first),
            "the timestamp must be captured once and not overwritten"
        );
        parser.finish();
    }

    #[test]
    fn stream_parser_counts_responses_api_deltas() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n");
        parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n");
        let usage = parser
            .finish()
            .expect("estimated usage should be produced from streamed text");
        assert!(
            usage.completion_tokens > 0,
            "estimated completion tokens should be non-zero"
        );
    }

    #[test]
    fn stream_parser_counts_legacy_completions_text() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"data: {\"choices\":[{\"text\":\"hello \"}]}\n\n");
        parser.push(b"data: {\"choices\":[{\"text\":\"world\"}]}\n\n");
        let usage = parser
            .finish()
            .expect("legacy completions text should produce an estimate");
        assert!(
            usage.completion_tokens >= 2,
            "legacy completions text should be counted, got {}",
            usage.completion_tokens
        );
    }

    #[test]
    fn stream_preview_is_bounded() {
        let mut parser = UsageParser::new(Instant::now());
        // Feed far more text than the preview budget across many frames.
        for _ in 0..200 {
            let frame = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
                "x".repeat(100)
            );
            parser.push(frame.as_bytes());
        }
        let preview = parser.preview().expect("preview should exist");
        assert!(
            preview.chars().count() <= PREVIEW_CHAR_LIMIT,
            "preview should be capped at {PREVIEW_CHAR_LIMIT}, got {}",
            preview.chars().count()
        );
    }

    #[test]
    fn anthropic_response_converts_to_openai_shape() {
        let (converted, usage) = convert_anthropic_response(&json!({
            "id": "msg_1",
            "model": "claude-x",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "hi there" }],
            "usage": { "input_tokens": 30, "output_tokens": 12 }
        }));
        assert_eq!(converted["object"], "chat.completion");
        assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
    }

    #[test]
    fn converts_openai_tool_calls_and_results_to_anthropic() {
        let converted = convert_request_to_anthropic(
            &json!({
                "messages": [
                    { "role": "user", "content": "weather in Paris?" },
                    {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_1", "content": "18C" }
                ]
            }),
            "claude-x",
            false,
        );

        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 3);

        let assistant = &messages[1]["content"];
        let blocks = assistant.as_array().expect("assistant content blocks");
        assert_eq!(blocks.len(), 1, "empty text block should be dropped");
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "call_1");
        assert_eq!(blocks[0]["name"], "get_weather");
        assert_eq!(blocks[0]["input"]["city"], "Paris");

        let tool_result = &messages[2]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        assert_eq!(tool_result["tool_use_id"], "call_1");
        assert_eq!(tool_result["content"], "18C");
    }

    #[tokio::test]
    async fn anthropic_stream_tool_use_becomes_openai_tool_calls() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        // Anthropic emits a tool_use block start followed by JSON argument deltas.
        for line in [
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        for line in [
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut frames = Vec::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            frames.push(String::from_utf8_lossy(&bytes).to_string());
        }
        let joined = frames.join("");

        assert!(saw_tool_use, "tool_use block start should be detected");
        assert!(
            joined.contains("\"tool_calls\""),
            "should emit tool_calls delta: {joined}"
        );
        assert!(
            joined.contains("toolu_1"),
            "should carry the tool call id: {joined}"
        );
        assert!(
            joined.contains("get_weather"),
            "should carry the function name: {joined}"
        );
        assert!(
            joined.contains("Paris"),
            "should carry streamed arguments: {joined}"
        );
    }

    #[tokio::test]
    async fn parallel_anthropic_tool_calls_keep_distinct_indices() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        let lines: [&[u8]; 8] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n",
        ];
        for line in lines {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut joined = String::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&bytes));
        }

        assert!(joined.contains("toolu_a"), "first tool call should appear");
        assert!(joined.contains("toolu_b"), "second tool call should appear");
        assert!(
            joined.contains("\"index\":1"),
            "second tool call should keep index 1 rather than collapsing to 0: {joined}"
        );
        assert!(
            joined.contains("get_time"),
            "second tool name should appear"
        );
    }

    #[tokio::test]
    async fn text_block_before_tools_does_not_shift_tool_indices() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        // Anthropic block 0 is text; the tool calls are blocks 1 and 2.
        // OpenAI must renumber the tool calls to 0 and 1.
        let lines: [&[u8]; 10] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n",
        ];
        for line in lines {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut joined = String::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&bytes));
        }

        assert!(
            joined.contains("toolu_a") && joined.contains("get_weather"),
            "first tool call should appear: {joined}"
        );
        assert!(
            joined.contains("\"index\":0") && joined.contains("\"index\":1"),
            "tool calls should be renumbered to 0 and 1: {joined}"
        );
        assert!(
            !joined.contains("\"index\":2"),
            "OpenAI tool indices must not include Anthropic text-block offsets: {joined}"
        );
    }
}
