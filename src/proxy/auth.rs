use super::*;

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
/// Reserves a global request slot before the handler buffers the body, so an
/// over-capacity request is refused without reading a potentially large body
/// into memory. The permit is attached to the response body and released only
/// once that body has been fully written, so a long stream keeps its slot for
/// its whole lifetime. When `OPENLLM_ADMISSION_WAIT_MS` is set, an
/// over-capacity request first waits that long for a slot before it is shed, so
/// a momentary burst serializes instead of being dropped.
pub(crate) async fn admission(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let anthropic = request.uri().path().starts_with("/v1/messages");
    let wait = state.admission_wait;
    // Refuse an over-capacity request before the body is read: a hanging or
    // huge body must not consume a slot or any memory first.
    let capacity = acquire_request_capacity(&state, wait).await;
    if let RequestCapacity::Overloaded = capacity {
        return overloaded_response(state.request_capacity_limit, anthropic);
    }

    let (parts, body) = request.into_parts();
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
/// alive for the whole request (see [`attach_request_guards`]).
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
    let mut total = guard.reserved();
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn selected_console_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ApiKeyRecord>> {
    let key_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
        .fetch_one(&state.pool)
        .await?;
    let selected_id = headers
        .get(CONSOLE_API_KEY_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| AppError::BadRequest("invalid gateway API key id".to_string()))
        })
        .transpose()?;

    let Some(key_id) = selected_id else {
        if key_count > 0 {
            return Err(AppError::BadRequest(
                "select a gateway API key for the playground".to_string(),
            ));
        }
        return Ok(None);
    };

    sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys \
         WHERE id = ? AND enabled = 1 \
           AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
    )
    .bind(key_id)
    .fetch_optional(&state.pool)
    .await?
    .map(Some)
    .ok_or_else(|| AppError::BadRequest("selected gateway API key is unavailable".to_string()))
}

pub(crate) async fn authenticate_gateway(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ApiKeyRecord>> {
    // Presence of any configured key enables gateway auth. Counting only
    // enabled keys would silently fall back to anonymous access when every
    // key is disabled, which is the opposite of the operator's intent.
    // Copy the cached value out in its own statement: keeping the read guard
    // alive across the match would deadlock when a write lock is requested
    // below (tokio's RwLock is write-preferring).
    let cached = *state.auth_required.read().await;
    let auth_required = match cached {
        Some(cached) => cached,
        None => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
                .fetch_one(&state.pool)
                .await?;
            let required = count > 0;
            *state.auth_required.write().await = Some(required);
            required
        }
    };
    if !auth_required {
        return Ok(None);
    }

    let supplied = bearer_token(headers)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .ok_or_else(|| {
            AppError::Unauthorized("missing gateway API key in Authorization header".to_string())
        })?;
    let hash = hash_secret(supplied);
    let record = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys \
         WHERE key_hash = ? AND enabled = 1 \
           AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
    )
    .bind(hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("invalid gateway API key".to_string()))?;

    // Writing on every request would take a SQLite write lock each time.
    // Once a minute per key is plenty for a "last used" display.
    let should_touch = {
        let mut touched = state.key_touched.lock().await;
        match touched.get(&record.id) {
            Some(last) if last.elapsed() < std::time::Duration::from_secs(60) => false,
            _ => {
                touched.insert(record.id, std::time::Instant::now());
                true
            }
        }
    };
    if should_touch
        && let Err(error) = sqlx::query(
            "UPDATE api_keys SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
        )
        .bind(record.id)
        .execute(&state.pool)
        .await
    {
        tracing::warn!(%error, key_id = record.id, "failed to update API key last_used_at");
    }
    Ok(Some(record))
}

