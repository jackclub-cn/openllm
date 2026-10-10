use super::*;
use futures_util::TryStreamExt;

const STREAM_RECOVERY_HOLDBACK: Duration = Duration::from_millis(750);
const STREAM_RECOVERY_MAX_BYTES: usize = 65_536;

pub(crate) type UpstreamByteStream =
    futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>>;

/// An upstream response whose body can be safely replayed after a bounded
/// pre-commit stream probe.
pub(crate) struct UpstreamResponse {
    status: StatusCode,
    headers: HeaderMap,
    stream: UpstreamByteStream,
}

impl UpstreamResponse {
    pub(crate) fn new(response: reqwest::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let stream = response
            .bytes_stream()
            .map_err(std::io::Error::other)
            .boxed();
        Self {
            status,
            headers,
            stream,
        }
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn from_stream(status: StatusCode, stream: UpstreamByteStream) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            stream,
        }
    }

    pub(crate) fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(crate) async fn bytes(mut self) -> Result<Bytes, std::io::Error> {
        collect_bytes_limited(&mut self.stream, crate::state::max_upstream_body_bytes()).await
    }

    pub(crate) fn into_stream(self) -> UpstreamByteStream {
        self.stream
    }

    /// Holds the opening stream window long enough to retry a cutoff that
    /// happened before any bytes reached the client. A terminal SSE marker,
    /// the byte cap, or the holdback deadline commits the buffered prefix.
    ///
    /// The window is released the moment it carries usable assistant output so
    /// a healthy stream pays no extra time-to-first-token: the holdback is only
    /// spent while the turn is still content-free, which is exactly the state a
    /// transparent retry is allowed to discard.
    pub(crate) async fn recover_prefix(mut self) -> Result<Self, String> {
        let mut chunks = Vec::<Bytes>::new();
        let mut raw = Vec::new();

        let deadline = tokio::time::sleep(STREAM_RECOVERY_HOLDBACK);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                chunk = self.stream.next() => {
                    match chunk {
                        Some(Ok(chunk)) => {
                            raw.extend_from_slice(&chunk);
                            chunks.push(chunk);
                            if raw.len() >= STREAM_RECOVERY_MAX_BYTES
                                || stream_has_terminal_marker(&raw)
                                || stream_has_usable_content(&raw)
                            {
                                break;
                            }
                        }
                        Some(Err(error)) => return Err(error.to_string()),
                        None => {
                            // A provider that closes the connection without a
                            // terminal marker is only treated as truncated when
                            // the window never carried usable content. Closing
                            // after delivering content is a legitimate end for
                            // several OpenAI-compatible servers, and replaying
                            // such a turn would burn a whole extra generation
                            // and then fail if it happened again.
                            if !stream_has_terminal_marker(&raw)
                                && !stream_has_usable_content(&raw)
                            {
                                return Err(
                                    "upstream stream ended without any content".to_string()
                                );
                            }
                            break;
                        }
                    }
                }
                _ = &mut deadline => break,
            }
        }

        Ok(self.prepend_prefix(chunks))
    }

    fn prepend_prefix(self, chunks: Vec<Bytes>) -> Self {
        let Self {
            status,
            headers,
            stream,
        } = self;
        let prefix = futures_util::stream::iter(chunks.into_iter().map(Ok));
        Self {
            status,
            headers,
            stream: prefix.chain(stream).boxed(),
        }
    }
}

