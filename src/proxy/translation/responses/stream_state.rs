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
                    "summary": [part],
                    "content": [{"type": "reasoning_text", "text": text}]
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