/// Checks a key's soft daily quotas before any upstream work begins.
///
/// Usage is committed after the response completes, so concurrent requests can
/// overshoot by at most the work already in flight. The guard still prevents a
/// key from continuing to spend after its previous usage has crossed a limit.
pub(crate) async fn enforce_api_key_daily_quota(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
) -> AppResult<()> {
    let Some(api_key) = api_key else {
        return Ok(());
    };
    if api_key.daily_token_limit.is_none() && api_key.daily_cost_limit_micros.is_none() {
        return Ok(());
    }

    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .to_rfc3339();
    let (tokens, cost_micros) = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT COALESCE(SUM(total_tokens), 0), SUM(estimated_cost_micros) \
         FROM usage_logs WHERE api_key_id = ? AND created_at >= ? AND in_flight = 0",
    )
    .bind(api_key.id)
    .bind(day_start)
    .fetch_one(&state.pool)
    .await?;

    if let Some(limit) = api_key.daily_token_limit
        && tokens >= limit
    {
        return Err(AppError::TooManyRequests(format!(
            "daily token limit reached for this API key ({tokens}/{limit})"
        )));
    }
    if let Some(limit) = api_key.daily_cost_limit_micros
        && cost_micros.unwrap_or(0) >= limit
    {
        return Err(AppError::TooManyRequests(format!(
            "daily cost limit reached for this API key (${:.4}/${:.4})",
            cost_micros.unwrap_or(0) as f64 / 1_000_000.0,
            limit as f64 / 1_000_000.0
        )));
    }
    Ok(())
}

/// Atomically checks and reserves a rate-limit slot.
///
/// Counting and inserting in one SQLite statement closes the race where a
/// burst of requests could all pass a read-only limit check. The inserted row
/// is the same in-flight row later completed by usage logging.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_api_key_rate_limit(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
) -> AppResult<()> {
    let Some(api_key) = api_key else {
        return Ok(());
    };
    if api_key.requests_per_minute.is_none() && api_key.max_concurrency.is_none() {
        return Ok(());
    }

    let minute_start = Utc::now()
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("current minute start is valid")
        .to_rfc3339();
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, session_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        )
        SELECT ?, ?, ?, NULL, NULL, ?, NULL, ?, ?, 0, ?, 0, 0, 0,
               NULL, NULL, 0, 1, 0, ?, NULL, NULL,
               strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        WHERE (
            ? IS NULL OR (
                SELECT COUNT(*) FROM usage_logs
                WHERE api_key_id = ? AND created_at >= ?
            ) < ?
        )
        AND (
            ? IS NULL OR (
                SELECT COUNT(*) FROM usage_logs
                WHERE api_key_id = ? AND in_flight = 1
            ) < ?
        )
        ON CONFLICT(request_id) DO NOTHING
        "#,
    )
    .bind(request_id)
    .bind(session_id)
    .bind(api_key.id)
    .bind(requested_model)
    .bind(endpoint)
    .bind(request_tokens)
    .bind(request_tokens)
    .bind(streamed as i64)
    .bind(api_key.requests_per_minute)
    .bind(api_key.id)
    .bind(&minute_start)
    .bind(api_key.requests_per_minute)
    .bind(api_key.max_concurrency)
    .bind(api_key.id)
    .bind(api_key.max_concurrency)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() > 0 {
        return Ok(());
    }

    if let Some(limit) = api_key.requests_per_minute {
        let requests = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_logs \
             WHERE api_key_id = ? AND created_at >= ?",
        )
        .bind(api_key.id)
        .bind(&minute_start)
        .fetch_one(&state.pool)
        .await?;
        if requests >= limit {
            return Err(AppError::TooManyRequests(format!(
                "requests per minute limit reached for this API key ({requests}/{limit})"
            )));
        }
    }

    if let Some(limit) = api_key.max_concurrency {
        let in_flight = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_logs WHERE api_key_id = ? AND in_flight = 1",
        )
        .bind(api_key.id)
        .fetch_one(&state.pool)
        .await?;
        if in_flight >= limit {
            return Err(AppError::TooManyRequests(format!(
                "concurrency limit reached for this API key ({in_flight}/{limit})"
            )));
        }
    }

    Err(AppError::TooManyRequests(
        "API key rate limit reached".to_string(),
    ))
}

