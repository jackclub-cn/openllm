use super::*;

pub async fn list_api_keys(State(state): State<AppState>) -> AppResult<Json<Vec<ApiKeyView>>> {
    let now = Utc::now();
    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .to_rfc3339();
    let minute_start = now
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("current minute start is valid")
        .to_rfc3339();
    let keys = sqlx::query_as::<_, ApiKeyStatsRow>(
        r#"
        SELECT
            k.id, k.name, k.key_prefix, k.key_suffix, k.enabled,
            k.last_used_at, k.created_at,
            k.daily_token_limit, k.daily_cost_limit_micros,
            k.requests_per_minute, k.max_concurrency,
            k.allowed_models, k.expires_at,
            (
                SELECT COUNT(*) FROM usage_logs rate
                WHERE rate.api_key_id = k.id AND rate.created_at >= ?
            ) AS requests_this_minute,
            (
                SELECT COUNT(*) FROM usage_logs active
                WHERE active.api_key_id = k.id AND active.in_flight = 1
            ) AS current_in_flight,
            COALESCE(today.today_requests, 0) AS today_requests,
            COALESCE(today.today_tokens, 0) AS today_tokens,
            COALESCE(today.today_prompt_tokens, 0) AS today_prompt_tokens,
            COALESCE(today.today_completion_tokens, 0) AS today_completion_tokens,
            today.today_cost_micros,
            k.lifetime_requests AS requests,
            k.lifetime_tokens AS tokens,
            k.lifetime_prompt_tokens AS prompt_tokens,
            k.lifetime_completion_tokens AS completion_tokens,
            CASE
                WHEN k.lifetime_requests > k.lifetime_unpriced_requests
                THEN k.lifetime_cost_micros
                ELSE NULL
            END AS cost_micros,
            k.lifetime_unpriced_requests AS unpriced_requests
        FROM api_keys k
        LEFT JOIN (
            SELECT api_key_id,
                   COUNT(*) AS today_requests,
                   COALESCE(SUM(total_tokens), 0) AS today_tokens,
                   COALESCE(SUM(prompt_tokens), 0) AS today_prompt_tokens,
                   COALESCE(SUM(completion_tokens), 0) AS today_completion_tokens,
                   SUM(estimated_cost_micros) AS today_cost_micros
            FROM usage_logs
            WHERE created_at >= ? AND in_flight = 0
            GROUP BY api_key_id
        ) today ON today.api_key_id = k.id
        ORDER BY k.enabled DESC, k.created_at DESC
        "#,
    )
    .bind(&minute_start)
    .bind(&day_start)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub(super) fn generate_api_key_material() -> (String, String, String, String) {
    let raw = format!("sk-openllm-{}", uuid::Uuid::new_v4().simple());
    let key_hash = hash_secret(&raw);
    let key_prefix = raw.chars().take(12).collect::<String>();
    let key_suffix = raw
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    (raw, key_hash, key_prefix, key_suffix)
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Json(input): Json<ApiKeyInput>,
) -> AppResult<(StatusCode, Json<ApiKeyCreated>)> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("key name is required".to_string()));
    }
    let daily_token_limit = normalize_api_key_limit("daily token", input.daily_token_limit)?;
    let daily_cost_limit_micros =
        normalize_api_key_limit("daily cost", input.daily_cost_limit_micros)?;
    let requests_per_minute =
        normalize_api_key_limit("requests per minute", input.requests_per_minute)?;
    let max_concurrency = normalize_api_key_limit("max concurrency", input.max_concurrency)?;
    let allowed_models = normalize_allowed_models(input.allowed_models)?;
    let expires_at = normalize_expiration(input.expires_at)?;

    let (raw, key_hash, key_prefix, key_suffix) = generate_api_key_material();

    let result = sqlx::query(
        "INSERT INTO api_keys (
            name, key_hash, key_prefix, key_suffix, enabled,
            daily_token_limit, daily_cost_limit_micros, requests_per_minute,
            max_concurrency, allowed_models, expires_at
         ) VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(key_hash)
    .bind(key_prefix)
    .bind(key_suffix)
    .bind(daily_token_limit)
    .bind(daily_cost_limit_micros)
    .bind(requests_per_minute)
    .bind(max_concurrency)
    .bind(allowed_models)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    let id = result.last_insert_rowid();
    let record = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    *state.auth_required.write().await = None;
    Ok((
        StatusCode::CREATED,
        Json(ApiKeyCreated {
            key: raw,
            item: record.into(),
        }),
    ))
}

