use super::*;

#[derive(Debug)]
pub(crate) enum ResponsesStreamBlock {
    Reasoning {
        item_id: String,
        output_index: usize,
        text: String,
    },
    Text {
        item_id: String,
        output_index: usize,
        text: String,
    },
    Tool {
        item_id: String,
        output_index: usize,
        call_id: Value,
        name: Value,
        arguments: String,
    },
}

pub(crate) struct ResponsesStreamState {
    pub(crate) response_id: String,
    pub(crate) model: String,
    pub(crate) created_at: i64,
    pub(crate) sequence_number: i64,
    pub(crate) created: bool,
    pub(crate) output: Vec<Value>,
    pub(crate) current: Option<ResponsesStreamBlock>,
    pub(crate) usage: Usage,
    pub(crate) stop_reason: Option<String>,
    pub(crate) first_token_ms: Option<i64>,
    pub(crate) output_chars: usize,
    pub(crate) started: Instant,
}

impl ResponsesStreamState {
    pub(crate) fn new(model: String, started: Instant) -> Self {
        Self {
            response_id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            model,
            created_at: chrono::Utc::now().timestamp(),
            sequence_number: 0,
            created: false,
            output: Vec::new(),
            current: None,
            usage: Usage::default(),
            stop_reason: None,
            first_token_ms: None,
            output_chars: 0,
            started,
        }
    }

