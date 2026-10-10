use super::*;

/// SSE comment used to keep an idle downstream stream alive.
///
/// A comment line is ignored by every SSE client but still counts as traffic,
/// so a load balancer or proxy idle timeout will not drop the connection while
/// a model is thinking and the upstream has sent nothing.
pub(crate) const SSE_KEEPALIVE_LINE: &[u8] = b": keep-alive\n\n";

/// Waits for upstream data or for the downstream client to disconnect.
///
/// Dropping the response body closes the receiver; observing that immediately
/// lets the spawned translator drop the upstream request instead of holding an
/// abandoned generation open until the next chunk arrives.
///
/// When `keepalive` is set, a silent upstream still emits an SSE comment on
/// that interval so intermediaries do not drop the connection. The comments
/// are written straight to `tx` and never surfaced to the caller's parser.
pub(crate) async fn next_upstream_chunk(
    upstream: &mut UpstreamByteStream,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
    keepalive: Option<Duration>,
) -> Option<Result<Bytes, io::Error>> {
    let Some(interval) = keepalive.filter(|interval| !interval.is_zero()) else {
        return tokio::select! {
            _ = tx.closed() => None,
            chunk = upstream.next() => chunk,
        };
    };
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `interval`'s first tick resolves immediately; consume it so the first
    // keep-alive only fires after a full idle interval, not at t=0.
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = tx.closed() => return None,
            chunk = upstream.next() => return chunk,
            _ = ticker.tick() => {
                if tx
                    .send(Ok(Bytes::from_static(SSE_KEEPALIVE_LINE)))
                    .await
                    .is_err()
                {
                    return None;
                }
            }
        }
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
