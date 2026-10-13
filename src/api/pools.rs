use super::*;

use crate::allocation::{AllocationPolicy, MAX_POOL_WINDOW_SECONDS, PoolSpec};

/// Validates one pool payload and returns its normalized fields.
struct NormalizedPool {
    window_seconds: i64,
    request_limit: Option<i64>,
    token_limit: Option<i64>,
    saturation_threshold_pct: i64,
    allocations: Vec<(i64, i64, AllocationPolicy)>,
}

fn normalize_pool(
    name: &str,
    window_seconds: i64,
    request_limit: Option<i64>,
    token_limit: Option<i64>,
    saturation_threshold_pct: i64,
    allocations: &[ProviderPoolAllocationInput],
) -> AppResult<NormalizedPool> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "pool name must be between 1 and 100 characters".to_string(),
        ));
    }
    if !(1..=MAX_POOL_WINDOW_SECONDS).contains(&window_seconds) {
        return Err(AppError::BadRequest(format!(
            "window_seconds must be between 1 and {MAX_POOL_WINDOW_SECONDS}"
        )));
    }
    for (label, limit) in [("request_limit", request_limit), ("token_limit", token_limit)] {
        if let Some(limit) = limit
            && limit <= 0
        {
            return Err(AppError::BadRequest(format!(
                "{label} must be a positive integer or null to leave it unlimited"
            )));
        }
    }
    if request_limit.is_none() && token_limit.is_none() {
        return Err(AppError::BadRequest(
            "set request_limit and/or token_limit; a pool without a limit cannot allocate anything"
                .to_string(),
        ));
    }
    if !(1..=100).contains(&saturation_threshold_pct) {
        return Err(AppError::BadRequest(
            "saturation_threshold_pct must be between 1 and 100".to_string(),
        ));
    }

    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(allocations.len());
    for allocation in allocations {
        if !seen.insert(allocation.api_key_id) {
            return Err(AppError::BadRequest(format!(
                "API key {} appears more than once in the pool allocations",
                allocation.api_key_id
            )));
        }
        if !(1..=100).contains(&allocation.weight_pct) {
            return Err(AppError::BadRequest(
                "allocation weight_pct must be between 1 and 100".to_string(),
            ));
        }
        let policy = AllocationPolicy::parse(&allocation.policy).ok_or_else(|| {
            AppError::BadRequest(format!(
                "unknown allocation policy '{}'; expected hard, soft or burst",
                allocation.policy
            ))
        })?;
        normalized.push((allocation.api_key_id, allocation.weight_pct, policy));
    }

    Ok(NormalizedPool {
        window_seconds,
        request_limit,
        token_limit,
        saturation_threshold_pct,
        allocations: normalized,
    })
}

