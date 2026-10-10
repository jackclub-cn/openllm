use super::*;

pub async fn list_providers(State(state): State<AppState>) -> AppResult<Json<Vec<ProviderView>>> {
    let providers = sqlx::query_as::<_, Provider>(
        "SELECT * FROM providers ORDER BY enabled DESC, name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut views = providers
        .into_iter()
        .map(ProviderView::from)
        .collect::<Vec<_>>();
    hydrate_provider_views(&state, &mut views).await?;
    Ok(Json(views))
}

pub async fn create_provider(
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> AppResult<(StatusCode, Json<ProviderView>)> {
    validate_provider_input(&input)?;
    let headers = serde_json::to_string(&input.headers).unwrap_or_else(|_| "{}".to_string());
    let model_prefix = normalize_model_prefix(&input.model_prefix)?;
    let api_key = normalize_optional(input.api_key.clone());
    let mut api_keys = input.api_keys.clone();
    if api_keys.is_empty()
        && let Some(secret) = api_key.clone()
    {
        api_keys.push(provider_api_key_input(None, "Default", Some(secret), true));
    }
    let base_url = normalize_base_url(&input.base_url);
    let health_check_interval_minutes =
        normalize_health_interval(input.health_check_interval_minutes)?;
    let health_check_model = normalize_health_check_model(input.health_check_model.as_deref());
    let models_sync_interval_minutes =
        normalize_health_interval(input.models_sync_interval_minutes)?;
    let timeout_seconds = normalize_provider_timeout(input.timeout_seconds)?;
    let cooldown_seconds = normalize_provider_cooldown(input.configured_cooldown_seconds)?;
    let max_concurrency = normalize_provider_concurrency(input.max_concurrency)?;
    let queue_timeout_seconds =
        normalize_provider_queue_timeout(input.queue_timeout_seconds)?;
    // Resolve metadata before opening the transaction: the catalog fetch may
    // hit the network, and holding a SQLite write transaction across it would
    // block every other writer.
    let catalog = models_dev::try_load(&state).await;
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(input.name.trim(), &base_url));

    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO providers (
            name, provider_type, base_url, model_prefix, models_dev_id,
            api_key, headers, enabled, tool_search_supported,
            health_check_interval_minutes, health_check_model,
            models_sync_interval_minutes, timeout_seconds, cooldown_seconds,
            max_concurrency, queue_timeout_seconds
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(api_key)
    .bind(headers)
    .bind(input.enabled as i64)
    .bind(health_check_interval_minutes)
    .bind(health_check_model)
    .bind(models_sync_interval_minutes)
    .bind(timeout_seconds)
    .bind(cooldown_seconds)
    .bind(max_concurrency)
    .bind(queue_timeout_seconds)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    let id = result.last_insert_rowid();
    replace_provider_api_keys(&mut tx, id, &api_keys).await?;
    let entries = names_to_entries(&input.models);
    replace_provider_models(
        &mut tx,
        id,
        &entries,
        catalog.as_deref(),
        models_dev_id.as_deref(),
    )
    .await?;
    tx.commit().await?;

    if input.auto_sync_models
        && let Err(error) = sync_provider(state.clone(), id).await
    {
        tracing::warn!(provider_id = id, %error, "automatic model sync failed");
    }
    record_audit(
        &state,
        "create",
        "provider",
        Some(&id.to_string()),
        &format!("created provider '{}'", input.name.trim()),
        Some(json!({
            "name": input.name.trim(),
            "base_url": input.base_url,
            "enabled": input.enabled,
        })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(get_provider(&state, id).await?)))
}

pub async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderUpdate>,
) -> AppResult<Json<ProviderView>> {
    let current = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;

    let name = input
        .name
        .unwrap_or_else(|| current.name.clone())
        .trim()
        .to_string();
    let provider_type = input
        .provider_type
        .unwrap_or(ProviderType::from_str(&current.provider_type).map_err(AppError::BadRequest)?);
    let base_url = normalize_base_url(
        input
            .base_url
            .as_deref()
            .unwrap_or(current.base_url.as_str()),
    );
    let model_prefix = normalize_model_prefix(
        input
            .model_prefix
            .as_deref()
            .unwrap_or(current.model_prefix.as_str()),
    )?;
    let enabled = input.enabled.unwrap_or(current.enabled != 0);
    let health_check_interval_minutes = match input.health_check_interval_minutes {
        Some(value) => normalize_health_interval(Some(value))?,
        None => current.health_check_interval_minutes,
    };
    let health_check_model = match input.health_check_model.as_deref() {
        Some(value) => normalize_health_check_model(Some(value)),
        None => current.health_check_model.clone(),
    };
    let models_sync_interval_minutes = match input.models_sync_interval_minutes {
        Some(value) => normalize_health_interval(Some(value))?,
        None => current.models_sync_interval_minutes,
    };
    let timeout_seconds = match input.timeout_seconds {
        Some(value) => normalize_provider_timeout(Some(value))?,
        None => current.timeout_seconds,
    };
    let cooldown_seconds = match input.configured_cooldown_seconds {
        Some(value) => normalize_provider_cooldown(Some(value))?,
        None => current.cooldown_seconds,
    };
    let max_concurrency = match input.max_concurrency {
        Some(value) => normalize_provider_concurrency(Some(value))?,
        None => current.max_concurrency,
    };
    let queue_timeout_seconds = match input.queue_timeout_seconds {
        Some(value) => normalize_provider_queue_timeout(Some(value))?,
        None => current.queue_timeout_seconds,
    };
    let concurrency_changed = max_concurrency != current.max_concurrency;
    let api_key_update = normalize_optional(input.api_key.clone());
    let api_keys_update = if let Some(api_keys) = input.api_keys.clone() {
        Some(api_keys)
    } else if input.clear_api_key.unwrap_or(false) {
        Some(Vec::new())
    } else {
        api_key_update
            .map(|secret| vec![provider_api_key_input(None, "Default", Some(secret), true)])
    };
    let api_keys_changed = api_keys_update.is_some();
    let previous_key_ids = if api_keys_changed {
        provider_api_key_records(&state.pool, id)
            .await?
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let headers = match input.headers {
        Some(value) => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
        None => current.headers.clone(),
    };

    if name.is_empty() || base_url.is_empty() {
        return Err(AppError::BadRequest(
            "provider name and base URL are required".to_string(),
        ));
    }

    // Name or base URL may have changed, which can change the models.dev match.
    // A status-only update must not wait on catalog refreshes.
    let identity_changed = name != current.name || base_url != current.base_url;
    let tool_search_context_changed = provider_type.as_str() != current.provider_type
        || base_url != current.base_url
        || headers != current.headers
        || api_keys_changed
        || health_check_model != current.health_check_model
        || input.models.is_some();
    let catalog = if identity_changed || input.models.is_some() {
        models_dev::try_load(&state).await
    } else {
        None
    };
    let models_dev_id = if identity_changed {
        catalog
            .as_ref()
            .and_then(|catalog| catalog.match_provider(&name, &base_url))
    } else {
        current.models_dev_id.clone()
    };

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE providers SET name = ?, provider_type = ?, base_url = ?, model_prefix = ?, models_dev_id = ?, headers = ?, enabled = ?, health_check_interval_minutes = ?, health_check_model = ?, models_sync_interval_minutes = ?, timeout_seconds = ?, cooldown_seconds = ?, max_concurrency = ?, queue_timeout_seconds = ?, tool_search_supported = CASE WHEN ? THEN 1 ELSE tool_search_supported END, tool_search_checked_at = CASE WHEN ? THEN NULL ELSE tool_search_checked_at END, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name.clone())
    .bind(provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(headers)
    .bind(enabled as i64)
    .bind(health_check_interval_minutes)
    .bind(health_check_model)
    .bind(models_sync_interval_minutes)
    .bind(timeout_seconds)
    .bind(cooldown_seconds)
    .bind(max_concurrency)
    .bind(queue_timeout_seconds)
    .bind(tool_search_context_changed as i64)
    .bind(tool_search_context_changed as i64)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(api_keys) = api_keys_update {
        replace_provider_api_keys(&mut tx, id, &api_keys).await?;
    }

    if let Some(models) = input.models {
        let entries = names_to_entries(&models);
        replace_provider_models(
            &mut tx,
            id,
            &entries,
            catalog.as_deref(),
            models_dev_id.as_deref(),
        )
        .await?;
    }
    tx.commit().await?;
    state.provider_cooldown.lock().await.remove(&id);
    if concurrency_changed {
        state.provider_concurrency.lock().await.remove(&id);
    }
    state
        .model_concurrency
        .lock()
        .await
        .retain(|(provider_id, _), _| *provider_id != id);

    if api_keys_changed {
        let current_key_ids =
            sqlx::query_scalar::<_, i64>("SELECT id FROM provider_api_keys WHERE provider_id = ?")
                .bind(id)
                .fetch_all(&state.pool)
                .await?;
        clear_provider_key_cooldowns(&state, previous_key_ids.into_iter().chain(current_key_ids))
            .await;
    }

    if input.auto_sync_models == Some(true)
        && let Err(error) = sync_provider(state.clone(), id).await
    {
        tracing::warn!(provider_id = id, %error, "automatic model sync failed");
    }
    record_audit(
        &state,
        "update",
        "provider",
        Some(&id.to_string()),
        &format!("updated provider '{}'", name.trim()),
        None,
    )
    .await;
    Ok(Json(get_provider(&state, id).await?))
}

pub async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let in_use: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM route_targets WHERE provider_id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await?;
    if in_use > 0 {
        return Err(AppError::Conflict(
            "provider is still used by one or more routes".to_string(),
        ));
    }

    let result = sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("provider not found".to_string()));
    }
    state.provider_cooldown.lock().await.remove(&id);
    state.provider_concurrency.lock().await.remove(&id);
    state
        .model_concurrency
        .lock()
        .await
        .retain(|(provider_id, _), _| *provider_id != id);
    record_audit(
        &state,
        "delete",
        "provider",
        Some(&id.to_string()),
        "deleted provider",
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
