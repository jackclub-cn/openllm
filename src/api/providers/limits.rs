use super::*;

pub async fn list_provider_model_limits(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<ProviderModelLimitView>>> {
    ensure_provider_exists(&state, id).await?;
    Ok(Json(provider_model_limits(&state, id).await?))
}

pub async fn list_model_inventory(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<ModelInventoryView>>> {
    let rows = sqlx::query_as::<_, ModelInventoryRow>(
        r#"
        SELECT pm.provider_id,
               p.name AS provider_name,
               p.provider_type AS provider_type,
               p.enabled AS provider_enabled,
               p.model_prefix,
               pm.model_name,
               pm.enabled,
               COALESCE(pm.context_override, pm.context_limit) AS context_limit,
               CASE
                   WHEN COALESCE(pm.input_override, pm.input_limit) IS NULL
                       THEN COALESCE(pm.context_override, pm.context_limit)
                   WHEN COALESCE(pm.context_override, pm.context_limit) IS NULL
                       THEN COALESCE(pm.input_override, pm.input_limit)
                   ELSE MIN(
                       COALESCE(pm.input_override, pm.input_limit),
                       COALESCE(pm.context_override, pm.context_limit)
                   )
               END AS input_limit,
               COALESCE(pm.output_override, pm.output_limit) AS output_limit,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override
        FROM provider_models pm
        JOIN providers p ON p.id = pm.provider_id
        ORDER BY p.name COLLATE NOCASE, pm.model_name COLLATE NOCASE
        "#,
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn update_provider_model_limits(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderModelLimitsUpdate>,
) -> AppResult<Json<Vec<ProviderModelLimitView>>> {
    ensure_provider_exists(&state, id).await?;
    let mut seen = HashSet::new();
    let mut endpoint_overrides = Vec::with_capacity(input.models.len());
    let mut concurrency_limits = Vec::with_capacity(input.models.len());
    for model in &input.models {
        let name = model.model_name.trim();
        if name.is_empty() {
            return Err(AppError::BadRequest("model name is required".to_string()));
        }
        if !seen.insert(name.to_string()) {
            return Err(AppError::BadRequest(format!(
                "model '{name}' appears more than once"
            )));
        }
        validate_limit("context", model.context_limit)?;
        validate_limit("input", model.input_limit)?;
        validate_limit("output", model.output_limit)?;
        let max_concurrency = normalize_provider_concurrency(model.max_concurrency)?;
        let queue_timeout_seconds =
            normalize_provider_queue_timeout(model.queue_timeout_seconds)?;
        validate_cost_override("input cost", model.cost_input_override)?;
        validate_cost_override("output cost", model.cost_output_override)?;
        validate_cost_override("cache read cost", model.cost_cache_read_override)?;
        validate_cost_override("cache write cost", model.cost_cache_write_override)?;
        if let (Some(context), Some(input)) = (model.context_limit, model.input_limit)
            && input > context
        {
            return Err(AppError::BadRequest(format!(
                "model '{name}' input limit cannot exceed its context limit"
            )));
        }
        endpoint_overrides.push(serialize_endpoint_override(
            model.supported_endpoints_override.as_deref(),
        )?);
        concurrency_limits.push((max_concurrency, queue_timeout_seconds));
    }

    let mut tx = state.pool.begin().await?;
    for ((model, endpoint_override), (max_concurrency, queue_timeout_seconds)) in input
        .models
        .iter()
        .zip(&endpoint_overrides)
        .zip(&concurrency_limits)
    {
        let result = sqlx::query(
            "UPDATE provider_models \
             SET enabled = ?, context_override = ?, input_override = ?, output_override = ?, \
                 max_concurrency = ?, queue_timeout_seconds = ?, \
                 supported_endpoints_override = ?, cost_input_override = ?, \
                 cost_output_override = ?, cost_cache_read_override = ?, \
                 cost_cache_write_override = ? \
             WHERE provider_id = ? AND model_name = ?",
        )
        .bind(model.enabled as i64)
        .bind(model.context_limit)
        .bind(model.input_limit)
        .bind(model.output_limit)
        .bind(max_concurrency)
        .bind(queue_timeout_seconds)
        .bind(endpoint_override)
        .bind(model.cost_input_override)
        .bind(model.cost_output_override)
        .bind(model.cost_cache_read_override)
        .bind(model.cost_cache_write_override)
        .bind(id)
        .bind(model.model_name.trim())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!(
                "model '{}' was not found for this provider",
                model.model_name.trim()
            )));
        }
    }
    tx.commit().await?;
    state
        .model_concurrency
        .lock()
        .await
        .retain(|(provider_id, _), _| *provider_id != id);

    Ok(Json(provider_model_limits(&state, id).await?))
}

pub(crate) async fn ensure_provider_exists(state: &AppState, id: i64) -> AppResult<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM providers WHERE id = ?)")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound("provider not found".to_string()))
    }
}

