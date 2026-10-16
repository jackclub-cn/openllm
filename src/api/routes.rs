use super::*;

pub async fn list_routes(State(state): State<AppState>) -> AppResult<Json<Vec<RouteView>>> {
    let routes = sqlx::query_as::<_, Route>(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy, \
                enabled, context_override, output_override, \
                all_targets_busy_wait_seconds, retry_jitter_ms, \
                max_waiting_requests, keepalive_during_wait, \
                created_at, updated_at \
         FROM routes ORDER BY enabled DESC, model_pattern COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut targets_by_route = HashMap::<i64, Vec<RouteTarget>>::new();
    for target in route_targets(&state, None).await? {
        if let Some(route_id) = target.route_id {
            targets_by_route.entry(route_id).or_default().push(target);
        }
    }
    let views = routes
        .into_iter()
        .map(|route| {
            let targets = targets_by_route.remove(&route.id).unwrap_or_default();
            build_route_view(route, targets)
        })
        .collect();
    Ok(Json(views))
}

pub async fn create_route(
    State(state): State<AppState>,
    Json(input): Json<RouteInput>,
) -> AppResult<(StatusCode, Json<RouteView>)> {
    validate_route_input(&input)?;
    let context_override = normalize_route_override(input.context_override, "context")?;
    let output_override = normalize_route_override(input.output_override, "output")?;
    let all_targets_busy_wait_seconds =
        normalize_busy_wait_seconds(input.all_targets_busy_wait_seconds)?;
    let retry_jitter_ms = normalize_retry_jitter(input.retry_jitter_ms)?;
    let max_waiting_requests = normalize_max_waiting_requests(input.max_waiting_requests)?;
    let keepalive_during_wait = input.keepalive_during_wait.unwrap_or(true);
    validate_route_overrides(&state, &input.targets, context_override, output_override).await?;
    let mut tx = state.pool.begin().await?;
    let (strategy, strategy_ext) = input.strategy.storage_values();
    let result = sqlx::query(
        "INSERT INTO routes (name, model_pattern, strategy, strategy_ext, enabled, \
                             context_override, output_override, \
                             all_targets_busy_wait_seconds, retry_jitter_ms, \
                             max_waiting_requests, keepalive_during_wait) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.model_pattern.trim())
    .bind(strategy)
    .bind(strategy_ext)
    .bind(input.enabled as i64)
    .bind(context_override)
    .bind(output_override)
    .bind(all_targets_busy_wait_seconds)
    .bind(retry_jitter_ms)
    .bind(max_waiting_requests)
    .bind(keepalive_during_wait as i64)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    let id = result.last_insert_rowid();
    replace_route_targets(&mut tx, id, &input.targets).await?;
    tx.commit().await?;

    record_audit(
        &state,
        "create",
        "route",
        Some(&id.to_string()),
        &format!("created route '{}'", input.name.trim()),
        Some(json!({
            "model_pattern": input.model_pattern.trim(),
            "strategy": input.strategy.as_str(),
            "targets": input.targets.len(),
            "context_override": context_override,
            "output_override": output_override,
            "all_targets_busy_wait_seconds": all_targets_busy_wait_seconds,
            "retry_jitter_ms": retry_jitter_ms,
            "max_waiting_requests": max_waiting_requests,
            "keepalive_during_wait": keepalive_during_wait,
        })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(get_route(&state, id).await?)))
}

pub async fn update_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<RouteUpdate>,
) -> AppResult<Json<RouteView>> {
    let current = sqlx::query_as::<_, Route>(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy, \
                enabled, context_override, output_override, \
                all_targets_busy_wait_seconds, retry_jitter_ms, \
                max_waiting_requests, keepalive_during_wait, \
                created_at, updated_at \
         FROM routes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("route not found".to_string()))?;

    let name = input.name.unwrap_or(current.name).trim().to_string();
    let model_pattern = input
        .model_pattern
        .unwrap_or(current.model_pattern)
        .trim()
        .to_string();
    let strategy = input
        .strategy
        .unwrap_or(RouteStrategy::from_str(&current.strategy).map_err(AppError::BadRequest)?);
    let enabled = input.enabled.unwrap_or(current.enabled != 0);
    let context_override = match input.context_override {
        Some(value) => normalize_route_override(value, "context")?,
        None => current.context_override,
    };
    let output_override = match input.output_override {
        Some(value) => normalize_route_override(value, "output")?,
        None => current.output_override,
    };
    let all_targets_busy_wait_seconds = match input.all_targets_busy_wait_seconds {
        Some(value) => normalize_busy_wait_seconds(Some(value))?,
        None => current.all_targets_busy_wait_seconds,
    };
    let retry_jitter_ms = match input.retry_jitter_ms {
        Some(value) => normalize_retry_jitter(Some(value))?,
        None => current.retry_jitter_ms,
    };
    let max_waiting_requests = match input.max_waiting_requests {
        Some(value) => normalize_max_waiting_requests(Some(value))?,
        None => current.max_waiting_requests,
    };
    let keepalive_during_wait = input
        .keepalive_during_wait
        .unwrap_or(current.keepalive_during_wait != 0);

    if name.is_empty() || model_pattern.is_empty() {
        return Err(AppError::BadRequest(
            "route name and model pattern are required".to_string(),
        ));
    }
    validate_model_pattern(&model_pattern)?;
    let targets_for_validation = match input.targets.as_ref() {
        Some(targets) => {
            validate_targets(targets)?;
            targets.clone()
        }
        None => route_targets(&state, Some(id))
            .await?
            .into_iter()
            .map(|target| RouteTargetInput {
                id: Some(target.id),
                provider_id: target.provider_id,
                upstream_model: target.upstream_model,
                weight: target.weight,
                priority: target.priority,
                enabled: target.enabled != 0,
            })
            .collect(),
    };
    validate_route_overrides(
        &state,
        &targets_for_validation,
        context_override,
        output_override,
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    let (stored_strategy, stored_strategy_ext) = strategy.storage_values();
    sqlx::query(
        "UPDATE routes SET name = ?, model_pattern = ?, strategy = ?, strategy_ext = ?, \
         enabled = ?, context_override = ?, output_override = ?, \
         all_targets_busy_wait_seconds = ?, retry_jitter_ms = ?, \
         max_waiting_requests = ?, keepalive_during_wait = ?, \
         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name.clone())
    .bind(model_pattern)
    .bind(stored_strategy)
    .bind(stored_strategy_ext)
    .bind(enabled as i64)
    .bind(context_override)
    .bind(output_override)
    .bind(all_targets_busy_wait_seconds)
    .bind(retry_jitter_ms)
    .bind(max_waiting_requests)
    .bind(keepalive_during_wait as i64)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(targets) = input.targets {
        replace_route_targets(&mut tx, id, &targets).await?;
    }
    tx.commit().await?;
    crate::proxy::clear_target_sinks_for_route(&state, id).await;

    record_audit(
        &state,
        "update",
        "route",
        Some(&id.to_string()),
        &format!("updated route '{name}'"),
        None,
    )
    .await;
    Ok(Json(get_route(&state, id).await?))
}

fn normalize_route_override(value: Option<i64>, label: &str) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value <= 0 => Err(AppError::BadRequest(format!(
            "route {label} override must be greater than zero"
        ))),
        value => Ok(value),
    }
}

fn normalize_busy_wait_seconds(value: Option<i64>) -> AppResult<i64> {
    const DEFAULT_BUSY_WAIT_SECONDS: i64 = 0;
    const MAX_BUSY_WAIT_SECONDS: i64 = 300;
    match value.unwrap_or(DEFAULT_BUSY_WAIT_SECONDS) {
        value if value < 0 => Err(AppError::BadRequest(
            "all-targets busy wait must be zero or a positive integer".to_string(),
        )),
        value if value > MAX_BUSY_WAIT_SECONDS => Err(AppError::BadRequest(format!(
            "all-targets busy wait must be at most {MAX_BUSY_WAIT_SECONDS} seconds"
        ))),
        value => Ok(value),
    }
}

fn normalize_retry_jitter(value: Option<i64>) -> AppResult<i64> {
    const DEFAULT_RETRY_JITTER_MS: i64 = 250;
    const MAX_RETRY_JITTER_MS: i64 = 10_000;
    match value.unwrap_or(DEFAULT_RETRY_JITTER_MS) {
        value if value < 0 => Err(AppError::BadRequest(
            "retry jitter must be zero or a positive integer".to_string(),
        )),
        value if value > MAX_RETRY_JITTER_MS => Err(AppError::BadRequest(format!(
            "retry jitter must be at most {MAX_RETRY_JITTER_MS} milliseconds"
        ))),
        value => Ok(value),
    }
}

fn normalize_max_waiting_requests(value: Option<i64>) -> AppResult<i64> {
    const DEFAULT_MAX_WAITING_REQUESTS: i64 = 32;
    const MAX_WAITING_REQUESTS: i64 = 1024;
    match value.unwrap_or(DEFAULT_MAX_WAITING_REQUESTS) {
        value if value < 0 => Err(AppError::BadRequest(
            "max waiting requests must be zero or a positive integer".to_string(),
        )),
        value if value > MAX_WAITING_REQUESTS => Err(AppError::BadRequest(format!(
            "max waiting requests must be at most {MAX_WAITING_REQUESTS}"
        ))),
        value => Ok(value),
    }
}

async fn validate_route_overrides(
    state: &AppState,
    targets: &[RouteTargetInput],
    context_override: Option<i64>,
    output_override: Option<i64>,
) -> AppResult<()> {
    if context_override.is_none() && output_override.is_none() {
        return Ok(());
    }
    let pairs = targets
        .iter()
        .filter(|target| target.enabled)
        .map(|target| (target.provider_id, target.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
    crate::registry::validate_route_capability_overrides(&barrel, context_override, output_override)
}

pub async fn diagnose_route(
    State(state): State<AppState>,
    Json(input): Json<RouteDiagnoseInput>,
) -> AppResult<Json<RouteDiagnoseView>> {
    let model = input.model.trim();
    if model.is_empty() {
        return Err(AppError::BadRequest("model is required".to_string()));
    }
    let endpoint = input.endpoint.trim().trim_end_matches('/');
    if !endpoint.starts_with('/') {
        return Err(AppError::BadRequest(
            "endpoint must start with '/'".to_string(),
        ));
    }
    let session_id = input
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(api_key_id) = input.api_key_id {
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM api_keys WHERE id = ?")
            .bind(api_key_id)
            .fetch_optional(&state.pool)
            .await?;
        if exists.is_none() {
            return Err(AppError::BadRequest(format!(
                "gateway API key {api_key_id} does not exist"
            )));
        }
    }
    let diagnosis = match input.api_key_id {
        Some(api_key_id) => {
            crate::proxy::diagnose_route_for_key(
                &state,
                model,
                endpoint,
                session_id,
                Some(api_key_id),
            )
            .await?
        }
        None => crate::proxy::diagnose_route(&state, model, endpoint, session_id).await?,
    };
    Ok(Json(diagnosis))
}

pub async fn delete_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM routes WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("route not found".to_string()));
    }
    record_audit(
        &state,
        "delete",
        "route",
        Some(&id.to_string()),
        "deleted route",
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
