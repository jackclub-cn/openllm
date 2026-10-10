use super::*;

pub(crate) struct UsageParser {
    pub(in crate::proxy) buffer: Vec<u8>,
    pub(in crate::proxy) usage: Option<Usage>,
    pub(in crate::proxy) output_chars: usize,
    pub(in crate::proxy) text: String,
    pub(in crate::proxy) started_at: Instant,
    pub(in crate::proxy) first_token_ms: Option<i64>,
}

impl UsageParser {
    pub(in crate::proxy) fn new(started_at: Instant) -> Self {
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
    pub(in crate::proxy) fn mark_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.started_at.elapsed().as_millis() as i64);
        }
    }

    pub(in crate::proxy) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=position).collect::<Vec<_>>();
            self.parse_line(&line);
        }
    }

    pub(in crate::proxy) fn finish(&mut self) -> Option<Usage> {
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
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            None => None,
        }
    }

    pub(in crate::proxy) fn preview(&self) -> Option<String> {
        response_preview(self.text.as_bytes())
    }

    pub(in crate::proxy) fn parse_line(&mut self, line: &[u8]) {
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
                    // Cache counts arrive alongside input tokens in the same
                    // event, so take the newer value but keep the older one
                    // when this event simply did not mention caching.
                    cache_read_tokens: if usage.cache_read_tokens > 0 {
                        usage.cache_read_tokens
                    } else {
                        previous.cache_read_tokens
                    },
                    cache_write_tokens: if usage.cache_write_tokens > 0 {
                        usage.cache_write_tokens
                    } else {
                        previous.cache_write_tokens
                    },
                }
                .normalized(),
                None => usage,
            });
        }
        // Native Anthropic streams emit text, reasoning, and tool arguments as
        // content block deltas instead of OpenAI choices.
        if let Some(delta) = value.get("delta") {
            let delta_type = delta.get("type").and_then(Value::as_str);
            let content = match delta_type {
                Some("text_delta") => delta.get("text").and_then(Value::as_str),
                Some("thinking_delta") => delta.get("thinking").and_then(Value::as_str),
                Some("input_json_delta") => delta.get("partial_json").and_then(Value::as_str),
                _ => None,
            };
            if let Some(content) = content.filter(|content| !content.is_empty()) {
                self.mark_first_token();
                self.output_chars += content.chars().count();
                if delta_type == Some("text_delta") {
                    push_preview_text(&mut self.text, content);
                }
            }
        }
        // Tool names arrive before their argument deltas. Count the block start
        // as the first output token for tool-only responses.
        if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
            self.mark_first_token();
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
        for path in [
            "/choices/0/delta/reasoning_content",
            "/choices/0/delta/reasoning",
        ] {
            if let Some(delta) = value.pointer(path).and_then(Value::as_str)
                && !delta.is_empty()
            {
                self.mark_first_token();
                self.output_chars += delta.chars().count();
            }
        }
        if let Some(tool_calls) = value
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in tool_calls {
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                    && !arguments.is_empty()
                {
                    self.mark_first_token();
                    self.output_chars += arguments.chars().count();
                }
            }
        }
        if let Some(arguments) = value
            .pointer("/choices/0/delta/function_call/arguments")
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            self.mark_first_token();
            self.output_chars += arguments.chars().count();
        }
        // Responses API emits several output delta families: visible text,
        // reasoning summaries, and function-call arguments. Tool-only turns
        // still produce tokens, so any non-empty `response.*.delta` starts the
        // first-token clock.
        if let Some(kind) = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| kind.starts_with("response.") && kind.ends_with(".delta"))
            && let Some(delta) = value.get("delta").and_then(Value::as_str)
            && !delta.is_empty()
        {
            self.mark_first_token();
            self.output_chars += delta.chars().count();
            if kind == "response.output_text.delta" {
                push_preview_text(&mut self.text, delta);
            }
        }
    }
}

pub(crate) fn extract_usage_from_json(bytes: &[u8]) -> Option<Usage> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    usage_from_value(&value)
}

