use super::*;

pub(super) async fn get_provider(state: &AppState, id: i64) -> AppResult<ProviderView> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let models: Vec<String> = sqlx::query_scalar(
        "SELECT model_name FROM provider_models WHERE provider_id = ? AND enabled = 1 ORDER BY model_name COLLATE NOCASE",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let mut view = ProviderView::from(provider);
    view.models = models;
    hydrate_provider_view(state, &mut view).await?;
    Ok(view)
}

pub(super) async fn get_route(state: &AppState, id: i64) -> AppResult<RouteView> {
    let route = sqlx::query_as::<_, Route>(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy, \
                enabled, created_at, updated_at \
         FROM routes WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    route_view(state, route).await
}

pub(super) async fn route_view(state: &AppState, route: Route) -> AppResult<RouteView> {
    let targets = route_targets(state, Some(route.id)).await?;
    Ok(build_route_view(route, targets))
}

pub(super) async fn route_targets(
    state: &AppState,
    route_id: Option<i64>,
) -> AppResult<Vec<RouteTarget>> {
    let mut query = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT rt.*, p.name AS provider_name, p.provider_type,
               p.base_url, p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               COALESCE(pm.context_override, pm.context_limit) AS context_limit,
               COALESCE(pm.input_override, pm.input_limit) AS input_limit,
               COALESCE(pm.output_override, pm.output_limit) AS output_limit,
               COALESCE(pm.enabled, 1) AS model_enabled,
               p.tool_search_supported,
               p.last_test_ok AS provider_health,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
        "#,
    );
    if let Some(route_id) = route_id {
        query.push(" WHERE rt.route_id = ").push_bind(route_id);
    }
    query.push(" ORDER BY rt.route_id, rt.priority ASC, rt.id");
    Ok(query
        .build_query_as::<RouteTarget>()
        .fetch_all(&state.pool)
        .await?)
}

pub(super) fn build_route_view(route: Route, targets: Vec<RouteTarget>) -> RouteView {
    let enabled_targets = targets
        .iter()
        .filter(|target| {
            target.enabled != 0
                && target.provider_enabled.unwrap_or(1) != 0
                && target.model_enabled.unwrap_or(1) != 0
        })
        .collect::<Vec<_>>();
    let mut incomplete = enabled_targets.is_empty();
    let capabilities = enabled_targets
        .iter()
        .map(|target| {
            let capabilities = ModelCapabilities {
                context_limit: target.context_limit,
                input_limit: target.input_limit,
                output_limit: target.output_limit,
                ..Default::default()
            }
            .with_effective_input_limit();
            if capabilities.input_limit.is_none() || capabilities.output_limit.is_none() {
                incomplete = true;
            }
            capabilities
        })
        .collect::<Vec<_>>();
    let barrel = ModelCapabilities::intersect(capabilities.iter()).unwrap_or_default();

    RouteView {
        id: route.id,
        name: route.name,
        model_pattern: route.model_pattern,
        strategy: route.strategy,
        enabled: route.enabled != 0,
        context_limit: barrel.total_context_tokens.or(barrel.context_limit),
        input_limit: barrel.input_limit,
        output_limit: barrel.output_limit,
        limits_verified: !incomplete,
        targets: targets
            .into_iter()
            .map(|target| RouteTargetView {
                id: target.id,
                provider_id: target.provider_id,
                provider_name: target.provider_name,
                provider_type: target.provider_type,
                upstream_model: target.upstream_model,
                supported_endpoints: target
                    .supported_endpoints
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                    .unwrap_or_default(),
                context_limit: target.context_limit,
                input_limit: target.input_limit,
                output_limit: target.output_limit,
                provider_enabled: target.provider_enabled.unwrap_or(1) != 0,
                model_enabled: target.model_enabled.unwrap_or(1) != 0,
                model_prefix: target.model_prefix,
                weight: target.weight,
                priority: target.priority,
                enabled: target.enabled != 0,
            })
            .collect(),
        created_at: route.created_at,
        updated_at: route.updated_at,
    }
}

/// Wraps plain model names (manually curated lists, Ollama) with empty upstream
/// metadata so they share one insert path with synced entries.
pub(super) fn names_to_entries(models: &[String]) -> Vec<(String, UpstreamModelInfo)> {
    models
        .iter()
        .map(|name| (name.clone(), UpstreamModelInfo::default()))
        .collect()
}

