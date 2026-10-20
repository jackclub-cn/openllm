use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn passthrough_stream_response(
    state: AppState,
    mut upstream: UpstreamByteStream,
    content_type: String,
    endpoint: String,
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
        let mut parser = UsageParser::new(started);
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());
        while let Some(chunk) = next_upstream_chunk(&mut upstream, &tx, state.sse_keepalive).await {
            match chunk {
                Ok(bytes) => {
                    heartbeat.touch().await;
                    parser.push(&bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    mark_stream_failure(&state, &target, &message).await;
                    stream_error = Some(message);
                    let _ = tx.send(Err(io::Error::other(error))).await;
                    break;
                }
            }
        }
        let usage = parser
            .finish()
            .map(|mut usage| {
                if usage.prompt_tokens == 0 {
                    usage.prompt_tokens = request_tokens;
                }
                usage.normalized()
            })
            .unwrap_or_else(|| Usage {
                prompt_tokens: request_tokens,
                completion_tokens: 1,
                total_tokens: request_tokens + 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = parser
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = parser.preview();
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: &endpoint,
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
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}