/// Replaces a pool's allocations inside an open transaction.
async fn replace_allocations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pool_id: i64,
    allocations: &[(i64, i64, AllocationPolicy)],
) -> AppResult<()> {
    sqlx::query("DELETE FROM provider_pool_allocations WHERE pool_id = ?")
        .bind(pool_id)
        .execute(&mut **tx)
        .await?;
    for (api_key_id, weight_pct, policy) in allocations {
        sqlx::query(
            "INSERT INTO provider_pool_allocations (pool_id, api_key_id, weight_pct, policy) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(pool_id)
        .bind(api_key_id)
        .bind(weight_pct)
        .bind(policy.as_str())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Rejects allocations that point at keys which do not exist, so the failure
/// is a clear `400` instead of a foreign-key error at insert time.
async fn ensure_api_keys_exist(state: &AppState, api_key_ids: &[i64]) -> AppResult<()> {
    for api_key_id in api_key_ids {
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
    Ok(())
}

async fn load_pool_view(state: &AppState, pool_id: i64) -> AppResult<ProviderPoolView> {
    let views = query_pool_views(state, Some(pool_id)).await?;
    views
        .into_iter()
        .next()
        .ok_or_else(|| AppError::NotFound("provider pool not found".to_string()))
}

async fn query_pool_views(
    state: &AppState,
    pool_id: Option<i64>,
) -> AppResult<Vec<ProviderPoolView>> {
    let pools = sqlx::query_as::<_, ProviderPool>(
        "SELECT * FROM provider_pools \
         WHERE (? IS NULL OR id = ?) \
         ORDER BY provider_id, id",
    )
    .bind(pool_id)
    .bind(pool_id)
    .fetch_all(&state.pool)
    .await?;
    if pools.is_empty() {
        return Ok(Vec::new());
    }
    let allocations = sqlx::query_as::<_, ProviderPoolAllocation>(
        "SELECT * FROM provider_pool_allocations ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    let provider_names = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM providers")
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();
    let key_names = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM api_keys")
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();

    Ok(pools
        .into_iter()
        .map(|pool| {
            // The share a key actually gets is derived from the same arithmetic
            // the hot path uses, so the console cannot show a different number.
            let spec: PoolSpec = PoolSpec::from_rows(&pool, &allocations);
            let allocations = allocations
                .iter()
                .filter(|allocation| allocation.pool_id == pool.id)
                .map(|allocation| {
                    let weight_pct = allocation.weight_pct.clamp(1, 100) as u64;
                    ProviderPoolAllocationView {
                        id: allocation.id,
                        api_key_id: allocation.api_key_id,
                        api_key_name: key_names.get(&allocation.api_key_id).cloned(),
                        weight_pct: allocation.weight_pct,
                        policy: allocation.policy.clone(),
                        request_share: pool
                            .request_limit
                            .map(|limit| spec.share_of(limit as u64, weight_pct) as i64),
                        token_share: pool
                            .token_limit
                            .map(|limit| spec.share_of(limit as u64, weight_pct) as i64),
                    }
                })
                .collect();
            ProviderPoolView {
                id: pool.id,
                provider_id: pool.provider_id,
                provider_name: provider_names.get(&pool.provider_id).cloned(),
                name: pool.name,
                enabled: pool.enabled != 0,
                window_seconds: pool.window_seconds,
                request_limit: pool.request_limit,
                token_limit: pool.token_limit,
                saturation_threshold_pct: pool.saturation_threshold_pct,
                allocations,
                created_at: pool.created_at,
                updated_at: pool.updated_at,
            }
        })
        .collect())
}

/// Lists provider pools with their allocations and effective shares.
pub async fn list_provider_pools(State(state): State<AppState>) -> AppResult<Json<Vec<ProviderPoolView>>> {
    Ok(Json(query_pool_views(&state, None).await?))
}

/// Creates a provider pool, optionally with its initial allocations.
pub async fn create_provider_pool(
    State(state): State<AppState>,
    Json(input): Json<ProviderPoolInput>,
) -> AppResult<Json<ProviderPoolView>> {
    let name = input.name.trim().to_string();
    let normalized = normalize_pool(
        &name,
        input.window_seconds,
        input.request_limit,
        input.token_limit,
        input.saturation_threshold_pct,
        &input.allocations,
    )?;
    let provider_exists: Option<i64> = sqlx::query_scalar("SELECT id FROM providers WHERE id = ?")
        .bind(input.provider_id)
        .fetch_optional(&state.pool)
        .await?;
    if provider_exists.is_none() {
        return Err(AppError::BadRequest(format!(
            "provider {} does not exist",
            input.provider_id
        )));
    }
    ensure_api_keys_exist(
        &state,
        &normalized
            .allocations
            .iter()
            .map(|(api_key_id, _, _)| *api_key_id)
            .collect::<Vec<_>>(),
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO provider_pools (
             provider_id, name, enabled, window_seconds, request_limit, token_limit,
             saturation_threshold_pct
         ) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(input.provider_id)
    .bind(&name)
    .bind(input.enabled as i64)
    .bind(normalized.window_seconds)
    .bind(normalized.request_limit)
    .bind(normalized.token_limit)
    .bind(normalized.saturation_threshold_pct)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| conflict_or(error, &name))?;
    replace_allocations(&mut tx, id, &normalized.allocations).await?;
    tx.commit().await?;

    state.invalidate_pool_catalog().await;
    record_audit(
        &state,
        "create",
        "provider_pool",
        Some(&id.to_string()),
        &format!("created provider pool '{name}'"),
        Some(json!({
            "provider_id": input.provider_id,
            "request_limit": normalized.request_limit,
            "token_limit": normalized.token_limit,
            "allocations": normalized.allocations.len(),
        })),
    )
    .await;
    Ok(Json(load_pool_view(&state, id).await?))
}

/// Updates a provider pool; omitted fields keep their current value.
pub async fn update_provider_pool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderPoolUpdate>,
) -> AppResult<Json<ProviderPoolView>> {
    let current = sqlx::query_as::<_, ProviderPool>("SELECT * FROM provider_pools WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider pool not found".to_string()))?;
    let allocations_input = match &input.allocations {
        Some(allocations) => allocations.clone(),
        None => sqlx::query_as::<_, ProviderPoolAllocation>(
            "SELECT * FROM provider_pool_allocations WHERE pool_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(|allocation| ProviderPoolAllocationInput {
            api_key_id: allocation.api_key_id,
            weight_pct: allocation.weight_pct,
            policy: allocation.policy,
        })
        .collect(),
    };
    let name = input.name.unwrap_or_else(|| current.name.clone());
    let normalized = normalize_pool(
        &name,
        input.window_seconds.unwrap_or(current.window_seconds),
        input.request_limit.unwrap_or(current.request_limit),
        input.token_limit.unwrap_or(current.token_limit),
        input
            .saturation_threshold_pct
            .unwrap_or(current.saturation_threshold_pct),
        &allocations_input,
    )?;
    ensure_api_keys_exist(
        &state,
        &normalized
            .allocations
            .iter()
            .map(|(api_key_id, _, _)| *api_key_id)
            .collect::<Vec<_>>(),
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE provider_pools SET name = ?, enabled = ?, window_seconds = ?, request_limit = ?, \
             token_limit = ?, saturation_threshold_pct = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(&name)
    .bind(input.enabled.unwrap_or(current.enabled != 0) as i64)
    .bind(normalized.window_seconds)
    .bind(normalized.request_limit)
    .bind(normalized.token_limit)
    .bind(normalized.saturation_threshold_pct)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|error| conflict_or(error, &name))?;
    replace_allocations(&mut tx, id, &normalized.allocations).await?;
    tx.commit().await?;

    state.invalidate_pool_catalog().await;
    record_audit(
        &state,
        "update",
        "provider_pool",
        Some(&id.to_string()),
        &format!("updated provider pool '{name}'"),
        Some(json!({
            "request_limit": normalized.request_limit,
            "token_limit": normalized.token_limit,
            "allocations": normalized.allocations.len(),
        })),
    )
    .await;
    Ok(Json(load_pool_view(&state, id).await?))
}

/// Deletes a provider pool and its allocations.
pub async fn delete_provider_pool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM provider_pools WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("provider pool not found".to_string()));
    }
    state.invalidate_pool_catalog().await;
    record_audit(
        &state,
        "delete",
        "provider_pool",
        Some(&id.to_string()),
        "deleted provider pool",
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Turns a duplicate-name unique violation into a clear conflict instead of a
/// generic database error.
fn conflict_or(error: sqlx::Error, name: &str) -> AppError {
    let message = error.to_string();
    if message.contains("UNIQUE") || message.contains("unique") {
        AppError::Conflict(format!("a pool named '{name}' already exists for this provider"))
    } else {
        AppError::Database(error)
    }
}

/// Live pool usage for one provider, for the console and diagnostics.
pub async fn provider_pool_usage(
    State(state): State<AppState>,
    Path(provider_id): Path<i64>,
) -> AppResult<Json<Vec<crate::allocation::PoolUsageView>>> {
    Ok(Json(crate::allocation::pool_usage(&state, provider_id).await?))
}
