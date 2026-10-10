use super::*;

pub async fn sync_provider_models(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ModelSyncResult>> {
    Ok(Json(sync_provider(state, id).await?))
}

pub async fn preview_provider_model_sync(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ModelSyncPreview>> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let entries = fetch_provider_entries(&state, &provider).await?;
    let existing = sqlx::query_as::<_, ProviderModelPreviewRow>(
        "SELECT model_name, enabled, context_limit, input_limit, output_limit, \
                supported_endpoints, cost, display_name \
         FROM provider_models WHERE provider_id = ?",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| (row.model_name.clone(), row))
    .collect::<HashMap<_, _>>();
    let catalog = models_dev::try_load(&state).await;
    let provider_hint = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(&provider.name, &provider.base_url));
    let changed = detect_model_sync_changes(
        &entries,
        &existing,
        catalog.as_deref(),
        provider_hint.as_deref(),
    );

    Ok(Json(build_model_sync_preview(
        id, &entries, &existing, changed,
    )))
}

pub(crate) fn build_model_sync_preview(
    provider_id: i64,
    entries: &[(String, UpstreamModelInfo)],
    existing: &HashMap<String, ProviderModelPreviewRow>,
    changed: Vec<ModelSyncChange>,
) -> ModelSyncPreview {
    let upstream = entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<HashSet<_>>();
    let added = entries
        .iter()
        .filter(|(name, _)| !existing.contains_key(name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let removed = existing
        .keys()
        .filter(|name| !upstream.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    let retained = entries
        .iter()
        .filter(|(name, _)| existing.contains_key(name))
        .count();
    let disabled_retained = existing
        .iter()
        .filter(|(name, row)| row.enabled == 0 && upstream.contains(*name))
        .count();
    ModelSyncPreview {
        provider_id,
        added,
        removed,
        changed,
        retained,
        disabled_retained,
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ProviderModelPreviewRow {
    pub(crate) model_name: String,
    pub(crate) enabled: i64,
    pub(crate) context_limit: Option<i64>,
    pub(crate) input_limit: Option<i64>,
    pub(crate) output_limit: Option<i64>,
    pub(crate) supported_endpoints: Option<String>,
    pub(crate) cost: Option<String>,
    pub(crate) display_name: Option<String>,
}

pub(crate) fn detect_model_sync_changes(
    entries: &[(String, UpstreamModelInfo)],
    existing: &HashMap<String, ProviderModelPreviewRow>,
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
) -> Vec<ModelSyncChange> {
    let mut changed = Vec::new();
    for (model, upstream) in entries {
        let Some(current) = existing.get(model) else {
            continue;
        };
        let capabilities = catalog
            .and_then(|catalog| catalog.lookup(provider_hint, model))
            .unwrap_or_default()
            .with_effective_input_limit();
        let context_limit = min_known(upstream.context_limit, capabilities.context_limit);
        let input_limit = min_known(context_limit, capabilities.input_limit);
        let supported_endpoints = (!upstream.supported_endpoints.is_empty())
            .then(|| serde_json::to_string(&upstream.supported_endpoints).ok())
            .flatten();
        let cost = capabilities
            .cost
            .as_ref()
            .and_then(|cost| serde_json::to_string(cost).ok());

        let mut fields = Vec::new();
        if current.context_limit != context_limit {
            fields.push("context_limit");
        }
        if current.input_limit != input_limit {
            fields.push("input_limit");
        }
        if current.output_limit != capabilities.output_limit {
            fields.push("output_limit");
        }
        if current.supported_endpoints != supported_endpoints {
            fields.push("supported_endpoints");
        }
        if current.cost != cost {
            fields.push("cost");
        }
        if current.display_name != upstream.display_name {
            fields.push("display_name");
        }
        if !fields.is_empty() {
            changed.push(ModelSyncChange {
                model_name: model.clone(),
                fields: fields.into_iter().map(ToOwned::to_owned).collect(),
            });
        }
    }
    changed
}

pub(crate) async fn fetch_provider_entries(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<(String, UpstreamModelInfo)>> {
    let provider_type =
        ProviderType::from_str(&provider.provider_type).map_err(AppError::BadRequest)?;
    let (url, ollama_style) = match provider_type {
        ProviderType::Anthropic => (
            format!("{}/v1/models", provider.base_url.trim_end_matches('/')),
            false,
        ),
        ProviderType::Ollama => (
            format!("{}/api/tags", ollama_root(&provider.base_url)),
            true,
        ),
        ProviderType::Openai | ProviderType::Custom => (
            format!("{}/models", provider.base_url.trim_end_matches('/')),
            false,
        ),
    };

    let mut last_error = None;
    for (_, key) in provider_key_candidates(state, provider).await? {
        let mut request = state.client.get(&url);
        if let Some(key) = key {
            request = match provider_type {
                ProviderType::Anthropic => request
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01"),
                _ => request.bearer_auth(key),
            };
        }
        request = apply_custom_headers(request, &provider.headers)?;

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(format!(
                    "failed to fetch models from {}: {error}",
                    provider.name
                ));
                continue;
            }
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes)
                .chars()
                .take(400)
                .collect::<String>();
            last_error = Some(format!("{} returned {}: {}", provider.name, status, body));
            if matches!(status.as_u16(), 401 | 403 | 408 | 409 | 425 | 429)
                || status.is_server_error()
            {
                continue;
            }
            break;
        }

        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Upstream(format!("invalid model list response: {error}")))?;
        let entries = if ollama_style {
            names_to_entries(&parse_ollama_models(&value))
        } else {
            parse_openai_model_entries(&value)
        };
        if entries.is_empty() {
            return Err(AppError::Upstream(
                "upstream model list did not contain any recognizable models".to_string(),
            ));
        }
        return Ok(entries);
    }

    Err(AppError::Upstream(last_error.unwrap_or_else(|| {
        format!("no credentials available for {}", provider.name)
    })))
}

pub(crate) async fn sync_provider(state: AppState, id: i64) -> AppResult<ModelSyncResult> {
    {
        let mut running = state.provider_model_sync.lock().await;
        if !running.insert(id) {
            return Err(AppError::Conflict(
                "model synchronization is already in progress for this provider".to_string(),
            ));
        }
    }

    let attempted_at = Utc::now().to_rfc3339();
    if let Err(error) = sqlx::query(
        "UPDATE providers \
         SET models_sync_attempted_at = ?, models_sync_error = NULL, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(&attempted_at)
    .bind(id)
    .execute(&state.pool)
    .await
    {
        state.provider_model_sync.lock().await.remove(&id);
        return Err(AppError::Database(error));
    }

    let result = sync_provider_inner(&state, id).await;
    if let Err(error) = &result {
        let _ = sqlx::query(
            "UPDATE providers \
             SET models_sync_error = ?, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(error.to_string())
        .bind(id)
        .execute(&state.pool)
        .await;
    }
    state.provider_model_sync.lock().await.remove(&id);
    result
}

pub(crate) async fn sync_provider_inner(state: &AppState, id: i64) -> AppResult<ModelSyncResult> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let entries = fetch_provider_entries(state, &provider).await?;
    let models = entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();

    let catalog = models_dev::try_load(state).await;
    // Re-resolve the provider match on every sync: the stored id may be stale
    // (for example a provider renamed after a catalog update).
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(&provider.name, &provider.base_url));

    let mut tx = state.pool.begin().await?;
    replace_provider_models(
        &mut tx,
        id,
        &entries,
        catalog.as_deref(),
        models_dev_id.as_deref(),
    )
    .await?;
    let synced_at = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE providers SET models_dev_id = ?, models_synced_at = ?, models_sync_error = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(models_dev_id.as_deref())
    .bind(&synced_at)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ModelSyncResult {
        ok: true,
        provider_id: id,
        count: models.len(),
        models,
        synced_at,
        message: "models synchronized".to_string(),
    })
}