pub async fn rotate_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ApiKeyCreated>> {
    let (raw, key_hash, key_prefix, key_suffix) = generate_api_key_material();
    let result = sqlx::query(
        "UPDATE api_keys \
         SET key_hash = ?, key_prefix = ?, key_suffix = ?, last_used_at = NULL \
         WHERE id = ?",
    )
    .bind(key_hash)
    .bind(key_prefix)
    .bind(key_suffix)
    .bind(id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    let record = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(ApiKeyCreated {
        key: raw,
        item: record.into(),
    }))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    *state.auth_required.write().await = None;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ApiKeyUpdate>,
) -> AppResult<Json<ApiKeyView>> {
    let current = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("API key not found".to_string()))?;
    let daily_token_limit = match input.daily_token_limit {
        Some(value) => normalize_api_key_limit("daily token", Some(value))?,
        None => current.daily_token_limit,
    };
    let daily_cost_limit_micros = match input.daily_cost_limit_micros {
        Some(value) => normalize_api_key_limit("daily cost", Some(value))?,
        None => current.daily_cost_limit_micros,
    };
    let requests_per_minute = match input.requests_per_minute {
        Some(value) => normalize_api_key_limit("requests per minute", Some(value))?,
        None => current.requests_per_minute,
    };
    let max_concurrency = match input.max_concurrency {
        Some(value) => normalize_api_key_limit("max concurrency", Some(value))?,
        None => current.max_concurrency,
    };
    let allowed_models = match input.allowed_models {
        Some(value) => normalize_allowed_models(Some(value))?,
        None => current.allowed_models,
    };
    let expires_at = match input.expires_at {
        Some(value) => normalize_expiration(Some(value))?,
        None => current.expires_at,
    };
    let result = sqlx::query(
        "UPDATE api_keys \
             SET enabled = ?, daily_token_limit = ?, daily_cost_limit_micros = ?, \
             requests_per_minute = ?, max_concurrency = ?, allowed_models = ?, \
             expires_at = ? \
         WHERE id = ?",
    )
    .bind(input.enabled as i64)
    .bind(daily_token_limit)
    .bind(daily_cost_limit_micros)
    .bind(requests_per_minute)
    .bind(max_concurrency)
    .bind(allowed_models)
    .bind(expires_at)
    .bind(id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    let record = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(record.into()))
}

pub(super) fn normalize_api_key_limit(name: &str, value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(format!(
            "{name} limit must be zero or a positive integer"
        ))),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

pub(super) fn parse_allowed_models(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|model| !model.trim().is_empty())
        .collect()
}

pub(super) fn normalize_allowed_models(value: Option<Vec<String>>) -> AppResult<Option<String>> {
    let Some(values) = value else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for value in values {
        let model = value.trim();
        if model.is_empty() {
            continue;
        }
        if model.chars().count() > 500 {
            return Err(AppError::BadRequest(
                "model permission entries must be at most 500 characters".to_string(),
            ));
        }
        if let Err(error) = globset::Glob::new(model) {
            return Err(AppError::BadRequest(format!(
                "invalid model permission pattern '{model}': {error}"
            )));
        }
        if seen.insert(model.to_string()) {
            models.push(model.to_string());
        }
    }
    if models.len() > 200 {
        return Err(AppError::BadRequest(
            "an API key can allow at most 200 model patterns".to_string(),
        ));
    }
    if models.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&models).unwrap_or_default()))
}

pub(super) fn normalize_expiration(value: Option<String>) -> AppResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|error| {
        AppError::BadRequest(format!("expiration must be an RFC3339 timestamp: {error}"))
    })?;
    if parsed <= Utc::now() {
        return Err(AppError::BadRequest(
            "expiration must be in the future".to_string(),
        ));
    }
    Ok(Some(parsed.with_timezone(&Utc).to_rfc3339()))
}

pub(super) fn normalize_retention_days(value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if !(0..=3650).contains(&value) => Err(AppError::BadRequest(
            "usage retention must be between 0 and 3650 days".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}