/// Reads an upstream body into memory, refusing to grow past `limit`.
///
/// A non-streaming call buffers the whole response, so an upstream that never
/// ends (or a hostile one) would otherwise grow the buffer without bound. When
/// the cap is crossed the read stops and the caller sees an upstream error
/// instead of the process running out of memory.
pub(crate) async fn collect_bytes_limited(
    stream: &mut UpstreamByteStream,
    limit: usize,
) -> Result<Bytes, std::io::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(std::io::Error::other(format!(
                "upstream response exceeded the {} byte limit",
                limit
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

fn stream_has_terminal_marker(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    [
        "[DONE]",
        "message_stop",
        "response.completed",
        "response.failed",
        "response.incomplete",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Whether the held window already carries assistant output worth keeping.
///
/// The check spans every protocol the gateway translates between, because the
/// holdback runs before the shape is known: an OpenAI chat chunk, a Responses
/// event, or a native Anthropic event all have to release the window.
pub(crate) fn stream_has_usable_content(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut saw_sse_field = false;
    let mut saw_bare_payload = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            saw_sse_field = true;
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                return true;
            }
            if let Ok(value) = serde_json::from_str::<Value>(data)
                && event_has_usable_content(&value)
            {
                return true;
            }
        } else if line.starts_with("event:") {
            saw_sse_field = true;
        } else {
            saw_bare_payload = true;
        }
    }
    // A 200 that is not an event stream is a complete body, whatever the client
    // asked for, so there is nothing to recover by holding it.
    !saw_sse_field && saw_bare_payload
}

fn event_has_usable_content(value: &Value) -> bool {
    let non_empty_str = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    };

    // OpenAI chat-completions and legacy completions chunks.
    if let Some(delta) = value.pointer("/choices/0/delta") {
        if non_empty_str(delta.get("content")) || non_empty_str(delta.get("reasoning_content")) {
            return true;
        }
        if delta.get("tool_calls").is_some_and(|calls| !calls.is_null()) {
            return true;
        }
        if delta
            .get("function_call")
            .is_some_and(|call| !call.is_null())
        {
            return true;
        }
    }
    if non_empty_str(value.pointer("/choices/0/text")) {
        return true;
    }

    match value.get("type").and_then(Value::as_str) {
        // Responses API events.
        Some("response.output_text.delta")
        | Some("response.reasoning_summary_text.delta")
        | Some("response.function_call_arguments.delta") => {
            return non_empty_str(value.get("delta"));
        }
        Some("response.output_item.added") => {
            return value.pointer("/item/type").and_then(Value::as_str) == Some("function_call");
        }
        // Anthropic Messages events.
        Some("content_block_start") => {
            return value
                .pointer("/content_block/type")
                .and_then(Value::as_str)
                == Some("tool_use");
        }
        Some("content_block_delta") => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            return match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => non_empty_str(delta.get("text")),
                Some("thinking_delta") => non_empty_str(delta.get("thinking")),
                Some("input_json_delta") => non_empty_str(delta.get("partial_json")),
                _ => false,
            };
        }
        _ => {}
    }
    false
}

/// Builds the error used when a retryable upstream failure should move on to
/// the next route target.
///
/// A provider rate limit keeps its status and `Retry-After` all the way through
/// the fallback loop, so that when *every* target is rate limited the client
/// still receives a `429` it can back off from instead of an opaque `502`.
pub(crate) fn upstream_failure_error(
    status: StatusCode,
    message: String,
    retry_after: Option<Duration>,
) -> AppError {
    if status == StatusCode::TOO_MANY_REQUESTS {
        AppError::UpstreamStatus {
            status,
            message,
            retry_after,
        }
    } else {
        AppError::Upstream(message)
    }
}

