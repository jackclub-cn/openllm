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
            "/choices/0/delta/reasoning_text",
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