    pub(crate) fn response_object(&self, status: &str) -> Value {
        let incomplete = status == "incomplete";
        let usage = if status == "in_progress" {
            Value::Null
        } else {
            responses_usage_json(&self.usage)
        };
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "error": Value::Null,
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
            "instructions": Value::Null,
            "max_output_tokens": Value::Null,
            "model": self.model,
            "output": self.output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": Value::Null,
            "store": false,
            "temperature": Value::Null,
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "truncation": "disabled",
            "usage": usage
        })
    }

    pub(crate) async fn send_event(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
        event: &str,
        mut data: Value,
    ) {
        data["sequence_number"] = json!(self.sequence_number);
        self.sequence_number += 1;
        let _ = tx.send(Ok(Bytes::from(sse_line(event, data)))).await;
    }

    pub(crate) async fn ensure_created(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        if self.created {
            return;
        }
        self.created = true;
        let response = self.response_object("in_progress");
        self.send_event(
            tx,
            "response.created",
            json!({"type": "response.created", "response": response.clone()}),
        )
        .await;
        self.send_event(
            tx,
            "response.in_progress",
            json!({"type": "response.in_progress", "response": response}),
        )
        .await;
    }

    pub(crate) fn mark_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.started.elapsed().as_millis() as i64);
        }
    }

    pub(crate) async fn finish_current(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        let Some(current) = self.current.take() else {
            return;
        };
        match current {
            ResponsesStreamBlock::Reasoning {
                item_id,
                output_index,
                text,
            } => {
                self.send_event(
                    tx,
                    "response.reasoning_summary_text.done",
                    json!({
                        "type": "response.reasoning_summary_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "text": text
                    }),
                )
                .await;
                let part = json!({"type": "summary_text", "text": text});
                self.send_event(
                    tx,
                    "response.reasoning_summary_part.done",
                    json!({
                        "type": "response.reasoning_summary_part.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "part": part
                    }),
                )
                .await;
                let item = json!({
                    "id": item_id,
                    "type": "reasoning",
                    "status": "completed",
                    "summary": [part]
                });
                self.send_event(
                    tx,
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                )
                .await;
                self.output.push(item);
            }
            ResponsesStreamBlock::Text {
                item_id,
                output_index,
                text,
            } => {
                self.send_event(
                    tx,
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text
                    }),
                )
                .await;
                let part = json!({
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                    "logprobs": []
                });
                self.send_event(
                    tx,
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": part
                    }),
                )
                .await;
                let item = json!({
                    "id": item_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [part]
                });
                self.send_event(
                    tx,
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                )
                .await;
                self.output.push(item);
            }
            ResponsesStreamBlock::Tool {
                item_id,
                output_index,
                call_id,
                name,
                arguments,
            } => {
                let arguments = if arguments.is_empty() {
                    "{}".to_string()
                } else {
                    arguments
                };
                self.send_event(
                    tx,
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "arguments": arguments
                    }),
                )
                .await;
                let item = json!({
                    "id": item_id,
                    "type": "function_call",
                    "status": "completed",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments
                });
                self.send_event(
                    tx,
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                )
                .await;
                self.output.push(item);
            }
        }
    }

    pub(crate) async fn handle_line(
        &mut self,
        line: &[u8],
        event_name: &mut String,
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
        self.ensure_created(tx).await;

        match event_name.as_str() {
            "message_start" => {
                if let Some(message_usage) = value.pointer("/message/usage") {
                    if let Some(input_tokens) =
                        message_usage.get("input_tokens").and_then(Value::as_i64)
                    {
                        self.usage.prompt_tokens = input_tokens;
                    }
                    self.usage.cache_read_tokens = cache_read_of(message_usage);
                    self.usage.cache_write_tokens = cache_write_of(message_usage);
                }
            }
            "content_block_start" => {
                if self.current.is_some() {
                    self.finish_current(tx).await;
                }
                let content_block = value.get("content_block").unwrap_or(&Value::Null);
                match content_block.get("type").and_then(Value::as_str) {
                    Some("thinking") => {
                        let item_id = format!("rs_{}", uuid::Uuid::new_v4().simple());
                        let output_index = self.output.len();
                        let initial = content_block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.current = Some(ResponsesStreamBlock::Reasoning {
                            item_id: item_id.clone(),
                            output_index,
                            text: initial.clone(),
                        });
                        self.send_event(
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
                        self.send_event(
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
                        if !initial.is_empty() {
                            self.mark_first_token();
                            self.output_chars += initial.chars().count();
                            self.send_event(
                                tx,
                                "response.reasoning_summary_text.delta",
                                json!({
                                    "type": "response.reasoning_summary_text.delta",
                                    "item_id": item_id,
                                    "output_index": output_index,
                                    "summary_index": 0,
                                    "delta": initial
                                }),
                            )
                            .await;
                        }
                    }
                    // `redacted_thinking` is encrypted and cannot be surfaced.
                    Some("redacted_thinking") => {}
                    Some("text") => {
                        let item_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                        let output_index = self.output.len();
                        let initial = content_block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.current = Some(ResponsesStreamBlock::Text {
                            item_id: item_id.clone(),
                            output_index,
                            text: initial.clone(),
                        });
                        self.send_event(
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
                        self.send_event(
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
                        if !initial.is_empty() {
                            self.mark_first_token();
                            self.output_chars += initial.chars().count();
                            self.send_event(
                                tx,
                                "response.output_text.delta",
                                json!({
                                    "type": "response.output_text.delta",
                                    "item_id": item_id,
                                    "output_index": output_index,
                                    "content_index": 0,
                                    "delta": initial
                                }),
                            )
                            .await;
                        }
                    }
                    Some("tool_use") => {
                        let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
                        let output_index = self.output.len();
                        let call_id = content_block.get("id").cloned().unwrap_or_else(|| {
                            json!(format!("call_{}", uuid::Uuid::new_v4().simple()))
                        });
                        let name = content_block.get("name").cloned().unwrap_or(Value::Null);
                        self.current = Some(ResponsesStreamBlock::Tool {
                            item_id: item_id.clone(),
                            output_index,
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        });
                        self.mark_first_token();
                        self.send_event(
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
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let Some(text) = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                        else {
                            return;
                        };
                        let (item_id, output_index) = match self.current.as_ref() {
                            Some(ResponsesStreamBlock::Text {
                                item_id,
                                output_index,
                                ..
                            }) => (item_id.clone(), *output_index),
                            _ => return,
                        };
                        if let Some(ResponsesStreamBlock::Text {
                            text: accumulated, ..
                        }) = self.current.as_mut()
                        {
                            accumulated.push_str(&text);
                        }
                        self.output_chars += text.chars().count();
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.output_text.delta",
                            json!({
                                "type": "response.output_text.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": 0,
                                "delta": text
                            }),
                        )
                        .await;
                    }
                    Some("input_json_delta") => {
                        let Some(partial) = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                        else {
                            return;
                        };
                        let (item_id, output_index) = match self.current.as_ref() {
                            Some(ResponsesStreamBlock::Tool {
                                item_id,
                                output_index,
                                ..
                            }) => (item_id.clone(), *output_index),
                            _ => return,
                        };
                        if let Some(ResponsesStreamBlock::Tool { arguments, .. }) =
                            self.current.as_mut()
                        {
                            arguments.push_str(&partial);
                        }
                        self.output_chars += partial.chars().count();
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.function_call_arguments.delta",
                            json!({
                                "type": "response.function_call_arguments.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "delta": partial
                            }),
                        )
                        .await;
                    }
                    Some("thinking_delta") => {
                        let Some(thinking) = delta.get("thinking").and_then(Value::as_str) else {
                            return;
                        };
                        let (item_id, output_index) = match self.current.as_ref() {
                            Some(ResponsesStreamBlock::Reasoning {
                                item_id,
                                output_index,
                                ..
                            }) => (item_id.clone(), *output_index),
                            _ => return,
                        };
                        if let Some(ResponsesStreamBlock::Reasoning { text, .. }) =
                            self.current.as_mut()
                        {
                            text.push_str(thinking);
                        }
                        self.output_chars += thinking.chars().count();
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.reasoning_summary_text.delta",
                            json!({
                                "type": "response.reasoning_summary_text.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "summary_index": 0,
                                "delta": thinking
                            }),
                        )
                        .await;
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                self.finish_current(tx).await;
            }
            "message_delta" => {
                if let Some(output_tokens) = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64)
                {
                    self.usage.completion_tokens = output_tokens;
                }
                if let Some(stop_reason) =
                    value.pointer("/delta/stop_reason").and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason.to_string());
                }
            }
            _ => {}
        }
    }

    pub(crate) async fn complete(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        self.finish_current(tx).await;
        let status = if self.stop_reason.as_deref() == Some("max_tokens") {
            "incomplete"
        } else {
            "completed"
        };
        let response = self.response_object(status);
        let event = if status == "incomplete" {
            "response.incomplete"
        } else {
            "response.completed"
        };
        self.send_event(tx, event, json!({"type": event, "response": response}))
            .await;
    }

    pub(crate) async fn fail(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
        message: &str,
    ) {
        self.finish_current(tx).await;
        let mut response = self.response_object("failed");
        response["error"] = json!({"code": "upstream_error", "message": message});
        self.send_event(
            tx,
            "response.failed",
            json!({"type": "response.failed", "response": response}),
        )
        .await;
    }
}

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
                process_chat_chunk_line(&mut stream, &line, &mut tool_index, &tx).await;
            }
        }
        if !buffer.is_empty() {
            process_chat_chunk_line(&mut stream, &buffer, &mut tool_index, &tx).await;
        }

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
                stream.handle_line(&line, &mut event_name, &tx).await;
            }
        }
        if !buffer.is_empty() {
            stream.handle_line(&buffer, &mut event_name, &tx).await;
        }

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

