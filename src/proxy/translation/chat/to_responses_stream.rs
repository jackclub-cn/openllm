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

    if let Some(reasoning) = chat_reasoning_text(delta) {
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
            text.push_str(&reasoning);
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