pub(crate) fn api_key_model_patterns(
    api_key: Option<&ApiKeyRecord>,
) -> AppResult<Option<Vec<String>>> {
    let Some(raw) = api_key.and_then(|api_key| api_key.allowed_models.as_deref()) else {
        return Ok(None);
    };
    let patterns = serde_json::from_str::<Vec<String>>(raw)
        .map_err(|_| AppError::Forbidden("API key model permissions are invalid".to_string()))?;
    Ok((!patterns.is_empty()).then_some(patterns))
}

pub(crate) fn model_matches_patterns(patterns: Option<&[String]>, model: &str) -> bool {
    patterns.is_none_or(|patterns| {
        patterns.iter().any(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher().is_match(model))
                .unwrap_or(false)
        })
    })
}

pub(crate) fn filter_allowed_models<T>(
    patterns: Option<&[String]>,
    models: Vec<T>,
    model_id: impl Fn(&T) -> &str,
) -> Vec<T> {
    models
        .into_iter()
        .filter(|model| model_matches_patterns(patterns, model_id(model)))
        .collect()
}

pub(crate) fn enforce_api_key_model_access(
    api_key: Option<&ApiKeyRecord>,
    requested_model: &str,
) -> AppResult<()> {
    let patterns = api_key_model_patterns(api_key)?;
    if !model_matches_patterns(patterns.as_deref(), requested_model) {
        return Err(AppError::Forbidden(format!(
            "API key is not allowed to call model '{requested_model}'"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn enforce_policy_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
) -> AppResult<()> {
    let rejection = match enforce_api_key_model_access(api_key, requested_model) {
        Ok(()) => enforce_api_key_daily_quota(state, api_key).await.err(),
        Err(error) => Some(error),
    };
    match rejection {
        None => Ok(()),
        Some(error) => {
            let message = error.to_string();
            let status_code = match error {
                AppError::Forbidden(_) => 403,
                AppError::TooManyRequests(_) => 429,
                _ => 500,
            };
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                requested_model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn enforce_api_key_rate_limit_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
    started: Instant,
) -> AppResult<()> {
    match reserve_api_key_rate_limit(
        state,
        api_key,
        request_id,
        session_id,
        requested_model,
        endpoint,
        request_tokens,
        streamed,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = error.to_string();
            let status_code = match error {
                AppError::TooManyRequests(_) => 429,
                _ => 500,
            };
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                requested_model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn log_request_rejection(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
    status_code: i64,
    message: &str,
) {
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: None,
            provider_id: None,
            requested_model,
            upstream_model: None,
            endpoint,
            usage: Usage::default(),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code,
            success: false,
            streamed,
            error_message: Some(message),
            response_preview: None,
        },
    )
    .await;
    if let Some(session_id) = session_id
        && let Err(error) = sqlx::query("UPDATE usage_logs SET session_id = ? WHERE request_id = ?")
            .bind(session_id)
            .bind(request_id)
            .execute(&state.pool)
            .await
    {
        tracing::warn!(%error, request_id, "failed to attach session to rejected usage log");
    }
}

pub(crate) fn enforce_context_capacity(
    request_tokens: i64,
    barrel: Option<&BarrelEnvelope>,
) -> AppResult<()> {
    let limit = barrel
        .and_then(|barrel| barrel.capabilities.as_ref())
        .and_then(effective_input_limit);
    if let Some(limit) = limit
        && request_tokens > limit
    {
        return Err(AppError::BadRequest(format!(
            "estimated input tokens ({request_tokens}) exceed the route input limit ({limit})"
        )));
    }
    Ok(())
}

/// Returns the strictest input capacity when a provider publishes both a
/// context window and a separate input ceiling.
pub(crate) fn effective_input_limit(capabilities: &ModelCapabilities) -> Option<i64> {
    match (capabilities.input_limit, capabilities.context_limit) {
        (Some(input), Some(context)) => Some(input.min(context)),
        (Some(input), None) => Some(input),
        (None, Some(context)) => Some(context),
        (None, None) => None,
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

pub(crate) fn hash_secret(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}
