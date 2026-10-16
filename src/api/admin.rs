use super::*;

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "openllm",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Readiness probe for a load balancer.
///
/// Unlike [`health`], this reflects whether the instance can actually serve:
/// it touches the database with a short timeout, and a `503` tells a proxy to
/// stop routing traffic here instead of returning errors for every request.
pub async fn ready(State(state): State<AppState>) -> Response {
    // A shutting-down instance must leave rotation before its connections
    // close, so the load balancer drains it while in-flight work completes.
    if state
        .shutting_down
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "draining",
                "service": "openllm",
                "version": env!("CARGO_PKG_VERSION")
            })),
        )
            .into_response();
    }
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&state.pool),
    )
    .await;
    let failure = match probe {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("database probe timed out".to_string()),
    };
    match failure {
        None => Json(json!({
            "status": "ready",
            "service": "openllm",
            "version": env!("CARGO_PKG_VERSION")
        }))
        .into_response(),
        Some(error) => {
            tracing::warn!(%error, "readiness probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "openllm",
                    "error": error
                })),
            )
                .into_response()
        }
    }
}

/// Live runtime snapshot for operators and external monitors.
///
/// Consolidates the process-local signals that otherwise only exist in the
/// Prometheus exposition or scattered endpoints, so one request answers "is
/// this instance healthy right now, and if not, why". Unlike `/api/ready` it
/// always answers `200` and reports the details rather than a bare status.
#[derive(Debug, serde::Serialize)]
pub struct RuntimeStatus {
    pub version: &'static str,
    pub uptime_seconds: u64,
    pub draining: bool,
    pub runtime_lag_millis: u64,
    pub memory: MemoryStatus,
    pub admission: AdmissionStatus,
    pub database: DatabasePoolStatus,
    pub maintenance: MaintenanceStatus,
    pub circuits: CircuitSummary,
}

#[derive(Debug, serde::Serialize)]
pub struct MemoryStatus {
    /// Configured ceiling in MiB; `0` means the pressure guard is off.
    pub limit_mib: u64,
    pub used_mib: Option<u64>,
    pub used_ratio: Option<f64>,
    pub pressure: bool,
    /// Where the ceiling came from: `off`, `env`, or `cgroup`.
    pub source: &'static str,
    pub shed_total: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct AdmissionStatus {
    pub in_flight: i64,
    pub max_concurrent_requests: usize,
    pub in_flight_request_mib: usize,
    pub max_inflight_request_mib: usize,
    pub requests_shed_total: u64,
    pub request_bytes_shed_total: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct DatabasePoolStatus {
    pub connections: u32,
    pub idle: usize,
    pub max_connections: u32,
}

#[derive(Debug, serde::Serialize)]
pub struct MaintenanceStatus {
    pub runs_total: u64,
    pub last_run_unix: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct CircuitSummary {
    /// Providers currently short-circuited (hard-open).
    pub open: usize,
    /// Providers whose cooldown elapsed and are probing recovery.
    pub half_open: usize,
    pub threshold: u32,
}

pub async fn get_status(State(state): State<AppState>) -> Json<RuntimeStatus> {
    Json(collect_status(&state).await)
}

/// Gathers the runtime status payload. The diagnostics snapshot embeds the same
/// value, so an operator's exported file and the console card cannot disagree.
pub(crate) async fn collect_status(state: &AppState) -> RuntimeStatus {
    use std::sync::atomic::Ordering::Relaxed;

    let limits = state.runtime_limits();
    let used_bytes = state
        .memory_pressure
        .as_ref()
        .and_then(|pressure| pressure.used_bytes())
        .or_else(crate::state::sample_process_memory_bytes);
    let memory_limit = state
        .memory_pressure
        .as_ref()
        .map(|pressure| pressure.limit_bytes())
        .unwrap_or(0);
    let memory_pressure = state
        .memory_pressure
        .as_ref()
        .is_some_and(|pressure| pressure.is_shedding());

    // A failing DB probe must not take the whole status down: report zero and
    // let the readiness probe be the one that fails loudly.
    let in_flight =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);

    let threshold = crate::state::provider_open_threshold();
    let (mut open, mut half_open) = (0usize, 0usize);
    if threshold > 0 {
        let now = std::time::Instant::now();
        let cooling = state
            .provider_cooldown
            .lock()
            .await
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(provider_id, _)| *provider_id)
            .collect::<HashSet<_>>();
        for (provider_id, streak) in state.provider_failure_streak.lock().await.iter() {
            match crate::state::provider_circuit_state(
                *streak,
                cooling.contains(provider_id),
                threshold,
            ) {
                "open" => open += 1,
                "half_open" => half_open += 1,
                _ => {}
            }
        }
    }

    RuntimeStatus {
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        draining: state.shutting_down.load(Relaxed),
        runtime_lag_millis: state.runtime_lag_millis.load(Relaxed),
        memory: MemoryStatus {
            limit_mib: memory_limit / (1024 * 1024),
            used_mib: used_bytes.map(|bytes| bytes / (1024 * 1024)),
            used_ratio: used_bytes
                .filter(|_| memory_limit > 0)
                .map(|bytes| bytes as f64 / memory_limit as f64),
            pressure: memory_pressure,
            source: state.memory_limit_source,
            shed_total: state.memory_shed.load(Relaxed),
        },
        admission: AdmissionStatus {
            in_flight,
            max_concurrent_requests: limits.max_concurrent_requests,
            in_flight_request_mib: state
                .inflight_request_bytes
                .as_ref()
                .map(|budget| budget.used() / (1024 * 1024))
                .unwrap_or(0),
            max_inflight_request_mib: limits.max_inflight_request_mib,
            requests_shed_total: state.requests_shed.load(Relaxed),
            request_bytes_shed_total: state.request_bytes_shed.load(Relaxed),
        },
        database: DatabasePoolStatus {
            connections: state.pool.size(),
            idle: state.pool.num_idle(),
            max_connections: crate::db::db_max_connections(),
        },
        maintenance: MaintenanceStatus {
            runs_total: state.maintenance_runs.load(Relaxed),
            last_run_unix: state.maintenance_last_run_unix.load(Relaxed),
        },
        circuits: CircuitSummary {
            open,
            half_open,
            threshold,
        },
    }
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
        limits: state.runtime_limits(),
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
        limits: state.runtime_limits(),
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
        if settings.stream_recovery_enabled {
            "enabled stream recovery and updated retry policy"
        } else if settings.max_retries == 0 {
            "disabled same-target retries"
        } else {
            "updated same-target retry policy"
        },
        Some(json!({
            "stream_recovery_enabled": settings.stream_recovery_enabled,
            "stream_recovery_max_retries": settings.stream_recovery_max_retries,
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
    if !(0..=STREAM_RECOVERY_MAX_RETRIES).contains(&input.stream_recovery_max_retries) {
        return Err(AppError::BadRequest(format!(
            "stream_recovery_max_retries must be between 0 and {STREAM_RECOVERY_MAX_RETRIES}"
        )));
    }
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
    if !input.stream_recovery_enabled {
        input.stream_recovery_max_retries =
            ResilienceSettings::default().stream_recovery_max_retries;
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