pub(crate) async fn provider_model_limits(
    state: &AppState,
    provider_id: i64,
) -> AppResult<Vec<ProviderModelLimitView>> {
    let rows = sqlx::query_as::<_, ProviderModelLimitRow>(
        r#"
        SELECT model_name, enabled,
               COALESCE(context_override, context_limit) AS context_limit,
               CASE
                   WHEN COALESCE(input_override, input_limit) IS NULL
                       THEN COALESCE(context_override, context_limit)
                   WHEN COALESCE(context_override, context_limit) IS NULL
                       THEN COALESCE(input_override, input_limit)
                   ELSE MIN(
                       COALESCE(input_override, input_limit),
                       COALESCE(context_override, context_limit)
                   )
               END AS input_limit,
               COALESCE(output_override, output_limit) AS output_limit,
               context_override,
               input_override,
               output_override,
               max_concurrency,
               queue_timeout_seconds,
               COALESCE(supported_endpoints_override, supported_endpoints)
                   AS supported_endpoints,
               supported_endpoints_override,
               cost,
               cost_input_override,
               cost_output_override,
               cost_cache_read_override,
               cost_cache_write_override
        FROM provider_models
        WHERE provider_id = ?
        ORDER BY model_name COLLATE NOCASE
        "#,
    )
    .bind(provider_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ProviderModelLimitRow {
    model_name: String,
    enabled: bool,
    context_limit: Option<i64>,
    input_limit: Option<i64>,
    output_limit: Option<i64>,
    context_override: Option<i64>,
    input_override: Option<i64>,
    output_override: Option<i64>,
    max_concurrency: Option<i64>,
    queue_timeout_seconds: Option<i64>,
    supported_endpoints: Option<String>,
    supported_endpoints_override: Option<String>,
    cost: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
}

impl From<ProviderModelLimitRow> for ProviderModelLimitView {
    fn from(value: ProviderModelLimitRow) -> Self {
        let cost = value
            .cost
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let effective_cost = crate::models::effective_cost_value(
            cost.as_ref(),
            value.cost_input_override,
            value.cost_output_override,
            value.cost_cache_read_override,
            value.cost_cache_write_override,
        );
        Self {
            model_name: value.model_name,
            enabled: value.enabled,
            supported_endpoints: value
                .supported_endpoints
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default(),
            supported_endpoints_override: value
                .supported_endpoints_override
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()),
            context_limit: value.context_limit,
            input_limit: value.input_limit,
            output_limit: value.output_limit,
            context_override: value.context_override,
            input_override: value.input_override,
            output_override: value.output_override,
            max_concurrency: value.max_concurrency,
            queue_timeout_seconds: value.queue_timeout_seconds,
            cost_input: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "input")),
            cost_output: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "output")),
            cost_cache_read: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_read")),
            cost_cache_write: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_write")),
            cost_input_override: value.cost_input_override,
            cost_output_override: value.cost_output_override,
            cost_cache_read_override: value.cost_cache_read_override,
            cost_cache_write_override: value.cost_cache_write_override,
        }
    }
}

pub(crate) fn validate_limit(name: &str, value: Option<i64>) -> AppResult<()> {
    if value.is_some_and(|value| value <= 0) {
        return Err(AppError::BadRequest(format!(
            "{name} limit must be a positive integer"
        )));
    }
    Ok(())
}

pub(crate) fn validate_cost_override(name: &str, value: Option<f64>) -> AppResult<()> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(AppError::BadRequest(format!(
            "{name} override must be a non-negative number"
        )));
    }
    Ok(())
}

pub(crate) fn serialize_endpoint_override(value: Option<&[String]>) -> AppResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut endpoints = Vec::new();
    for raw in value {
        let endpoint = raw.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            continue;
        }
        if !endpoint.starts_with('/') {
            return Err(AppError::BadRequest(format!(
                "endpoint '{raw}' must start with '/'"
            )));
        }
        if endpoint.len() > 200 {
            return Err(AppError::BadRequest(format!(
                "endpoint '{raw}' is too long"
            )));
        }
        if !endpoints.iter().any(|existing| existing == endpoint) {
            endpoints.push(endpoint.to_string());
        }
    }
    if endpoints.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&endpoints)
        .map(Some)
        .map_err(|error| AppError::Internal(error.into()))
}