/// Serialises the split modality fields back into the nested shape used for
/// storage, keeping one canonical on-disk representation.
pub(super) fn modalities_to_storage(capabilities: &ModelCapabilities) -> Option<String> {
    if capabilities.input_modalities.is_none() && capabilities.output_modalities.is_none() {
        return None;
    }
    let mut value = serde_json::Map::new();
    if let Some(input) = &capabilities.input_modalities {
        value.insert("input".to_string(), json!(input));
    }
    if let Some(output) = &capabilities.output_modalities {
        value.insert("output".to_string(), json!(output));
    }
    serde_json::to_string(&Value::Object(value)).ok()
}

pub(super) async fn replace_provider_models(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    models: &[(String, UpstreamModelInfo)],
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
) -> AppResult<()> {
    let existing_overrides = sqlx::query_as::<_, ProviderModelOverride>(
        "SELECT model_name, enabled, context_override, input_override, output_override, \
                supported_endpoints_override, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models WHERE provider_id = ?",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| (row.model_name.clone(), row))
    .collect::<HashMap<_, _>>();
    sqlx::query("DELETE FROM provider_models WHERE provider_id = ?")
        .bind(provider_id)
        .execute(&mut **tx)
        .await?;
    let mut seen = HashSet::new();
    let synced_at = Utc::now().to_rfc3339();
    for (model, upstream) in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model.to_string()) {
            continue;
        }
        let found = catalog.and_then(|catalog| catalog.lookup(provider_hint, model));
        let has_capabilities = found.is_some();
        let capabilities = found.unwrap_or_default().with_effective_input_limit();
        // The provider's own context window is more trustworthy than a
        // models.dev guess, and is the only source for models models.dev lacks.
        // A provider may publish both a total window and a smaller accepted
        // input ceiling; keep the strictest value so clients never see an
        // optimistic limit.
        let context_limit = min_known(upstream.context_limit, capabilities.context_limit);
        let input_limit = min_known(context_limit, capabilities.input_limit);
        let overrides = existing_overrides.get(model).cloned().unwrap_or_default();
        sqlx::query(
            r#"
            INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, output_limit,
                input_limit, attachment, reasoning, tool_call, structured_output,
                temperature, open_weights, modalities, cost, family, knowledge,
                release_date, last_updated, canonical_model_id, capabilities_synced_at,
                upstream_context_limit, supported_endpoints, display_name,
                context_override, input_override, output_override,
                supported_endpoints_override, cost_input_override, cost_output_override,
                cost_cache_read_override, cost_cache_write_override
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(provider_id)
        .bind(model)
        .bind(overrides.enabled.unwrap_or(1))
        .bind(context_limit)
        .bind(capabilities.output_limit)
        .bind(input_limit)
        .bind(capabilities.attachment.map(i64::from))
        .bind(capabilities.reasoning.map(i64::from))
        .bind(capabilities.tool_call.map(i64::from))
        .bind(capabilities.structured_output.map(i64::from))
        .bind(capabilities.temperature.map(i64::from))
        .bind(capabilities.open_weights.map(i64::from))
        // Persisted in models.dev's nested shape; the read path flattens it.
        .bind(modalities_to_storage(&capabilities))
        .bind(
            capabilities
                .cost
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .unwrap_or_default(),
        )
        .bind(capabilities.family)
        .bind(capabilities.knowledge)
        .bind(capabilities.release_date)
        .bind(capabilities.last_updated)
        .bind(capabilities.canonical_model_id)
        .bind((has_capabilities || upstream.context_limit.is_some()).then_some(synced_at.clone()))
        .bind(upstream.context_limit)
        .bind(
            (!upstream.supported_endpoints.is_empty())
                .then(|| serde_json::to_string(&upstream.supported_endpoints))
                .transpose()
                .unwrap_or_default(),
        )
        .bind(upstream.display_name.clone())
        .bind(overrides.context_override)
        .bind(overrides.input_override)
        .bind(overrides.output_override)
        .bind(overrides.supported_endpoints_override)
        .bind(overrides.cost_input_override)
        .bind(overrides.cost_output_override)
        .bind(overrides.cost_cache_read_override)
        .bind(overrides.cost_cache_write_override)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Extra facts a provider reports about a model in its own `/models` response.
///
/// These are kept separate from models.dev metadata so the upstream's own
/// numbers can win: a provider knows its real context window even when
/// models.dev has never heard of the model.
#[derive(Debug, Clone, Default)]
pub(crate) struct UpstreamModelInfo {
    pub(crate) context_limit: Option<i64>,
    pub(crate) supported_endpoints: Vec<String>,
    /// Provider-supplied label, e.g. "DeepSeek V4.1 Flash".
    pub(crate) display_name: Option<String>,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub(super) struct ProviderModelOverride {
    model_name: String,
    enabled: Option<i64>,
    context_override: Option<i64>,
    input_override: Option<i64>,
    output_override: Option<i64>,
    supported_endpoints_override: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
}

/// Reads every common context/input spelling and keeps the strictest value.
///
/// OpenAI-compatible providers are inconsistent here: some expose
/// `context_length`, others `context_window`, and several publish both a total
/// window and a smaller `max_input_tokens`. Taking the minimum keeps the
/// gateway conservative when those fields disagree.
pub(super) fn upstream_context_limit(model: &Value) -> Option<i64> {
    [
        "context_length",
        "context_window",
        "context_size",
        "max_input_tokens",
        "max_context_window",
        "max_context_tokens",
    ]
    .iter()
    .filter_map(|key| model.get(*key).and_then(Value::as_i64))
    .filter(|value| *value > 0)
    .min()
}

pub(super) fn min_known(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// Parses an OpenAI-style model list, preserving each entry's name and any
/// upstream-reported limits.
pub(super) fn parse_openai_model_entries(value: &Value) -> Vec<(String, UpstreamModelInfo)> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model
                .get("id")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)?
                .trim();
            if name.is_empty() {
                return None;
            }
            let context_limit = upstream_context_limit(model);
            let supported_endpoints = model
                .get("supported_endpoints")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            // Providers commonly send the friendly label as `name`. Only keep
            // it when it adds information beyond the id itself.
            let display_name = model
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty() && *label != name)
                .map(ToOwned::to_owned);
            Some((
                name.to_string(),
                UpstreamModelInfo {
                    context_limit,
                    supported_endpoints,
                    display_name,
                },
            ))
        })
        .collect()
}

