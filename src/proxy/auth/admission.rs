use super::*;

/// Authenticated gateway identity passed from admission to the protocol handler.
///
/// `None` means gateway API-key auth is disabled or no keys are configured.
#[derive(Debug, Clone)]
pub(crate) struct GatewayAuth(pub(crate) Option<ApiKeyRecord>);

/// Outcome of the global admission check.
pub(crate) enum RequestCapacity {
    /// No global cap is configured.
    Disabled,
    /// A slot was taken; the permit must outlive the response body.
    Acquired(OwnedSemaphorePermit),
    /// The global cap is saturated.
    Overloaded,
}

/// Reserves a slot against the optional global request cap.
///
/// `wait` bounds how long an over-capacity request waits for a slot. A zero
/// wait is non-blocking: once the cap is reached the request is shed at once.
/// A positive wait lets a momentary burst serialize instead of being dropped,
/// then still sheds with a `429` if no slot frees in time.
pub(crate) async fn acquire_request_capacity(state: &AppState, wait: Duration) -> RequestCapacity {
    let Some(semaphore) = &state.request_capacity else {
        return RequestCapacity::Disabled;
    };
    let permit = if wait.is_zero() {
        semaphore.clone().try_acquire_owned().ok()
    } else {
        match tokio::time::timeout(wait, semaphore.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Some(permit),
            _ => None,
        }
    };
    match permit {
        Some(permit) => RequestCapacity::Acquired(permit),
        None => {
            state
                .requests_shed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            RequestCapacity::Overloaded
        }
    }
}

/// Admission middleware for the public proxy routes.
///
/// The gateway API key is checked before any capacity is reserved or body
/// bytes are read. The authenticated record is attached to the request so the
/// protocol handler can enforce model limits without querying credentials a
/// second time. Capacity is reserved before the handler buffers the body, so an
/// over-capacity request is refused without reading a potentially large body
/// into memory. The permit is attached to the response body and released only
/// once that body has been fully written, so a long stream keeps its slot for
/// its whole lifetime.
pub(crate) async fn admission(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let anthropic = request.uri().path().starts_with("/v1/messages");
    let api_key = match authenticate_gateway(&state, request.headers()).await {
        Ok(api_key) => api_key,
        Err(error) if anthropic => return anthropic_error_response(error),
        Err(error) => return error.into_response(),
    };
    let wait = state.admission_wait;
    // Shed before reserving anything when the process is low on memory: the
    // point is to keep headroom for the work already in flight.
    if state
        .memory_pressure
        .as_ref()
        .is_some_and(|pressure| pressure.is_shedding())
    {
        state
            .memory_shed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return memory_pressure_response(anthropic);
    }
    // Refuse an over-capacity request before the body is read: a hanging or
    // huge body must not consume a slot or any memory first.
    let capacity = acquire_request_capacity(&state, wait).await;
    if let RequestCapacity::Overloaded = capacity {
        return overloaded_response(state.request_capacity_limit, anthropic);
    }

    let (mut parts, body) = request.into_parts();
    parts.extensions.insert(GatewayAuth(api_key));
    // Bound how long a client may trickle its body so it cannot pin the slot
    // it just took; the wrapped stream flows through to the handler.
    let body = with_body_read_deadline(body, state.body_read_timeout);
    let (body, byte_guard) = match state.inflight_request_bytes.clone() {
        None => (body, None),
        Some(budget) => match charge_request_body(&budget, &parts.headers, body, wait).await {
            Ok(charged) => charged,
            Err(BodyCharge::OverBudget) => {
                state
                    .request_bytes_shed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return body_budget_response(budget.limit(), anthropic);
            }
            Err(BodyCharge::Read) => {
                return AppError::BadRequest("failed to read request body".to_string())
                    .into_response();
            }
        },
    };

    let permit = match capacity {
        RequestCapacity::Acquired(permit) => Some(permit),
        _ => None,
    };
    let request = axum::extract::Request::from_parts(parts, body);
    let response = next.run(request).await;
    attach_request_guards(response, permit, byte_guard)
}

/// Why a request body could not be admitted against the in-flight byte budget.
enum BodyCharge {
    /// The buffered body would exceed the in-flight byte budget.
    OverBudget,
    /// The client's body stream failed before it was fully read.
    Read,
}

/// Buffers the request body while charging its bytes against the budget.
///
/// Reads the body itself so an over-budget request is refused mid-stream
/// instead of after the `Bytes` extractor has already buffered it. The returned
/// body is re-inserted for the handler, and the guard keeps the reservation
/// alive for the whole request (see `attach_request_guards`).
async fn charge_request_body(
    budget: &std::sync::Arc<crate::state::RequestByteBudget>,
    headers: &HeaderMap,
    body: Body,
    wait: Duration,
) -> Result<(Body, Option<crate::state::RequestByteGuard>), BodyCharge> {
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok());
    // Reserve the declared size up front so a known-large body is shed before
    // any of it is read, and concurrent requests count their size immediately.
    // A known size also honors the admission wait; an unknown one reserves
    // nothing up front and charges as it reads.
    let mut guard = match declared {
        Some(bytes) => budget.reserve_within(bytes, wait).await,
        None => budget.reserve(0),
    }
    .ok_or(BodyCharge::OverBudget)?;

    let mut stream = body.into_data_stream();
    let mut buffer: Vec<u8> = Vec::with_capacity(declared.unwrap_or(0).min(64 * 1024));
    let mut total = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| BodyCharge::Read)?;
        total += chunk.len();
        if total > guard.reserved() && !guard.extend(total - guard.reserved()) {
            return Err(BodyCharge::OverBudget);
        }
        buffer.extend_from_slice(&chunk);
    }
    // A dishonest `Content-Length` may overstate the body; release the slack so
    // the budget tracks bytes that actually exist.
    guard.shrink_to(total);
    Ok((Body::from(Bytes::from(buffer)), Some(guard)))
}
