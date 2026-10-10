use super::*;

pub async fn list_routes(State(state): State<AppState>) -> AppResult<Json<Vec<RouteView>>> {
    let routes = sqlx::query_as::<_, Route>(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy, \
                enabled, created_at, updated_at \
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
    let mut tx = state.pool.begin().await?;
    let (strategy, strategy_ext) = input.strategy.storage_values();
    let result = sqlx::query(
        "INSERT INTO routes (name, model_pattern, strategy, strategy_ext, enabled) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.model_pattern.trim())
    .bind(strategy)
    .bind(strategy_ext)
    .bind(input.enabled as i64)
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
                enabled, created_at, updated_at \
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

    if name.is_empty() || model_pattern.is_empty() {
        return Err(AppError::BadRequest(
            "route name and model pattern are required".to_string(),
        ));
    }
    validate_model_pattern(&model_pattern)?;

    let mut tx = state.pool.begin().await?;
    let (stored_strategy, stored_strategy_ext) = strategy.storage_values();
    sqlx::query(
        "UPDATE routes SET name = ?, model_pattern = ?, strategy = ?, strategy_ext = ?, \
         enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name.clone())
    .bind(model_pattern)
    .bind(stored_strategy)
    .bind(stored_strategy_ext)
    .bind(enabled as i64)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(targets) = input.targets {
        validate_targets(&targets)?;
        replace_route_targets(&mut tx, id, &targets).await?;
    }
    tx.commit().await?;

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
    Ok(Json(
        crate::proxy::diagnose_route(&state, model, endpoint, session_id).await?,
    ))
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
