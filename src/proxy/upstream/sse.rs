use super::*;

/// Keeps model and provider concurrency permits alive until the outbound
/// response body completes, including translated and pass-through streams.
///
/// `keepalive` additionally injects SSE comments into a quiet `text/event-stream`
/// body so intermediaries and clients do not time out while the upstream is
/// still thinking.
#[derive(Clone, Copy)]
pub(crate) enum SseErrorProtocol {
    OpenAi,
    Anthropic,
}

/// Returns an SSE response immediately and keeps it alive while a routing
/// future waits for a target to become available.
///
/// The future owns everything it needs; the stream's receiver acts as the
/// client-side cancellation signal, so a disconnected caller drops the future
/// before it can start another upstream attempt.
pub(crate) fn stream_sse_until_response<F>(
    interval: Duration,
    protocol: SseErrorProtocol,
    future: F,
) -> Response
where
    F: std::future::Future<Output = AppResult<Response>> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let result = tokio::select! {
            _ = tx.closed() => return,
            result = future => result,
        };
        match result {
            Ok(response) if response.status().is_success() => {
                let mut body = response.into_body().into_data_stream();
                while let Some(chunk) = body.next().await {
                    let chunk = chunk.map_err(std::io::Error::other);
                    if tx.send(chunk).await.is_err() {
                        break;
                    }
                }
            }
            Ok(response) => {
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap_or_default();
                let _ = tx.send(Ok(sse_error_event(protocol, status, &body))).await;
            }
            Err(error) => {
                let status = app_error_status(&error);
                let body = error.to_string().into_bytes();
                let _ = tx.send(Ok(sse_error_event(protocol, status, &body))).await;
            }
        }
    });

    let interval = interval.max(Duration::from_secs(1));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stream = async_stream::stream! {
        // Commit the response headers immediately. This is also useful to
        // clients that wait for the first byte before applying their own idle
        // timeout.
        yield Ok(Bytes::from_static(b": keep-alive\n\n"));
        loop {
            tokio::select! {
                next = rx.recv() => match next {
                    Some(chunk) => yield chunk,
                    None => break,
                },
                _ = ticker.tick() => {
                    yield Ok(Bytes::from_static(b": keep-alive\n\n"));
                }
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            AppError::Internal(anyhow::anyhow!("could not build SSE response")).into_response()
        })
}

pub(super) fn app_error_status(error: &AppError) -> StatusCode {
    match error {
        AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
        AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        AppError::Forbidden(_) => StatusCode::FORBIDDEN,
        AppError::NotFound(_) => StatusCode::NOT_FOUND,
        AppError::Conflict(_) => StatusCode::CONFLICT,
        AppError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
        AppError::Upstream(_) => StatusCode::BAD_GATEWAY,
        AppError::UpstreamStatus { status, .. } => *status,
        AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub(super) fn sse_error_event(
    protocol: SseErrorProtocol,
    status: StatusCode,
    body: &[u8],
) -> Bytes {
    let message = upstream_error_message(body);
    let payload = match protocol {
        SseErrorProtocol::OpenAi => {
            let error_type = if status == StatusCode::TOO_MANY_REQUESTS {
                "rate_limit_error"
            } else {
                "upstream_error"
            };
            json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "code": status.as_u16()
                }
            })
        }
        SseErrorProtocol::Anthropic => {
            let error_type = if status == StatusCode::TOO_MANY_REQUESTS {
                "rate_limit_error"
            } else if status == StatusCode::UNAUTHORIZED {
                "authentication_error"
            } else {
                "api_error"
            };
            json!({
                "type": "error",
                "error": {
                    "type": error_type,
                    "message": message
                }
            })
        }
    };
    match protocol {
        SseErrorProtocol::OpenAi => Bytes::from(format!("data: {payload}\n\ndata: [DONE]\n\n")),
        SseErrorProtocol::Anthropic => Bytes::from(format!("event: error\ndata: {payload}\n\n")),
    }
}

pub(super) fn upstream_error_message(body: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<Value>(body)
        && let Some(message) = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
    {
        return message.to_string();
    }
    let text = String::from_utf8_lossy(body).trim().to_string();
    if text.is_empty() {
        "upstream request failed".to_string()
    } else {
        text.chars().take(2_000).collect()
    }
}

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
    let (provider_key, provider, model) = match slots {
        Some(UpstreamSlots {
            provider_key,
            provider,
            model,
        }) => (provider_key, provider, model),
        None => (None, None, None),
    };
    let mut stream = body.into_data_stream();
    let guarded: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>> =
        match keepalive {
            Some(interval) => {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                Box::pin(async_stream::stream! {
                    let _provider_key_slot = provider_key;
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
                let _provider_key_slot = provider_key;
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
pub(super) fn is_event_stream(headers: &HeaderMap) -> bool {
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