/// Converts a legacy OpenAI `/v1/completions` request into the Anthropic
/// Messages shape by way of the chat shape that `convert_request_to_anthropic`
/// already understands. The legacy `prompt` field replaces the message list.
/// Converts an OpenAI chat-completions request into the Responses shape.
///
/// This is the inverse of [`responses_request_to_chat`] and lets a
/// Responses-only upstream serve callers that speak `/v1/chat/completions`.
pub(crate) fn chat_request_to_responses(input: &Value, model: &str, streamed: bool) -> Value {
    let mut instructions = Vec::new();
    let mut items = Vec::new();
    if let Some(messages) = input.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content)
                    && !text.is_empty()
                {
                    instructions.push(text);
                }
                continue;
            }
            if role == "tool" {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "output": content_text(&content).unwrap_or_default()
                }));
                continue;
            }
            let text_type = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            let parts = chat_content_to_responses_parts(&content, text_type);
            if !parts.is_empty() {
                items.push(json!({"type": "message", "role": role, "content": parts}));
            }
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
            {
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call
                            .get("id")
                            .or_else(|| call.get("call_id"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": match function.get("arguments") {
                            Some(Value::String(text)) => json!(text),
                            Some(other) => json!(other.to_string()),
                            None => json!("{}"),
                        }
                    }));
                }
            }
        }
    }

    let mut output = json!({"model": model, "input": items});
    if !instructions.is_empty() {
        output["instructions"] = json!(instructions.join("\n\n"));
    }
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_tokens", "max_output_tokens"),
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("parallel_tool_calls", "parallel_tool_calls"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(effort) = input.get("reasoning_effort") {
        output["reasoning"] = json!({"effort": effort.clone()});
    }
    if let Some(format) = input
        .get("response_format")
        .and_then(chat_format_to_responses)
    {
        output["text"] = json!({"format": format});
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(chat_tool_to_responses)
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
                json!({"type": "function", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

pub(crate) fn chat_content_to_responses_parts(content: &Value, text_type: &str) -> Vec<Value> {
    match content {
        Value::String(text) => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": text_type, "text": text})]
            }
        }
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                Some("image_url") => item
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .map(|url| json!({"type": "input_image", "image_url": url})),
                _ => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!({"type": text_type, "text": text})),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Maps a chat-completions `response_format` onto the Responses `text.format`.
pub(crate) fn chat_format_to_responses(format: &Value) -> Option<Value> {
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => Some(json!({"type": "json_object"})),
        Some("json_schema") => {
            let inner = format.get("json_schema").cloned().unwrap_or(Value::Null);
            let mut mapped = inner;
            if let Some(object) = mapped.as_object_mut() {
                object.insert("type".to_string(), json!("json_schema"));
            }
            Some(mapped)
        }
        _ => None,
    }
}

pub(crate) fn chat_tool_to_responses(tool: &Value) -> Option<Value> {
    let function = tool.get("function").unwrap_or(tool);
    function.get("name")?;
    Some(json!({
        "type": "function",
        "name": function.get("name")?.clone(),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    }))
}

pub(crate) fn chat_response_to_responses(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut output = Vec::new();
    if let Some(reasoning) = message
        .and_then(|message| {
            message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
        })
        .and_then(Value::as_str)
        .filter(|reasoning| !reasoning.is_empty())
    {
        output.push(json!({
            "id": format!("rs_{}", uuid::Uuid::new_v4().simple()),
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": reasoning}]
        }));
    }
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
                "logprobs": []
            }]
        }));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let call_id = call
                .get("id")
                .cloned()
                .unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple())));
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!("{}"));
            output.push(json!({
                "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "arguments": match arguments {
                    Value::String(_) => arguments,
                    other => json!(serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()))
                }
            }));
        }
    }

    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let incomplete = finish_reason == Some("length");
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("resp_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let created_at = value
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "response",
            "created_at": created_at,
            "status": if incomplete { "incomplete" } else { "completed" },
            "error": Value::Null,
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
            "instructions": Value::Null,
            "max_output_tokens": Value::Null,
            "model": requested_model,
            "output": output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": Value::Null,
            "store": false,
            "temperature": Value::Null,
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "truncation": "disabled",
            "usage": responses_usage_json(&usage)
        }),
        usage,
    )
}
