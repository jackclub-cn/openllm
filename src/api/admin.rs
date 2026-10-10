use super::*;

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "openllm",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub async fn admin_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> AppResult<Response> {
    if let Some(expected) = &state.admin_token {
        let token = request
            .headers()
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(request.headers()));

        if token != Some(expected.as_str()) {
            return Err(AppError::Unauthorized(
                "a valid admin token is required".to_string(),
            ));
        }
    }

    Ok(next.run(request).await)
}

pub async fn get_settings(State(state): State<AppState>) -> AppResult<Json<SettingsView>> {
    Ok(Json(SettingsView {
        admin_auth_enabled: state.admin_token.is_some(),
        database: "sqlite",
        version: env!("CARGO_PKG_VERSION"),
        database_stats: load_database_stats(&state).await?,
    }))
}

pub(super) async fn load_database_stats(state: &AppState) -> AppResult<DatabaseStats> {
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&state.pool)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&state.pool)
        .await?;
    let free_pages: i64 = sqlx::query_scalar("PRAGMA freelist_count")
        .fetch_one(&state.pool)
        .await?;
    let (_, _, path) = sqlx::query_as::<_, (i64, String, String)>("PRAGMA database_list")
        .fetch_one(&state.pool)
        .await?;
    let (
        providers,
        provider_models,
        provider_api_keys,
        routes,
        access_keys,
        webhooks,
        webhook_deliveries,
        audit_logs,
        usage_logs,
        in_flight_requests,
    ) = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM providers), \
            (SELECT COUNT(*) FROM provider_models), \
            (SELECT COUNT(*) FROM provider_api_keys), \
            (SELECT COUNT(*) FROM routes), \
            (SELECT COUNT(*) FROM api_keys), \
            (SELECT COUNT(*) FROM webhooks), \
            (SELECT COUNT(*) FROM webhook_deliveries), \
            (SELECT COUNT(*) FROM audit_logs), \
            (SELECT requests FROM usage_lifetime_stats WHERE id = 1) \
                + (SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1), \
            (SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1)",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(DatabaseStats {
        path: (!path.is_empty()).then_some(path),
        size_bytes: page_count.saturating_mul(page_size),
        free_bytes: free_pages.saturating_mul(page_size),
        providers,
        provider_models,
        provider_api_keys,
        routes,
        access_keys,
        webhooks,
        webhook_deliveries,
        audit_logs,
        usage_logs,
        in_flight_requests,
    })
}

pub async fn get_runtime_settings(
    State(state): State<AppState>,
) -> AppResult<Json<RuntimeSettingsView>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await?;
    let usage_retention_days = raw.and_then(|value| value.parse::<i64>().ok());
    Ok(Json(RuntimeSettingsView {
        usage_retention_days,
    }))
}

pub async fn update_runtime_settings(
    State(state): State<AppState>,
    Json(input): Json<RuntimeSettingsUpdate>,
) -> AppResult<Json<RuntimeSettingsView>> {
    let usage_retention_days = normalize_retention_days(input.usage_retention_days)?;
    if let Some(days) = usage_retention_days {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .bind(days.to_string())
        .execute(&state.pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTING_USAGE_RETENTION_DAYS)
            .execute(&state.pool)
            .await?;
    }
    *state.retention_last_run.lock().await = None;
    record_audit(
        &state,
        "update",
        "settings",
        Some(SETTING_USAGE_RETENTION_DAYS),
        match usage_retention_days {
            Some(days) => format!("set usage retention to {days} day(s)"),
            None => "cleared usage retention (keep logs forever)".to_string(),
        }
        .as_str(),
        usage_retention_days.map(|days| json!({ "usage_retention_days": days })),
    )
    .await;
    Ok(Json(RuntimeSettingsView {
        usage_retention_days,
    }))
}

pub async fn get_guardrails_settings(
    State(state): State<AppState>,
) -> AppResult<Json<GuardrailSettings>> {
    Ok(Json(state.guardrail_settings().await?))
}

