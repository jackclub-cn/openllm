use super::*;

pub(super) const STREAM_RECOVERY_HOLDBACK: Duration = Duration::from_millis(750);
pub(super) const STREAM_RECOVERY_MAX_BYTES: usize = 65_536;

pub(crate) type UpstreamByteStream =
    futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>>;

/// An upstream response whose body can be safely replayed after a bounded
/// pre-commit stream probe.
pub(crate) struct UpstreamResponse {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    pub(super) stream: UpstreamByteStream,
    activity: Option<(Box<AppState>, String)>,
}

impl UpstreamResponse {
    pub(crate) fn new(response: reqwest::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let stream = response
            .bytes_stream()
            .map_err(std::io::Error::other)
            .boxed();
        let stream = with_stream_deadline(stream, crate::state::max_stream_lifetime());
        Self {
            status,
            headers,
            stream,
            activity: None,
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
            activity: None,
        }
    }

    pub(crate) fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(crate) async fn bytes(mut self) -> Result<Bytes, std::io::Error> {
        let bytes =
            collect_bytes_limited(&mut self.stream, crate::state::max_upstream_body_bytes())
                .await?;
        if let Some((state, request_id)) = &self.activity
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            observe_activity_value(state, request_id, &value);
        }
        Ok(bytes)
    }

    pub(crate) fn into_stream(self) -> UpstreamByteStream {
        match self.activity {
            Some((state, request_id)) => observe_activity_stream(self.stream, *state, request_id),
            None => self.stream,
        }
    }

    pub(crate) fn with_activity(mut self, state: &AppState, request_id: &str) -> Self {
        if state.activity.is_enabled() {
            self.activity = Some((Box::new(state.clone()), request_id.to_string()));
        }
        self
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

    pub(super) fn prepend_prefix(self, chunks: Vec<Bytes>) -> Self {
        let Self {
            status,
            headers,
            stream,
            activity,
        } = self;
        let prefix = futures_util::stream::iter(chunks.into_iter().map(Ok));
        Self {
            status,
            headers,
            stream: prefix.chain(stream).boxed(),
            activity,
        }
    }
}

/// Wraps an upstream byte stream with a hard total-lifetime deadline.
///
/// The client's idle timeout only bounds the gap between reads, so a stream
/// that keeps trickling bytes can pin a provider slot forever. Once `lifetime`
/// elapses the stream ends with a timeout error so the caller can cool the
/// provider and free the slot. `None` or zero leaves the stream untouched.
pub(crate) fn with_stream_deadline(
    stream: UpstreamByteStream,
    lifetime: Option<Duration>,
) -> UpstreamByteStream {
    let Some(lifetime) = lifetime.filter(|lifetime| !lifetime.is_zero()) else {
        return stream;
    };
    async_stream::stream! {
        let deadline = tokio::time::sleep(lifetime);
        tokio::pin!(deadline);
        let mut stream = stream;
        loop {
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(chunk) => yield chunk,
                    None => break,
                },
                _ = &mut deadline => {
                    yield Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "upstream stream exceeded its {} second lifetime",
                            lifetime.as_secs()
                        ),
                    ));
                    break;
                }
            }
        }
    }
    .boxed()
}

/// Bounds how long the gateway will spend reading a request body.
///
/// A slow or hostile client can otherwise hold an admission slot (and, with a
/// large body, memory) by trickling bytes forever. Once `timeout` elapses the
/// wrapped stream yields an error so the body extractor fails fast. `None` or
/// zero leaves the body untouched.
pub(crate) fn with_body_read_deadline(body: Body, timeout: Option<Duration>) -> Body {
    let Some(timeout) = timeout.filter(|timeout| !timeout.is_zero()) else {
        return body;
    };
    let mut stream = body.into_data_stream();
    let stream = async_stream::stream! {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(chunk) => yield chunk,
                    None => break,
                },
                _ = &mut deadline => {
                    yield Err(axum::Error::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "request body was not fully received within {} seconds",
                            timeout.as_secs()
                        ),
                    )));
                    break;
                }
            }
        }
    };
    Body::from_stream(stream)
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

pub(super) fn stream_has_terminal_marker(bytes: &[u8]) -> bool {
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

pub(in crate::proxy) fn event_has_usable_content(value: &Value) -> bool {
    let non_empty_str = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    };

    // OpenAI chat-completions and legacy completions chunks.
    if let Some(delta) = value.pointer("/choices/0/delta") {
        if non_empty_str(delta.get("content"))
            || non_empty_str(delta.get("reasoning_content"))
            || non_empty_str(delta.get("reasoning_text"))
            || non_empty_str(delta.get("reasoning"))
        {
            return true;
        }
        if delta
            .get("tool_calls")
            .is_some_and(|calls| !calls.is_null())
        {
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
        | Some("response.reasoning_text.delta")
        | Some("response.function_call_arguments.delta") => {
            return non_empty_str(value.get("delta"));
        }
        Some("response.output_item.added") => {
            return value.pointer("/item/type").and_then(Value::as_str) == Some("function_call");
        }
        // Anthropic Messages events.
        Some("content_block_start") => {
            return value.pointer("/content_block/type").and_then(Value::as_str)
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