pub(crate) fn usage_from_value(value: &Value) -> Option<Usage> {
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
    let cache_read_tokens = cache_read_of(usage);
    let cache_write_tokens = cache_write_of(usage);
    // Normalise input accounting across providers.
    //
    // OpenAI-style responses already fold cache traffic into `prompt_tokens`
    // (verified: prompt_tokens stays constant while `cached_tokens` rises), so
    // the value is used as-is. Anthropic reports the cache numbers *besides*
    // `input_tokens`, so the totals only add up once they are included.
    // The discriminator is placement: nested under `prompt_tokens_details` means
    // already counted; top-level means additional.
    let cache_included_in_prompt = usage.pointer("/prompt_tokens_details").is_some()
        || usage.pointer("/input_tokens_details").is_some();
    let prompt_tokens = if cache_included_in_prompt {
        prompt_tokens
    } else {
        prompt_tokens + cache_read_tokens + cache_write_tokens
    };
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(prompt_tokens + completion_tokens)
        .max(prompt_tokens + completion_tokens);
    Some(
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
            cache_write_tokens,
        }
        .normalized(),
    )
}

/// Reads cache-read tokens across provider shapes.
///
/// Anthropic reports `cache_read_input_tokens` at the top level; OpenAI-style
/// providers nest `cached_tokens` (and, on some gateways,
/// `cache_read_input_tokens`) under `prompt_tokens_details`.
pub(crate) fn cache_read_of(usage: &Value) -> i64 {
    [
        usage.get("cache_read_input_tokens"),
        usage.pointer("/prompt_tokens_details/cached_tokens"),
        usage.pointer("/prompt_tokens_details/cache_read_input_tokens"),
        usage.pointer("/input_tokens_details/cached_tokens"),
        usage.get("cache_read_tokens"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
    .unwrap_or_default()
}

/// Reads cache-write tokens across provider shapes. Anthropic calls these
/// `cache_creation_input_tokens`; some OpenAI-compatible gateways use
/// `cache_write_tokens`.
pub(crate) fn cache_write_of(usage: &Value) -> i64 {
    [
        usage.get("cache_creation_input_tokens"),
        usage.get("cache_write_tokens"),
        usage.pointer("/prompt_tokens_details/cache_write_tokens"),
        usage.pointer("/prompt_tokens_details/cache_creation_input_tokens"),
        usage.pointer("/input_tokens_details/cache_write_tokens"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
    .unwrap_or_default()
}

pub(crate) fn estimated_completion_usage(request_tokens: i64, bytes: &[u8]) -> Usage {
    let completion_tokens = (bytes.len() / 4).max(1) as i64;
    Usage::new(request_tokens, completion_tokens)
}

pub(crate) fn estimate_request_tokens(value: &Value) -> i64 {
    // Walk the structured content and sum only text-bearing fields. The
    // previous implementation stringified the whole payload, so JSON keys,
    // braces and quotes inflated the estimate on every request.
    let chars = count_text_chars(value, None);
    // Rough per-item framing overhead: providers wrap each message and tool in
    // a handful of structural tokens beyond its visible text.
    let items = value
        .get("messages")
        .or_else(|| value.get("input"))
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.len())
        .unwrap_or(0);
    let framing = (items as i64) * 4 + (tools as i64) * 8;
    (chars as i64 / 4 + framing).max(1)
}

/// Field names whose string values represent prompt content worth counting.
/// Anything else (ids, roles, media URLs, base64 blobs) is ignored so the
/// estimate tracks text, not wire format.
pub(crate) const TEXT_BEARING_KEYS: [&str; 9] = [
    "text",
    "content",
    "input",
    "prompt",
    "system",
    "description",
    "arguments",
    "partial_json",
    "name",
];

/// Sums the characters of text-bearing strings nested anywhere in `value`.
///
/// A string is counted only when its key is text-bearing, or when it is the
/// root value (as in a bare `input: "..."` payload). This deliberately skips
/// `image_url` / base64 data, which providers meter separately from text.
pub(crate) fn count_text_chars(value: &Value, key: Option<&str>) -> usize {
    match value {
        Value::String(text) => {
            if key.is_none_or(|key| TEXT_BEARING_KEYS.contains(&key)) {
                text.chars().count()
            } else {
                0
            }
        }
        Value::Array(items) => items.iter().map(|item| count_text_chars(item, key)).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| count_text_chars(value, Some(key)))
            .sum(),
        _ => 0,
    }
}

pub(crate) fn fill_usage(usage: Usage, request_tokens: i64, response: &Value) -> Usage {
    let mut usage = usage;
    if usage.prompt_tokens == 0 {
        usage.prompt_tokens = request_tokens;
    }
    if usage.completion_tokens == 0 {
        usage.completion_tokens = (response.to_string().chars().count() / 4).max(1) as i64;
    }
    usage.normalized()
}

pub(crate) fn content_text(value: &Value) -> Option<String> {
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

/// Downward compatibility for multi-target routes.
///
/// A route may fan out to models with different ceilings. Clamping the
/// requested output length to the strictest common `output_limit` keeps the
/// request valid for *every* target, so whichever one the strategy picks can
/// serve it instead of failing with a "max_tokens too large" error.
///
/// Returns the clamped value when a reduction happened, so the caller can
/// surface it in the response receipt.
pub(crate) fn clamp_output_request(
    body: &mut Value,
    barrel: Option<&BarrelEnvelope>,
) -> Option<i64> {
    let limit = barrel
        .and_then(|barrel| barrel.capabilities.as_ref())
        .and_then(|capabilities| capabilities.output_limit)?;
    let mut clamped = None;
    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if let Some(requested) = body.get(key).and_then(Value::as_i64)
            && requested > limit
        {
            body[key] = json!(limit);
            clamped = Some(limit);
        }
    }
    clamped
}

pub(crate) fn requested_output_tokens_of(body: &Value) -> Option<i64> {
    ["max_tokens", "max_completion_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| body.get(key).and_then(Value::as_i64))
}

/// Builds the capability receipt returned alongside a completion.
///
/// `requested_output_tokens` and `clamped_output_tokens` make the barrel
/// behaviour visible: a client that asked for more than the route supports can
/// see that the gateway reduced the request rather than silently ignoring it.
pub(crate) fn capability_receipt(
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
pub(crate) fn inject_capability_receipt(bytes: &[u8], receipt: &Option<Value>) -> Option<Vec<u8>> {
    let receipt = receipt.as_ref()?;
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    value
        .as_object_mut()?
        .insert("capabilities".to_string(), receipt.clone());
    serde_json::to_vec(&value).ok()
}

/// Limits mirrored onto response headers so streaming clients, which never
/// receive a single JSON body, can still read the effective ceiling.
pub(crate) fn apply_capability_headers(response: &mut Response, receipt: &Option<Value>) {
    let Some(receipt) = receipt.as_ref() else {
        return;
    };
    if let Some(output) = receipt.get("output_limit").and_then(Value::as_i64)
        && let Ok(value) = HeaderValue::from_str(&output.to_string())
    {
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
    let input = match (
        receipt.get("input_limit").and_then(Value::as_i64),
        receipt.get("context_limit").and_then(Value::as_i64),
    ) {
        (Some(input), Some(context)) => Some(input.min(context)),
        (Some(input), None) => Some(input),
        (None, Some(context)) => Some(context),
        (None, None) => None,
    };
    if let Some(input) = input
        && let Ok(value) = HeaderValue::from_str(&input.to_string())
    {
        response
            .headers_mut()
            .insert("x-openllm-max-input-tokens", value);
    }
}

/// Keep a short, human-readable slice of the response for later debugging
/// without storing unbounded payloads.
pub(crate) fn response_preview(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = trimmed.chars().take(2000).collect::<String>();
    Some(preview)
}

/// Builds a bounded request preview with common credential fields masked.
///
/// Stored previews are opt-in because prompts may contain sensitive data. The
/// masking pass covers the usual secret-bearing keys without touching normal
/// budget fields such as `max_tokens`.
pub(crate) fn request_preview(body: &Value, max_chars: usize) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let masked = mask_sensitive_values(body);
    let text = serde_json::to_string_pretty(&masked).ok()?;
    let preview = text.trim().chars().take(max_chars).collect::<String>();
    (!preview.is_empty()).then_some(preview)
}

fn mask_sensitive_values(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = if sensitive_request_key(key) {
                        Value::String("<redacted>".to_string())
                    } else {
                        mask_sensitive_values(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(mask_sensitive_values).collect())
        }
        _ => value.clone(),
    }
}

fn sensitive_request_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key"
            | "apikey"
            | "authorization"
            | "password"
            | "secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "cookie"
            | "set-cookie"
    ) || key.ends_with("_token")
        || key.ends_with("-token")
}

/// Upper bound on how much streamed text is retained for the preview. Anything
/// beyond this is discarded as it arrives, so a very long generation does not
/// buffer its entire output in memory just to store 2000 characters.
pub(crate) const PREVIEW_CHAR_LIMIT: usize = 2000;

/// Appends `chunk` to `target` only while the preview budget allows it.
pub(crate) fn push_preview_text(target: &mut String, chunk: &str) {
    let remaining = PREVIEW_CHAR_LIMIT.saturating_sub(target.chars().count());
    if remaining == 0 {
        return;
    }
    target.extend(chunk.chars().take(remaining));
}

pub(crate) struct UsageLogEntry<'a> {
    pub(in crate::proxy) request_id: &'a str,
    pub(in crate::proxy) api_key_id: Option<i64>,
    pub(in crate::proxy) route_id: Option<i64>,
    pub(in crate::proxy) provider_id: Option<i64>,
    pub(in crate::proxy) requested_model: &'a str,
    pub(in crate::proxy) upstream_model: Option<&'a str>,
    pub(in crate::proxy) endpoint: &'a str,
    pub(in crate::proxy) usage: Usage,
    pub(in crate::proxy) latency_ms: i64,
    pub(in crate::proxy) first_token_ms: Option<i64>,
    pub(in crate::proxy) status_code: i64,
    pub(in crate::proxy) success: bool,
    pub(in crate::proxy) streamed: bool,
    pub(in crate::proxy) error_message: Option<&'a str>,
    pub(in crate::proxy) response_preview: Option<&'a str>,
}