pub async fn update_guardrails_settings(
    State(state): State<AppState>,
    Json(input): Json<GuardrailSettings>,
) -> AppResult<Json<GuardrailSettings>> {
    let settings = normalize_guardrail_settings(input)?;
    if settings.is_empty() {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTING_GUARDRAILS)
            .execute(&state.pool)
            .await?;
    } else {
        let value = serde_json::to_string(&settings).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to serialize guardrail settings: {error}"
            ))
        })?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_GUARDRAILS)
        .bind(value)
        .execute(&state.pool)
        .await?;
    }
    *state.guardrails.write().await = None;
    record_audit(
        &state,
        "update",
        "settings",
        Some(SETTING_GUARDRAILS),
        if settings.is_empty() {
            "cleared request guardrails"
        } else {
            "updated request guardrails"
        },
        Some(json!({
            "blocked_term_count": settings.blocked_terms.len(),
            "max_prompt_tokens": settings.max_prompt_tokens,
        })),
    )
    .await;
    Ok(Json(settings))
}

fn normalize_guardrail_settings(mut input: GuardrailSettings) -> AppResult<GuardrailSettings> {
    const MAX_BLOCKED_TERMS: usize = 100;
    const MAX_BLOCKED_TERM_CHARS: usize = 200;
    const MAX_PROMPT_TOKENS: i64 = 10_000_000;

    if input.blocked_terms.len() > MAX_BLOCKED_TERMS {
        return Err(AppError::BadRequest(format!(
            "at most {MAX_BLOCKED_TERMS} blocked terms are allowed"
        )));
    }
    if let Some(limit) = input.max_prompt_tokens
        && !(1..=MAX_PROMPT_TOKENS).contains(&limit)
    {
        return Err(AppError::BadRequest(format!(
            "max_prompt_tokens must be between 1 and {MAX_PROMPT_TOKENS}"
        )));
    }

    let mut seen = std::collections::HashSet::new();
    let mut blocked_terms = Vec::new();
    for term in input.blocked_terms.drain(..) {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        let chars = term.chars().count();
        if chars < 2 || chars > MAX_BLOCKED_TERM_CHARS {
            return Err(AppError::BadRequest(format!(
                "blocked terms must contain between 2 and {MAX_BLOCKED_TERM_CHARS} characters"
            )));
        }
        if seen.insert(term.to_lowercase()) {
            blocked_terms.push(term.to_string());
        }
    }
    input.blocked_terms = blocked_terms;
    Ok(input)
}

pub async fn get_inspector_settings(
    State(state): State<AppState>,
) -> AppResult<Json<InspectorSettings>> {
    Ok(Json(state.inspector_settings().await?))
}

pub async fn update_inspector_settings(
    State(state): State<AppState>,
    Json(input): Json<InspectorSettings>,
) -> AppResult<Json<InspectorSettings>> {
    let settings = normalize_inspector_settings(input)?;
    if settings == InspectorSettings::default() {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTING_INSPECTOR)
            .execute(&state.pool)
            .await?;
    } else {
        let value = serde_json::to_string(&settings).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to serialize inspector settings: {error}"
            ))
        })?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_INSPECTOR)
        .bind(value)
        .execute(&state.pool)
        .await?;
    }
    *state.inspector.write().await = None;
    record_audit(
        &state,
        "update",
        "settings",
        Some(SETTING_INSPECTOR),
        if settings.capture_request_previews {
            "updated request inspector"
        } else {
            "disabled request inspector"
        },
        Some(json!({
            "capture_request_previews": settings.capture_request_previews,
            "request_preview_max_chars": settings.request_preview_max_chars,
        })),
    )
    .await;
    Ok(Json(settings))
}