pub(super) fn parse_ollama_models(value: &Value) -> Vec<String> {
    value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("name").or_else(|| model.get("model")))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) async fn replace_route_targets(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    route_id: i64,
    targets: &[RouteTargetInput],
) -> AppResult<()> {
    validate_targets(targets)?;
    sqlx::query("DELETE FROM route_targets WHERE route_id = ?")
        .bind(route_id)
        .execute(&mut **tx)
        .await?;

    for target in targets {
        sqlx::query(
            "INSERT INTO route_targets (route_id, provider_id, upstream_model, weight, priority, enabled) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(route_id)
        .bind(target.provider_id)
        .bind(target.upstream_model.trim())
        .bind(target.weight)
        .bind(target.priority)
        .bind(target.enabled as i64)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlite_conflict)?;
    }
    Ok(())
}

pub(super) fn validate_provider_input(input: &ProviderInput) -> AppResult<()> {
    if input.name.trim().is_empty() || normalize_base_url(&input.base_url).is_empty() {
        return Err(AppError::BadRequest(
            "provider name and base URL are required".to_string(),
        ));
    }
    if !input.headers.is_object() && !input.headers.is_null() {
        return Err(AppError::BadRequest(
            "provider headers must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

impl From<ProviderApiKeyRecord> for ProviderApiKeyView {
    fn from(value: ProviderApiKeyRecord) -> Self {
        let api_key_suffix = api_key_suffix(&value.secret);
        Self {
            id: value.id,
            name: value.name,
            api_key_set: !value.secret.is_empty(),
            api_key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            last_error_at: value.last_error_at,
            last_error: value.last_error,
            last_test_at: value.last_test_at,
            last_test_ok: value.last_test_ok.map(|value| value != 0),
            last_test_latency_ms: value.last_test_latency_ms,
            last_test_checked: value.last_test_checked,
            last_test_message: value.last_test_message,
            requests: value.lifetime_requests,
            success_rate: if value.lifetime_requests > 0 {
                value.lifetime_successes as f64 / value.lifetime_requests as f64 * 100.0
            } else {
                0.0
            },
            avg_latency_ms: if value.lifetime_requests > 0 {
                value.lifetime_latency_ms as f64 / value.lifetime_requests as f64
            } else {
                0.0
            },
            prompt_tokens: value.lifetime_prompt_tokens,
            completion_tokens: value.lifetime_completion_tokens,
            cooldown_seconds: None,
            created_at: value.created_at,
        }
    }
}

pub(super) fn api_key_suffix(secret: &str) -> String {
    let suffix = secret.chars().rev().take(4).collect::<String>();
    suffix.chars().rev().collect()
}

pub(super) async fn provider_api_key_records(
    pool: &sqlx::SqlitePool,
    provider_id: i64,
) -> AppResult<Vec<ProviderApiKeyRecord>> {
    Ok(sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

pub(super) async fn clear_provider_key_cooldowns(
    state: &AppState,
    provider_api_key_ids: impl IntoIterator<Item = i64>,
) {
    let mut cooldowns = state.provider_key_cooldown.lock().await;
    for provider_api_key_id in provider_api_key_ids {
        cooldowns.remove(&provider_api_key_id);
    }
}

pub(super) async fn provider_key_candidates(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<(Option<i64>, Option<String>)>> {
    let mut candidates = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| (Some(record.id), Some(record.secret)))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates.push((None, provider.api_key.clone()));
    }
    Ok(candidates)
}

pub(super) async fn provider_probe_keys(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<ProviderProbeKey>> {
    let mut keys = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| ProviderProbeKey {
            id: Some(record.id),
            name: record.name,
            api_key_suffix: api_key_suffix(&record.secret),
            secret: Some(record.secret),
        })
        .collect::<Vec<_>>();
    if keys.is_empty()
        && let Some(secret) = normalize_optional(provider.api_key.clone())
    {
        keys.push(ProviderProbeKey {
            id: None,
            name: "Default".to_string(),
            api_key_suffix: api_key_suffix(&secret),
            secret: Some(secret),
        });
    }
    Ok(keys)
}

pub(super) async fn hydrate_provider_view(
    state: &AppState,
    view: &mut ProviderView,
) -> AppResult<()> {
    view.api_keys = provider_api_key_records(&state.pool, view.id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    let now = std::time::Instant::now();
    view.cooldown_seconds = state
        .provider_cooldown
        .lock()
        .await
        .get(&view.id)
        .and_then(|until| {
            let remaining = until.saturating_duration_since(now);
            (!remaining.is_zero()).then(|| (remaining.as_secs_f64().ceil() as i64).max(1))
        });
    let cooldowns = state.provider_key_cooldown.lock().await;
    for key in &mut view.api_keys {
        if let Some(until) = cooldowns.get(&key.id) {
            let remaining = until.saturating_duration_since(now);
            if !remaining.is_zero() {
                key.cooldown_seconds = Some((remaining.as_secs_f64().ceil() as i64).max(1));
            }
        }
    }
    view.api_key_set = !view.api_keys.is_empty();
    Ok(())
}

pub(super) async fn hydrate_provider_views(
    state: &AppState,
    views: &mut [ProviderView],
) -> AppResult<()> {
    if views.is_empty() {
        return Ok(());
    }

    let mut models_by_provider = HashMap::<i64, Vec<String>>::new();
    for (provider_id, model_name) in sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, model_name \
         FROM provider_models \
         WHERE enabled = 1 \
         ORDER BY provider_id, model_name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    {
        models_by_provider
            .entry(provider_id)
            .or_default()
            .push(model_name);
    }

    let mut keys_by_provider = HashMap::<i64, Vec<ProviderApiKeyView>>::new();
    for record in sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys ORDER BY provider_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        if let Some(provider_id) = record.provider_id {
            keys_by_provider
                .entry(provider_id)
                .or_default()
                .push(record.into());
        }
    }

    let now = std::time::Instant::now();
    let provider_cooldowns = state.provider_cooldown.lock().await;
    let cooldowns = state.provider_key_cooldown.lock().await;
    for view in views {
        view.models = models_by_provider.remove(&view.id).unwrap_or_default();
        view.api_keys = keys_by_provider.remove(&view.id).unwrap_or_default();
        view.cooldown_seconds = provider_cooldowns.get(&view.id).and_then(|until| {
            let remaining = until.saturating_duration_since(now);
            (!remaining.is_zero()).then(|| (remaining.as_secs_f64().ceil() as i64).max(1))
        });
        for key in &mut view.api_keys {
            if let Some(until) = cooldowns.get(&key.id) {
                let remaining = until.saturating_duration_since(now);
                if !remaining.is_zero() {
                    key.cooldown_seconds = Some((remaining.as_secs_f64().ceil() as i64).max(1));
                }
            }
        }
        view.api_key_set = !view.api_keys.is_empty();
    }
    Ok(())
}

pub(super) fn provider_api_key_input(
    id: Option<i64>,
    name: impl Into<String>,
    api_key: Option<String>,
    enabled: bool,
) -> ProviderApiKeyInput {
    ProviderApiKeyInput {
        id,
        name: name.into(),
        api_key,
        enabled,
    }
}

pub(super) async fn replace_provider_api_keys(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    inputs: &[ProviderApiKeyInput],
) -> AppResult<()> {
    let existing = sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|record| (record.id, record))
    .collect::<HashMap<_, _>>();
    let mut retained = HashSet::new();
    let mut secrets = HashSet::new();

    for (index, input) in inputs.iter().enumerate() {
        let current = match input.id {
            Some(id) => Some(
                existing
                    .get(&id)
                    .ok_or_else(|| {
                        AppError::BadRequest("API key does not belong to this provider".to_string())
                    })?
                    .clone(),
            ),
            None => None,
        };
        let requested_secret = normalize_optional(input.api_key.clone());
        let secret = match (requested_secret, current.as_ref()) {
            (Some(secret), _) => secret,
            (None, Some(current)) => current.secret.clone(),
            (None, None) => {
                return Err(AppError::BadRequest(
                    "new provider API keys must include a secret".to_string(),
                ));
            }
        };
        if !secrets.insert(secret.clone()) {
            return Err(AppError::BadRequest(
                "provider API keys must be unique".to_string(),
            ));
        }
        let name = match input.name.trim() {
            "" => current
                .as_ref()
                .map(|current| current.name.clone())
                .unwrap_or_else(|| format!("Key {}", index + 1)),
            name => name.to_string(),
        };
        if name.chars().count() > 80 {
            return Err(AppError::BadRequest(
                "provider API key name must be at most 80 characters".to_string(),
            ));
        }

        if let Some(current) = current {
            retained.insert(current.id);
            if current.secret != secret {
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         last_used_at = NULL, last_error_at = NULL, last_error = NULL, \
                         last_test_at = NULL, last_test_ok = NULL, \
                         last_test_latency_ms = NULL, last_test_checked = NULL, \
                         last_test_message = NULL, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            } else {
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            }
        } else {
            let result = sqlx::query(
                "INSERT INTO provider_api_keys (provider_id, name, secret, enabled) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(provider_id)
            .bind(name)
            .bind(secret)
            .bind(input.enabled as i64)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlite_conflict)?;
            retained.insert(result.last_insert_rowid());
        }
    }

    for id in existing.keys().filter(|id| !retained.contains(id)) {
        sqlx::query("DELETE FROM provider_api_keys WHERE id = ? AND provider_id = ?")
            .bind(id)
            .bind(provider_id)
            .execute(&mut **tx)
            .await?;
    }

    let first_secret = sqlx::query_scalar::<_, String>(
        "SELECT secret FROM provider_api_keys \
         WHERE provider_id = ? AND enabled = 1 ORDER BY id LIMIT 1",
    )
    .bind(provider_id)
    .fetch_optional(&mut **tx)
    .await?;
    sqlx::query("UPDATE providers SET api_key = ? WHERE id = ?")
        .bind(first_secret)
        .bind(provider_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) fn validate_route_input(input: &RouteInput) -> AppResult<()> {
    if input.name.trim().is_empty() || input.model_pattern.trim().is_empty() {
        return Err(AppError::BadRequest(
            "route name and model pattern are required".to_string(),
        ));
    }
    validate_model_pattern(&input.model_pattern)?;
    validate_targets(&input.targets)
}

/// Reject patterns that `globset` cannot compile. Otherwise the route would be
/// stored successfully but silently skipped at request time, surfacing as a
/// confusing "no route matches" error much later.
pub(super) fn validate_model_pattern(pattern: &str) -> AppResult<()> {
    let pattern = pattern.trim();
    globset::Glob::new(pattern).map(|_| ()).map_err(|error| {
        AppError::BadRequest(format!("invalid model pattern '{pattern}': {error}"))
    })
}

pub(super) fn validate_targets(targets: &[RouteTargetInput]) -> AppResult<()> {
    if targets.is_empty() {
        return Err(AppError::BadRequest(
            "a route must have at least one enabled target".to_string(),
        ));
    }
    // A route whose targets are all disabled would still match incoming
    // requests, then fail every one with "no enabled provider targets". Reject
    // it at write time; disable the route itself instead.
    if !targets.iter().any(|target| target.enabled) {
        return Err(AppError::BadRequest(
            "a route must have at least one enabled target; disable the route instead of all of its targets".to_string(),
        ));
    }
    if targets
        .iter()
        .any(|target| target.provider_id <= 0 || target.upstream_model.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "every route target needs a provider and upstream model".to_string(),
        ));
    }
    if targets.iter().any(|target| target.weight <= 0) {
        return Err(AppError::BadRequest(
            "route target weight must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

pub(super) fn ollama_root(base_url: &str) -> &str {
    base_url
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or_else(|| base_url.trim_end_matches('/'))
}

pub(super) fn normalize_model_prefix(value: &str) -> AppResult<String> {
    let value = value.trim().trim_matches('/').trim().to_string();
    if value.is_empty() {
        return Ok(value);
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(AppError::BadRequest(
            "model prefix may only contain letters, numbers, '-', '_' and '.'".to_string(),
        ));
    }
    Ok(format!("{value}/"))
}

pub(super) fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

pub(super) fn normalize_health_check_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn parse_provider_probe_endpoints(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

pub(super) fn provider_probe_supports(endpoints: &[String], expected: &str) -> bool {
    let expected = expected.trim_end_matches('/');
    endpoints.iter().any(|endpoint| {
        let endpoint = endpoint.trim_end_matches('/');
        !endpoint.is_empty() && (expected == endpoint || expected.ends_with(endpoint))
    })
}

pub(super) fn normalize_health_interval(value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "health check interval must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

pub(super) fn map_sqlite_conflict(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database_error) = &error {
        if database_error.is_unique_violation() {
            // SQLite reports the violated index/column in the message; use it
            // to tell the operator exactly which field collided instead of a
            // generic "something already exists".
            let detail = database_error.message().to_ascii_lowercase();
            let message = if detail.contains("model_prefix") {
                "another provider already uses this model prefix"
            } else if detail.contains("providers.name") || detail.contains("idx_providers_name") {
                "a provider with this name already exists"
            } else if detail.contains("idx_routes_model_pattern")
                || detail.contains("routes.model_pattern")
            {
                "a route with this model pattern already exists"
            } else if detail.contains("routes.name") {
                "a route with this name already exists"
            } else if detail.contains("route_targets") {
                "this provider and upstream model are already used by the route"
            } else {
                "an item with the same name/pattern already exists"
            };
            return AppError::Conflict(message.to_string());
        }
        if database_error.is_foreign_key_violation() {
            return AppError::BadRequest("referenced provider does not exist".to_string());
        }
    }
    AppError::Database(error)
}

pub(super) fn hash_secret(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

pub(super) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

pub(super) fn apply_usage_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    query: &'a UsageQuery,
) {
    if let Some(provider_id) = query.provider_id {
        builder.push(" AND u.provider_id = ").push_bind(provider_id);
    }
    if let Some(provider_api_key_id) = query.provider_api_key_id {
        builder
            .push(" AND u.provider_api_key_id = ")
            .push_bind(provider_api_key_id);
    }
    if let Some(api_key_id) = query.api_key_id {
        builder.push(" AND u.api_key_id = ").push_bind(api_key_id);
    }
    if let Some(route_id) = query.route_id {
        builder.push(" AND u.route_id = ").push_bind(route_id);
    }
    if let Some(model) = &query.model
        && !model.trim().is_empty()
    {
        builder
            .push(" AND u.requested_model LIKE ")
            .push_bind(format!("%{}%", model.trim()));
    }
    if let Some(request_id) = &query.request_id
        && !request_id.trim().is_empty()
    {
        builder
            .push(" AND u.request_id LIKE ")
            .push_bind(format!("%{}%", request_id.trim()));
    }
    if let Some(session_id) = &query.session_id
        && !session_id.trim().is_empty()
    {
        builder
            .push(" AND u.session_id LIKE ")
            .push_bind(format!("%{}%", session_id.trim()));
    }
    if let Some(endpoint) = &query.endpoint
        && !endpoint.trim().is_empty()
    {
        builder
            .push(" AND u.endpoint LIKE ")
            .push_bind(format!("%{}%", endpoint.trim()));
    }
    if let Some(success) = query.success {
        builder.push(" AND u.success = ").push_bind(success as i64);
        if !success {
            builder.push(" AND u.in_flight = 0");
        }
    }
    if let Some(in_flight) = query.in_flight {
        builder
            .push(" AND u.in_flight = ")
            .push_bind(in_flight as i64);
    }
    if let Some(gateway_adjusted) = query.gateway_adjusted {
        if gateway_adjusted {
            builder.push(" AND u.warning_message IS NOT NULL AND TRIM(u.warning_message) <> ''");
        } else {
            builder.push(" AND (u.warning_message IS NULL OR TRIM(u.warning_message) = '')");
        }
    }
    if let Some(from) = &query.from {
        builder.push(" AND u.created_at >= ").push_bind(from);
    }
    if let Some(to) = &query.to {
        builder.push(" AND u.created_at <= ").push_bind(to);
    }
}

pub(super) fn usage_query_is_unfiltered(query: &UsageQuery) -> bool {
    query.provider_id.is_none()
        && query.provider_api_key_id.is_none()
        && query.api_key_id.is_none()
        && query.route_id.is_none()
        && query
            .model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .request_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .session_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .endpoint
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query.success.is_none()
        && query.in_flight.is_none()
        && query.gateway_adjusted.is_none()
        && query.from.is_none()
        && query.to.is_none()
}

impl From<ApiKeyRecord> for ApiKeyView {
    fn from(value: ApiKeyRecord) -> Self {
        Self {
            id: value.id,
            name: value.name,
            key_prefix: value.key_prefix,
            key_suffix: value.key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            created_at: value.created_at,
            requests: 0,
            tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_micros: None,
            unpriced_requests: 0,
            daily_token_limit: value.daily_token_limit,
            daily_cost_limit_micros: value.daily_cost_limit_micros,
            requests_per_minute: value.requests_per_minute,
            max_concurrency: value.max_concurrency,
            today_requests: 0,
            today_tokens: 0,
            today_prompt_tokens: 0,
            today_completion_tokens: 0,
            today_cost_micros: None,
            requests_this_minute: 0,
            current_in_flight: 0,
            allowed_models: parse_allowed_models(value.allowed_models.as_deref()),
            expires_at: value.expires_at,
        }
    }
}

impl From<ApiKeyStatsRow> for ApiKeyView {
    fn from(value: ApiKeyStatsRow) -> Self {
        Self {
            id: value.id,
            name: value.name,
            key_prefix: value.key_prefix,
            key_suffix: value.key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            created_at: value.created_at,
            requests: value.requests,
            tokens: value.tokens,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            cost_micros: value.cost_micros,
            unpriced_requests: value.unpriced_requests,
            daily_token_limit: value.daily_token_limit,
            daily_cost_limit_micros: value.daily_cost_limit_micros,
            requests_per_minute: value.requests_per_minute,
            max_concurrency: value.max_concurrency,
            today_requests: value.today_requests,
            today_tokens: value.today_tokens,
            today_prompt_tokens: value.today_prompt_tokens,
            today_completion_tokens: value.today_completion_tokens,
            today_cost_micros: value.today_cost_micros,
            requests_this_minute: value.requests_this_minute,
            current_in_flight: value.current_in_flight,
            allowed_models: parse_allowed_models(value.allowed_models.as_deref()),
            expires_at: value.expires_at,
        }
    }
}
