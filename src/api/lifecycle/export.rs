use super::*;

// -------------------------------------------------------------------------
// Configuration export
// -------------------------------------------------------------------------

pub async fn export_config(
    State(state): State<AppState>,
    Query(query): Query<ExportConfigQuery>,
) -> AppResult<Json<ConfigDocument>> {
    Ok(Json(
        build_config_document(&state, query.include_secrets).await?,
    ))
}

/// Build the portable configuration document.
///
/// `include_secrets = false` masks every credential-shaped value instead of
/// omitting the field, so the file still documents *that* a credential exists
/// without being a secret store itself. Masked values are never applied by the
/// importer, so a masked round-trip cannot overwrite a real credential.
pub(crate) async fn build_config_document(
    state: &AppState,
    include_secrets: bool,
) -> AppResult<ConfigDocument> {
    let mut notes = vec![
        "网关 API Key 只保存哈希，导出文件仅包含名称与限额，导入时按“跳过”处理。".to_string(),
        "用量与审计历史属于运行时数据，不在导出范围内。".to_string(),
        "并发、内存、超时等上限来自环境变量，不在导出范围内。".to_string(),
    ];
    if include_secrets {
        notes.push("该文件包含明文上游凭据，请按密钥管理并限制访问权限。".to_string());
    } else {
        notes.push("上游凭据与凭据类请求头已掩码为 ***；导入时不会被覆盖。".to_string());
    }

    let settings = ConfigSettings {
        guardrails: stored_setting(state, crate::state::SETTING_GUARDRAILS, &mut notes).await?,
        inspector: stored_setting(state, crate::state::SETTING_INSPECTOR, &mut notes).await?,
        resilience: stored_setting(state, crate::state::SETTING_RESILIENCE, &mut notes).await?,
        usage_retention_days: stored_usage_retention_days(state).await?,
    };

    // Disabled models are not part of a `ProviderView`, so they are read
    // separately: without them a round-trip would silently re-enable models an
    // operator had turned off.
    let mut disabled_models = HashMap::<i64, Vec<String>>::new();
    for (provider_id, model_name) in sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, model_name FROM provider_models WHERE enabled = 0 \
         ORDER BY provider_id, model_name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    {
        disabled_models
            .entry(provider_id)
            .or_default()
            .push(model_name);
    }

    let mut provider_keys = HashMap::<i64, Vec<ConfigProviderKey>>::new();
    // Key names are not unique inside a provider (only the secret is), so the
    // document is built from one ordered row per key: collapsing the rows into
    // a name-keyed map would drop the secret of a second key with the same
    // name.
    for (
        provider_id,
        name,
        enabled,
        secret,
        max_concurrency,
        requests_per_minute,
        queue_limit,
        queue_wait_ms,
    ) in sqlx::query_as::<
        _,
        (
            i64,
            String,
            i64,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ),
    >(
        "SELECT provider_id, name, enabled, secret, max_concurrency, \
                requests_per_minute, queue_limit, queue_wait_ms \
         FROM provider_api_keys ORDER BY provider_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        provider_keys
            .entry(provider_id)
            .or_default()
            .push(ConfigProviderKey {
                name,
                enabled: enabled != 0,
                max_concurrency,
                requests_per_minute,
                queue_limit,
                queue_wait_ms,
                api_key: include_secrets.then_some(secret),
            });
    }

    let mut legacy_keys = HashMap::<i64, String>::new();
    if include_secrets {
        for (id, api_key) in sqlx::query_as::<_, (i64, String)>(
            "SELECT id, api_key FROM providers WHERE COALESCE(api_key, '') <> ''",
        )
        .fetch_all(&state.pool)
        .await?
        {
            legacy_keys.insert(id, api_key);
        }
    }

    let provider_rows = sqlx::query_as::<_, Provider>("SELECT * FROM providers ORDER BY id")
        .fetch_all(&state.pool)
        .await?;
    let mut views = provider_rows
        .into_iter()
        .map(ProviderView::from)
        .collect::<Vec<_>>();
    hydrate_provider_views(state, &mut views).await?;

    let providers = views
        .into_iter()
        .map(|view| ConfigProvider {
            name: view.name.clone(),
            provider_type: view.provider_type.as_str().to_string(),
            base_url: view.base_url.clone(),
            model_prefix: view.model_prefix.clone(),
            enabled: view.enabled,
            // Deliberately false: the document already carries the full model
            // list, so an import must not depend on reaching the provider.
            auto_sync_models: false,
            models: view.models.clone(),
            disabled_models: disabled_models.remove(&view.id).unwrap_or_default(),
            model_auto_include_rules: view.model_auto_include_rules.clone(),
            health_check_interval_minutes: view.health_check_interval_minutes,
            health_check_model: view.health_check_model.clone(),
            models_sync_interval_minutes: view.models_sync_interval_minutes,
            timeout_seconds: view.timeout_seconds,
            configured_cooldown_seconds: view.configured_cooldown_seconds,
            max_concurrency: view.max_concurrency,
            queue_timeout_seconds: view.queue_timeout_seconds,
            headers: if include_secrets {
                view.headers.clone()
            } else {
                redact_headers(&view.headers)
            },
            api_key_set: view.api_key_set,
            api_key: legacy_keys.remove(&view.id),
            provider_keys: provider_keys.remove(&view.id).unwrap_or_default(),
        })
        .collect();

    // Provider and gateway-key names are exported instead of ids so the file
    // can be imported into a database where the ids differ.
    let mut provider_names = HashMap::<i64, String>::new();
    for (id, name) in
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM providers ORDER BY id")
            .fetch_all(&state.pool)
            .await?
    {
        provider_names.insert(id, name);
    }
    let mut api_key_names = HashMap::<i64, String>::new();
    for (id, name) in
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM api_keys ORDER BY id")
            .fetch_all(&state.pool)
            .await?
    {
        api_key_names.insert(id, name);
    }

    let mut route_targets = HashMap::<i64, Vec<ConfigRouteTarget>>::new();
    for (route_id, provider_id, upstream_model, weight, priority, enabled) in
        sqlx::query_as::<_, (i64, i64, String, i64, i64, i64)>(
            "SELECT route_id, provider_id, upstream_model, weight, priority, enabled \
             FROM route_targets ORDER BY route_id, priority, id",
        )
        .fetch_all(&state.pool)
        .await?
    {
        route_targets
            .entry(route_id)
            .or_default()
            .push(ConfigRouteTarget {
                provider: provider_names
                    .get(&provider_id)
                    .cloned()
                    .unwrap_or_else(|| format!("provider #{provider_id}")),
                upstream_model,
                weight,
                priority,
                enabled: enabled != 0,
            });
    }

    let routes = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END, enabled, \
                context_override, output_override, \
                all_targets_busy_wait_seconds, retry_jitter_ms, \
                max_waiting_requests, keepalive_during_wait \
         FROM routes ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(
            id,
            name,
            model_pattern,
            strategy,
            enabled,
            context_override,
            output_override,
            all_targets_busy_wait_seconds,
            retry_jitter_ms,
            max_waiting_requests,
            keepalive_during_wait,
        )| {
            ConfigRoute {
                name,
                model_pattern,
                strategy,
                enabled: enabled != 0,
                context_override,
                output_override,
                all_targets_busy_wait_seconds,
                retry_jitter_ms,
                max_waiting_requests,
                keepalive_during_wait: keepalive_during_wait != 0,
                targets: route_targets.remove(&id).unwrap_or_default(),
            }
        },
    )
    .collect();

    let mut pool_allocations = HashMap::<i64, Vec<ConfigPoolAllocation>>::new();
    for (pool_id, api_key_id, weight_pct, policy) in sqlx::query_as::<_, (i64, i64, i64, String)>(
        "SELECT pool_id, api_key_id, weight_pct, policy \
         FROM provider_pool_allocations ORDER BY pool_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        pool_allocations
            .entry(pool_id)
            .or_default()
            .push(ConfigPoolAllocation {
                api_key: api_key_names
                    .get(&api_key_id)
                    .cloned()
                    .unwrap_or_else(|| format!("api key #{api_key_id}")),
                weight_pct,
                policy,
            });
    }

    let provider_pools =
        sqlx::query_as::<_, (i64, i64, String, i64, i64, Option<i64>, Option<i64>, i64)>(
            "SELECT id, provider_id, name, enabled, window_seconds, request_limit, token_limit, \
                saturation_threshold_pct \
         FROM provider_pools ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(
            |(
                id,
                provider_id,
                name,
                enabled,
                window_seconds,
                request_limit,
                token_limit,
                threshold,
            )| {
                ConfigPool {
                    provider: provider_names
                        .get(&provider_id)
                        .cloned()
                        .unwrap_or_else(|| format!("provider #{provider_id}")),
                    name,
                    enabled: enabled != 0,
                    window_seconds,
                    request_limit,
                    token_limit,
                    saturation_threshold_pct: threshold,
                    allocations: pool_allocations.remove(&id).unwrap_or_default(),
                }
            },
        )
        .collect();

    let gateway_keys = sqlx::query_as::<
        _,
        (
            String,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ),
    >(
        "SELECT name, enabled, allowed_models, daily_token_limit, daily_cost_limit_micros, \
                requests_per_minute, max_concurrency, expires_at \
         FROM api_keys ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(
            name,
            enabled,
            allowed_models,
            daily_token_limit,
            daily_cost_limit_micros,
            requests_per_minute,
            max_concurrency,
            expires_at,
        )| ConfigGatewayKey {
            name,
            enabled: enabled != 0,
            allowed_models: parse_string_list(allowed_models.as_deref().unwrap_or_default()),
            daily_token_limit,
            daily_cost_limit_micros,
            requests_per_minute,
            max_concurrency,
            expires_at,
        },
    )
    .collect();

    Ok(ConfigDocument {
        openllm_config_version: crate::models::CONFIG_DOCUMENT_VERSION,
        exported_at: Utc::now().to_rfc3339(),
        gateway_version: env!("CARGO_PKG_VERSION").to_string(),
        secrets_included: include_secrets,
        notes,
        settings,
        providers,
        routes,
        provider_pools,
        retired_webhooks: Vec::new(),
        gateway_keys,
    })
}

/// Read a settings row as JSON, keeping the difference between "absent" (the
/// built-in default applies) and "explicitly set to the default".
pub(super) async fn stored_setting<T: serde::de::DeserializeOwned>(
    state: &AppState,
    key: &str,
    notes: &mut Vec<String>,
) -> AppResult<Option<T>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&state.pool)
        .await?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            notes.push(format!("设置 {key} 无法解析，已跳过：{error}"));
            Ok(None)
        }
    }
}

pub(super) async fn stored_usage_retention_days(state: &AppState) -> AppResult<Option<i64>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(super::SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await?;
    Ok(raw.and_then(|raw| raw.trim().parse::<i64>().ok()))
}

pub(super) fn parse_json_or_empty(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

pub(super) fn parse_string_list(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}
