use super::*;

const OBSERVATION_LINE_LIMIT: usize = 64 * 1024;

pub(crate) fn observe_activity_value(state: &AppState, request_id: &str, value: &Value) {
    let Some(usage) = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        .or_else(|| value.pointer("/message/usage"))
    else {
        return;
    };
    let input_reported = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_i64)
        .is_some_and(|value| value >= 0);
    let cache_reported = [
        "/cache_read_input_tokens",
        "/cache_read_tokens",
        "/cache_write_tokens",
        "/cache_creation_input_tokens",
        "/prompt_tokens_details/cached_tokens",
        "/input_tokens_details/cached_tokens",
        "/prompt_tokens_details/cache_read_input_tokens",
        "/prompt_tokens_details/cache_write_tokens",
        "/prompt_tokens_details/cache_creation_input_tokens",
        "/input_tokens_details/cache_write_tokens",
    ]
    .iter()
    .any(|path| usage.pointer(path).and_then(Value::as_i64).is_some());
    if let Some(usage) = usage_from_value(value) {
        let _ = state
            .activity
            .report_usage(request_id, usage, input_reported, cache_reported);
    }
}

pub(crate) fn observe_activity_stream(
    mut stream: UpstreamByteStream,
    state: AppState,
    request_id: String,
) -> UpstreamByteStream {
    Box::pin(async_stream::stream! {
        let mut line = Vec::new();
        let mut discarded = false;
        let mut content_observed = false;
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = &chunk {
                for part in bytes.split_inclusive(|byte| *byte == b'\n') {
                    if line.len().saturating_add(part.len()) > OBSERVATION_LINE_LIMIT {
                        line.clear();
                        discarded = true;
                    }
                    if !discarded {
                        line.extend_from_slice(part);
                    }
                    if part.last() == Some(&b'\n') {
                        if !discarded { observe_line(&state, &request_id, &line, &mut content_observed); }
                        line.clear();
                        discarded = false;
                    }
                }
            }
            yield chunk;
        }
        if !discarded && !line.is_empty() { observe_line(&state, &request_id, &line, &mut content_observed); }
    })
}

fn observe_line(state: &AppState, request_id: &str, line: &[u8], content_observed: &mut bool) {
    let text = String::from_utf8_lossy(line);
    let Some(data) = text.trim().strip_prefix("data:") else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        return;
    };
    observe_activity_value(state, request_id, &value);
    if !*content_observed
        && event_has_usable_content(&value)
        && let Some(snapshot) = state.activity.snapshot(request_id)
        && snapshot.streamed
        && snapshot.timings.time_to_first_token_ms.is_none()
    {
        *content_observed = true;
        state.activity_first_token(request_id, snapshot.timings.total_ms);
    }
}