/// Owned mirror of [`UsageLogEntry`]; needed when the log is written from a
/// detached task that outlives the request's borrowed values.
pub(crate) struct OwnedUsageLogEntry {
    pub(in crate::proxy) request_id: String,
    pub(in crate::proxy) api_key_id: Option<i64>,
    pub(in crate::proxy) route_id: Option<i64>,
    pub(in crate::proxy) provider_id: Option<i64>,
    pub(in crate::proxy) requested_model: String,
    pub(in crate::proxy) upstream_model: Option<String>,
    pub(in crate::proxy) endpoint: String,
    pub(in crate::proxy) usage: Usage,
    pub(in crate::proxy) latency_ms: i64,
    pub(in crate::proxy) first_token_ms: Option<i64>,
    pub(in crate::proxy) status_code: i64,
    pub(in crate::proxy) success: bool,
    pub(in crate::proxy) streamed: bool,
    pub(in crate::proxy) error_message: Option<String>,
    pub(in crate::proxy) response_preview: Option<String>,
}

impl OwnedUsageLogEntry {
    pub(in crate::proxy) fn as_borrowed(&self) -> UsageLogEntry<'_> {
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
pub(crate) fn log_usage_detached(state: AppState, entry: OwnedUsageLogEntry) {
    tokio::spawn(async move {
        log_usage(&state, entry.as_borrowed()).await;
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn log_usage_started(
    state: &AppState,
    request_id: &str,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
    request_preview: Option<&str>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, session_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, request_preview, response_preview,
            last_activity_at
        ) VALUES (
            ?, ?, ?, ?, NULL, ?, NULL, ?, ?, 0, ?, 0, 0, 0, NULL, NULL, 0, 1, 0, ?,
            NULL, ?, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            session_id = COALESCE(excluded.session_id, usage_logs.session_id),
            route_id = excluded.route_id,
            requested_model = excluded.requested_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            total_tokens = excluded.total_tokens,
            streamed = excluded.streamed,
            request_preview = COALESCE(excluded.request_preview, usage_logs.request_preview),
            last_activity_at = excluded.last_activity_at,
            in_flight = 1
        WHERE usage_logs.in_flight = 1
        "#,
    )
    .bind(request_id)
    .bind(session_id)
    .bind(api_key_id)
    .bind(route_id)
    .bind(requested_model)
    .bind(endpoint)
    .bind(request_tokens)
    .bind(request_tokens)
    .bind(streamed as i64)
    .bind(request_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(result) => {
            if result.rows_affected() > 0 {
                match sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                    .bind(request_id)
                    .fetch_one(&state.pool)
                    .await
                {
                    Ok(id) => {
                        let _ = state.events.send(crate::state::UsageEvent {
                            id,
                            request_id: request_id.to_string(),
                            success: false,
                            streamed,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, request_id, "failed to find in-flight usage row");
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, request_id, "failed to write in-flight usage log");
        }
    }
}

pub(crate) async fn log_usage_target(
    state: &AppState,
    request_id: &str,
    provider_id: i64,
    upstream_model: &str,
    provider_api_key_id: Option<i64>,
) {
    if let Err(error) = sqlx::query(
        "UPDATE usage_logs SET provider_id = ?, provider_api_key_id = ?, \
             provider_api_key_name = (SELECT name FROM provider_api_keys WHERE id = ?), \
             upstream_model = ?, \
             last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE request_id = ? AND in_flight = 1",
    )
    .bind(provider_id)
    .bind(provider_api_key_id)
    .bind(provider_api_key_id)
    .bind(upstream_model)
    .bind(request_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, request_id, "failed to update in-flight usage target");
    }
}

pub(crate) async fn log_usage_warning(state: &AppState, request_id: &str, warning: &str) {
    let warning = warning.chars().take(500).collect::<String>();
    if let Err(error) = sqlx::query(
        "UPDATE usage_logs \
         SET warning_message = CASE \
             WHEN warning_message IS NULL OR warning_message = '' THEN ? \
             WHEN instr(warning_message, ?) > 0 THEN warning_message \
             ELSE warning_message || char(10) || ? \
         END \
         WHERE request_id = ?",
    )
    .bind(&warning)
    .bind(&warning)
    .bind(&warning)
    .bind(request_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, request_id, "failed to update usage warning");
    }
}

pub(crate) async fn log_usage(state: &AppState, entry: UsageLogEntry<'_>) {
    let usage = entry.usage.normalized();
    let estimated_cost_micros =
        if matches!(entry.status_code, 400 | 403 | 429) && entry.provider_id.is_none() {
            Some(0)
        } else {
            match (entry.provider_id, entry.upstream_model) {
                (Some(provider_id), Some(upstream_model)) if usage.has_tokens() => {
                    estimate_usage_cost(state, provider_id, upstream_model, usage).await
                }
                _ => None,
            }
        };
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            api_key_id = excluded.api_key_id,
            route_id = excluded.route_id,
            provider_id = excluded.provider_id,
            requested_model = excluded.requested_model,
            upstream_model = excluded.upstream_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            completion_tokens = excluded.completion_tokens,
            total_tokens = excluded.total_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cache_write_tokens = excluded.cache_write_tokens,
            latency_ms = excluded.latency_ms,
            estimated_cost_micros = excluded.estimated_cost_micros,
            first_token_ms = excluded.first_token_ms,
            status_code = excluded.status_code,
            in_flight = 0,
            success = excluded.success,
            streamed = excluded.streamed,
            error_message = excluded.error_message,
            response_preview = excluded.response_preview,
            last_activity_at = excluded.last_activity_at
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
    .bind(usage.cache_read_tokens)
    .bind(usage.cache_write_tokens)
    .bind(entry.latency_ms)
    .bind(estimated_cost_micros)
    .bind(entry.first_token_ms)
    .bind(entry.status_code)
    .bind(entry.success as i64)
    .bind(entry.streamed as i64)
    .bind(entry.error_message)
    .bind(entry.response_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => {
            let id = sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                .bind(entry.request_id)
                .fetch_one(&state.pool)
                .await;
            match id {
                Ok(id) => {
                    let _ = state.events.send(crate::state::UsageEvent {
                        id,
                        request_id: entry.request_id.to_string(),
                        success: entry.success,
                        streamed: entry.streamed,
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_id = entry.request_id,
                        "failed to read usage log id after update"
                    );
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, request_id = %entry.request_id, "failed to write usage log");
        }
    }
}

pub(crate) async fn estimate_usage_cost(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
    usage: Usage,
) -> Option<i64> {
    let row = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
        ),
    >(
        "SELECT cost, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models \
         WHERE provider_id = ? AND model_name = ? AND enabled = 1",
    )
    .bind(provider_id)
    .bind(upstream_model)
    .fetch_optional(&state.pool)
    .await
    .ok()??;
    let synced = row
        .0
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let cost = effective_cost_value(synced.as_ref(), row.1, row.2, row.3, row.4)?;
    estimate_cost_micros(Some(&cost), usage)
}
