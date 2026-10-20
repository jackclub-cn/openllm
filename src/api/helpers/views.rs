use super::*;

pub(in crate::api) async fn get_provider(state: &AppState, id: i64) -> AppResult<ProviderView> {
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

pub(in crate::api) async fn get_route(state: &AppState, id: i64) -> AppResult<RouteView> {
    let route = sqlx::query_as::<_, Route>(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy, \
                enabled, context_override, output_override, \
                all_targets_busy_wait_seconds, retry_jitter_ms, \
                max_waiting_requests, keepalive_during_wait, \
                created_at, updated_at \
         FROM routes WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    route_view(state, route).await
}

pub(in crate::api) async fn route_view(state: &AppState, route: Route) -> AppResult<RouteView> {
    let targets = route_targets(state, Some(route.id)).await?;
    Ok(build_route_view(route, targets))
}

pub(in crate::api) async fn route_targets(
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
               p.timeout_seconds, p.cooldown_seconds,
               p.max_concurrency, p.queue_timeout_seconds,
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

pub(in crate::api) fn build_route_view(route: Route, targets: Vec<RouteTarget>) -> RouteView {
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
    let mut barrel = crate::registry::BarrelEnvelope {
        capabilities: ModelCapabilities::intersect(capabilities.iter()),
        incomplete,
        target_count: enabled_targets.len(),
    };
    let context_limit_base = barrel
        .capabilities
        .as_ref()
        .and_then(|caps| caps.total_context_tokens.or(caps.context_limit));
    let output_limit_base = barrel
        .capabilities
        .as_ref()
        .and_then(|caps| caps.output_limit);
    crate::registry::apply_route_capability_overrides(
        &mut barrel,
        route.context_override,
        route.output_override,
    );
    let capabilities = barrel.capabilities.unwrap_or_default();

    RouteView {
        id: route.id,
        name: route.name,
        model_pattern: route.model_pattern,
        strategy: route.strategy,
        enabled: route.enabled != 0,
        context_limit: capabilities
            .total_context_tokens
            .or(capabilities.context_limit),
        input_limit: capabilities.input_limit,
        output_limit: capabilities.output_limit,
        context_limit_base,
        output_limit_base,
        context_override: route.context_override,
        output_override: route.output_override,
        all_targets_busy_wait_seconds: route.all_targets_busy_wait_seconds,
        retry_jitter_ms: route.retry_jitter_ms,
        max_waiting_requests: route.max_waiting_requests,
        keepalive_during_wait: route.keepalive_during_wait != 0,
        limits_verified: !barrel.incomplete,
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
            routing_policy: parse_routing_policy(value.routing_policy.as_deref()),
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
            routing_policy: parse_routing_policy(value.routing_policy.as_deref()),
        }
    }
}