fn normalize_inspector_settings(mut input: InspectorSettings) -> AppResult<InspectorSettings> {
    const MIN_PREVIEW_CHARS: i64 = 256;
    const MAX_PREVIEW_CHARS: i64 = 65_536;
    if !(MIN_PREVIEW_CHARS..=MAX_PREVIEW_CHARS).contains(&input.request_preview_max_chars) {
        return Err(AppError::BadRequest(format!(
            "request_preview_max_chars must be between {MIN_PREVIEW_CHARS} and {MAX_PREVIEW_CHARS}"
        )));
    }
    if !input.capture_request_previews {
        input.request_preview_max_chars = InspectorSettings::default().request_preview_max_chars;
    }
    Ok(input)
}

pub async fn get_resilience_settings(
    State(state): State<AppState>,
) -> AppResult<Json<ResilienceSettings>> {
    Ok(Json(state.resilience_settings().await?))
}

pub async fn update_resilience_settings(
    State(state): State<AppState>,
    Json(input): Json<ResilienceSettings>,
) -> AppResult<Json<ResilienceSettings>> {
    let settings = normalize_resilience_settings(input)?;
    if settings == ResilienceSettings::default() {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTING_RESILIENCE)
            .execute(&state.pool)
            .await?;
    } else {
        let value = serde_json::to_string(&settings).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to serialize resilience settings: {error}"
            ))
        })?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_RESILIENCE)
        .bind(value)
        .execute(&state.pool)
        .await?;
    }
    *state.resilience.write().await = None;
    record_audit(
        &state,
        "update",
        "settings",
        Some(SETTING_RESILIENCE),
        if settings.max_retries == 0 {
            "disabled same-target retries"
        } else {
            "updated same-target retry policy"
        },
        Some(json!({
            "max_retries": settings.max_retries,
            "retry_backoff_ms": settings.retry_backoff_ms,
            "retry_max_backoff_ms": settings.retry_max_backoff_ms,
        })),
    )
    .await;
    Ok(Json(settings))
}

fn normalize_resilience_settings(mut input: ResilienceSettings) -> AppResult<ResilienceSettings> {
    const MAX_RETRIES: i64 = 5;
    const MAX_BACKOFF_MS: i64 = 60_000;
    if !(0..=MAX_RETRIES).contains(&input.max_retries) {
        return Err(AppError::BadRequest(format!(
            "max_retries must be between 0 and {MAX_RETRIES}"
        )));
    }
    if !(0..=MAX_BACKOFF_MS).contains(&input.retry_backoff_ms) {
        return Err(AppError::BadRequest(format!(
            "retry_backoff_ms must be between 0 and {MAX_BACKOFF_MS}"
        )));
    }
    if !(0..=MAX_BACKOFF_MS).contains(&input.retry_max_backoff_ms) {
        return Err(AppError::BadRequest(format!(
            "retry_max_backoff_ms must be between 0 and {MAX_BACKOFF_MS}"
        )));
    }
    if input.retry_max_backoff_ms < input.retry_backoff_ms {
        return Err(AppError::BadRequest(
            "retry_max_backoff_ms must not be smaller than retry_backoff_ms".to_string(),
        ));
    }
    if input.max_retries == 0 {
        let defaults = ResilienceSettings::default();
        input.retry_backoff_ms = defaults.retry_backoff_ms;
        input.retry_max_backoff_ms = defaults.retry_max_backoff_ms;
    }
    Ok(input)
}

pub async fn event_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AdminTokenQuery>,
) -> Response {
    if let Some(expected) = &state.admin_token {
        let query_token = query.admin_token.as_deref();
        let header_token = headers
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(&headers));
        if query_token != Some(expected.as_str()) && header_token != Some(expected.as_str()) {
            return AppError::Unauthorized("a valid admin token is required".to_string())
                .into_response();
        }
    }
    let mut receiver = state.events.subscribe();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b"event: ready\ndata: {}\n\n"));
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    yield Ok(Bytes::from_static(b": keep-alive\n\n"));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(event) => {
                            let payload = json!({
                                "id": event.id,
                                "request_id": event.request_id,
                                "success": event.success,
                                "streamed": event.streamed
                            });
                            yield Ok(Bytes::from(format!("event: usage\ndata: {payload}\n\n")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