pub(crate) fn upstream_rejects_tool_search(body: &[u8]) -> bool {
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    if !message.contains("tool_search") {
        return false;
    }
    [
        "unknown tool type",
        "unsupported tool type",
        "invalid tool type",
        "tool.type",
        "tool_search is not supported",
        "tool_search not supported",
        "tool_search is unsupported",
        "does not support tool_search",
        "doesn't support tool_search",
        "unsupported tool_search",
        "unknown tool: tool_search",
        "unsupported tool: tool_search",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upstream_error_response(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    target: &RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    status: StatusCode,
    response_headers: HeaderMap,
    response_body: Bytes,
) -> AppResult<Response> {
    let message = String::from_utf8_lossy(&response_body)
        .chars()
        .take(600)
        .collect::<String>();

    record_upstream_failure(state, target, status, &response_headers, &message).await;

    if should_try_next_target(target, status) {
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            %status,
            "upstream failed, trying next route target"
        );
        return Err(upstream_failure_error(
            status,
            format!("{} returned {}: {}", target.provider_name, status, message),
            retry_after_from_headers(&response_headers),
        ));
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
            usage: Usage::new(request_tokens, 0),
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
    Ok(builder
        .body(Body::from(response_body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_to_target(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    request_json: &Value,
    _raw_body: &Bytes,
    session_id: Option<&str>,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let provider_type =
        ProviderType::from_str(&target.provider_type).map_err(AppError::BadRequest)?;
    let upstream_endpoint = target_upstream_endpoint(&target, endpoint).unwrap_or(endpoint);
    // Chat completions upstreams can serve a Responses client through
    // translation; everything else is a straight passthrough.
    let translate_responses_to_chat =
        endpoint == OPENAI_RESPONSES && upstream_endpoint == OPENAI_CHAT_COMPLETIONS;
    let translate_chat_to_responses =
        endpoint == OPENAI_CHAT_COMPLETIONS && upstream_endpoint == OPENAI_RESPONSES;
    // Legacy completions clients are served through chat or Responses when the
    // upstream does not expose `/v1/completions` itself.
    let translate_completions_to_chat =
        endpoint == OPENAI_COMPLETIONS && upstream_endpoint == OPENAI_CHAT_COMPLETIONS;
    let translate_completions_to_responses =
        endpoint == OPENAI_COMPLETIONS && upstream_endpoint == OPENAI_RESPONSES;

    let (url, mut request_body) = match provider_type {
        ProviderType::Anthropic => {
            let body = match endpoint {
                OPENAI_RESPONSES => {
                    responses_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                OPENAI_COMPLETIONS => {
                    completions_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                _ => convert_request_to_anthropic(request_json, &target.upstream_model, streamed),
            };
            (join_upstream_url(&target.base_url, "/v1/messages"), body)
        }
        ProviderType::Openai | ProviderType::Ollama | ProviderType::Custom => {
            let body = if translate_completions_to_responses {
                let chat =
                    completions_request_to_chat(request_json, &target.upstream_model, streamed);
                chat_request_to_responses(&chat, &target.upstream_model, streamed)
            } else if translate_completions_to_chat {
                completions_request_to_chat(request_json, &target.upstream_model, streamed)
            } else if translate_responses_to_chat {
                responses_request_to_chat(request_json, &target.upstream_model, streamed)
            } else if translate_chat_to_responses {
                chat_request_to_responses(request_json, &target.upstream_model, streamed)
            } else {
                let mut body = request_json.clone();
                body["model"] = json!(target.upstream_model);
                body
            };
            (join_upstream_url(&target.base_url, upstream_endpoint), body)
        }
    };

    let shape = classify_upstream_shape(provider_type, &request_body);
    apply_upstream_compat(state, request_id, &target, shape, &mut request_body).await;

    let response = match send_provider_request_with_compat_retry(
        state,
        &target,
        request_id,
        endpoint,
        streamed,
        &mut request_body,
        |body| build_upstream_request(state, &url, provider_type, &target, body, session_id),
    )
    .await?
    {
        UpstreamAttempt::Ok(response) => response,
        UpstreamAttempt::Error {
            status,
            headers: response_headers,
            body: response_body,
        } => {
            return upstream_error_response(
                state,
                request_id,
                endpoint,
                requested_model,
                &target,
                streamed,
                request_tokens,
                api_key,
                started,
                status,
                response_headers,
                response_body,
            )
            .await;
        }
    };
    let status = response.status();
    let response_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if provider_type == ProviderType::Anthropic {
        if streamed {
            if endpoint == OPENAI_RESPONSES {
                return Ok(anthropic_stream_to_responses(
                    state.clone(),
                    response.into_stream(),
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            if endpoint == OPENAI_COMPLETIONS {
                return Ok(anthropic_stream_to_completions(
                    state.clone(),
                    response.into_stream(),
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            return Ok(anthropic_stream_response(
                state.clone(),
                response.into_stream(),
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
        let (converted, usage) = match endpoint {
            OPENAI_RESPONSES => anthropic_response_to_responses(&upstream_json, requested_model),
            OPENAI_COMPLETIONS => {
                anthropic_response_to_completions(&upstream_json, requested_model)
            }
            _ => convert_anthropic_response(&upstream_json),
        };
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
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
                latency_ms,
                first_token_ms: Some(latency_ms),
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

    if translate_responses_to_chat {
        if streamed {
            return Ok(openai_chat_stream_to_responses(
                state.clone(),
                response.into_stream(),
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
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from chat completions: {error}"))
        })?;
        let (converted, usage) = chat_response_to_responses(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
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
                latency_ms,
                first_token_ms: Some(latency_ms),
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

    if translate_chat_to_responses {
        if streamed {
            return Ok(responses_stream_to_chat(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt.clone(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from responses upstream: {error}"))
        })?;
        let (converted, usage) = responses_response_to_chat(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
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
                latency_ms,
                first_token_ms: Some(latency_ms),
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

    if translate_completions_to_chat || translate_completions_to_responses {
        if streamed {
            return Ok(chat_stream_to_completions(
                state.clone(),
                response.into_stream(),
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt.clone(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Upstream(format!("invalid JSON from upstream: {error}")))?;
        let chat = if translate_completions_to_responses {
            responses_response_to_chat(&upstream_json, requested_model).0
        } else {
            upstream_json
        };
        let (converted, usage) = chat_response_to_completions(&chat, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
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
                latency_ms,
                first_token_ms: Some(latency_ms),
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
            response.into_stream(),
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
    let latency_ms = started.elapsed().as_millis() as i64;
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
            latency_ms,
            first_token_ms: Some(latency_ms),
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

/// Keeps model and provider concurrency permits alive until the outbound
/// response body completes, including translated and pass-through streams.
///
/// `keepalive` additionally injects SSE comments into a quiet `text/event-stream`
/// body so intermediaries and clients do not time out while the upstream is
/// still thinking.
pub(crate) fn attach_provider_slot(
    response: Response,
    slots: Option<UpstreamSlots>,
    keepalive: Option<Duration>,
) -> Response {
    let (parts, body) = response.into_parts();
    let keepalive = keepalive
        .filter(|interval| !interval.is_zero())
        .filter(|_| is_event_stream(&parts.headers));
    if slots.is_none() && keepalive.is_none() {
        return Response::from_parts(parts, body);
    }
    let (provider, model) = match slots {
        Some(UpstreamSlots { provider, model }) => (provider, model),
        None => (None, None),
    };
    let mut stream = body.into_data_stream();
    let guarded: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>> =
        match keepalive {
            Some(interval) => {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                Box::pin(async_stream::stream! {
                    let _provider_slot = provider;
                    let _model_slot = model;
                    // Skip the immediate first tick so the first keep-alive
                    // waits a full interval instead of firing at t=0.
                    ticker.tick().await;
                    loop {
                        tokio::select! {
                            chunk = stream.next() => match chunk {
                                Some(chunk) => yield chunk,
                                None => break,
                            },
                            _ = ticker.tick() => {
                                yield Ok(Bytes::from_static(SSE_KEEPALIVE_LINE));
                            }
                        }
                    }
                })
            }
            None => Box::pin(async_stream::stream! {
                let _provider_slot = provider;
                let _model_slot = model;
                while let Some(chunk) = stream.next().await {
                    yield chunk;
                }
            }),
        };
    Response::from_parts(parts, Body::from_stream(guarded))
}

/// True when the response advertises an SSE body, ignoring any parameters.
fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        })
}

/// Keeps a global admission permit alive until the outbound body completes.
///
/// Error responses finish immediately, so this is a no-op for anything that is
/// not a stream: the permit simply drops when the body is consumed.
pub(crate) fn attach_request_capacity(
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
) -> Response {
    attach_request_guards(response, permit, None)
}

/// Keeps the global admission guards alive until the outbound body completes.
///
/// Both the request-count permit and the in-flight byte guard are moved into
/// the response body, so a long stream holds its slot and its byte reservation
/// for its whole lifetime rather than just until the headers are returned.
pub(crate) fn attach_request_guards(
    response: Response,
    permit: Option<OwnedSemaphorePermit>,
    bytes: Option<crate::state::RequestByteGuard>,
) -> Response {
    if permit.is_none() && bytes.is_none() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let mut stream = body.into_data_stream();
    let guarded = async_stream::stream! {
        let _permit = permit;
        let _bytes = bytes;
        while let Some(chunk) = stream.next().await {
            yield chunk;
        }
    };
    Response::from_parts(parts, Body::from_stream(guarded))
}

/// Builds the `429` returned when the global request cap is saturated.
///
/// `anthropic` selects the Anthropic error envelope so each client family can
/// parse the refusal with its usual error handling.
pub(crate) fn overloaded_response(limit: usize, anthropic: bool) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let message = format!(
        "gateway is at its concurrency limit ({limit}); retry after {RETRY_AFTER_SECS}s"
    );
    shed_response(&message, anthropic, Some(limit.to_string()))
}

/// Builds the `429` returned when the in-flight request-byte budget is full.
///
/// Separate from [`overloaded_response`] so the message and the advertised
/// limit (bytes, not slots) point an operator at the knob that actually shed
/// the request.
pub(crate) fn body_budget_response(limit_bytes: usize, anthropic: bool) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let message = format!(
        "gateway is at its in-flight request-body budget ({} MiB); retry after {RETRY_AFTER_SECS}s",
        limit_bytes / (1024 * 1024)
    );
    shed_response(&message, anthropic, Some(limit_bytes.to_string()))
}

/// Renders a retryable `429` refusal in the client's expected envelope.
fn shed_response(message: &str, anthropic: bool, limit_header: Option<String>) -> Response {
    const RETRY_AFTER_SECS: u64 = 5;
    let body = if anthropic {
        anthropic_error_body("overloaded_error", message)
    } else {
        json!({"error": {"message": message, "type": "rate_limit_error", "code": 429}})
    };
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&RETRY_AFTER_SECS.to_string()) {
        headers.insert(reqwest::header::RETRY_AFTER, value);
    }
    if let Some(limit_header) = limit_header
        && let Ok(value) = HeaderValue::from_str(&limit_header)
    {
        headers.insert(HeaderName::from_static("x-ratelimit-limit"), value);
    }
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from_static("0"),
    );
    response
}

pub(crate) fn join_upstream_url(base: &str, path: &str) -> String {
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

pub(crate) fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// Statuses worth one more attempt against the *same* target before the request
/// falls back to another route target.
///
/// Rate limits (`429`) are deliberately excluded: the provider just refused the
/// call, and an immediate retry would spend quota instead of letting the cooldown
/// and fallback machinery work. `409` is excluded too because it is usually a
/// client-side state conflict rather than a blip.
pub(crate) fn retryable_same_target_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 500..=599)
}

/// A transport fault that another attempt could plausibly survive.
///
/// Builder errors (bad URL, unsupported scheme) are permanent, so they are not
/// retried; connection, timeout, and body faults are.
pub(crate) fn retryable_transport_error(error: &reqwest::Error) -> bool {
    !error.is_builder()
        && (error.is_connect() || error.is_timeout() || error.is_request() || error.is_body())
}

/// Detects the opaque 4xx that aggregator upstreams return when one of their
/// internal channels fails: an "invalid request error" that carries only a
/// trace id and no actionable `param`. These are transient routing failures, so
/// retrying is worthwhile instead of surfacing them as client errors.
pub(crate) fn transient_upstream_4xx(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("trace_id") && text.contains("invalid request") && !text.contains("\"param\"")
}

pub(crate) fn upstream_trace_id(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let message = value.pointer("/error/message")?.as_str()?;
    let start = message.to_ascii_lowercase().find("trace_id:")? + "trace_id:".len();
    let trace = message[start..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect::<String>();
    (!trace.is_empty()).then_some(trace)
}

pub(crate) fn should_try_next_target(target: &RouteTarget, status: StatusCode) -> bool {
    retryable_status(status)
        || (target.auth_retryable
            && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN))
}

pub(crate) fn provider_key_failure(status: StatusCode) -> bool {
    retryable_status(status) || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}
