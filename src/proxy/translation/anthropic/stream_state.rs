use super::*;

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
    pub(crate) output_chars: usize,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    /// Bounded output preview; never the complete generated response.
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
