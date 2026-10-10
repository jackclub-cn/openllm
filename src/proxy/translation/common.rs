use super::*;

/// Waits for upstream data or for the downstream client to disconnect.
///
/// Dropping the response body closes the receiver; observing that immediately
/// lets the spawned translator drop the upstream request instead of holding an
/// abandoned generation open until the next chunk arrives.
pub(crate) async fn next_upstream_chunk(
    upstream: &mut UpstreamByteStream,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) -> Option<Result<Bytes, io::Error>> {
    tokio::select! {
        _ = tx.closed() => None,
        chunk = upstream.next() => chunk,
    }
}

/// Extracts the required `model` field from a request body.
pub(crate) fn requested_model_of(body: &Value) -> AppResult<String> {
    body.get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::BadRequest("request body must include a model".to_string()))
}

/// Validates Anthropic's required `max_tokens` field.
///
/// Mirrors the upstream contract: it must be present and a positive integer.
/// A zero or negative value would be rejected later anyway, but catching it
/// here yields the precise `invalid_request_error` the client expects.
pub(crate) fn validate_anthropic_max_tokens(body: &Value) -> AppResult<()> {
    match body.get("max_tokens") {
        Some(value) => match value.as_i64() {
            Some(tokens) if tokens > 0 => Ok(()),
            Some(_) => Err(AppError::BadRequest(
                "max_tokens must be a positive integer".to_string(),
            )),
            None => Err(AppError::BadRequest(
                "max_tokens must be an integer".to_string(),
            )),
        },
        None => Err(AppError::BadRequest(
            "max_tokens is required for the Anthropic Messages API".to_string(),
        )),
    }
}

/// Formats one server-sent event the way Anthropic clients expect it.
pub(crate) fn sse_line(event: &str, data: Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    )
}

/// Maps an OpenAI finish reason onto the Anthropic `stop_reason` vocabulary.
pub(crate) fn anthropic_stop_reason(finish: Option<&str>, has_tool_use: bool) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ if has_tool_use => "tool_use",
        _ => "end_turn",
    }
}

/// Wraps a message in Anthropic's error envelope so Anthropic clients can parse
/// gateway-side failures the same way they parse upstream errors.
pub(crate) fn anthropic_error_body(error_type: &str, message: &str) -> Value {
    json!({"type": "error", "error": {"type": error_type, "message": message}})
}

pub(crate) const USAGE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Refreshes a streaming request's activity timestamp at a bounded rate.
pub(crate) struct UsageHeartbeat {
    state: AppState,
    request_id: String,
    last_touch: Instant,
}

impl UsageHeartbeat {
    pub(crate) fn new(state: AppState, request_id: String) -> Self {
        Self {
            state,
            request_id,
            last_touch: Instant::now() - USAGE_HEARTBEAT_INTERVAL,
        }
    }

    pub(crate) async fn touch(&mut self) {
        if self.last_touch.elapsed() < USAGE_HEARTBEAT_INTERVAL {
            return;
        }
        self.last_touch = Instant::now();
        if let Err(error) = sqlx::query(
            "UPDATE usage_logs \
             SET last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE request_id = ? AND in_flight = 1",
        )
        .bind(&self.request_id)
        .execute(&self.state.pool)
        .await
        {
            tracing::warn!(
                %error,
                request_id = self.request_id,
                "failed to update streaming activity heartbeat"
            );
        }
    }
}

pub(crate) fn responses_usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.cache_read_tokens
        },
        "output_tokens": usage.completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": 0
        },
        "total_tokens": usage.total_tokens
    })
}

pub(crate) fn openai_stream_chunk(
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
